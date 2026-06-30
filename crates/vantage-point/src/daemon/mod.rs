//! VP Daemon — TheWorld プロセス管理デーモン
//!
//! SP の自己登録（registry channel）を受け、process lifecycle / wire / event log を
//! Unison Protocol 経由で中継する。

pub mod client;
pub mod event_log;
pub mod hub_client;
pub mod process;
pub mod protocol;
pub mod pty_slot;
pub mod server;
pub mod world_capabilities;
