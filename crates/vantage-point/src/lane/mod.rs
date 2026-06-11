//! lane (Stone Free 🧵) — Git worktree-based performer workspace manager
//!
//! lane = `<repo>/.vp/lanes/<name>` の git worktree (conductor の `.git` を共有)。
//! `--isolation clone` で旧来の独立 clone も選べる (escape hatch)。
//! 操作は `vp lane` サブコマンド (vp-cli) に一本化。
//!
//! ## Library API
//!
//! `commands` モジュールが performer 操作の高レベル API を提供:
//! - `new_performer(name, branch, force, isolation)`
//! - `fork_performer(name, branch, force, isolation)`
//! - `list_performers()`
//! - `performer_path(name)`
//! - `remove_performer(name, all, force)`
//! - `status_performers()`
//! - `cleanup_performers(force)`

/// lane 単位の CC session id 永続化 (R3-b、 `--resume` 再利用の土台)
pub mod cc_session;
pub mod commands;
pub mod config;
