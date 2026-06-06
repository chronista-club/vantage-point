//! lane (Stone Free 🧵) — Git worktree-based wing workspace manager
//!
//! lane = `<repo>/.vp/lanes/<name>` の git worktree (lead の `.git` を共有)。
//! `--isolation clone` で旧来の独立 clone も選べる (escape hatch)。
//! 操作は `vp lane` サブコマンド (vp-cli) に一本化。
//!
//! ## Library API
//!
//! `commands` モジュールが wing 操作の高レベル API を提供:
//! - `new_wing(name, branch, force, isolation)`
//! - `fork_wing(name, branch, force, isolation)`
//! - `list_wings()`
//! - `wing_path(name)`
//! - `remove_wing(name, all, force)`
//! - `status_wings()`
//! - `cleanup_wings(force)`

pub mod commands;
pub mod config;
