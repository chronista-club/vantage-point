//! lane (Stone Free 🧵) — Git clone-based wing workspace manager
//!
//! ## Phase 2.x-e (2026-04-27): 旧 worker Lane crate を vp-cli に統合
//!
//! 旧 worker Lane crate (= 当時の名称) は独立 crate (lib + bin) だったが、 workspace 内
//! caller が vp-cli のみだったため、 vp-cli に取り込んで「浮いてる crate」 を 1 つ削減。
//! VP-196 Phase 2 で旧 `ccws` 標準 binary を retire、 操作は `vp lane` サブコマンドに一本化。
//! 2026-05-18 に Worker → Wing rename。 旧 worker 名称は legacy alias / 旧ファイル名でのみ残る。
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
