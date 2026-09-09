//! HTTP（axum）の route handler。残っているのは health / shutdown / update だけで、他の操作は
//! Unison の `process` channel（`repo/unison_server.rs`）と daemon control channel に移った（doc 45）。
//! Router は `repo/server.rs::build_daemon_router` が組む。

pub mod health;
pub mod update;
