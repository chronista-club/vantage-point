//! VP Daemon — プロセス管理デーモン
//!
//! PTYプロセスを所有し、Unison Protocol経由でConsoleに出力を転送する。
//! Daemon生存中はプロセスが存続し、Console（vp hd attach）は何度でも接続/切断可能。

pub mod client;
pub mod process;
pub mod protocol;
pub mod pty_slot;
pub mod registry;
pub mod server;
