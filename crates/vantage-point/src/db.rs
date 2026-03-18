//! SurrealDB 統合モジュール
//!
//! VP の状態管理を SurrealDB サーバーに統一する。
//! SurrealDB は独立デーモンとして起動し（`vp db start` / TheWorld 起動時に自動起動）、
//! 全クライアント（TheWorld, SP, Native App, Canvas）が同一 DB に接続する。
//! TheWorld が停止しても SurrealDB は継続稼働する。
//!
//! ## 接続方式
//!
//! - 本番: WebSocket (`ws://[::1]:32001`) で外部サーバーに接続
//! - テスト: `kv-mem` で in-memory embedded DB を使用
//!
//! ## テーブル設計
//!
//! - `processes`: プロセス状態（QUIC Registry + HTTP polling 代替）
//! - `projects`: プロジェクト一覧（config.toml 代替）
//! - `mailbox`: cross-process メッセージング
//! - `pane_contents`: Canvas ペイン状態
//! - `stand_status`: Stand ステータス
//! - `prompts`: User Prompt
//! - `notifications`: CC 通知

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::opt::auth::Root;

/// SurrealDB のデフォルトポート
pub const SURREAL_PORT: u16 = 32001;

/// SurrealDB の名前空間
const NS: &str = "vp";

/// SurrealDB のデータベース名
const DB_NAME: &str = "vp";

/// SurrealDB のデータディレクトリ
fn db_data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("vantage")
        .join("db")
}

/// DB 認証パスワードファイルのパス
fn db_pass_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("vantage")
        .join("db_pass")
}

/// DB 認証パスワードを取得（なければランダム生成して保存）
pub fn ensure_db_password() -> String {
    let path = db_pass_path();
    if let Ok(pass) = std::fs::read_to_string(&path) {
        let pass = pass.trim().to_string();
        if !pass.is_empty() {
            return pass;
        }
    }

    // ランダムパスワードを生成
    let pass = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // 0600 パーミッションで排他作成（create_new で二重生成を防止）
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut f) => {
                let _ = f.write_all(pass.as_bytes());
            }
            Err(_) => {
                // 競合: 他プロセスが先に書いた。再度読み込む
                if let Ok(existing) = std::fs::read_to_string(&path) {
                    let existing = existing.trim().to_string();
                    if !existing.is_empty() {
                        return existing;
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::write(&path, &pass);
    }

    tracing::info!("SurrealDB パスワード生成: {}", path.display());
    pass
}

/// VP のデータベースクライアント
///
/// `Surreal<Any>` を使うことで WebSocket (本番) と kv-mem (テスト) の両方に対応。
pub struct VpDb {
    db: Surreal<Any>,
}

/// Arc でラップした VpDb（複数コンポーネントで共有するため）
pub type SharedVpDb = Arc<VpDb>;

impl VpDb {
    /// WebSocket で SurrealDB サーバーに接続
    ///
    /// リトライ付き（最大 `max_retries` 回、100ms 間隔）
    pub async fn connect(port: u16, password: &str, max_retries: u32) -> Result<Self> {
        let endpoint = format!("ws://[::1]:{}", port);
        let mut last_err = None;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }

            match surrealdb::engine::any::connect(&endpoint).await {
                Ok(db) => {
                    // root ユーザーで認証
                    db.signin(Root {
                        username: "root".to_string(),
                        password: password.to_string(),
                    })
                    .await?;

                    // 名前空間・DB を選択
                    db.use_ns(NS).use_db(DB_NAME).await?;

                    tracing::info!("SurrealDB 接続成功 ({}), attempt={}", endpoint, attempt + 1);
                    return Ok(Self { db });
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < max_retries {
                        tracing::debug!(
                            "SurrealDB 接続リトライ {}/{}: {}",
                            attempt + 1,
                            max_retries,
                            last_err.as_ref().unwrap()
                        );
                    }
                }
            }
        }

        Err(anyhow::anyhow!(
            "SurrealDB 接続失敗 ({}回リトライ): {}",
            max_retries,
            last_err.unwrap()
        ))
    }

    /// kv-mem (in-memory) で接続（テスト用）
    #[cfg(test)]
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

    // =========================================================================
    // Projects CRUD
    // =========================================================================

    /// プロジェクト一覧を取得（sort_order 順）
    pub async fn list_projects(&self) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM projects ORDER BY sort_order ASC")
            .await
            .map_err(|e| anyhow::anyhow!("projects 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// プロジェクトを追加（UPSERT: 同じ path なら更新）
    pub async fn upsert_project(&self, name: &str, path: &str, sort_order: i64) -> Result<()> {
        self.db
            .query("INSERT INTO projects { name: $name, path: $path, sort_order: $sort_order } ON DUPLICATE KEY UPDATE name = $input.name, sort_order = $input.sort_order")
            .bind(("name", name.to_string()))
            .bind(("path", path.to_string()))
            .bind(("sort_order", sort_order))
            .await
            .map_err(|e| anyhow::anyhow!("project upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("project upsert エラー: {}", e))?;
        Ok(())
    }

    /// プロジェクトを削除（path で特定）
    pub async fn delete_project(&self, path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM projects WHERE path = $path")
            .bind(("path", path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("project 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("project 削除エラー: {}", e))?;
        Ok(())
    }

    /// プロジェクト名を更新
    pub async fn update_project_name(&self, path: &str, new_name: &str) -> Result<()> {
        self.db
            .query("UPDATE projects SET name = $name WHERE path = $path")
            .bind(("path", path.to_string()))
            .bind(("name", new_name.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("project 名前変更失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("project 名前変更エラー: {}", e))?;
        Ok(())
    }

    /// プロジェクトの並び順を一括更新
    pub async fn reorder_projects(&self, paths: &[String]) -> Result<()> {
        for (i, path) in paths.iter().enumerate() {
            self.db
                .query("UPDATE projects SET sort_order = $order WHERE path = $path")
                .bind(("path", path.clone()))
                .bind(("order", i as i64))
                .await
                .map_err(|e| anyhow::anyhow!("project 並び順更新失敗: {}", e))?
                .check()
                .map_err(|e| anyhow::anyhow!("project 並び順エラー: {}", e))?;
        }
        Ok(())
    }

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
    pub async fn list_pane_contents(
        &self,
        project_path: &str,
    ) -> Result<Vec<serde_json::Value>> {
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

    /// projects テーブルの LIVE SELECT を開始
    ///
    /// 現時点では未使用（将来: Native App への projects 変更通知に利用予定）
    #[allow(dead_code)]
    pub async fn live_projects(
        &self,
    ) -> Result<surrealdb::method::Stream<Vec<serde_json::Value>>> {
        let stream = self
            .db
            .select("projects")
            .live()
            .await
            .map_err(|e| anyhow::anyhow!("LIVE SELECT projects 失敗: {}", e))?;
        Ok(stream)
    }

    /// プロジェクトの全 Stand ステータスを取得
    pub async fn list_stand_status(
        &self,
        project_path: &str,
    ) -> Result<Vec<serde_json::Value>> {
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
// SurrealDB デーモン管理（PID ファイルベース）
//
// TheWorld と同様の独立デーモン方式。
// - 起動時: DB が上がっていなければ自動起動
// - 終了時: DB は止めない（独立デーモンとして生存し続ける）
// - 再起動: `vp db restart` でいつでも再起動可能
// =============================================================================

/// SurrealDB の PID ファイルパス
fn surreal_pid_path() -> PathBuf {
    PathBuf::from("/tmp/vantage-point/surreal.pid")
}

/// SurrealDB が稼働中か確認（PID ファイルベース）
pub fn is_surreal_running() -> Option<u32> {
    let path = surreal_pid_path();
    let content = std::fs::read_to_string(&path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;

    // kill(pid, 0) の結果を errno も含めて判定する。
    // - 戻り値 0: プロセスが存在し、シグナルを送れる
    // - 戻り値 -1 + ESRCH: プロセスが存在しない → ゴースト
    // - 戻り値 -1 + EPERM: 権限なし（プロセスは存在する。別ユーザーが PID を再使用）→ alive 扱い
    let alive = i32::try_from(pid).is_ok_and(|pid_i32| {
        let ret = unsafe { libc::kill(pid_i32, 0) };
        if ret == 0 {
            true
        } else {
            // EPERM: プロセスは存在するが権限がない（alive とみなす）
            let err = std::io::Error::last_os_error();
            err.raw_os_error() == Some(libc::EPERM)
        }
    });
    if alive {
        Some(pid)
    } else {
        // ゴースト PID ファイルを掃除
        let _ = std::fs::remove_file(&path);
        None
    }
}

/// SurrealDB がまだ起動していなければバックグラウンドで自動起動
///
/// 既に稼働中ならその PID を返す。
pub fn ensure_surreal_running(port: u16, password: &str) -> Result<u32> {
    if let Some(pid) = is_surreal_running() {
        tracing::info!("SurrealDB は既に起動中 (pid={})", pid);
        return Ok(pid);
    }

    let data_dir = db_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let bind_addr = format!("[::1]:{}", port);
    let data_path = format!("rocksdb:{}", data_dir.display());

    tracing::info!(
        "SurrealDB サーバー起動: bind={}, path={}",
        bind_addr,
        data_path
    );

    let child = std::process::Command::new("surreal")
        .args([
            "start",
            "--bind",
            &bind_addr,
            "--path",
            &data_path,
            "--user",
            "root",
            "--pass",
            password,
            "--log",
            "warn",
            "--no-banner",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "surreal コマンドが見つかりません。`brew install surrealdb/tap/surreal` でインストールしてください: {}",
                e
            )
        })?;

    let pid = child.id();

    // child をここでスコープ外に出して drop する。
    // Rust の Child::drop は wait() を呼ばないため、プロセスはデタッチされて独立デーモンとして継続する。
    // （意図的。stop は PID ファイル + SIGTERM/SIGKILL で行う）
    drop(child);

    // PID ファイルに書き出し
    let pid_path = surreal_pid_path();
    if let Some(parent) = pid_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&pid_path, pid.to_string());

    tracing::info!("SurrealDB サーバー起動 (pid={})", pid);
    Ok(pid)
}

/// SurrealDB デーモンを停止
///
/// SIGTERM → 2秒待ち → SIGKILL のフォールバック付き。
pub fn stop_surreal() -> Option<u32> {
    let pid = is_surreal_running()?;
    tracing::info!("SurrealDB サーバー停止中 (pid={})", pid);

    let pid_i32 = match i32::try_from(pid) {
        Ok(p) => p,
        Err(_) => {
            let _ = std::fs::remove_file(surreal_pid_path());
            return Some(pid);
        }
    };

    // SIGTERM を送信
    unsafe {
        libc::kill(pid_i32, libc::SIGTERM);
    }

    // 最大 2 秒待つ
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        // ESRCH（プロセスなし）のみ停止完了とみなす。EPERM は alive 扱い。
        let alive = unsafe {
            let ret = libc::kill(pid_i32, 0);
            if ret == 0 {
                true
            } else {
                let err = std::io::Error::last_os_error();
                err.raw_os_error() == Some(libc::EPERM)
            }
        };
        if !alive {
            let _ = std::fs::remove_file(surreal_pid_path());
            tracing::info!("SurrealDB サーバー停止完了 (pid={})", pid);
            return Some(pid);
        }
    }

    // タイムアウト → SIGKILL
    tracing::warn!("SurrealDB SIGTERM タイムアウト、SIGKILL (pid={})", pid);
    unsafe {
        libc::kill(pid_i32, libc::SIGKILL);
    }
    let _ = std::fs::remove_file(surreal_pid_path());
    Some(pid)
}

/// SurrealDB デーモンを再起動
pub fn restart_surreal(port: u16, password: &str) -> Result<u32> {
    stop_surreal();
    // 停止完了を少し待つ
    std::thread::sleep(std::time::Duration::from_millis(500));
    ensure_surreal_running(port, password)
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

-- プロジェクト一覧（config.toml 代替）
DEFINE TABLE IF NOT EXISTS projects SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name ON projects TYPE string;
DEFINE FIELD IF NOT EXISTS path ON projects TYPE string;
DEFINE FIELD IF NOT EXISTS sort_order ON projects TYPE int;
DEFINE INDEX IF NOT EXISTS idx_projects_path ON projects COLUMNS path UNIQUE;

-- Mailbox（cross-process メッセージング）
DEFINE TABLE IF NOT EXISTS mailbox SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS from_addr ON mailbox TYPE string;
DEFINE FIELD IF NOT EXISTS to_addr ON mailbox TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON mailbox TYPE string;
DEFINE FIELD IF NOT EXISTS payload ON mailbox TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS reply_to ON mailbox TYPE option<string>;
DEFINE FIELD IF NOT EXISTS delivered ON mailbox TYPE bool DEFAULT false;
DEFINE FIELD IF NOT EXISTS created_at ON mailbox TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS idx_to ON mailbox COLUMNS to_addr, delivered;

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
    fn test_db_data_dir() {
        let dir = db_data_dir();
        assert!(
            dir.to_string_lossy().contains("vantage"),
            "データディレクトリに 'vantage' が含まれていない: {}",
            dir.display()
        );
    }

    #[test]
    fn test_constants() {
        assert_eq!(SURREAL_PORT, 32001);
        assert_eq!(NS, "vp");
        assert_eq!(DB_NAME, "vp");
    }

    #[tokio::test]
    async fn test_define_schema_mem() {
        let db = make_test_db().await;
        assert!(db.health().await, "ヘルスチェック失敗");
    }

    #[tokio::test]
    async fn test_mailbox_crud_mem() {
        let db = make_test_db().await;

        // メッセージ送信
        db.inner()
            .query(
                "INSERT INTO mailbox {
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
            .query("SELECT * FROM mailbox WHERE to_addr = 'notify' AND delivered = false")
            .await
            .unwrap();
        let records: Vec<serde_json::Value> = result.take(0).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["kind"], "notification");
    }

    // =========================================================================
    // Projects CRUD テスト
    // =========================================================================

    #[tokio::test]
    async fn test_projects_crud() {
        let db = make_test_db().await;

        // 追加
        db.upsert_project("vp", "/Users/test/repos/vp", 0)
            .await
            .unwrap();
        db.upsert_project("creo", "/Users/test/repos/creo", 1)
            .await
            .unwrap();

        // 一覧
        let projects = db.list_projects().await.unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0]["name"], "vp");
        assert_eq!(projects[1]["name"], "creo");

        // 名前変更
        db.update_project_name("/Users/test/repos/vp", "vantage-point")
            .await
            .unwrap();
        let projects = db.list_projects().await.unwrap();
        assert_eq!(projects[0]["name"], "vantage-point");

        // 削除
        db.delete_project("/Users/test/repos/creo").await.unwrap();
        let projects = db.list_projects().await.unwrap();
        assert_eq!(projects.len(), 1);
    }

    #[tokio::test]
    async fn test_projects_reorder() {
        let db = make_test_db().await;

        db.upsert_project("a", "/a", 0).await.unwrap();
        db.upsert_project("b", "/b", 1).await.unwrap();
        db.upsert_project("c", "/c", 2).await.unwrap();

        // b, c, a の順に並び替え
        db.reorder_projects(&["/b".to_string(), "/c".to_string(), "/a".to_string()])
            .await
            .unwrap();

        let projects = db.list_projects().await.unwrap();
        assert_eq!(projects[0]["name"], "b");
        assert_eq!(projects[1]["name"], "c");
        assert_eq!(projects[2]["name"], "a");
    }

    #[tokio::test]
    async fn test_projects_upsert_idempotent() {
        let db = make_test_db().await;

        db.upsert_project("vp", "/repos/vp", 0).await.unwrap();
        // 同じ path で再度 upsert → 名前が更新される
        db.upsert_project("vantage-point", "/repos/vp", 0)
            .await
            .unwrap();

        let projects = db.list_projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["name"], "vantage-point");
    }

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
}
