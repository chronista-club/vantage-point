//! lane (Stone Free 🧵) — Git clone-based wing workspace manager
//!
//! 操作は `vp lane` サブコマンド (vp-cli) に一本化。
//!
//! ## Library API
//!
//! `commands` モジュールが wing 操作の高レベル API を提供:
//! - `new_wing(name, branch, force)`
//! - `fork_wing(name, branch, force)`
//! - `list_wings()`
//! - `wing_path(name)`
//! - `remove_wing(name, all, force)`
//! - `status_wings()`
//! - `cleanup_wings(force)`

pub mod commands;
pub mod config;
