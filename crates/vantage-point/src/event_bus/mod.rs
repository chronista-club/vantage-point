//! VP-74 R1 Phase A — Stand Ensemble Event Bus
//!
//! `creo::Event` の pub/sub + SurrealDB `event_log` 永続化 + projection 購読。
//! `process::topic_router::TopicRouter` と **並置**:
//! - Bus = domain event (CreoContent + causation、永続化前提)
//! - Router = infra message (Canvas/PTY/permission の既存 pane-level msg)
//!
//! Phase A (R1 初期) は以下のみ実装:
//! - Bus (broadcast + validator + alias 解決)
//! - Persistor (event_log への非同期 INSERT)
//! - lane_map + permission_audit projection
//!
//! Phase B 以降で canvas_state / hd_sessions / user_context / build_status を追加。

pub mod alias;
pub mod bus;
pub mod persistor;
pub mod projection;
pub mod validator;

pub use alias::AliasTable;
pub use bus::{Bus, BusError, BusHandle, Subscription};
pub use persistor::{EventPersistor, PersistorHandle};
pub use projection::{LaneMapProjection, PermissionAuditProjection, Projection};
pub use validator::{ValidationError, validate_topic};
