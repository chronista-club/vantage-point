//! ccws (Stone Free 🧵) — Git clone-based worker workspace manager
//!
//! ## Phase 2.x-e (2026-04-27): vp-ccws crate を vp-cli に統合
//!
//! 旧 `vp-ccws` は独立 crate (lib + bin) だったが、 workspace 内 caller が
//! vp-cli のみだったため、 vp-cli に取り込んで「浮いてる crate」 を 1 つ削減。
//! 後方互換のため `ccws` 標準 binary は `crates/vp-cli/src/bin/ccws.rs` に残置。
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

pub mod commands;
pub mod config;
