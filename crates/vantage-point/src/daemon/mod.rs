//! VP Daemon — プロセス管理デーモン
//!
//! PTYプロセスを所有し、Unison Protocol経由で出力を転送する。
//! Daemon 生存中はプロセスが存続する。

pub mod client;
pub mod event_log;
pub mod hub_client;
pub mod process;
pub mod protocol;
pub mod pty_slot;
pub mod registry;
pub mod server;
pub mod world_capabilities;
