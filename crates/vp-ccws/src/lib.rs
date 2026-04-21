//! vp-ccws — Stone Free 🧵 worker workspace manager (formerly ccws)
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
//!
//! ## Bin
//!
//! `[[bin]] name="ccws"` で後方互換 binary を提供（Stone Free Phase 3 まで維持）。
//!
//! ## VP 統合
//!
//! `vp ws` サブコマンド (vp-cli 配下) からこの library を直接呼び出す。
//! Native App も将来 vp-bridge FFI 経由で本 library を call 予定 (Phase 2+)。

pub mod commands;
pub mod config;
