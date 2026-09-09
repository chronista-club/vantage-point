//! VP Daemon — daemon プロセス管理デーモン
//!
//! repo の自己登録（registry channel）を受け、process lifecycle / wire / event log を
//! Unison Protocol 経由で中継する。

pub mod client;
/// daemon control（`handle_daemon_control`）の core 関数群 — apply_repo_update / resolve_create_lane_args / collect_lanes（7b で repo/routes/daemon から）
pub(crate) mod control_ops;
/// 委譲（delegation）の daemon 中央 store への method dispatch（7b で repo/routes/delegation から）
pub(crate) mod delegation_ops;
pub mod dialer;
pub mod event_log;
pub mod hub_client;
pub mod machine_capabilities;
pub mod process;
pub mod protocol;
pub mod pty_slot;
pub mod server;
/// wiremsg の daemon 中央 store（`WiremsgStore`）への 8 操作と method dispatch（7b で repo/routes/wire から）
pub(crate) mod wire_ops;
