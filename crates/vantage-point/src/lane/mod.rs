//! lane (Stone Free 🧵) — Git worktree-based performer workspace manager
//!
//! lane = `<repo>/.vp/lanes/<name>` の git worktree (conductor の `.git` を共有)。
//! `--isolation clone` で旧来の独立 clone も選べる (escape hatch)。
//! 操作は `vp lane` サブコマンド (vp-cli) に一本化。
//!
//! ## Library API
//!
//! `commands` モジュールが performer 操作の高レベル API を提供:
//! - `new_performer(name, branch, force, isolation, base, model)`
//! - `fork_performer(name, branch, force, isolation, base, model)`
//! - `list_performers()`
//! - `performer_path(name)`
//! - `remove_performer(name, all, force)`
//! - `status_performers()`
//! - `cleanup_performers(force)`

/// lane 単位の CC session id 永続化 (R3-b、 `--resume` 再利用の土台)
pub mod cc_session;
/// lane 単位の Codex thread id 永続化（codex stand の指名 resume の土台、doc 37 §7）
pub mod codex_session;
pub mod commands;
pub mod config;
pub mod console_mode;
/// lane 単位の Cursor chat id 永続化（cursor stand の Act I `--resume` の土台）
pub mod cursor_session;
/// lane 単位の chat engine model 永続化 (Act II モデル切替、 `--model` の SSOT)
pub mod engine_model;
/// lane 単位の 位置独立 安定 id 永続化 (I1、 doc 24 §7、 address→id を disk に load_or_create)
pub mod lane_id;
/// engine session id 永続の共通機構（cc/cursor/codex_session の共通核、doc 37）
pub(crate) mod session_store;
