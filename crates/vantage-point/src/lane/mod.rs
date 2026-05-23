//! lane (Stone Free 🧵) — Git clone-based worker workspace manager
//!
//! ## Phase 2.x-e (2026-04-27): 旧 worker Lane crate を vp-cli に統合
//!
//! 旧 worker Lane crate は独立 crate (lib + bin) だったが、 workspace 内 caller が
//! vp-cli のみだったため、 vp-cli に取り込んで「浮いてる crate」 を 1 つ削減。
//! VP-196 Phase 2 で旧 `ccws` 標準 binary を retire、 操作は `vp lane` サブコマンドに一本化。
//!
//! ## Library API
//!
//! `commands` モジュールが worker 操作の高レベル API を提供:
//! - `new_worker(name, branch, force)`
//! - `fork_worker(name, branch, force)`
//! - `list_workers()`
//! - `worker_path(name)`
//! - `remove_worker(name, all, force)`
//! - `status_workers()`
//! - `cleanup_workers(force)`

pub mod claude_trust;
pub mod commands;
pub mod config;
