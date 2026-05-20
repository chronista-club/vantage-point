//! SurrealDB 統合モジュール (embed mode)
//!
//! VP の状態管理を **プロセス内の SurrealDB** に統一する。
//! surrealkv backend を使い、外部 `surreal` バイナリは不要。
//!
//! ## 接続方式
//!
//! - 本番: `surrealkv://<data_dir>` で in-process embedded DB
//! - テスト: `kv-mem` で in-memory embedded DB
//!
//! 単一プロセスが DB を保持する single-writer モデル。
//! 複数 Process が同時に書くユースケースは現状ない (TheWorld が集約点)。
//!
//! ## テーブル設計
//!
//! - `processes`: プロセス状態（QUIC Registry + HTTP polling 代替）
//! - `msgbox`: cross-process メッセージング
//! - `pane_contents`: Canvas ペイン状態
//! - `stand_status`: Stand ステータス
//! - `prompts`: User Prompt
//! - `notifications`: CC 通知

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

// LIVE SELECT の Action enum を caller に露出 (downstream が surrealdb 直接依存しなくて済むように)
pub use surrealdb::types::Action;

/// SurrealDB の名前空間
const NS: &str = "vp";

/// SurrealDB のデータベース名
const DB_NAME: &str = "vp";

/// SurrealDB データディレクトリの root (`vp_data_dir()/db`)
///
/// VP-192: config/data パス一本化。 DB は生成データなので `vp_data_dir()`
/// (macOS `~/Library/Application Support/vp/`、 Linux `~/.local/share/vp/`、
/// Windows `%LOCALAPPDATA%\vp\`) 配下に置く。 Windows の `%APPDATA%` は roaming
/// 同期対象で DB 破損リスクがあるため、 data には `%LOCALAPPDATA%` を使う。
fn db_root() -> PathBuf {
    crate::config::vp_data_dir().join("db")
}

/// World daemon (TheWorld) 専用の DB ディレクトリ (`vp_data_dir()/db/world`)
///
/// VP-182: surrealkv は OS レベル排他ロック (`try_lock_exclusive`) を持つため、
/// World と SP が同一ディレクトリを open すると LOCK 衝突で 2 番目が失敗する。
/// World は `processes` テーブルを保持する専用 DB を `db/world/` に分離。
/// (VP-188: 旧 `projects` テーブルは projects.kdl に移行済)
pub fn db_data_dir_for_world() -> PathBuf {
    db_root().join("world")
}

/// Project (SP / Star Platinum) 専用の DB ディレクトリ (`vp_data_dir()/db/sp_{slug}`)
///
/// VP-182: 各 SP は project slug 別の独立 DB ディレクトリを使う。 doc 19 §4.6 の
/// 「各 SP は自分の DB に write」 設計をディレクトリ分離で物理化し、 World との
/// surrealkv LOCK 衝突を構造的に解消する。 cross-process forward は受信側 SP の
/// DB に書く設計 (doc 19 §4.6) なので per-SP 分離と整合する。
pub fn db_data_dir_for_project(slug: &str) -> PathBuf {
    db_root().join(format!("sp_{}", slug))
}

/// VP のデータベースクライアント
///
/// `Surreal<Any>` を使うことで embedded (surrealkv) と kv-mem (テスト) の両方に対応。
pub struct VpDb {
    db: Surreal<Any>,
}

/// Arc でラップした VpDb（複数コンポーネントで共有するため）
pub type SharedVpDb = Arc<VpDb>;

impl VpDb {
    /// ローカルファイルシステム上の surrealkv DB を開いて接続する
    ///
    /// - `data_dir` が無ければ作成
    /// - 認証なし (in-process のみアクセス可能なのでパスワード不要)
    pub async fn connect_embedded(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| anyhow::anyhow!("DB data dir 作成失敗 ({}): {}", data_dir.display(), e))?;
        let endpoint = format!("surrealkv://{}", data_dir.display());
        let db = surrealdb::engine::any::connect(&endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("SurrealDB embedded 接続失敗 ({}): {}", endpoint, e))?;
        db.use_ns(NS).use_db(DB_NAME).await?;
        tracing::info!("SurrealDB 接続成功 (embedded: {})", endpoint);
        Ok(Self { db })
    }

    /// kv-mem (in-memory) で接続（テスト用、 integration test からも利用可）
    ///
    /// VP-174 (Phase 3 PR-2) で `#[cfg(test)]` 除去、 integration test (= `tests/*.rs`) からも
    /// 呼べるように pub 化。 production code は `connect_embedded` を使う。
    pub async fn connect_mem() -> Result<Self> {
        let db = surrealdb::engine::any::connect("mem://").await?;
        db.use_ns(NS).use_db(DB_NAME).await?;
        Ok(Self { db })
    }

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
        Ok(())
    }

    /// ヘルスチェック（DB に接続できているか確認）
    pub async fn health(&self) -> bool {
        self.db.query("RETURN true").await.is_ok()
    }

    /// 内部の Surreal への参照を取得
    pub fn inner(&self) -> &Surreal<Any> {
        &self.db
    }

    // VP-188: Projects CRUD は撤去。 registered projects の SSOT は embedded DB から
    // `~/.config/vp/projects.kdl` に移行 (= VP-182 の「DB dir 変更で projects 消失」
    // regression を構造的に解消、 council 2026-05-16)。 projects 永続化は
    // `crate::projects_file::ProjectsFile` が担う。

    // =========================================================================
    // Processes CRUD
    // =========================================================================

    /// 稼働中プロセスを登録（UPSERT）
    pub async fn upsert_process(
        &self,
        project_path: &str,
        project_name: &str,
        port: u16,
        pid: u32,
        status: &str,
        tmux_session: Option<&str>,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO processes {
                    project_path: $project_path,
                    project_name: $project_name,
                    port: $port,
                    pid: $pid,
                    status: $status,
                    started_at: time::now(),
                    tmux_session: $tmux_session
                } ON DUPLICATE KEY UPDATE
                    project_name = $input.project_name,
                    port = $input.port,
                    pid = $input.pid,
                    status = $input.status,
                    tmux_session = $input.tmux_session",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("project_name", project_name.to_string()))
            .bind(("port", port as i64))
            .bind(("pid", pid as i64))
            .bind(("status", status.to_string()))
            .bind(("tmux_session", tmux_session.map(|s| s.to_string())))
            .await
            .map_err(|e| anyhow::anyhow!("process upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("process upsert エラー: {}", e))?;
        Ok(())
    }

    /// プロセスを登録解除（project_path で特定）
    pub async fn delete_process(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM processes WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("process 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("process 削除エラー: {}", e))?;
        Ok(())
    }

    /// 稼働中プロセス一覧を取得
    pub async fn list_processes(&self) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM processes")
            .await
            .map_err(|e| anyhow::anyhow!("processes 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// 全プロセスを削除（TheWorld 再起動時のクリーンアップ用）
    pub async fn clear_all_processes(&self) -> Result<()> {
        self.db
            .query("DELETE FROM processes")
            .await
            .map_err(|e| anyhow::anyhow!("processes クリア失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("processes クリアエラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Pane Contents CRUD（Canvas ペイン状態の永続化）
    // =========================================================================

    /// ペイン状態を保存（UPSERT: project_path + pane_id で一意）
    pub async fn upsert_pane_content(
        &self,
        project_path: &str,
        pane_id: &str,
        content_type: &str,
        content: &str,
        title: Option<&str>,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO pane_contents {
                    project_path: $project_path,
                    pane_id: $pane_id,
                    content_type: $content_type,
                    content: $content,
                    title: $title,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("content_type", content_type.to_string()))
            .bind(("content", content.to_string()))
            .bind(("title", title.map(|s| s.to_string())))
            .await
            .map_err(|e| anyhow::anyhow!("pane_content upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pane_content upsert エラー: {}", e))?;
        Ok(())
    }

    /// プロジェクトの全ペイン状態を取得
    pub async fn list_pane_contents(&self, project_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM pane_contents WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// プロジェクトの全ペイン状態を削除
    pub async fn clear_pane_contents(&self, project_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM pane_contents WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pane_contents 削除エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // Stand Status CRUD
    // =========================================================================

    /// Stand ステータスを更新（UPSERT）
    pub async fn upsert_stand_status(
        &self,
        project_path: &str,
        stand_key: &str,
        status: &str,
        detail: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.db
            .query(
                "INSERT INTO stand_status {
                    project_path: $project_path,
                    stand_key: $stand_key,
                    status: $status,
                    detail: $detail,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    status = $input.status,
                    detail = $input.detail,
                    updated_at = time::now()",
            )
            .bind(("project_path", project_path.to_string()))
            .bind(("stand_key", stand_key.to_string()))
            .bind(("status", status.to_string()))
            .bind(("detail", detail.cloned()))
            .await
            .map_err(|e| anyhow::anyhow!("stand_status upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("stand_status upsert エラー: {}", e))?;
        Ok(())
    }

    // =========================================================================
    // LIVE SELECT（リアルタイム変更通知）
    // =========================================================================

    /// processes テーブルの LIVE SELECT を開始
    ///
    /// INSERT/UPDATE/DELETE のたびに `Notification<serde_json::Value>` を返すストリーム。
    /// TheWorld が購読して DistributedNotification に変換する。
    ///
    /// 返り値は `'static` ライフタイム（`Surreal<Any>` は内部 Arc なので clone が軽量）。
    pub async fn live_processes(
        &self,
    ) -> Result<surrealdb::method::Stream<Vec<serde_json::Value>>> {
        let stream = self
            .db
            .select("processes")
            .live()
            .await
            .map_err(|e| anyhow::anyhow!("LIVE SELECT processes 失敗: {}", e))?;
        Ok(stream)
    }

    /// プロジェクトの全 Stand ステータスを取得
    pub async fn list_stand_status(&self, project_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM stand_status WHERE project_path = $path")
            .bind(("path", project_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("stand_status 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
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
DEFINE FIELD IF NOT EXISTS project_path ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS project_name ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS port ON processes TYPE int;
DEFINE FIELD IF NOT EXISTS pid ON processes TYPE int;
DEFINE FIELD IF NOT EXISTS status ON processes TYPE string;
DEFINE FIELD IF NOT EXISTS started_at ON processes TYPE datetime;
DEFINE FIELD IF NOT EXISTS stands ON processes TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS tmux_session ON processes TYPE option<string>;
DEFINE INDEX IF NOT EXISTS idx_processes_path ON processes COLUMNS project_path UNIQUE;

-- VP-188: projects テーブルは撤去。 registered projects の SSOT は
-- ~/.config/vp/projects.kdl に移行 (crate::projects_file)。

-- Msgbox（cross-process メッセージング）
DEFINE TABLE IF NOT EXISTS msgbox SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS from_addr ON msgbox TYPE string;
DEFINE FIELD IF NOT EXISTS to_addr ON msgbox TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON msgbox TYPE string;
DEFINE FIELD IF NOT EXISTS payload ON msgbox TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS reply_to ON msgbox TYPE option<string>;
DEFINE FIELD IF NOT EXISTS delivered ON msgbox TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS created_at ON msgbox TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_to ON msgbox COLUMNS to_addr, delivered;

-- =========================================================================
-- SP 固有テーブル（project_path でフィルタ — D11 準拠）
-- =========================================================================

-- Canvas ペイン状態（RetainedStore + JSON 永続化 代替）
DEFINE TABLE IF NOT EXISTS pane_contents SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS pane_id ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS content_type ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS content ON pane_contents TYPE string;
DEFINE FIELD IF NOT EXISTS title ON pane_contents TYPE option<string>;
DEFINE FIELD IF NOT EXISTS updated_at ON pane_contents TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_pane ON pane_contents COLUMNS project_path, pane_id UNIQUE;

-- Stand ステータス
DEFINE TABLE IF NOT EXISTS stand_status SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON stand_status TYPE string;
DEFINE FIELD IF NOT EXISTS stand_key ON stand_status TYPE string;
DEFINE FIELD IF NOT EXISTS status ON stand_status TYPE string;
DEFINE FIELD IF NOT EXISTS detail ON stand_status TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON stand_status TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_stand ON stand_status COLUMNS project_path, stand_key UNIQUE;

-- User Prompt（2秒ポーリング廃止）
DEFINE TABLE IF NOT EXISTS prompts SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project_path ON prompts TYPE string;
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
DEFINE FIELD IF NOT EXISTS project_path ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS project_name ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS message ON notifications TYPE string;
DEFINE FIELD IF NOT EXISTS read ON notifications TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS created_at ON notifications TYPE datetime DEFAULT time::now();

-- =========================================================================
-- VP-169 msgs (Whitesnake-primary msgbox、 SDG doc 19 §4.1)
-- =========================================================================
-- Phase 2 PR-1 で受け皿 schema 追加。 既存 msgbox table (= 旧 mpsc + DISC hybrid) と並走、
-- Phase 5 で旧 msgbox 削除。 SDG §4.8 status field lifecycle (active/dead_letter/archived)。
DEFINE TABLE IF NOT EXISTS msgs SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS id ON msgs TYPE string;
DEFINE FIELD IF NOT EXISTS ts ON msgs TYPE number;
DEFINE FIELD IF NOT EXISTS kind ON msgs TYPE string DEFAULT 'direct';
DEFINE FIELD IF NOT EXISTS payload ON msgs TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS reply_to ON msgs TYPE option<string>;
-- routing target (= denormalized for indexed query)
DEFINE FIELD IF NOT EXISTS to_addr ON msgs TYPE string;
DEFINE FIELD IF NOT EXISTS to_actor ON msgs TYPE string;
DEFINE FIELD IF NOT EXISTS to_lane ON msgs TYPE string;
DEFINE FIELD IF NOT EXISTS to_project ON msgs TYPE option<string>;
DEFINE FIELD IF NOT EXISTS to_world ON msgs TYPE option<string>;
-- routing source
DEFINE FIELD IF NOT EXISTS from_addr ON msgs TYPE string;
DEFINE FIELD IF NOT EXISTS from_actor ON msgs TYPE string;
DEFINE FIELD IF NOT EXISTS from_lane ON msgs TYPE string;
DEFINE FIELD IF NOT EXISTS from_project ON msgs TYPE option<string>;
DEFINE FIELD IF NOT EXISTS from_world ON msgs TYPE option<string>;
-- lifecycle markers (VP-164 schema 継承)
DEFINE FIELD IF NOT EXISTS expires_at ON msgs TYPE option<number>;
DEFINE FIELD IF NOT EXISTS manual_ack ON msgs TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS forwarded_at ON msgs TYPE option<number>;
DEFINE FIELD IF NOT EXISTS consumed_at ON msgs TYPE option<number>;
-- status field (SDG §4.8)
DEFINE FIELD IF NOT EXISTS status ON msgs TYPE string DEFAULT 'active';
DEFINE FIELD IF NOT EXISTS status_at ON msgs TYPE number;
-- concurrent recv claim (SDG §4.3)
DEFINE FIELD IF NOT EXISTS claim_id ON msgs TYPE option<string>;
DEFINE FIELD IF NOT EXISTS claimed_at ON msgs TYPE option<number>;
-- audit trail (SDG §4.10、 改善 8 吸収)
DEFINE FIELD IF NOT EXISTS trace ON msgs TYPE array DEFAULT [];
-- 主 query path index (SDG §4.1)
DEFINE INDEX IF NOT EXISTS recv_idx ON msgs FIELDS status, to_actor, to_lane, consumed_at;
DEFINE INDEX IF NOT EXISTS status_idx ON msgs FIELDS status, expires_at;

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
--     各 SP は自分の accumulation の唯一の writer なので厳密単調。 cursor 比較は
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
"#;

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用ヘルパー: kv-mem VpDb をスキーマ付きで作成
    async fn make_test_db() -> VpDb {
        let db = VpDb::connect_mem().await.unwrap();
        db.define_schema().await.unwrap();
        db
    }

    #[test]
    fn test_db_data_dir_world_and_project_separated() {
        let world = db_data_dir_for_world();
        let proj = db_data_dir_for_project("vantage-point");

        // VP-192: 両方とも vp_data_dir()/db 配下
        assert!(
            world.to_string_lossy().contains("db"),
            "World DB dir に 'db' が含まれていない: {}",
            world.display()
        );
        assert!(
            world.starts_with(crate::config::vp_data_dir()),
            "World DB dir は vp_data_dir() 配下であるべき: {}",
            world.display()
        );
        // VP-182: World と SP の DB ディレクトリは別であること (= surrealkv LOCK 衝突回避)
        assert_ne!(
            world, proj,
            "World と project の DB ディレクトリは分離されているべき"
        );
        assert!(
            world.ends_with("world"),
            "World DB dir は 'world' で終わるべき: {}",
            world.display()
        );
        assert!(
            proj.ends_with("sp_vantage-point"),
            "project DB dir は 'sp_<slug>' 形式であるべき: {}",
            proj.display()
        );
    }

    #[test]
    fn test_constants() {
        assert_eq!(NS, "vp");
        assert_eq!(DB_NAME, "vp");
    }

    #[tokio::test]
    async fn test_define_schema_mem() {
        let db = make_test_db().await;
        assert!(db.health().await, "ヘルスチェック失敗");
    }

    #[tokio::test]
    async fn test_msgbox_crud_mem() {
        let db = make_test_db().await;

        // メッセージ送信
        db.inner()
            .query(
                "INSERT INTO msgbox {
                    from_addr: 'mcp',
                    to_addr: 'notify',
                    kind: 'notification',
                    payload: { message: 'テスト完了' },
                    delivered: false,
                    created_at: time::now()
                }",
            )
            .await
            .unwrap();

        // 未配信メッセージを取得
        let mut result = db
            .inner()
            .query("SELECT * FROM msgbox WHERE to_addr = 'notify' AND delivered = false")
            .await
            .unwrap();
        let records: Vec<serde_json::Value> = result.take(0).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["kind"], "notification");
    }

    // VP-188: Projects CRUD テストは撤去 (= projects は projects.kdl に移行、
    // crate::projects_file 側の round-trip test でカバー)。

    // =========================================================================
    // Processes CRUD テスト
    // =========================================================================

    #[tokio::test]
    async fn test_processes_crud() {
        let db = make_test_db().await;

        // 登録
        db.upsert_process("/repos/vp", "vp", 33000, 1234, "running", None)
            .await
            .unwrap();

        // 一覧
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["project_name"], "vp");
        assert_eq!(procs[0]["port"], 33000);

        // 更新（同じ path で upsert）
        db.upsert_process("/repos/vp", "vp", 33001, 5678, "running", Some("vp-vp"))
            .await
            .unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0]["port"], 33001);
        assert_eq!(procs[0]["tmux_session"], "vp-vp");

        // 削除
        db.delete_process("/repos/vp").await.unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    #[tokio::test]
    async fn test_processes_clear_all() {
        let db = make_test_db().await;

        db.upsert_process("/a", "a", 33000, 1, "running", None)
            .await
            .unwrap();
        db.upsert_process("/b", "b", 33001, 2, "running", None)
            .await
            .unwrap();

        db.clear_all_processes().await.unwrap();
        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
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

    // =========================================================================
    // Processes エッジケーステスト
    // =========================================================================

    /// tmux_session=None を保存 → NULL として保存される
    #[tokio::test]
    async fn test_processes_tmux_session_none() {
        let db = make_test_db().await;

        db.upsert_process("/repos/vp", "vp", 33000, 1234, "running", None)
            .await
            .unwrap();

        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 1);
        // NULL は serde_json::Value::Null として取得される
        assert!(
            procs[0]["tmux_session"].is_null(),
            "tmux_session が NULL でない: {:?}",
            procs[0]["tmux_session"]
        );
    }

    /// 存在しない project_path を delete_process してもエラーにならない
    #[tokio::test]
    async fn test_processes_delete_nonexistent() {
        let db = make_test_db().await;

        // 何も INSERT せずに DELETE → エラーなし（空操作）
        db.delete_process("/repos/nonexistent")
            .await
            .expect("存在しないレコードの削除はエラーにならない");

        let procs = db.list_processes().await.unwrap();
        assert_eq!(procs.len(), 0);
    }

    // =========================================================================
    // Pane Contents CRUD テスト
    // =========================================================================

    /// 基本的な INSERT → SELECT フロー
    #[tokio::test]
    async fn test_pane_contents_basic_crud() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r##"{"Markdown":"# Hello"}"##,
            Some("テストペイン"),
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["pane_id"], "pane-1");
        assert_eq!(panes[0]["content_type"], "markdown");
        assert_eq!(panes[0]["content"], r##"{"Markdown":"# Hello"}"##);
        assert_eq!(panes[0]["title"], "テストペイン");
    }

    /// 同一 (project_path, pane_id) で再度 upsert → content が更新される（UPSERT 冪等性）
    #[tokio::test]
    async fn test_pane_contents_upsert_updates_content() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r#"{"Markdown":"初回内容"}"#,
            Some("初回タイトル"),
        )
        .await
        .unwrap();

        // 同じ pane_id で異なる content
        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "html",
            r#"{"Html":"<h1>更新後</h1>"}"#,
            Some("更新後タイトル"),
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        // レコード数は1のまま（UPSERT）
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["content_type"], "html");
        assert_eq!(panes[0]["content"], r#"{"Html":"<h1>更新後</h1>"}"#);
        assert_eq!(panes[0]["title"], "更新後タイトル");
    }

    /// 異なる project_path のペインは list_pane_contents で見えない（プロジェクト分離）
    #[tokio::test]
    async fn test_pane_contents_project_isolation() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r#"{"Markdown":"VP の内容"}"#,
            None,
        )
        .await
        .unwrap();

        db.upsert_pane_content(
            "/repos/creo",
            "pane-1",
            "markdown",
            r#"{"Markdown":"Creo の内容"}"#,
            None,
        )
        .await
        .unwrap();

        // VP のペイン → VP の内容だけ見える
        let vp_panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(vp_panes.len(), 1);
        assert_eq!(vp_panes[0]["content"], r#"{"Markdown":"VP の内容"}"#);

        // Creo のペイン → Creo の内容だけ見える
        let creo_panes = db.list_pane_contents("/repos/creo").await.unwrap();
        assert_eq!(creo_panes.len(), 1);
        assert_eq!(creo_panes[0]["content"], r#"{"Markdown":"Creo の内容"}"#);
    }

    /// clear_pane_contents は対象 project_path のみ削除（他プロジェクトに影響なし）
    #[tokio::test]
    async fn test_pane_contents_clear_isolates_projects() {
        let db = make_test_db().await;

        db.upsert_pane_content("/repos/vp", "pane-1", "log", r#"{"Log":[]}"#, None)
            .await
            .unwrap();
        db.upsert_pane_content("/repos/creo", "pane-2", "log", r#"{"Log":[]}"#, None)
            .await
            .unwrap();

        // VP のみクリア
        db.clear_pane_contents("/repos/vp").await.unwrap();

        let vp_panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(vp_panes.len(), 0, "VP のペインはクリアされている");

        let creo_panes = db.list_pane_contents("/repos/creo").await.unwrap();
        assert_eq!(creo_panes.len(), 1, "Creo のペインは残っている");
    }

    /// title が None → NULL で保存・復元できる
    #[tokio::test]
    async fn test_pane_contents_title_null() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-notitle",
            "url",
            r#"{"Url":"https://example.com"}"#,
            None,
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(panes.len(), 1);
        assert!(
            panes[0]["title"].is_null(),
            "title が NULL でない: {:?}",
            panes[0]["title"]
        );
    }

    // =========================================================================
    // Stand Status CRUD テスト
    // =========================================================================

    /// 基本的な INSERT → SELECT フロー
    #[tokio::test]
    async fn test_stand_status_basic_crud() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["stand_key"], "heaven-door");
        assert_eq!(statuses[0]["status"], "running");
    }

    /// 同一 (project_path, stand_key) で再度 upsert → status が更新される
    #[tokio::test]
    async fn test_stand_status_upsert_updates_status() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();

        db.upsert_stand_status("/repos/vp", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        // レコード数は1のまま（UPSERT）
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["status"], "stopped");
    }

    /// detail が None → NULL で保存できる
    #[tokio::test]
    async fn test_stand_status_detail_null() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "paisley-park", "idle", None)
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert!(
            statuses[0]["detail"].is_null(),
            "detail が NULL でない: {:?}",
            statuses[0]["detail"]
        );
    }

    /// detail に JSON オブジェクト → 保存・復元できる
    #[tokio::test]
    async fn test_stand_status_detail_with_json() {
        let db = make_test_db().await;

        let detail = serde_json::json!({
            "canvas_open": true,
            "pane_count": 3
        });

        db.upsert_stand_status("/repos/vp", "paisley-park", "running", Some(&detail))
            .await
            .unwrap();

        let statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["detail"]["canvas_open"], true);
        assert_eq!(statuses[0]["detail"]["pane_count"], 3);
    }

    /// 異なる project_path の stand_status は分離される
    #[tokio::test]
    async fn test_stand_status_project_isolation() {
        let db = make_test_db().await;

        db.upsert_stand_status("/repos/vp", "heaven-door", "running", None)
            .await
            .unwrap();
        db.upsert_stand_status("/repos/creo", "heaven-door", "stopped", None)
            .await
            .unwrap();

        let vp_statuses = db.list_stand_status("/repos/vp").await.unwrap();
        assert_eq!(vp_statuses.len(), 1);
        assert_eq!(vp_statuses[0]["status"], "running");

        let creo_statuses = db.list_stand_status("/repos/creo").await.unwrap();
        assert_eq!(creo_statuses.len(), 1);
        assert_eq!(creo_statuses[0]["status"], "stopped");
    }

    // =========================================================================
    // Msgbox 配信フローテスト
    // =========================================================================

    /// delivered=false で INSERT → 取得できる
    /// delivered=true に UPDATE → 未配信一覧から消える
    #[tokio::test]
    async fn test_msgbox_delivered_flag() {
        let db = make_test_db().await;

        // 未配信メッセージを挿入（RETURN AFTER で挿入後レコードを取得）
        let mut result = db
            .inner()
            .query(
                "INSERT INTO msgbox {
                    from_addr: 'mcp',
                    to_addr: 'notify',
                    kind: 'notification',
                    payload: { message: '配信テスト' },
                    delivered: false,
                    created_at: time::now()
                } RETURN AFTER",
            )
            .await
            .unwrap();
        let inserted: Vec<serde_json::Value> = result.take(0).unwrap();
        assert_eq!(inserted.len(), 1);
        let msg_id = inserted[0]["id"].as_str().unwrap().to_string();

        // 未配信一覧に含まれる
        let mut result = db
            .inner()
            .query("SELECT * FROM msgbox WHERE to_addr = 'notify' AND delivered = false")
            .await
            .unwrap();
        let pending: Vec<serde_json::Value> = result.take(0).unwrap();
        assert_eq!(pending.len(), 1, "未配信メッセージが1件あるはず");

        // delivered=true に更新（type::record はレコード ID を RecordId 型に変換する SurrealDB v2 の関数）
        db.inner()
            .query("UPDATE type::record($id) SET delivered = true")
            .bind(("id", msg_id))
            .await
            .unwrap()
            .check()
            .unwrap();

        // 未配信一覧から消える
        let mut result = db
            .inner()
            .query("SELECT * FROM msgbox WHERE to_addr = 'notify' AND delivered = false")
            .await
            .unwrap();
        let pending: Vec<serde_json::Value> = result.take(0).unwrap();
        assert_eq!(
            pending.len(),
            0,
            "配信済みメッセージは未配信一覧に出てはいけない"
        );

        // delivered=true のレコードは全件取得では見える
        let mut result = db
            .inner()
            .query("SELECT * FROM msgbox WHERE to_addr = 'notify'")
            .await
            .unwrap();
        let all: Vec<serde_json::Value> = result.take(0).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0]["delivered"], true);
    }

    // =========================================================================
    // LIVE SELECT テスト
    // =========================================================================

    /// kv-mem で live_processes を開始してストリームが取得できる（接続確認）
    #[tokio::test]
    async fn test_live_processes_stream_connects() {
        let db = make_test_db().await;

        // ストリーム開始がエラーにならないことを確認
        let _stream = db
            .live_processes()
            .await
            .expect("live_processes ストリームの開始が失敗してはいけない");
    }
}
