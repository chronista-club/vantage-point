//! ccwire — tmux session tracking（2026-04-19 役割再定義）
//!
//! ## 役割
//!
//! **tmux session のライフサイクル追跡** に特化した薄いクライアント。
//! HD（Heaven's Door）が tmux セッション作成時に直接登録し、TUI 終了時に解除する。
//!
//! `~/repos/claude-plugin-ccwire`（外部プラグイン）と SQLite DB を共有して、
//! session 一覧の grobal view を実現する。
//!
//! ## 役割境界（重要）
//!
//! | 機能 | 担当 |
//! |------|------|
//! | tmux session の register/unregister/list | **このモジュール（ccwire）** |
//! | actor 間 messaging（cross-Process 含む） | **VP Msgbox** (`capability::msgbox`) |
//! | ccwire の wire-send/wire-receive 系 CLI | 削除予定（messaging は msgbox に集約） |
//!
//! ## プロトコル概要
//!
//! - DB: `~/.cache/ccwire/ccwire.db`（SQLite WAL）
//! - sessions テーブルに INSERT OR REPLACE で登録
//! - 10分 TTL、3分ごとの heartbeat で生存管理
//! - TUI 終了時に DELETE で即時解除
//!
//! ## 進化方針（次セッション以降）
//!
//! tmux power-tool 化:
//! - Pane orchestration（split / select / swap / resize）
//! - メタデータ管理（pane title / border / formats）
//! - capture-pane / send-keys 拡張
//! - monitor-activity / pipe-pane / hooks 連携
//!
//! VP 既存の `tmux_actor` / `vp tmux` コマンドとの境界線は spec 起こし時に決定。

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use rusqlite::Connection;

/// ccwire DB のパス
///
/// ccwire は XDG 準拠で `~/.cache/ccwire/ccwire.db` を使用する。
/// macOS の `dirs::cache_dir()` は `~/Library/Caches` を返すので直接構築する。
fn db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".cache")
        .join("ccwire")
        .join("ccwire.db")
}

/// ccwire セッションを登録する
///
/// tmux セッション作成直後に呼ぶ。
/// `session_name` は ccwire 上のセッション名（= tmux セッション名）。
/// `tmux_target` は tmux のターゲットペイン（例: `project-vp:0.0`）。
pub fn register(session_name: &str, tmux_target: &str) -> Result<()> {
    let db = db_path();
    if !db.exists() {
        tracing::warn!("ccwire DB not found: {:?} — スキップ", db);
        return Ok(());
    }

    let conn = Connection::open(&db)?;
    // WAL モードを確認（ccwire が設定済みのはず）
    conn.pragma_update(None, "journal_mode", "wal")?;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let pid = std::process::id() as i64;

    // 既存の registered_at を保持（再登録時）
    let registered_at: Option<String> = conn
        .query_row(
            "SELECT registered_at FROM sessions WHERE name = ?1",
            rusqlite::params![session_name],
            |row| row.get(0),
        )
        .ok();
    let registered_at = registered_at.as_deref().unwrap_or(&now);

    conn.execute(
        "INSERT OR REPLACE INTO sessions (name, tmux_target, pid, broadcast_cursor, status, registered_at, last_seen)
         VALUES (?1, ?2, ?3, ?4, 'idle', ?5, ?4)",
        rusqlite::params![session_name, tmux_target, pid, now, registered_at],
    )?;

    tracing::info!(
        "ccwire 登録完了: {} (target: {})",
        session_name,
        tmux_target
    );
    Ok(())
}

/// ccwire セッションを解除する
///
/// TUI 終了時（Ctrl+Q detach）に呼ぶ。
pub fn unregister(session_name: &str) -> Result<()> {
    let db = db_path();
    if !db.exists() {
        return Ok(());
    }

    let conn = Connection::open(&db)?;
    conn.execute(
        "DELETE FROM sessions WHERE name = ?1",
        rusqlite::params![session_name],
    )?;

    tracing::info!("ccwire 解除完了: {}", session_name);
    Ok(())
}

/// heartbeat — `last_seen` を更新する
///
/// TUI メインループ内で定期的に（3分間隔）呼ぶ。
pub fn heartbeat(session_name: &str) -> Result<()> {
    let db = db_path();
    if !db.exists() {
        return Ok(());
    }

    let conn = Connection::open(&db)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    conn.execute(
        "UPDATE sessions SET last_seen = ?1 WHERE name = ?2",
        rusqlite::params![now, session_name],
    )?;

    Ok(())
}

/// heartbeat インターバル（3分）— レガシー、vp sp では不使用
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(180);

/// 古いゴーストセッションを掃除
///
/// tmux セッションが存在しないのに ccwire に登録が残ってるエントリを削除。
/// `vp sp start` 時に呼ぶ。
pub fn cleanup_stale() -> Result<()> {
    let db = db_path();
    if !db.exists() {
        return Ok(());
    }

    let conn = Connection::open(&db)?;
    let mut stmt = conn.prepare("SELECT name FROM sessions")?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut removed = 0;
    for name in &names {
        // tmux セッションが存在しない → ゴースト
        if !crate::tmux::session_exists(name) {
            conn.execute(
                "DELETE FROM sessions WHERE name = ?1",
                rusqlite::params![name],
            )?;
            removed += 1;
            tracing::info!("ccwire ゴースト除去: {}", name);
        }
    }

    if removed > 0 {
        tracing::info!("ccwire ゴースト掃除完了: {}件削除", removed);
    }

    Ok(())
}

/// ccwire セッション情報（API レスポンス用）
#[derive(Debug, serde::Serialize)]
pub struct CcwireSession {
    pub name: String,
    pub status: String,
    pub pid: Option<i64>,
    pub tmux_target: Option<String>,
    pub registered_at: String,
    pub last_seen: String,
    /// 未読（pending）メッセージ数
    pub pending_messages: u32,
}

/// 全セッション一覧を取得（未読メッセージ数付き）
///
/// Phase L7c: Mailbox Router 移行中のため empty を返す stub。
/// 将来的に daemon の Mailbox Router.boxes + msgbox table を aggregate する
/// /api/world/msgbox/sessions endpoint に差し替え予定。
pub fn list_sessions() -> Result<Vec<CcwireSession>> {
    Ok(vec![])
}

/// セッションが登録されているか確認
///
/// Phase L7c: ccwire registry 廃止、false 固定 stub。
/// 呼び側 (hd_list / sp_cmd) は registration status 表示のみなので影響軽微。
pub fn is_registered(_session_name: &str) -> bool {
    false
}
