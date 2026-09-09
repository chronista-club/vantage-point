//! DB の schema 定義と、起動時に DB を現行の形へ合わせる migration。
//!
//! [`VpDb::define_schema`] が [`SCHEMA_SQL`] を **1 クエリで**投げ、`.check()` で各
//! ステートメントのエラーまで見る。その後に旧 address の正規化を best-effort で回す。
//!
//! ## なぜ `SCHEMA_SQL` を domain ごとに割らないか
//!
//! - 1 クエリ + `.check()` という実行の形が、分割すると実行順と冪等性の検証を増やす。
//! - `wire_messages` / `agent_cursor` / `thread_participant` / `wire_acks` / `delegations`
//!   / `prompts` / `notifications` の **7 table は [`VpDb`] に method が 1 つも無い**。wire 4 table の
//!   owner は `capability/wiremsg_store.rs`、`delegations` は `capability/delegation_store.rs`。
//!   どちらも生の handle を受け取るだけで、[`VpDb::inner`] を呼ぶのは **store を組み立てる側**
//!   （`repo/server.rs` / `daemon/wire_ops.rs` 他）。**`prompts` / `notifications` は誰も触って
//!   いない**（dead schema）。いずれもここが唯一の定義点なので、domain module に割ると行き場を失う。
//!
//! ## 定義している table（18）
//!
//! グローバル: `processes` / `node_identity` / `repos` / `active_lane` / `lane` /
//! `lane_lifecycle` / `host_origin` / `host_lane_order` / `host_farewell`。
//! repo 固有（`repo_path` 列で分離）: `pane_contents` / `service_status` / `prompts` /
//! `notifications`。wire: `wire_messages` / `agent_cursor` / `thread_participant` /
//! `wire_acks`。delegation: `delegations`。
//!
//! 設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    /// スキーマを定義（全テーブル）
    ///
    /// 冪等: 既にテーブルが存在しても安全に実行できる。
    /// `.check()` で各ステートメントのエラーも検出する。
    pub async fn define_schema(&self) -> Result<()> {
        self.db
            .query(SCHEMA_SQL)
            .await
            .map_err(|e| anyhow::anyhow!("スキーマ定義失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("スキーマ定義エラー: {}", e))?;
        tracing::info!("SurrealDB スキーマ定義完了");
        self.normalize_legacy_lane_addresses().await;
        Ok(())
    }

    /// doc 44 P2: `lane` / `lane/lifecycle` の **address 文字列列**を新形へ正規化する（冪等）。
    ///
    /// フラット化で address の表示形が `<repo>/sub/<name>` → `<repo>/<name>` に
    /// 変わった。descriptor（object 列）は `LaneAddress` の serde default が吸収するが、
    /// **address を文字列 key として持つ列は吸収できない** — 旧形の行が残ると
    /// upsert（DELETE+CREATE の WHERE が新形で当たらない）が重複行を作り、
    /// lifecycle は照合できず孤児になる。
    ///
    /// 失敗しても起動は続ける（best-effort）。正規化できなかった行は旧形のまま残るだけで、
    /// 次回起動で再試行される。
    async fn normalize_legacy_lane_addresses(&self) {
        // ⚠️ **列名が table ごとに違う**。`active_lane` は `lane_address` なので、table 名だけの
        // 列挙では網から漏れる（doc 44 P2 の時点で実際に漏れており、旧形が残っていた）。
        // 「消えたか」でなく「address を持つ列が他に無いか」で数えること。
        for (table, column) in [
            ("lane", "address"),
            ("lane_lifecycle", "address"),
            ("active_lane", "lane_address"),
        ] {
            match self.normalize_lane_addresses_in(table, column).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("doc 44 P2: {} の旧形 address を {} 件正規化", table, n),
                Err(e) => {
                    tracing::warn!("{} の address 正規化に失敗（旧形のまま継続）: {}", table, e)
                }
            }
        }
    }

    /// 1 テーブル分の address 正規化。戻り値は書き換えた行数。
    async fn normalize_lane_addresses_in(&self, table: &str, column: &str) -> Result<usize> {
        let mut result = self
            .db
            .query(format!(
                "SELECT meta::id(id) AS rid, {column} AS address FROM {table}"
            ))
            .await?;
        let rows: Vec<serde_json::Value> = result.take(0)?;

        let mut fixed = 0;
        for row in rows {
            let (Some(rid), Some(old)) = (
                row.get("rid").and_then(|v| v.as_str()),
                row.get("address").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            // parse_address は旧 3 分節形を受理して新形に正規化する。
            let Some(new) = crate::repo::lane::parse_address(old)
                .map(|a| a.to_string())
                .filter(|new| new != old)
            else {
                continue;
            };
            // 1 行の失敗で他行を巻き込まない。典型的な失敗は
            // `(repo_path, address)` の UNIQUE 衝突（旧形と新形が同じ lane を指して
            // 両方残っているケース）で、これは当該行だけの問題。`?` で抜けると同じ
            // SELECT に載った**残り全行**の正規化が飛び、次回起動でも衝突源が在る限り
            // 毎回巻き添えになる（= 恒久的に旧形が残る）。
            let updated = self
                .db
                .query(format!(
                    "UPDATE type::record('{table}', $rid) SET {column} = $addr"
                ))
                .bind(("rid", rid.to_string()))
                .bind(("addr", new.clone()))
                .await
                .and_then(|mut r| r.take::<Vec<serde_json::Value>>(0));
            match updated {
                Ok(_) => fixed += 1,
                Err(e) => tracing::warn!(
                    "{}:{} の address 正規化に失敗（この行のみ旧形のまま継続、{} → {}）: {}",
                    table,
                    rid,
                    old,
                    new,
                    e
                ),
            }
        }
        Ok(fixed)
    }
}

// =============================================================================
// スキーマ定義 SQL
// =============================================================================

/// 全テーブルのスキーマ定義（冪等）
const SCHEMA_SQL: &str = r#"
-- =========================================================================
-- グローバルテーブル
-- =========================================================================

-- プロセス状態（QUIC Registry + HTTP polling 代替）
DEFINE TABLE IF NOT EXISTS processes SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS repo_name ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS port ON processes TYPE int;
DEFINE FIELD IF NOT EXISTS pid ON processes TYPE int;
DEFINE FIELD IF NOT EXISTS status ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS started_at ON processes TYPE datetime;
DEFINE FIELD IF NOT EXISTS agents ON processes TYPE option<object> FLEXIBLE;
DEFINE INDEX IF NOT EXISTS idx_processes_path ON processes COLUMNS repo_path UNIQUE;

-- home-node identity (federation L2、 ADR-020 D2): 位置独立な安定 id `nd_xxx`。
-- daemon が初回起動で 1 度だけ発行し db/machine に永続する singleton (固定 record id
-- node_identity:self、 index 不要)。machine/hostname/endpoint から独立で、 hub の routing
-- key になる。書き手は daemon 起動路のみ (doc 44 P1 PR4 で db は単一化されたが、 本 table を
-- 触るのは Daemon bootstrap だけなので daemon-canonical な truth であることは変わらない)。
DEFINE TABLE IF NOT EXISTS node_identity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS node_id ON node_identity TYPE string;
DEFINE FIELD IF NOT EXISTS created_at ON node_identity TYPE datetime DEFAULT time::now();

-- registered repos (PoC: VP-188 を revert し DB 真実源へ戻す)。
-- 当時 council (2026-05-16) が file に逃した理由は VP-182 (surrealkv の OS 排他
-- ロックで DB dir を分離 → DB dir 変更で repos 消失)。 本 PoC の仮説:
--   ① repos を **Daemon 専用 DB (db/machine) に限定** すれば repo は触らず LOCK 衝突なし
--   ② DB 消失耐性 + 人間可読性は repos.kdl への **一方向 export** で担保
-- ord = sidebar 並び順 (repos.kdl の node 出現順を保持)。
DEFINE TABLE IF NOT EXISTS repos SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS path ON repos TYPE string;
DEFINE FIELD IF NOT EXISTS name ON repos TYPE string;
DEFINE FIELD IF NOT EXISTS enabled ON repos TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS slot ON repos TYPE option<int>;
DEFINE FIELD IF NOT EXISTS ord ON repos TYPE int DEFAULT 0;
DEFINE INDEX IF NOT EXISTS idx_repos_path ON repos COLUMNS path UNIQUE;

-- active lane (presence、 Model Q): repo ごとの選択中 lane。 daemon-canonical。
-- presence なので repos とは別テーブル (repos.kdl export に混ぜず、 click ごとの
-- 高頻度 upsert を 1 行に閉じる)。 §4.6 durability tier: presence は tail-loss 許容。
DEFINE TABLE IF NOT EXISTS active_lane SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON active_lane TYPE string;
DEFINE FIELD IF NOT EXISTS lane_address ON active_lane TYPE string;
DEFINE FIELD IF NOT EXISTS updated_at ON active_lane TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_active_lane_path ON active_lane COLUMNS repo_path UNIQUE;

-- lane descriptor (doc 24 §10 Phase 2: LanePool authority を repo→daemon に反転)。
-- 旧来 lane_registry は「repo push の in-memory cache、 repo disconnect で全 drop」だったが、
-- これを daemon-canonical な **durable truth** にする。 repo が落ちても descriptor は残り
-- (§4.1 app quit = 喪失ゼロ)、 daemon 再起動は db から re-animate する (§3.3)。
--   descriptor = LaneInfo を丸ごと持つ FLEXIBLE object (descriptor truth、 pane_contents.stack 前例)。
--     (列名 `info` は SurrealQL 予約語 `INFO` と衝突するため `descriptor` を使う)
--   key       = (repo_path, address) 複合 UNIQUE (1 repo 内で lane address は一意)。
-- §4.6 durability tier: descriptor は堅く durable / live 値 (pid/state) は projection なので
-- boot-load 値が stale でも repo reconnect の snapshot が上書きする (= 正直な tier 分け)。
DEFINE TABLE IF NOT EXISTS lane SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON lane TYPE string;
DEFINE FIELD IF NOT EXISTS address ON lane TYPE string;
DEFINE FIELD IF NOT EXISTS descriptor ON lane TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON lane TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_lane_addr ON lane COLUMNS repo_path, address UNIQUE;

-- lane lifecycle (doc 24 §4.6: daemon 堅牢化の durable lifecycle state machine = 軽量 WAL)。
-- provisioning / ready / dead を **descriptor (lane table) とは別テーブル** に持つ。 分離理由:
-- descriptor は repo が push で round-trip するため、 repo snapshot (lifecycle 未知=default) が
-- daemon の `provisioning` intent を clobber してしまう。 lifecycle は daemon-internal な
-- crash-recovery state なので、 active_lane (presence) と同じく独立 table にする。
-- process liveness (LaneInfo.state) とも別軸 (= ground の lifecycle、 PtySlot の生死ではない)。
-- intent-first bracket: create は descriptor+provisioning を先に書く → worktree provision →
-- ready。 crash で provisioning が残れば boot reconcile が ground 存在で heal する。
DEFINE TABLE IF NOT EXISTS lane_lifecycle SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON lane_lifecycle TYPE string;
DEFINE FIELD IF NOT EXISTS address ON lane_lifecycle TYPE string;
DEFINE FIELD IF NOT EXISTS lifecycle ON lane_lifecycle TYPE string;
DEFINE FIELD IF NOT EXISTS updated_at ON lane_lifecycle TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_lane_lifecycle_addr ON lane_lifecycle COLUMNS repo_path, address UNIQUE;

-- Repo Host の帳簿①: 開発起点ポインタ (doc 44 D4 / §8)。
-- 「この repo の開発の起点はどの lane か」を Host が 1 本だけ持つ。
--
-- ⚠️ active_lane (注視) とは別物 — D5 が明示的に分けている:
--   active_lane = 今どの lane を見ているか (presence、click ごとに動く)
--   host_origin = 開発の起点はどこか       (intent、明示的に指定した時だけ動く)
--
-- key が address 文字列ではなく **lane_id (UUID)** なのは、将来 lane 名を変えられるように
-- するため。名前は表示のための自然キーで、rename で動く。ポインタが指すのは lane そのもの
-- なので surrogate key で持つ (doc 44 §8.2)。行が無い / 指す lane が実在しない場合は
-- 予約名 `main` にフォールバックする (= 従来挙動、`ledger::resolve_origin_name`)。
DEFINE TABLE IF NOT EXISTS host_origin SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON host_origin TYPE string;
DEFINE FIELD IF NOT EXISTS lane_id ON host_origin TYPE string;
DEFINE FIELD IF NOT EXISTS updated_at ON host_origin TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_host_origin_path ON host_origin COLUMNS repo_path UNIQUE;

-- Repo Host の帳簿②: lane の並び順 (doc 44 D5 / §12)。
--
-- ⚠️ `lane` table には置けない — `upsert_lane` が DELETE+CREATE なので、repo/repo 由来の
-- descriptor push が来るたびに ord が消える。`lane/lifecycle` を別 table にしたのと同じ理由で、
-- 「Host の intent」と「lane が報告する state」は table を分ける。
--
-- key は `host_origin` と同じく **lane_id (UUID)**。並び順は lane そのものに付く指定なので、
-- 表示名が変わっても動いてはいけない (doc 44 §8.2)。
-- 行が無い lane は「未指定」= 既定順 (開発起点が先頭 → created_at) の末尾に付く。
DEFINE TABLE IF NOT EXISTS host_lane_order SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON host_lane_order TYPE string;
DEFINE FIELD IF NOT EXISTS lane_id ON host_lane_order TYPE string;
DEFINE FIELD IF NOT EXISTS ord ON host_lane_order TYPE int;
DEFINE FIELD IF NOT EXISTS updated_at ON host_lane_order TYPE datetime;
DEFINE INDEX IF NOT EXISTS idx_host_lane_order ON host_lane_order COLUMNS repo_path, lane_id UNIQUE;

-- Repo Host の帳簿③: 見送りの記録 (doc 44 §7.5 / §8.5)。
--
-- 帳簿に書くのは **計算で復元できない事実だけ** という規律で 2 種類に絞ってある:
--   kind='reclaimed' = 実際に見送った (lane を消したので survey では二度と再現できない)
--   kind='pending'   = AskHuman の滞留。「今 AskHuman か」は survey が都度計算できるが、
--                      「いつから」「何回目」は観測の履歴なので計算では出てこない
-- Keep と「判定だけの Reclaim」を書かないのは、どちらも次の survey で同じ答えが出るため。
--
-- ⚠️ 同じ判定の**連続は 1 行に畳む** (streak + first_seen_at)。観測ごとに行を足すと
-- `vp lane cleanup` を走らせるほど帳簿が太り、放置された lane ほど重くなる (滞留を追う
-- ための表が滞留で壊れる)。行数は「判定が変わった回数」に比例し、実行回数には比例しない。
--
-- key は host_origin / host_lane_order と同じ **lane_id (UUID)**。lane_name は
-- **記録時点のスナップショット**で、後から更新しない (履歴が rename で書き換わらない)。
-- lane 削除時に lane_ids state file も消えるので、同名 lane を作り直すと別 id になり
-- 前の履歴と混ざらない。
--
-- 時刻は datetime ではなく **RFC3339 の string**。記録時刻は純関数に注入してテストで
-- 固定する事実なので、DB 側の time::now() ではなく呼び出し側が渡す (UTC 表記なので
-- 文字列の辞書順 = 時系列順)。
DEFINE TABLE IF NOT EXISTS host_farewell SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON host_farewell TYPE string;
DEFINE FIELD IF NOT EXISTS lane_id ON host_farewell TYPE string;
DEFINE FIELD IF NOT EXISTS lane_name ON host_farewell TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON host_farewell TYPE string;
DEFINE FIELD IF NOT EXISTS reason ON host_farewell TYPE string DEFAULT '';
DEFINE FIELD IF NOT EXISTS streak ON host_farewell TYPE int DEFAULT 1;
DEFINE FIELD IF NOT EXISTS first_seen_at ON host_farewell TYPE string;
DEFINE FIELD IF NOT EXISTS last_seen_at ON host_farewell TYPE string;
DEFINE FIELD IF NOT EXISTS ongoing ON host_farewell TYPE bool DEFAULT false;
DEFINE INDEX IF NOT EXISTS idx_host_farewell_lane ON host_farewell COLUMNS repo_path, lane_id;

-- wiremsg R6: 旧 msgbox table (VP-169 以前の cross-process メッセージング) は撤去。
-- agent 間通信は wiremsg (下記 wire_messages table) に一本化済。
-- R5-3 で VP-169 msgs table、 R6 で本 table を撤去し msgbox 系が完全消滅した。

-- =========================================================================
-- repo 固有テーブル（repo_path でフィルタ — D11 準拠）
--
-- doc 44 P1 PR4 (DB 統合): 旧称「repo 固有テーブル」。repo プロセス時代は per-repo DB
-- (`db/sp_{slug}/`) に置かれ、1 DB = 1 repo だったため repo_path 列は事実上
-- 冗長だった。db 単一化で、この列が唯一の repo 次元になる（= 全クエリが
-- `WHERE repo_path = $path` で絞る前提。これを欠くと他 repo の行を掴む）。
-- =========================================================================

-- Canvas ペイン状態（board Canvas Stack Model 永続化、 doc 19）
--
-- 2026-05-28 [pp-content-persist]:
--   lane scope 対応 — 旧 idx_pane (repo_path, pane_id) を
--   (repo_path, lane_name, pane_id) UNIQUE に置換。 lane_name="" が main、
--   "<name>" が sub。 同一 repo の main と sub は **独立した board state** を持つ。
--   追加 field:
--     - lane_name: string DEFAULT ''       — lane scope key (空文字=main / 非空=sub 名)
--     - stack:     option<object> FLEXIBLE — Canvas Stack { items: [], cursor: id, capacity: 10 }
--     - ui_state:  option<object> FLEXIBLE — { visible, collapsed, width, height }
--   注: lane_name を **option ではなく DEFAULT 空文字** にしたのは、 SurrealDB の UNIQUE INDEX が
--   NULL 同士を不一致扱いし、 (path, NONE, pane_id) の UNIQUE 制約が成立せず ON DUPLICATE が
--   発火しないため。 IPC contract 上は lane: string|null を保ち、 Rust 側で null↔'' を変換。
--   旧 record (lane_name 不在) は schema DEFAULT '' で self-heal してそのまま main 扱いになる。
REMOVE INDEX IF EXISTS idx_pane ON pane_contents;
DEFINE TABLE IF NOT EXISTS pane_contents SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS pane_id ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS content_type ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS content ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS title ON pane_contents TYPE option<string>;
DEFINE FIELD IF NOT EXISTS lane_name ON pane_contents TYPE string DEFAULT '';
-- board モデル (2026-07-15): scope 軸を追加し (repo_path, scope, lane_name, pane_id) で board を
--   分離する。 scope='lane' が lane board (lane_name で lane ごとに独立)、 'proj' が repo 共有 board
--   (lane_name='')。 旧 record (scope 不在) は DEFAULT 'lane' で self-heal され、 既存 lane/root
--   board を現挙動のまま保存する。 現状の scope は lane/proj の 2 つ。
--   (doc 44 P1 PR4 まで「将来の 'vp'(全体 board) は別 DB 行き」と書かれていたが、 db 単一化で
--    その制約は消えた — 全体 board を足すなら repo_path を跨ぐ scope 値を 1 つ増やすだけで済む。)
DEFINE FIELD IF NOT EXISTS scope ON pane_contents TYPE string DEFAULT 'lane';
DEFINE FIELD IF NOT EXISTS stack ON pane_contents TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS ui_state ON pane_contents TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON pane_contents TYPE datetime DEFAULT time::now();
-- 旧 UNIQUE (repo_path, lane_name, pane_id) を破棄し scope を含む新 index に置換。
REMOVE INDEX IF EXISTS idx_pane_lane ON pane_contents;
DEFINE INDEX IF NOT EXISTS idx_pane_scope ON pane_contents COLUMNS repo_path, scope, lane_name, pane_id UNIQUE;

-- Agent ステータス
DEFINE TABLE IF NOT EXISTS service_status SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON service_status TYPE string;
DEFINE FIELD IF NOT EXISTS agent_key ON service_status TYPE string;
DEFINE FIELD IF NOT EXISTS status ON service_status TYPE string;
DEFINE FIELD IF NOT EXISTS detail ON service_status TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON service_status TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_stand ON service_status COLUMNS repo_path, agent_key UNIQUE;

-- User Prompt（2秒ポーリング廃止）
DEFINE TABLE IF NOT EXISTS prompts SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS request_id ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS prompt_type ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS title ON prompts TYPE string;
DEFINE FIELD IF NOT EXISTS description ON prompts TYPE option<string>;
DEFINE FIELD IF NOT EXISTS options ON prompts TYPE option<array>;
DEFINE FIELD IF NOT EXISTS timeout_seconds ON prompts TYPE int;
DEFINE FIELD IF NOT EXISTS response ON prompts TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS created_at ON prompts TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_request ON prompts COLUMNS request_id UNIQUE;

-- CC 通知（DistributedNotification 代替）
DEFINE TABLE IF NOT EXISTS notifications SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS repo_path ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS repo_name ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS message ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS read ON notifications TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS created_at ON notifications TYPE datetime DEFAULT time::now();

-- =========================================================================
-- wiremsg R5-3: 旧 VP-169 msgs table (file-backed msgbox) は撤去。
-- msg messaging は下記 wiremsg threaded inbox (messages table) に一本化済。
-- =========================================================================

-- =========================================================================
-- wiremsg threaded inbox (Phase A ① / R1、 設計 memory mem_1CbDLrECNZiNEZqjySLfSB)
-- =========================================================================
-- 既存 msgs table (= Mailbox の claim-based inbox) と並走する threading 対応 inbox。
-- `wire_send` / `wire_recv` が直接 long-poll する store。 TopicRouter は介さない。
--
-- 設計判断: `prev` は record link ではなく plain string (= message の local id) で
-- 保持する。 理由:
--   1. 既存 msgs table の `id` / `reply_to` も plain string で、 同型を踏襲
--   2. record-link traversal を query で使うと migration / 部分適用で壊れやすい
--      (creo-memories mem: 「migration の data-UPDATE 句は record-link traversal を避ける」)
-- `created_at` も datetime ではなく epoch ms (number) で保持
-- (= msgs.ts と同じ表現、 thread 内表示順の比較を素直な数値比較にする)。
--
-- R1 (決定 thread_id 全廃 / cursor local-seq 化):
--   - `thread_id` field を全廃。 thread 構造は `prev` (parent-pointer forest) 一本。
--     thread の識別子が要る場面では root message の id (`prev` を辿った先) を使う。
--   - `local_seq` を追加。 ローカル accumulation の厳密単調 ingestion 順序 (number)。
--     各 repo は自分の accumulation の唯一の writer なので厳密単調。 cursor 比較は
--     この `local_seq` で行う (`created_at` は同一 ms 衝突や clock skew で取りこぼす)。
-- 既存 DB の旧 schema 残骸を除去 (thread_id field / wire_thread_idx index)。
-- wiremsg は Phase A 新設で deployed data はごく僅か。
REMOVE INDEX IF EXISTS wire_thread_idx ON wire_messages;
REMOVE FIELD IF EXISTS thread_id ON wire_messages;
DEFINE TABLE IF NOT EXISTS wire_messages SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS id ON wire_messages TYPE string;
DEFINE FIELD IF NOT EXISTS prev ON wire_messages TYPE option<string>;
DEFINE FIELD IF NOT EXISTS from_addr ON wire_messages TYPE string;
DEFINE FIELD IF NOT EXISTS to_addrs ON wire_messages TYPE array<string>;
DEFINE FIELD IF NOT EXISTS body ON wire_messages TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS created_at ON wire_messages TYPE number;
-- ローカル accumulation の厳密単調 ingestion 順序。 cursor 比較の基準。
DEFINE FIELD IF NOT EXISTS local_seq ON wire_messages TYPE number;
-- 主 query path index: 「agent 宛 message を cursor 超過で引く」 (to ベース配送)。
-- 旧 wire_thread_idx (thread_id, created_at) の置き換え (moody #4)。
DEFINE INDEX IF NOT EXISTS wire_to_seq_idx ON wire_messages FIELDS to_addrs, local_seq;

-- per-agent 単一 cursor (決定 III)。 1 agent 1 行 = O(agents)。
-- `last_read` = 最後に読んだ message の local_seq。 NONE = 全 message 未読。
-- 配送は wire_messages.to_addrs から創発する (= to ベース配送)。
DEFINE TABLE IF NOT EXISTS agent_cursor SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS agent ON agent_cursor TYPE string;
DEFINE FIELD IF NOT EXISTS last_read ON agent_cursor TYPE option<number>;
DEFINE FIELD IF NOT EXISTS updated_at ON agent_cursor TYPE number;
DEFINE INDEX IF NOT EXISTS agent_cursor_uniq ON agent_cursor FIELDS agent UNIQUE;

-- thread 参加の sparse 例外表 (決定 III)。 status ∈ {muted, left} の行のみ持つ。
-- default (active) は行を持たない — active 参加は wire_messages.to_addrs から創発。
-- 行数 = O(mute・leave 回数)。
-- R1: `thread` field は thread の root message id (`prev` を辿った先)。
-- thread_id 全廃のため denormalize copy ではなく root id そのものを使う。
DEFINE TABLE IF NOT EXISTS thread_participant SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS thread ON thread_participant TYPE string;
DEFINE FIELD IF NOT EXISTS agent ON thread_participant TYPE string;
DEFINE FIELD IF NOT EXISTS status ON thread_participant TYPE string DEFAULT 'active';
DEFINE FIELD IF NOT EXISTS updated_at ON thread_participant TYPE number;
-- (thread, agent) で一意
DEFINE INDEX IF NOT EXISTS thread_participant_uniq ON thread_participant FIELDS thread, agent UNIQUE;
DEFINE INDEX IF NOT EXISTS thread_participant_agent_idx ON thread_participant FIELDS agent, status;

-- wiremsg R2-a (設計 mem_1CbvcJj4ppU3QKH9d7xMpT 決定 D3): per-message ack 台帳。
-- command category の「読まれた」確認用。cursor (agent_cursor) とは独立で、
-- recv で cursor が進んでも wire_ack されるまで delivery loop (R2-b) の再掲示対象。
DEFINE TABLE IF NOT EXISTS wire_acks SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS message_id ON wire_acks TYPE string;
DEFINE FIELD IF NOT EXISTS agent ON wire_acks TYPE string;
DEFINE FIELD IF NOT EXISTS acked_at ON wire_acks TYPE number;
DEFINE INDEX IF NOT EXISTS wire_acks_uniq ON wire_acks FIELDS message_id, agent UNIQUE;

-- agent 委譲 (delegation、 doc 28 §4 / §6): durable cross-agent future の daemon 中央 store。
-- wire と同じく daemon の SurrealDB に持つ (= repo 再起動を跨いで生存、 Daemon reconcile の駆動源)。
-- requester / doer は論理 wire address。 state ∈ {pending, active, awaiting_response, done, failed}。
-- outcome = {kind, result|reason|question} (= Outcome の serde 形)。 created_at/updated_at は ms
-- (B reconcile の timeout 判定用)。 delivered = 直近 wake が target に届いたか (B/C の取りこぼし検出用)。
DEFINE TABLE IF NOT EXISTS delegations SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS id ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS requester ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS doer ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS task ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS state ON delegations TYPE string;
DEFINE FIELD IF NOT EXISTS outcome ON delegations TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS created_at ON delegations TYPE number;
DEFINE FIELD IF NOT EXISTS updated_at ON delegations TYPE number;
DEFINE FIELD IF NOT EXISTS delivered ON delegations TYPE bool DEFAULT false;
DEFINE INDEX IF NOT EXISTS delegations_id_idx ON delegations FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS delegations_state_idx ON delegations FIELDS state;
"#;

#[cfg(test)]
mod tests {
    use crate::db::{VpDb, make_test_db};
    /// doc 44 P2: 旧形の address 文字列（`<repo>/sub/<name>`）が起動時に新形へ
    /// 正規化されること。
    ///
    /// これを怠ると実害が出る: `lane` は upsert（DELETE+CREATE）の WHERE が新形で当たらず
    /// **旧形の行が残って重複**し、`lane/lifecycle` は照合できず**孤児**になる。
    /// descriptor（object 列）は `LaneAddress` の serde default が吸収するが、
    /// address を文字列 key として持つ列はそれでは救えない。
    #[tokio::test]
    async fn legacy_address_strings_are_normalized_on_schema_define() {
        let db = VpDb::connect_mem().await.unwrap();
        db.define_schema().await.unwrap();

        // 旧形の行を直に流し込む（P2 以前の永続状態を再現）
        db.inner()
            .query(
                "CREATE lane_lifecycle CONTENT {
                     repo_path: '/repos/vp', address: 'vp/sub/foo',
                     lifecycle: 'ready', updated_at: time::now()
                 };
                 CREATE lane_lifecycle CONTENT {
                     repo_path: '/repos/vp', address: 'vp/root',
                     lifecycle: 'ready', updated_at: time::now()
                 };",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        // define_schema が正規化を走らせる（冪等なので 2 度目も安全）
        db.define_schema().await.unwrap();

        let rows = db.list_lane_lifecycles().await.unwrap();
        let addrs: Vec<&str> = rows.iter().map(|(_, a, _)| a.as_str()).collect();
        // ⚠️ canonical は `<repo>/lane/<name>`。旧 3 分節（sub）も旧 2 分節（フラット）も
        // ここへ寄せる — 2 世代ぶんの旧形が永続に残っているため。
        assert!(
            addrs.contains(&"vp/lane/foo"),
            "旧形 vp/sub/foo は canonical へ正規化されるべき: {addrs:?}"
        );
        assert!(
            !addrs.contains(&"vp/sub/foo") && !addrs.contains(&"vp/foo"),
            "旧形が残ってはならない（孤児化する）: {addrs:?}"
        );
        assert!(
            addrs.contains(&"vp/lane/main"),
            "旧 2 分節 vp/root は canonical + 新予約名（main）へ寄る: {addrs:?}"
        );
    }

    /// ⚠️ **`active_lane` も正規化の対象**（列名が `lane_address` で違う）。
    ///
    /// doc 44 P2 の migration は `for table in ["lane", "lane_lifecycle"]` で列名 `address`
    /// 固定だったため、この table だけ**網から漏れて旧形が残っていた**。table を足すときは
    /// 「address を持つ列が他に無いか」で数えること。
    #[tokio::test]
    async fn active_lane_address_is_normalized_too() {
        let db = VpDb::connect_mem().await.unwrap();
        db.define_schema().await.unwrap();
        db.inner()
            .query(
                "CREATE active_lane CONTENT {
                     repo_path: '/repos/vp', lane_address: 'vp/sub/foo', updated_at: time::now()
                 };",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        db.define_schema().await.unwrap();

        let rows = db.list_active_lanes().await.unwrap();
        let addrs: Vec<&str> = rows.iter().map(|(_, a)| a.as_str()).collect();
        assert!(
            addrs.contains(&"vp/lane/foo"),
            "active_lane の旧形が canonical へ正規化されるべき: {addrs:?}"
        );
    }

    #[tokio::test]
    async fn test_define_schema_mem() {
        let db = make_test_db().await;
        assert!(db.health().await, "ヘルスチェック失敗");
    }

    // =========================================================================
    // define_schema 冪等性テスト
    // =========================================================================

    /// define_schema を2回呼び出しても失敗しない（IF NOT EXISTS を検証）
    #[tokio::test]
    async fn test_define_schema_idempotent() {
        let db = VpDb::connect_mem().await.unwrap();
        // 1回目
        db.define_schema().await.unwrap();
        // 2回目: DEFINE TABLE IF NOT EXISTS / DEFINE FIELD IF NOT EXISTS があるため再定義でもエラーにならない
        db.define_schema()
            .await
            .expect("2回目の define_schema が失敗してはいけない");
        assert!(
            db.health().await,
            "2回目の define_schema 後もヘルスチェックが通る"
        );
    }
}
