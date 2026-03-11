//! ccwire — セッション間通信プロトコル（SQLite ベース）
//!
//! Claude Code セッション間のメッセージングを提供する ccwire の
//! Rust ネイティブクライアント。HD（Heaven's Door）が tmux セッション
//! 作成時に直接登録し、TUI 終了時に解除する。
//!
//! ## プロトコル概要
//!
//! - DB: `~/.cache/ccwire/ccwire.db`（SQLite WAL）
//! - sessions テーブルに INSERT OR REPLACE で登録
//! - 10分 TTL、3分ごとの heartbeat で生存管理
//! - TUI 終了時に DELETE で即時解除

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

    tracing::info!("ccwire 登録完了: {} (target: {})", session_name, tmux_target);
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

/// heartbeat インターバル（3分）
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(180);
