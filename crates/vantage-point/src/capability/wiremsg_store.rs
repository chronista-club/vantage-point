//! wiremsg threaded inbox store (設計 memory `mem_1CbDLrECNZiNEZqjySLfSB`)
//!
//! agent 間メッセージング「wiremsg」の inbox 実体。 既存 [`WhitesnakeStore`] (= msgs table,
//! claim-based Mailbox) と **並存** する threading 対応 store。 撤去は後続 Phase。
//!
//! ## per-reader state = per-agent 単一 cursor (決定 III)
//!
//! per-reader state を irreducible な最小核に絞る (「derive できるものは store しない」):
//!
//! - **cursor は per-agent 単一** — `agent_cursor { agent, last_read }`、 1 agent 1 行
//!   (O(agents))。 旧 `thread_participant.read_cursor` (per thread×agent、 O(agents×threads))
//!   を廃止。
//! - **配送は `to` ベース** — `fetch_unread(agent)` = `wire_messages` で
//!   `agent ∈ to_addrs` AND `local_seq > last_read`。 旧 participation ベース
//!   (`thread_participant` を引いて thread ごとに引く) を廃止。
//! - **`thread_participant` は sparse 例外表** — `status` ∈ {muted, left} の行のみ持つ。
//!   default (active) は行を持たない。 active 参加は message の `to_addrs` から創発。
//! - **derive されるもの** — per-thread unread 数 ([`unread_count_by_thread`](WiremsgStore::unread_count_by_thread))。
//!
//! ## R1 — cursor local-seq 化 / thread_id 全廃
//!
//! - **cursor は `local_seq`** (決定 F4-2 / moody #1 解消) — 旧 cursor は message の
//!   `created_at` (epoch ms) で `fetch_unread` は `created_at > cursor` だった。 同一 ms
//!   衝突や cross-process clock skew で message を取りこぼした。 R1 では `local_seq` という
//!   **ローカル accumulation の厳密単調 ingestion 順序** を持ち、 cursor 比較を
//!   `local_seq > cursor` にする。 採番は [`WiremsgStore`] が持つ `Arc<AtomicU64>` の
//!   `fetch_add(1)` (INSERT 毎)。 起動時に `math::max(local_seq)` で復元する
//!   ([`WiremsgStore::new`])。 各 SP は自分の accumulation の唯一の writer なので厳密単調。
//! - **`thread_id` 全廃** (決定 `mem_1CbDSnSTPkfQyJsfEcf5ea`) — `thread_id` は root id の
//!   denormalize copy で `prev` から derive 可能。 全廃し、 thread 構造は `prev`
//!   (parent-pointer forest) 一本にした。 「thread の識別子」が要る場面では root message
//!   の id (`prev` を辿った先、 [`walk_to_root`](WiremsgStore::walk_to_root)) を使う。
//! - **left 強制を send 側へ** — 旧モデルは recv 側で left thread の message を filter
//!   していた (moody #5: record-link 取りこぼし)。 R1 では `send_reply` が reply-all
//!   展開時に left agent を `to` から外す (= 「以後 ping を受けない」)。 recv 側 filter は
//!   削除され、 `fetch_unread` は純粋な `to_addrs ∋ agent AND local_seq > cursor` になった。
//!
//! ## 設計判断
//!
//! - **TopicRouter を使わない**: inbox = SurrealDB の message store。 `wire_recv` がその
//!   store を直接 long-poll する (= 既存 `msg_recv` / `WhitesnakeStore.claim` と同型)。
//! - **`prev` は record link でなく plain string** (= message の local id)。 既存 msgs
//!   table の `id` / `reply_to` も plain string で同型、 record-link traversal は
//!   migration 部分適用で壊れやすい (creo-memories の教訓)。
//! - **`created_at` は epoch ms (number)**: 既存 msgs.ts と同じ表現。 thread 内表示順の
//!   ためだけに残し、 cursor 比較には使わない (cursor は `local_seq`)。
//! - **id は uuidv7**: 時刻順 sortable id (= ULID 相当)。 `uuid` crate の `now_v7()`。
//!
//! ## table
//!
//! - `wire_messages`: message 本体 (`prev` / `from_addr` / `to_addrs` / `body` /
//!   `created_at` / `local_seq`)
//! - `agent_cursor`: per-agent 単一既読 cursor (`agent` / `last_read` = local_seq)
//! - `thread_participant`: mute/left の sparse 例外表 (`thread` = root id / `agent` / `status`)
//!
//! schema は `db/mod.rs` の `SCHEMA_SQL` で define 済。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use tokio::sync::{Mutex, Notify};

/// 現在時刻 (Unix epoch ms)
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 時刻順 sortable id を生成 (uuidv7 = ULID 相当)
fn new_message_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

// =============================================================================
// WireMessage — wiremsg の message data type
// =============================================================================

/// wiremsg thread に属する 1 件の message
///
/// `wire_messages` table の row に 1:1 対応する。 `id` は時刻順 (uuidv7)。
/// `prev` は親 message id (`None` = thread の root)。 thread 構造は `prev` 一本で表す
/// (R1 で `thread_id` 全廃 — root id が要れば `prev` を辿る)。
/// `local_seq` は accumulation の厳密単調 ingestion 順序 (cursor 比較の基準)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WireMessage {
    /// message id (uuidv7、 時刻順 sortable)
    pub id: String,
    /// 親 message id。 `None` = thread の root
    #[serde(default)]
    pub prev: Option<String>,
    /// 送信 agent address
    pub from: String,
    /// 宛先 agent address 群
    pub to: Vec<String>,
    /// message 本文 (任意 JSON object)
    pub body: serde_json::Value,
    /// 作成時刻 (Unix epoch ms)。 thread 内表示順の比較用 (cursor には使わない)
    pub created_at: u64,
    /// ローカル accumulation の厳密単調 ingestion 順序。 cursor 比較の基準。
    /// INSERT 時に store が採番する (`new_root` / `new_reply` 直後は 0、 INSERT で確定)。
    #[serde(default)]
    pub local_seq: u64,
}

impl WireMessage {
    /// 新規 thread の root message を構築 (`prev = None`)
    ///
    /// `local_seq` は 0 で構築し、 [`WiremsgStore::insert_message`] が INSERT 時に採番する。
    pub fn new_root(from: impl Into<String>, to: Vec<String>, body: serde_json::Value) -> Self {
        Self {
            id: new_message_id(),
            prev: None,
            from: from.into(),
            to,
            body,
            created_at: now_ms(),
            local_seq: 0,
        }
    }

    /// 既存 thread への reply message を構築
    ///
    /// `prev` = 返信先 message id。 thread 構造は `prev` 一本で表す (R1)。
    /// `local_seq` は 0 で構築し、 INSERT 時に採番する。
    pub fn new_reply(
        from: impl Into<String>,
        to: Vec<String>,
        body: serde_json::Value,
        prev_id: impl Into<String>,
    ) -> Self {
        Self {
            id: new_message_id(),
            prev: Some(prev_id.into()),
            from: from.into(),
            to,
            body,
            created_at: now_ms(),
            local_seq: 0,
        }
    }
}

// =============================================================================
// ParticipantStatus
// =============================================================================

/// thread 参加の sparse 例外表 (`thread_participant.status`) が取りうる状態
///
/// 決定 III: `thread_participant` は mute/left の sparse 例外表に縮小された。
/// default (active) は **行を持たない** ため、 行が存在する = `Muted` か `Left`。
/// `Active` は「行が無い」状態の論理的対概念として残す (mute/leave 操作 tool を
/// 足す後続 Phase で enum → 文字列の変換が要れば拡張する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantStatus {
    /// 参加中 (例外表に行を持たない default 状態)
    Active,
    /// ミュート中 (recv 対象だが notify は鳴らさない想定、 操作 tool は後続 Phase)
    Muted,
    /// 離脱済 (以後の reply の `to` に入らない、 REQ-THREAD-007)
    Left,
}

// =============================================================================
// WiremsgStore — SurrealDB embedded impl
// =============================================================================

/// wiremsg threaded inbox の store (決定 III — per-agent 単一 cursor)
///
/// 既存 [`WhitesnakeStore`](super::WhitesnakeStore) と同じく `Surreal<Any>` を共有して
/// 持つ。 `wire_messages` / `agent_cursor` / `thread_participant` の 3 table を扱う。
///
/// `seq` は accumulation の `local_seq` 採番器。 各 SP は自分の accumulation の唯一の
/// writer なので、 INSERT 毎の `fetch_add(1)` で厳密単調な ingestion 順序が得られる。
/// 起動時に既存 `wire_messages` の `math::max(local_seq)` で復元する ([`WiremsgStore::new`])。
#[derive(Clone)]
pub struct WiremsgStore {
    db: Arc<Surreal<Any>>,
    /// `local_seq` 採番器 — 起動時に既存最大値で復元、 INSERT 毎に `fetch_add(1)`
    seq: Arc<AtomicU64>,
}

impl WiremsgStore {
    /// 既存 `Surreal<Any>` connection から store を構築
    ///
    /// 起動時に `wire_messages` の `math::max(local_seq)` を引き、 `local_seq` 採番器を
    /// 復元する (空 table なら 0 起点)。 R1 で `local_seq` を導入したため、 SP プロセスの
    /// 再起動を跨いでも accumulation の ingestion 順序が厳密単調に続く。
    pub async fn new(db: Arc<Surreal<Any>>) -> Result<Self> {
        let max = Self::query_max_local_seq(&db).await?;
        Ok(Self {
            db,
            seq: Arc::new(AtomicU64::new(max)),
        })
    }

    /// `wire_messages` の `local_seq` 最大値を引く (空 table なら 0)
    async fn query_max_local_seq(db: &Surreal<Any>) -> Result<u64> {
        let mut res = db
            .query("SELECT VALUE math::max(local_seq) FROM wire_messages GROUP ALL;")
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg query_max_local_seq failed: {e}"))?;
        // `SELECT VALUE math::max(...) ... GROUP ALL` は 1 要素の配列を返す。
        // 空 table のときは NONE (= JSON null) になりうるため or_default で 0 起点に。
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg query_max_local_seq take failed: {e}"))?;
        Ok(rows.first().and_then(|v| v.as_u64()).unwrap_or(0))
    }

    /// underlying connection を参照
    pub fn db(&self) -> &Surreal<Any> {
        &self.db
    }

    // -------------------------------------------------------------------------
    // wire_send 系
    // -------------------------------------------------------------------------

    /// 新規 thread (root message) を送信
    ///
    /// Operations (決定 III):
    /// 1. `wire_messages` に root message (`prev=None`) を INSERT (`local_seq` を採番)
    ///
    /// participant 行は作らない。 配送は `to_addrs` から創発し、 既読判定は
    /// per-agent `agent_cursor` が担う。 送信者は `from` であり `to` に居ないので、
    /// `to` フィルタ配送で自動的に自分の root を未読として見ない (= sender cursor 処理不要)。
    ///
    /// 戻り値: INSERT した root [`WireMessage`] (`local_seq` 採番済、 caller が notify /
    /// id 通知に使う)。
    pub async fn send_root(
        &self,
        from: &str,
        to: &[String],
        body: serde_json::Value,
    ) -> Result<WireMessage> {
        let mut msg = WireMessage::new_root(from, to.to_vec(), body);
        self.insert_message(&mut msg).await?;
        Ok(msg)
    }

    /// 既存 thread への reply を送信 (reply-all、 REQ-THREAD-005)
    ///
    /// Operations (R1 / 決定 III):
    /// 1. `prev` = 返信先 message を `wire_messages` から取得 (存在検証)
    /// 2. `to` を **返信先 (`prev`) の参加者を継いだ集合** に展開する (reply-all):
    ///    - 参加者集合 = `prev_msg.from` ∪ `prev_msg.to` ∪ caller 指定の `extra`
    ///      (各 reply が親の参加者集合を継ぐので、 thread 全走査は不要 — `prev` 1 件で足りる)
    ///    - `left` の agent を除外 (`prev` から root まで walk して thread root id を得、
    ///      `thread_participant` の sparse 例外表を引く)
    ///    - 送信者自身 (`from`) は `to` から除外 (= 自分の reply を未読で見ない)
    /// 3. 展開した `to` で reply message を INSERT (`local_seq` を採番)
    ///
    /// participation 行を持たなくても、 `to` 展開により thread 参加者が受信を継続できる。
    ///
    /// 戻り値: INSERT した reply [`WireMessage`]。
    /// `prev_id` の message が存在しなければ `Err`。
    pub async fn send_reply(
        &self,
        from: &str,
        to: &[String],
        body: serde_json::Value,
        prev_id: &str,
    ) -> Result<WireMessage> {
        // 返信先 message を取得 (存在検証)
        let prev_msg = self.get_message(prev_id).await?.ok_or_else(|| {
            anyhow::anyhow!("wiremsg send_reply: prev message '{prev_id}' not found")
        })?;

        // reply-all 展開: prev の参加者を継ぐ ∪ caller 指定 to、 left 除外、 from 除外
        let expanded_to = self.expand_reply_recipients(&prev_msg, from, to).await?;

        let mut msg = WireMessage::new_reply(from, expanded_to, body, prev_id);
        self.insert_message(&mut msg).await?;
        Ok(msg)
    }

    /// reply の `to` を返信先 (`prev`) の参加者集合を継いで展開する (reply-all)
    ///
    /// 参加者集合 = `prev_msg.from` ∪ `prev_msg.to`、 ∪ caller 指定 `extra`。
    /// 各 reply が親の参加者集合を継ぐので、 thread 全走査は不要 (`prev` 1 件で足りる) —
    /// 参加者は枝に沿って単調増加する。
    /// `left` の agent と 送信者自身 (`from`) を除外する。 結果は安定順序 (BTreeSet)。
    async fn expand_reply_recipients(
        &self,
        prev_msg: &WireMessage,
        from: &str,
        extra: &[String],
    ) -> Result<Vec<String>> {
        use std::collections::BTreeSet;

        // prev の参加者 (from ∪ to) を継ぐ
        let mut set: BTreeSet<String> = BTreeSet::new();
        set.insert(prev_msg.from.clone());
        for t in &prev_msg.to {
            set.insert(t.clone());
        }
        // caller 指定 (新規参加者追加用) を union
        for e in extra {
            set.insert(e.clone());
        }
        // left の agent を除外 (thread root id を walk して引く)。
        // left 強制は send 側の責務 (R1) — recv 側 filter は廃止。
        let root_id = self.walk_to_root(&prev_msg.id).await?;
        let left = self.left_agents(&root_id).await?;
        for l in &left {
            set.remove(l);
        }
        // 送信者自身は to に入れない (= 自分の reply を未読で見ない)
        set.remove(from);
        Ok(set.into_iter().collect())
    }

    // -------------------------------------------------------------------------
    // wire_recv 系
    // -------------------------------------------------------------------------

    /// 指定 agent の未読 message を 1 回分取得 (long-poll はしない、 caller がループ制御)
    ///
    /// `to` ベース配送 (決定 III / R1): `wire_messages` で `agent ∈ to_addrs` AND
    /// `local_seq > cursor` (`cursor = None` なら全件) の message を `local_seq` 昇順で
    /// 取得する。
    ///
    /// R1: cursor 比較は `local_seq`。 left thread の recv 側 filter は廃止された
    /// (left 強制は `send_reply` 側 — left agent は以後の reply の `to` に入らない)。
    /// leave 前に既に `to` に入っている未読 message は通常通り drain される。
    ///
    /// 取得後、 caller は [`advance_cursor`](Self::advance_cursor) で agent の単一 cursor を
    /// **取得した最新 message の `local_seq`** に前進させること。 本メソッドは cursor を
    /// 変更しない。
    pub async fn fetch_unread(&self, agent: &str) -> Result<Vec<WireMessage>> {
        // 1. agent の per-agent cursor (last_read = local_seq) を取得
        let cursor = self.get_cursor(agent).await?;
        // 2. agent ∈ to_addrs かつ cursor 超過の message を取得 (local_seq 昇順)
        self.messages_to_after(agent, cursor).await
    }

    /// `wire_recv` 1 回分の store 操作: 未読取得 + per-agent cursor 前進をまとめて行う
    ///
    /// [`fetch_unread`](Self::fetch_unread) で未読を取得し、 単一 cursor を
    /// 「取得した最新 message の `local_seq`」 まで前進させる。
    /// 未読が空なら cursor は触らない。
    ///
    /// cursor を取得済 message の `local_seq` 最大値に合わせる。 `local_seq` は厳密単調
    /// なので、 同一 ms 衝突や clock skew に影響されず取りこぼさない (R1 / moody #1)。
    pub async fn recv(&self, agent: &str) -> Result<Vec<WireMessage>> {
        let unread = self.fetch_unread(agent).await?;
        if unread.is_empty() {
            return Ok(unread);
        }
        // 取得済 message の local_seq 最大値まで単一 cursor を前進
        let max = unread.iter().map(|m| m.local_seq).max().unwrap_or(0);
        self.advance_cursor(agent, max).await?;
        Ok(unread)
    }

    /// 指定 agent の per-agent cursor (`last_read`) を前進させる
    ///
    /// `cursor` は [`fetch_unread`](Self::fetch_unread) で取得した最新 message の
    /// `local_seq` を渡す。 既存 cursor より小さい値は無視 (= 後退させない)。
    /// 行が無ければ作成する。
    ///
    /// R1 / moody #2: SELECT→CREATE の non-atomic race (並行 recv の二重 CREATE で
    /// unique 制約エラー) を避けるため `UPSERT` 文を使う。 `agent` 一致の 1 行を
    /// atomic に upsert する。
    pub async fn advance_cursor(&self, agent: &str, cursor: u64) -> Result<()> {
        let now = now_ms();
        // UPSERT ... WHERE agent = $agent: agent 一致行があれば update、 無ければ create。
        // SELECT→CREATE を 1 文に畳んで並行 recv の二重 CREATE race を消す (moody #2)。
        // SET 句は last_read を「単調前進」させる: 既存値が NONE か cursor 未満のときだけ
        // cursor に上げ、 そうでなければ据え置く (後退禁止)。
        self.db
            .query(
                "UPSERT agent_cursor
                     SET agent = $agent,
                         last_read = IF last_read = NONE OR last_read < $cursor
                                     THEN $cursor ELSE last_read END,
                         updated_at = $now
                     WHERE agent = $agent;",
            )
            .bind(("agent", agent.to_string()))
            .bind(("cursor", cursor))
            .bind(("now", now))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg advance_cursor upsert failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("wiremsg advance_cursor upsert check failed: {e}"))?;
        Ok(())
    }

    /// 指定 agent の per-thread 未読数を derive して返す (「derive できるものは store しない」)
    ///
    /// [`fetch_unread`](Self::fetch_unread) と同じ未読集合を thread root id で GROUP BY
    /// した count。 thread root id は各 message の `prev` を辿った先
    /// ([`walk_to_root`](Self::walk_to_root))。 未読 0 の thread は HashMap に現れない。
    pub async fn unread_count_by_thread(&self, agent: &str) -> Result<HashMap<String, u64>> {
        let unread = self.fetch_unread(agent).await?;
        let mut counts: HashMap<String, u64> = HashMap::new();
        for m in &unread {
            // R1: thread_id 全廃 — root id は prev を辿って得る
            let root = self.walk_to_root(&m.id).await?;
            *counts.entry(root).or_insert(0) += 1;
        }
        Ok(counts)
    }

    // -------------------------------------------------------------------------
    // 内部 helper
    // -------------------------------------------------------------------------

    /// `wire_messages` table に message を INSERT (`local_seq` を採番)
    ///
    /// `seq` 採番器を `fetch_add(1)` し、 確定した `local_seq` を `msg` に書き戻してから
    /// INSERT する (caller に採番済 message を返すため `&mut`)。
    async fn insert_message(&self, msg: &mut WireMessage) -> Result<()> {
        // local_seq を採番 (起動時復元値 + 1 起点で厳密単調)
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        msg.local_seq = seq;
        self.db
            .query(
                r#"
                CREATE type::record('wire_messages', $id) CONTENT {
                    id: $id, prev: $prev,
                    from_addr: $from_addr, to_addrs: $to_addrs,
                    body: $body, created_at: $created_at, local_seq: $local_seq
                }
                "#,
            )
            .bind(("id", msg.id.clone()))
            .bind(("prev", msg.prev.clone()))
            .bind(("from_addr", msg.from.clone()))
            .bind(("to_addrs", msg.to.clone()))
            .bind(("body", msg.body.clone()))
            .bind(("created_at", msg.created_at))
            .bind(("local_seq", seq))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg insert_message failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("wiremsg insert_message check failed: {e}"))?;
        Ok(())
    }

    /// id 指定で 1 件の message を取得
    ///
    /// `CONTENT { id: $id }` で record id を message の uuidv7 に固定しているため、
    /// `type::record('wire_messages', $id)` で record id 直引きできる
    /// (= `WHERE id = $id` だと返却 `id` field が `wire_messages:<uuid>` 形式で
    /// bare uuid と一致しないため不可)。
    async fn get_message(&self, id: &str) -> Result<Option<WireMessage>> {
        let mut res = self
            .db
            .query("SELECT * FROM type::record('wire_messages', $id);")
            .bind(("id", id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg get_message failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg get_message take failed: {e}"))?;
        match rows.first() {
            Some(row) => Ok(Some(Self::row_to_message(row)?)),
            None => Ok(None),
        }
    }

    /// prev chain walk の循環防止上限 (現実の thread 深さを十分超える値)
    const WALK_MAX_DEPTH: usize = 10_000;

    /// 指定 message id から `prev` を `None` まで辿り、 道中の message を収集する
    /// (`ancestor_chain` / `walk_to_root` の共通 walk helper、 R2 で重複排除)
    ///
    /// 返り順は **leaf-first** — 起点 message が先頭、 root が末尾。 呼び元が要求順に
    /// 並べ替える (`ancestor_chain` は reverse して root-first にする)。
    ///
    /// 防御的挙動: prev chain が断裂して親 message が見つからない場合は、 そこまで収集
    /// した分を返す (`ancestor_chain` の「辿れた分を返す」 / `walk_to_root` の
    /// 「辿れた最後を root とみなす」 がともにこの挙動から導かれる)。 起点 message 自体が
    /// 存在しなければ空 vec を返す (呼び元が Err 判定する)。
    async fn collect_prev_chain(&self, message_id: &str) -> Result<Vec<WireMessage>> {
        let mut chain: Vec<WireMessage> = Vec::new();
        let mut current = message_id.to_string();
        // prev chain を辿る。 循環 / 過剰な深さは上限で打ち切る。
        for _ in 0..Self::WALK_MAX_DEPTH {
            let msg = match self.get_message(&current).await? {
                Some(m) => m,
                // 親が見つからない (起点不在 or prev chain 断裂) → 収集済みを返す
                None => break,
            };
            let prev = msg.prev.clone();
            chain.push(msg);
            match prev {
                None => break,
                Some(p) => current = p,
            }
        }
        Ok(chain)
    }

    /// 指定 message id から `prev` を `None` まで辿り、 thread の root message id を返す
    ///
    /// R1: `thread_id` 全廃の代替 — 「thread の識別子」が要る場面では root message の id
    /// を使う。 `send_reply` の left 判定 / `unread_count_by_thread` の GROUP BY key
    /// として使われる。 R2 で `ancestor_chain` と walk ロジックを共通化した
    /// ([`collect_prev_chain`])。
    ///
    /// 自身が root (`prev = None`) なら自身の id をそのまま返す。 prev chain が壊れて
    /// 親 message が見つからない場合は、 辿れた最後の id を root とみなす (= 防御的)。
    pub async fn walk_to_root(&self, message_id: &str) -> Result<String> {
        let chain = self.collect_prev_chain(message_id).await?;
        // chain は leaf-first。 末尾が「辿れた最後」 = root とみなす要素。
        // chain が空 (= 起点 message が存在しない) なら起点 id をそのまま返す
        // (循環時 / 既存挙動と整合 — root id が引けないとき起点を返す)。
        Ok(chain
            .last()
            .map(|m| m.id.clone())
            .unwrap_or_else(|| message_id.to_string()))
    }

    /// 指定 message から `prev` を root まで辿った **ancestor-chain (系譜)** を返す (R2)
    ///
    /// `wire_thread` tool の中核。 thread に途中参加した agent が backlog (= 受け取った
    /// message に至る文脈) を取得するための read-only な走査。 `wire_recv` の増分 drain
    /// とは対で、 **cursor を一切触らない** (read-only・冪等)。
    ///
    /// 返り順は **root-first** — root が先頭、 指定 message が末尾 (= chronological)。
    /// 「全枝ツリー」 は返さない (子孫は含まない) — agent が要るのは指定 message に至る
    /// 系譜のみ。
    ///
    /// edge: 指定 message が存在しなければ `Err`。 prev chain が断裂して親 message が
    /// 見つからない場合は、 そこまで収集した分を root-first で返す
    /// ([`collect_prev_chain`] の防御的挙動と整合)。
    pub async fn ancestor_chain(&self, message_id: &str) -> Result<Vec<WireMessage>> {
        let mut chain = self.collect_prev_chain(message_id).await?;
        // chain が空 = 指定 message 自体が存在しない → Err
        if chain.is_empty() {
            anyhow::bail!("wiremsg ancestor_chain: message '{message_id}' not found");
        }
        // collect_prev_chain は leaf-first。 root-first (= chronological) に反転する。
        chain.reverse();
        Ok(chain)
    }

    /// `agent ∈ to_addrs` かつ `local_seq > cursor` の message を `local_seq` 昇順で取得
    /// (`cursor = None` なら全件)
    ///
    /// `to` ベース配送の中核 query (決定 III / R1)。 `to_addrs CONTAINS $agent` で
    /// agent 宛 message を引き、 cursor 比較は `local_seq` で行う。
    async fn messages_to_after(
        &self,
        agent: &str,
        cursor: Option<u64>,
    ) -> Result<Vec<WireMessage>> {
        // cursor IS NONE と cursor 指定で query を分岐 (= bind の None を WHERE で扱うと
        // SurrealDB の比較が意図せぬ挙動になりうるため、 明示的に 2 query に分ける)。
        let mut res = match cursor {
            Some(c) => {
                self.db
                    .query(
                        "SELECT * FROM wire_messages
                             WHERE to_addrs CONTAINS $agent AND local_seq > $cursor
                             ORDER BY local_seq ASC;",
                    )
                    .bind(("agent", agent.to_string()))
                    .bind(("cursor", c))
                    .await
            }
            None => {
                self.db
                    .query(
                        "SELECT * FROM wire_messages
                             WHERE to_addrs CONTAINS $agent
                             ORDER BY local_seq ASC;",
                    )
                    .bind(("agent", agent.to_string()))
                    .await
            }
        }
        .map_err(|e| anyhow::anyhow!("wiremsg messages_to_after failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg messages_to_after take failed: {e}"))?;
        rows.iter().map(Self::row_to_message).collect()
    }

    /// 指定 agent の per-agent cursor (`last_read` = local_seq) を返す (行が無ければ `None`)
    async fn get_cursor(&self, agent: &str) -> Result<Option<u64>> {
        let mut res = self
            .db
            .query("SELECT last_read FROM agent_cursor WHERE agent = $agent LIMIT 1;")
            .bind(("agent", agent.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg get_cursor failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg get_cursor take failed: {e}"))?;
        Ok(rows.first().and_then(|row| row["last_read"].as_u64()))
    }

    /// 指定 thread (root id) で `left` した agent の address 集合を返す (sparse 例外表 query)
    async fn left_agents(&self, root_id: &str) -> Result<std::collections::HashSet<String>> {
        let mut res = self
            .db
            .query(
                "SELECT agent FROM thread_participant
                     WHERE thread = $thread AND status = 'left';",
            )
            .bind(("thread", root_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg left_agents failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg left_agents take failed: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|row| row["agent"].as_str().map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// SurrealDB record id (`tb:<local>` 形式 or object) から local 部分を抽出
    fn extract_record_local_id(id_value: &serde_json::Value, table: &str) -> String {
        if let Some(s) = id_value.as_str() {
            let prefix = format!("{table}:");
            return s
                .strip_prefix(&prefix)
                .unwrap_or(s)
                .trim_matches('`')
                .to_string();
        }
        // raw record id object ({ "tb": ..., "id": ... })
        id_value["id"]
            .as_str()
            .or_else(|| id_value["String"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// `wire_messages` row JSON を [`WireMessage`] に hydrate
    fn row_to_message(row: &serde_json::Value) -> Result<WireMessage> {
        let id = Self::extract_record_local_id(&row["id"], "wire_messages");
        let to = row["to_addrs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(WireMessage {
            id,
            prev: row["prev"].as_str().map(|s| s.to_string()),
            from: row["from_addr"].as_str().unwrap_or_default().to_string(),
            to,
            body: row["body"].clone(),
            created_at: row["created_at"].as_u64().unwrap_or_default(),
            local_seq: row["local_seq"].as_u64().unwrap_or_default(),
        })
    }
}

// =============================================================================
// WireNotifier — wire_recv long-poll の SP 内 in-process 起床機構
// =============================================================================

/// `wire_recv` の long-poll 待機を `wire_send` から起こすための notifier
///
/// agent address ごとに [`tokio::sync::Notify`] を持つ。 `wire_recv` が未読 0 のとき
/// その agent の `Notify` で待機し、 `wire_send` が宛先 agent の `Notify` を
/// `notify_waiters` する。 TopicRouter は介さず、 SP プロセス内の純粋な in-process 機構。
///
/// ## 取りこぼし防止プロトコル (重要)
///
/// `Notify::notify_waiters` は **その時点の待機者のみ** を起こし、 permit を貯めない。
/// したがって「notify → 後から wait」 の順だと取りこぼす。 これを避けるため
/// `wire_recv` handler は必ず次の順序を守ること:
///
/// 1. [`handle`](Self::handle) で agent の `Arc<Notify>` を取得
/// 2. `notify.notified()` で **待機 future を先に生成** (この瞬間以降の notify を捕捉)
/// 3. store を poll → 未読あれば即 return
/// 4. 未読なければ step 2 の future を await
///
/// store poll の前に future を生成しておけば、 poll と await の隙間に来た `wire_send`
/// も確実に拾える。
#[derive(Clone, Default)]
pub struct WireNotifier {
    /// agent address → Notify
    waiters: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

impl WireNotifier {
    /// 空の notifier を構築
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定 agent の `Notify` ハンドルを取得 (無ければ作成)
    ///
    /// `wire_recv` handler は戻り値の `notified()` を **store poll の前に** 生成すること
    /// (struct doc の取りこぼし防止プロトコル参照)。
    pub async fn handle(&self, agent: &str) -> Arc<Notify> {
        let mut guard = self.waiters.lock().await;
        guard
            .entry(agent.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    /// 指定 agent の待機中 `wire_recv` を全て起こす
    ///
    /// `wire_send` が宛先 agent ごとに呼ぶ。 待機者がいなければ no-op。
    pub async fn notify(&self, agent: &str) {
        let notify = self.handle(agent).await;
        notify.notify_waiters();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// kv-mem で wiremsg schema を define 済の Surreal を返す
    async fn make_test_store() -> WiremsgStore {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("kv-mem connect");
        db.use_ns("vp").use_db("vp").await.expect("use_ns/db");
        // db/mod.rs SCHEMA_SQL の wiremsg 部分を再現 (= テスト独立性)。 R1 schema:
        // thread_id 全廃 / local_seq 追加 / wire_to_seq_idx。
        db.query(
            r#"
            DEFINE TABLE wire_messages SCHEMAFULL;
            DEFINE FIELD id ON wire_messages TYPE string;
            DEFINE FIELD prev ON wire_messages TYPE option<string>;
            DEFINE FIELD from_addr ON wire_messages TYPE string;
            DEFINE FIELD to_addrs ON wire_messages TYPE array<string>;
            DEFINE FIELD body ON wire_messages TYPE object FLEXIBLE;
            DEFINE FIELD created_at ON wire_messages TYPE number;
            DEFINE FIELD local_seq ON wire_messages TYPE number;
            DEFINE INDEX wire_to_seq_idx ON wire_messages FIELDS to_addrs, local_seq;

            DEFINE TABLE agent_cursor SCHEMAFULL;
            DEFINE FIELD agent ON agent_cursor TYPE string;
            DEFINE FIELD last_read ON agent_cursor TYPE option<number>;
            DEFINE FIELD updated_at ON agent_cursor TYPE number;
            DEFINE INDEX agent_cursor_uniq ON agent_cursor FIELDS agent UNIQUE;

            DEFINE TABLE thread_participant SCHEMAFULL;
            DEFINE FIELD thread ON thread_participant TYPE string;
            DEFINE FIELD agent ON thread_participant TYPE string;
            DEFINE FIELD status ON thread_participant TYPE string DEFAULT 'active';
            DEFINE FIELD updated_at ON thread_participant TYPE number;
            DEFINE INDEX thread_participant_uniq ON thread_participant FIELDS thread, agent UNIQUE;
            DEFINE INDEX thread_participant_agent_idx ON thread_participant FIELDS agent, status;
            "#,
        )
        .await
        .expect("schema query")
        .check()
        .expect("schema check");

        WiremsgStore::new(Arc::new(db))
            .await
            .expect("WiremsgStore::new")
    }

    /// body helper
    fn body(text: &str) -> serde_json::Value {
        serde_json::json!({ "text": text })
    }

    /// 指定 agent が thread (root id) を left した sparse 例外表行を CREATE する test helper
    async fn mark_left(store: &WiremsgStore, root_id: &str, agent: &str) {
        store
            .db()
            .query(
                "CREATE thread_participant CONTENT {
                     thread: $thread, agent: $agent, status: 'left', updated_at: 0
                 };",
            )
            .bind(("thread", root_id.to_string()))
            .bind(("agent", agent.to_string()))
            .await
            .expect("create left")
            .check()
            .expect("create left check");
    }

    /// WireMessage::new_root は prev = None (R1: thread_id field は廃止)
    #[test]
    fn wire_message_root_has_no_prev() {
        let msg = WireMessage::new_root("a@vp", vec!["b@vp".into()], body("hi"));
        assert!(msg.prev.is_none(), "root の prev は None");
        assert_eq!(msg.local_seq, 0, "INSERT 前は local_seq = 0");
    }

    /// WireMessage::new_reply は prev = 返信先 id (R1: thread_id 引数は廃止)
    #[test]
    fn wire_message_reply_carries_prev() {
        let reply = WireMessage::new_reply("a@vp", vec!["b@vp".into()], body("re"), "parent-id");
        assert_eq!(reply.prev.as_deref(), Some("parent-id"), "prev = 返信先 id");
        assert_eq!(reply.local_seq, 0, "INSERT 前は local_seq = 0");
    }

    // -------------------------------------------------------------------------
    // send_root / fetch / cursor — to ベース配送 + per-agent cursor (決定 III)
    // -------------------------------------------------------------------------

    /// send_root: 受信者は起点 message を未読として受け取る (to ベース配送)
    #[tokio::test]
    async fn send_root_recipient_sees_root_message() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("hello bob"))
            .await
            .expect("send_root");

        // 受信者 bob は起点 message を未読として受け取れる (cursor 行なし = 全件未読)
        let unread = store.fetch_unread("bob@vp").await.expect("fetch bob");
        assert_eq!(unread.len(), 1, "起点 message が未読で 1 件届く");
        assert_eq!(unread[0].id, root.id);
        assert_eq!(unread[0].body, body("hello bob"));
    }

    /// send_root: 送信者は to に居ないため自分の root を未読として見ない
    #[tokio::test]
    async fn send_root_sender_does_not_see_own_message() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("hello"))
            .await
            .expect("send_root");

        // 送信者 alice は from であり to に居ない → to フィルタで未読 0
        let unread = store.fetch_unread("alice@vp").await.expect("fetch alice");
        assert!(unread.is_empty(), "送信者は自分の root message を読まない");
    }

    /// send_root / send_reply は local_seq を厳密単調 (1, 2, 3...) に採番する
    ///
    /// R1: local_seq は accumulation の ingestion 順序。 INSERT 毎に +1。
    #[tokio::test]
    async fn local_seq_is_strictly_monotonic() {
        let store = make_test_store().await;
        let m1 = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("1"))
            .await
            .expect("m1");
        let m2 = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("2"))
            .await
            .expect("m2");
        let m3 = store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("3"), &m1.id)
            .await
            .expect("m3");
        assert_eq!(m1.local_seq, 1, "1 件目の local_seq は 1");
        assert_eq!(m2.local_seq, 2, "2 件目の local_seq は 2");
        assert_eq!(m3.local_seq, 3, "3 件目 (reply) の local_seq は 3");
    }

    /// WiremsgStore::new は既存 wire_messages の local_seq 最大値から採番を復元する
    ///
    /// R1: SP プロセス再起動を跨いでも accumulation の ingestion 順序が厳密単調に続く。
    #[tokio::test]
    async fn new_store_restores_seq_from_max_local_seq() {
        let store = make_test_store().await;
        // 既存 message を 2 件作る (local_seq 1, 2)
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("a"))
            .await
            .expect("a");
        let m2 = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("b"))
            .await
            .expect("b");
        assert_eq!(m2.local_seq, 2);

        // 同じ DB connection から新しい WiremsgStore を構築 (= 再起動相当)
        let restored = WiremsgStore::new(store.db.clone())
            .await
            .expect("restore store");
        // 次に採番される local_seq は 3 (= max 2 の続き、 1 起点に戻らない)
        let m3 = restored
            .send_root("alice@vp", &["bob@vp".to_string()], body("c"))
            .await
            .expect("c");
        assert_eq!(
            m3.local_seq, 3,
            "再構築 store は max local_seq から採番を継ぐ"
        );
    }

    /// 空 table から構築した store は local_seq を 1 起点で採番する
    #[tokio::test]
    async fn new_store_starts_seq_at_one_when_empty() {
        let store = make_test_store().await;
        let first = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("first"))
            .await
            .expect("first");
        assert_eq!(
            first.local_seq, 1,
            "空 accumulation の最初の message は seq 1"
        );
    }

    /// fetch → advance_cursor → 再 fetch で同じ message は二度読まれない
    #[tokio::test]
    async fn cursor_advances_and_message_not_redelivered() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("once"))
            .await
            .expect("send_root");

        let unread = store.fetch_unread("bob@vp").await.expect("fetch 1");
        assert_eq!(unread.len(), 1);

        // per-agent cursor を取得した最新 message の local_seq に前進
        let last = unread.last().unwrap();
        store
            .advance_cursor("bob@vp", last.local_seq)
            .await
            .expect("advance");

        // 再 fetch で空 (= 二度読みしない)
        let again = store.fetch_unread("bob@vp").await.expect("fetch 2");
        assert!(
            again.is_empty(),
            "cursor 前進後は同じ message を再配信しない"
        );
    }

    /// recv() は未読取得 + per-agent cursor 前進をまとめて行う
    #[tokio::test]
    async fn recv_fetches_then_advances_cursor() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("once"))
            .await
            .expect("send_root");

        // 1 回目の recv: 1 件取得
        let first = store.recv("bob@vp").await.expect("recv 1");
        assert_eq!(first.len(), 1, "未読 1 件");

        // 2 回目の recv: cursor 前進済なので空 (= 二度読みしない)
        let second = store.recv("bob@vp").await.expect("recv 2");
        assert!(second.is_empty(), "recv 後は cursor が前進し再配信されない");
    }

    /// recv() は単一 cursor で複数 thread の未読をまとめて drain する
    #[tokio::test]
    async fn recv_drains_multiple_threads_with_single_cursor() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("t1"))
            .await
            .expect("t1");
        store
            .send_root("carol@vp", &["bob@vp".to_string()], body("t2"))
            .await
            .expect("t2");

        // recv で 2 thread 分まとめて取得
        let first = store.recv("bob@vp").await.expect("recv 1");
        assert_eq!(first.len(), 2, "2 thread 分の未読");

        // 再 recv で空 (= 単一 cursor が両 thread の最新まで前進している)
        let second = store.recv("bob@vp").await.expect("recv 2");
        assert!(second.is_empty(), "単一 cursor 前進で再配信なし");
    }

    /// 同一 ms 内に着信した複数 message を local_seq cursor で取りこぼさない
    ///
    /// R1 / moody #1: 旧 cursor は created_at (epoch ms) で `created_at > cursor` だった。
    /// 同一 ms に複数 message が来ると、 fetch〜advance の隙間や境界で取りこぼした。
    /// local_seq は厳密単調なので、 created_at が同値でも区別され取りこぼさない。
    #[tokio::test]
    async fn same_millisecond_messages_not_lost_with_local_seq() {
        let store = make_test_store().await;
        // 同一 ms で 3 件 INSERT されることを再現するため created_at を直接揃える。
        // (連続 send_root でも実環境では同一 ms になりうる)
        let fixed_ts = 1_700_000_000_000u64;
        for i in 0..3 {
            let mut msg = WireMessage::new_root(
                "alice@vp",
                vec!["bob@vp".to_string()],
                body(&format!("m{i}")),
            );
            msg.created_at = fixed_ts; // 3 件すべて同一 ms
            store.insert_message(&mut msg).await.expect("insert");
        }
        // 3 件すべて created_at が同値であることを確認
        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert_eq!(unread.len(), 3, "同一 ms の 3 件すべてが未読で見える");
        assert!(
            unread.iter().all(|m| m.created_at == fixed_ts),
            "3 件は created_at 同値"
        );
        // local_seq は 1, 2, 3 と区別されている
        let seqs: Vec<u64> = unread.iter().map(|m| m.local_seq).collect();
        assert_eq!(seqs, vec![1, 2, 3], "local_seq で厳密に区別される");

        // 1 件目だけ読んだ位置 (cursor = 1) に進めても、 残り 2 件は取りこぼさない
        store.advance_cursor("bob@vp", 1).await.expect("advance 1");
        let rest = store.fetch_unread("bob@vp").await.expect("fetch rest");
        assert_eq!(
            rest.len(),
            2,
            "同一 ms でも cursor 後続の 2 件を取りこぼさない"
        );
        assert_eq!(rest[0].local_seq, 2);
        assert_eq!(rest[1].local_seq, 3);
    }

    /// reply: thread 参加者が reply を未読として受け取る
    #[tokio::test]
    async fn reply_delivered_to_thread_participants() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("q"))
            .await
            .expect("send_root");

        // bob が root を読んで cursor 前進
        let _ = store.recv("bob@vp").await.expect("bob recv root");

        // alice が reply
        let reply = store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("a"), &root.id)
            .await
            .expect("send_reply");
        assert_eq!(reply.prev.as_deref(), Some(root.id.as_str()), "prev = root");

        // bob は reply を未読として受け取る
        let bob_unread = store.fetch_unread("bob@vp").await.expect("bob fetch 2");
        assert_eq!(bob_unread.len(), 1, "reply 1 件が未読で届く");
        assert_eq!(bob_unread[0].id, reply.id);
    }

    /// reply: 送信者は to に展開されないため、自分の reply を未読として見ない
    #[tokio::test]
    async fn reply_sender_does_not_see_own_reply() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("q"))
            .await
            .expect("send_root");
        store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("a"), &root.id)
            .await
            .expect("send_reply");

        // alice は from であり、reply-all 展開でも from は to から除外される
        let alice_unread = store.fetch_unread("alice@vp").await.expect("alice fetch");
        assert!(
            alice_unread.is_empty(),
            "送信者は自分の reply を未読として見ない (to に居ないため)"
        );
    }

    /// reply-all: reply の to は返信先 (prev) の参加者集合を継いで展開される (REQ-THREAD-005)
    #[tokio::test]
    async fn reply_expands_carrying_forward_prev_participants() {
        let store = make_test_store().await;
        // alice が bob・carol 宛に root
        let root = store
            .send_root(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("root"),
            )
            .await
            .expect("send_root");

        // bob が reply。 to に carol を明示しなくても reply-all で carol に届く
        let reply = store
            .send_reply("bob@vp", &[], body("reply"), &root.id)
            .await
            .expect("send_reply");
        // 展開後の to: prev の参加者 (alice, bob, carol) から from=bob を除く
        assert!(reply.to.contains(&"alice@vp".to_string()), "alice に展開");
        assert!(reply.to.contains(&"carol@vp".to_string()), "carol に展開");
        assert!(
            !reply.to.contains(&"bob@vp".to_string()),
            "from は to に含めない"
        );

        // carol は participation 行を持たなくても reply を受信できる
        let carol_unread = store.fetch_unread("carol@vp").await.expect("carol fetch");
        assert!(
            carol_unread.iter().any(|m| m.id == reply.id),
            "reply-all 展開で carol が reply を受信"
        );
    }

    /// reply-all: 参加者は枝に沿って carry-forward され単調増加する (prev 1 件で足りる)
    ///
    /// R1: thread 全走査せず prev の from/to のみ継ぐ。 多段 reply でも参加者集合が
    /// 親から子へ伝播することを確認する。
    #[tokio::test]
    async fn reply_participants_carry_forward_across_chain() {
        let store = make_test_store().await;
        // root: alice → bob
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        // reply1: bob が carol を巻き込む。 to 展開 = {alice, carol}
        let reply1 = store
            .send_reply("bob@vp", &["carol@vp".to_string()], body("r1"), &root.id)
            .await
            .expect("reply1");
        assert!(reply1.to.contains(&"alice@vp".to_string()));
        assert!(reply1.to.contains(&"carol@vp".to_string()));
        // reply2: carol が reply1 に返信。 prev=reply1 の参加者 {bob, alice, carol} を継ぎ
        // from=carol を除いた {alice, bob} に展開される (= alice/bob が伝播)
        let reply2 = store
            .send_reply("carol@vp", &[], body("r2"), &reply1.id)
            .await
            .expect("reply2");
        assert!(
            reply2.to.contains(&"alice@vp".to_string()),
            "alice が枝を伝播して reply2 に残る"
        );
        assert!(
            reply2.to.contains(&"bob@vp".to_string()),
            "bob が枝を伝播して reply2 に残る"
        );
        assert!(!reply2.to.contains(&"carol@vp".to_string()), "from は除外");
    }

    /// reply: thread に途中参加した agent は参加時点以降の message のみ受信する
    #[tokio::test]
    async fn reply_new_participant_sees_messages_from_join() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("send_root");

        // alice が carol を新規に巻き込んで reply
        let reply = store
            .send_reply(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("reply"),
                &root.id,
            )
            .await
            .expect("send_reply");

        // carol は reply の to に入った → reply のみ受信。 root (参加前) は受信しない
        let carol_unread = store.fetch_unread("carol@vp").await.expect("carol fetch");
        assert_eq!(
            carol_unread.len(),
            1,
            "途中参加者は参加時点以降の message のみ受信"
        );
        assert_eq!(carol_unread[0].id, reply.id, "受信するのは reply");
        assert!(
            !carol_unread.iter().any(|m| m.id == root.id),
            "参加前の root は受信しない"
        );
    }

    /// send_reply: 存在しない prev_id を指定したら Err
    #[tokio::test]
    async fn reply_to_missing_message_errors() {
        let store = make_test_store().await;
        let result = store
            .send_reply("a@vp", &["b@vp".to_string()], body("x"), "nonexistent-id")
            .await;
        assert!(result.is_err(), "存在しない prev は Err");
    }

    // -------------------------------------------------------------------------
    // walk_to_root — prev forest walk (R1: thread_id 全廃の代替)
    // -------------------------------------------------------------------------

    /// walk_to_root: root message は自身を返す
    #[tokio::test]
    async fn walk_to_root_of_root_is_self() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        let resolved = store.walk_to_root(&root.id).await.expect("walk");
        assert_eq!(resolved, root.id, "root の root は自身");
    }

    /// walk_to_root: 多段 reply から prev を辿って root id に到達する
    #[tokio::test]
    async fn walk_to_root_traverses_prev_chain() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        let r1 = store
            .send_reply("bob@vp", &[], body("r1"), &root.id)
            .await
            .expect("r1");
        let r2 = store
            .send_reply("alice@vp", &[], body("r2"), &r1.id)
            .await
            .expect("r2");
        // r2 → r1 → root と辿る
        assert_eq!(
            store.walk_to_root(&r2.id).await.expect("walk r2"),
            root.id,
            "多段 reply の root は thread の起点"
        );
        assert_eq!(store.walk_to_root(&r1.id).await.expect("walk r1"), root.id,);
    }

    // -------------------------------------------------------------------------
    // ancestor_chain — R2: 指定 message から root までの系譜 (wire_thread の中核)
    // -------------------------------------------------------------------------

    /// ancestor_chain: root message 単体は自身のみ 1 件返す
    #[tokio::test]
    async fn ancestor_chain_of_root_is_self_only() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        let chain = store.ancestor_chain(&root.id).await.expect("chain");
        assert_eq!(chain.len(), 1, "root の系譜は自身のみ");
        assert_eq!(chain[0].id, root.id);
    }

    /// ancestor_chain: 多段 reply から root までの系譜を root-first で返す
    ///
    /// R2: 返り順は root が先頭・指定 message が末尾 (= chronological)。
    #[tokio::test]
    async fn ancestor_chain_returns_root_first_chronological() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        let r1 = store
            .send_reply("bob@vp", &[], body("r1"), &root.id)
            .await
            .expect("r1");
        let r2 = store
            .send_reply("alice@vp", &[], body("r2"), &r1.id)
            .await
            .expect("r2");

        let chain = store.ancestor_chain(&r2.id).await.expect("chain");
        // root → r1 → r2 の順 (root-first)
        let ids: Vec<&str> = chain.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![root.id.as_str(), r1.id.as_str(), r2.id.as_str()],
            "系譜は root 先頭・指定 message 末尾の chronological 順"
        );
        // local_seq も昇順 (chronological の裏付け)
        assert!(
            chain[0].local_seq < chain[1].local_seq && chain[1].local_seq < chain[2].local_seq,
            "root-first は local_seq 昇順と整合"
        );
    }

    /// ancestor_chain: 中間 message を起点にするとそこまでの系譜のみ返す (子孫は含まない)
    #[tokio::test]
    async fn ancestor_chain_from_middle_excludes_descendants() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        let r1 = store
            .send_reply("bob@vp", &[], body("r1"), &root.id)
            .await
            .expect("r1");
        // r1 の子孫 r2 を作る。 r1 起点の系譜には r2 は含まれない
        let _r2 = store
            .send_reply("alice@vp", &[], body("r2"), &r1.id)
            .await
            .expect("r2");

        let chain = store.ancestor_chain(&r1.id).await.expect("chain");
        let ids: Vec<&str> = chain.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![root.id.as_str(), r1.id.as_str()],
            "中間 message の系譜は root→自身のみ (子孫は含まない)"
        );
    }

    /// ancestor_chain: 存在しない message id は Err
    #[tokio::test]
    async fn ancestor_chain_missing_message_errors() {
        let store = make_test_store().await;
        let result = store.ancestor_chain("nonexistent-id").await;
        assert!(result.is_err(), "存在しない message は Err");
    }

    /// ancestor_chain: prev chain が壊れて親が見つからない場合は辿れた分を返す (防御的)
    ///
    /// R2: `walk_to_root` の既存の防御的挙動と整合 — prev chain 断裂時は
    /// そこまで収集した分を root-first で返す。
    #[tokio::test]
    async fn ancestor_chain_broken_prev_returns_collected_so_far() {
        let store = make_test_store().await;
        // root を作り、 その後 root レコードを削除して prev chain を断裂させる
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        let r1 = store
            .send_reply("bob@vp", &[], body("r1"), &root.id)
            .await
            .expect("r1");
        // root レコードを物理削除 → r1.prev が dangling になる
        store
            .db()
            .query("DELETE type::record('wire_messages', $id);")
            .bind(("id", root.id.clone()))
            .await
            .expect("delete root")
            .check()
            .expect("delete root check");

        // r1 起点: r1 自身は引けるが prev (= root) が見つからない → r1 のみ返す
        let chain = store.ancestor_chain(&r1.id).await.expect("chain");
        let ids: Vec<&str> = chain.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![r1.id.as_str()],
            "prev chain 断裂時は辿れた分のみ返す (防御的)"
        );
    }

    /// ancestor_chain は read-only — agent_cursor を一切前進させない (冪等)
    ///
    /// R2: `wire_thread` は `wire_recv` と対で、 cursor を触らない read-only tool。
    #[tokio::test]
    async fn ancestor_chain_does_not_touch_cursor() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("root");
        let r1 = store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("r1"), &root.id)
            .await
            .expect("r1");

        // bob の未読は root + r1 の 2 件
        let before = store.fetch_unread("bob@vp").await.expect("before");
        assert_eq!(before.len(), 2, "ancestor_chain 呼出前は未読 2 件");

        // ancestor_chain を複数回呼んでも cursor は不変
        let _ = store.ancestor_chain(&r1.id).await.expect("chain 1");
        let _ = store.ancestor_chain(&r1.id).await.expect("chain 2");

        let after = store.fetch_unread("bob@vp").await.expect("after");
        assert_eq!(
            after.len(),
            2,
            "ancestor_chain は read-only — cursor を前進させない"
        );
    }

    // -------------------------------------------------------------------------
    // thread_participant — sparse 例外表 (left のみ行を持つ)
    // -------------------------------------------------------------------------

    /// reply-all 展開は left した agent を除外する (send 側 left 強制、 R1)
    ///
    /// R1: left 強制を send 側に移した。 reply-all 展開時に thread root id を walk して
    /// 求め、 `thread_participant` の left を引いて `to` から外す。
    #[tokio::test]
    async fn reply_expansion_excludes_left() {
        let store = make_test_store().await;
        let root = store
            .send_root(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("root"),
            )
            .await
            .expect("send_root");

        // carol が left (thread = root id)
        mark_left(&store, &root.id, "carol@vp").await;

        // bob が reply — reply-all 展開でも carol (left) は to に入らない
        let reply = store
            .send_reply("bob@vp", &[], body("reply"), &root.id)
            .await
            .expect("send_reply");
        assert!(
            !reply.to.contains(&"carol@vp".to_string()),
            "left した agent は reply-all 展開から除外される"
        );
        assert!(reply.to.contains(&"alice@vp".to_string()), "alice は残る");
    }

    /// left した agent は以後の reply の to に入らないが、 leave 前の未読は drain できる
    ///
    /// R1: recv 側 left filter を廃止 — left の意味論は「以後 (henceforth) 新規 reply の
    /// `to` に入らない」。 leave 前に既に `to` に入っている未読 message は通常通り読める。
    /// REQ-THREAD-007「以後 ping を受けない」と整合。
    #[tokio::test]
    async fn left_agent_drains_pre_leave_unread_but_not_new_replies() {
        let store = make_test_store().await;
        // root が bob 宛 — bob は未読として 1 件持つ
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("send_root");

        // bob が leave する (root より後、 だが root はまだ未読)
        mark_left(&store, &root.id, "bob@vp").await;

        // leave 前から to に入っていた未読 (root) は drain できる
        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert_eq!(
            unread.len(),
            1,
            "leave 前から to に入っていた未読は drain できる (recv 側 filter 廃止)"
        );
        assert_eq!(unread[0].id, root.id);

        // alice が reply — left した bob は reply-all 展開の to に入らない
        let reply = store
            .send_reply("alice@vp", &[], body("reply"), &root.id)
            .await
            .expect("send_reply");
        assert!(
            !reply.to.contains(&"bob@vp".to_string()),
            "left した agent は以後の reply の to に入らない"
        );

        // bob の未読は依然 root の 1 件のみ — left 後の reply は届かない
        let after = store.fetch_unread("bob@vp").await.expect("fetch after");
        assert_eq!(after.len(), 1, "left 後の reply は to に入らず届かない");
        assert_eq!(
            after[0].id, root.id,
            "届くのは leave 前から to の root のみ"
        );
    }

    // -------------------------------------------------------------------------
    // fetch_unread — 横断 / 空 / unread_count derive
    // -------------------------------------------------------------------------

    /// 複数 thread にまたがる未読が local_seq 昇順で返る
    #[tokio::test]
    async fn fetch_unread_spans_multiple_threads_sorted() {
        let store = make_test_store().await;
        // bob 宛に 2 つの別 thread
        let t1 = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("t1"))
            .await
            .expect("t1");
        let t2 = store
            .send_root("carol@vp", &["bob@vp".to_string()], body("t2"))
            .await
            .expect("t2");
        assert_ne!(t1.id, t2.id, "別 thread (root id が異なる)");

        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert_eq!(unread.len(), 2, "2 thread 分の未読");
        assert!(
            unread[0].local_seq < unread[1].local_seq,
            "local_seq 昇順で整列"
        );
    }

    /// fetch_unread で宛先になったことが無い agent は空 vec
    #[tokio::test]
    async fn fetch_unread_no_messages_is_empty() {
        let store = make_test_store().await;
        let unread = store.fetch_unread("stranger@vp").await.expect("fetch");
        assert!(unread.is_empty());
    }

    /// unread_count_by_thread: 未読を thread root id で GROUP BY した count を derive
    #[tokio::test]
    async fn unread_count_by_thread_derives_per_thread() {
        let store = make_test_store().await;
        // thread t1: root + reply の 2 件が bob 宛
        let t1 = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("t1-root"))
            .await
            .expect("t1");
        store
            .send_reply(
                "alice@vp",
                &["bob@vp".to_string()],
                body("t1-reply"),
                &t1.id,
            )
            .await
            .expect("t1 reply");
        // thread t2: root の 1 件が bob 宛
        let t2 = store
            .send_root("carol@vp", &["bob@vp".to_string()], body("t2-root"))
            .await
            .expect("t2");

        let counts = store
            .unread_count_by_thread("bob@vp")
            .await
            .expect("unread count");
        // thread root id が GROUP BY key (R1: thread_id 全廃)
        assert_eq!(counts.get(&t1.id).copied(), Some(2), "t1 は 2 件");
        assert_eq!(counts.get(&t2.id).copied(), Some(1), "t2 は 1 件");

        // recv で drain した後は未読 0 → HashMap は空
        let _ = store.recv("bob@vp").await.expect("drain");
        let after = store
            .unread_count_by_thread("bob@vp")
            .await
            .expect("unread count after");
        assert!(after.is_empty(), "drain 後は未読 thread なし");
    }

    // -------------------------------------------------------------------------
    // advance_cursor — 単一 cursor の前進規律
    // -------------------------------------------------------------------------

    /// advance_cursor は per-agent 単一 cursor を後退させない
    #[tokio::test]
    async fn advance_cursor_does_not_regress() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // 大きい local_seq 値に前進
        store
            .advance_cursor("bob@vp", 1_000_000)
            .await
            .expect("advance big");
        // 小さい値で再前進を試みる
        store
            .advance_cursor("bob@vp", 1)
            .await
            .expect("advance small");

        // cursor が 1 に後退していないこと → message は依然既読 (未読 0)
        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert!(unread.is_empty(), "cursor は後退しない");
    }

    /// advance_cursor は agent_cursor 行が無ければ作成する (UPSERT)
    #[tokio::test]
    async fn advance_cursor_creates_row_if_absent() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // cursor 行が無い状態でいきなり advance
        store
            .advance_cursor("bob@vp", 1_000_000)
            .await
            .expect("advance creates row");

        // 行が作られ cursor が効いている → 未読 0
        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert!(unread.is_empty(), "cursor 行が作成され未読が drain される");
    }

    /// advance_cursor を同一 agent で並行実行しても二重 CREATE で失敗しない (moody #2)
    ///
    /// R1: 旧実装は SELECT→CREATE が non-atomic で、 並行 recv の二重 CREATE が unique
    /// 制約エラーになった。 UPSERT 文に畳んで atomic にした。
    #[tokio::test]
    async fn advance_cursor_concurrent_is_atomic() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // cursor 行が無い状態から、 同一 agent の advance を並行実行する
        let s1 = store.clone();
        let s2 = store.clone();
        let h1 = tokio::spawn(async move { s1.advance_cursor("bob@vp", 5).await });
        let h2 = tokio::spawn(async move { s2.advance_cursor("bob@vp", 7).await });
        let (r1, r2) = (h1.await.expect("join1"), h2.await.expect("join2"));
        assert!(r1.is_ok(), "並行 advance #1 が unique 制約で失敗しない");
        assert!(r2.is_ok(), "並行 advance #2 が unique 制約で失敗しない");

        // 行は 1 つだけ (二重 CREATE していない)
        let mut res = store
            .db()
            .query("SELECT count() FROM agent_cursor WHERE agent = 'bob@vp' GROUP ALL;")
            .await
            .expect("count query");
        let rows: Vec<serde_json::Value> = res.take(0).expect("count take");
        let count = rows.first().and_then(|r| r["count"].as_u64()).unwrap_or(0);
        assert_eq!(count, 1, "agent_cursor 行は per-agent 1 つだけ");
    }

    // -------------------------------------------------------------------------
    // WireNotifier — 取りこぼし防止
    // -------------------------------------------------------------------------

    /// WireNotifier: notified() future を先に生成しておけば後続 notify を拾える
    #[tokio::test]
    async fn wire_notifier_future_before_notify_is_caught() {
        let notifier = WireNotifier::new();
        let handle = notifier.handle("bob@vp").await;
        let fut = handle.notified();
        tokio::pin!(fut);
        notifier.notify("bob@vp").await;
        tokio::time::timeout(std::time::Duration::from_millis(500), fut)
            .await
            .expect("future 生成後の notify は捕捉される");
    }

    /// WireNotifier: 別 agent への notify では起きない
    #[tokio::test]
    async fn wire_notifier_isolated_per_agent() {
        let notifier = WireNotifier::new();
        let handle = notifier.handle("bob@vp").await;
        let fut = handle.notified();
        tokio::pin!(fut);
        notifier.notify("carol@vp").await;
        let result = tokio::time::timeout(std::time::Duration::from_millis(150), fut).await;
        assert!(result.is_err(), "別 agent への notify では起床しない");
    }
}
