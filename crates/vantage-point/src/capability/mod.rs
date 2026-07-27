//! Capability Module - 拡張可能な能力システム
//!
//! このモジュールはプロセスに拡張可能な「能力（Capability）」システムを提供します。
//! 各能力が独立しながらも協調動作します。
//!
//! ## モジュール構成
//!
//! - `core`: Capabilityトレイトとライフサイクル管理（REQ-CAP-001）
//!
//! ## 関連ドキュメント
//!
//! - [docs/spec/02-capability.md](../../../docs/spec/02-capability.md)

pub mod actor_registry;
pub mod agent_capability;
pub mod component_service;
pub mod core;
pub mod delegation_store;
pub mod eventbus;
pub mod protocol_capability;
pub mod repo_manager_capability;
pub mod update_capability;
pub mod wiremsg_store;

pub use actor_registry::{ActorKind, ActorRegistry, ActorRegistryEntry};
pub use agent_capability::AgentCapability;
pub use core::{
    CapabilityContext, CapabilityEvent, CapabilityInfo, CapabilityState, DiagnosticReport,
};
pub use eventbus::EventBus;
// wiremsg R5-4: 旧 msgbox の registry サブシステム (`msgbox_registry` / `msgbox_remote`) を
// 完全撤去。 msg messaging は wiremsg (`wiremsg_store`) に一本化済。
pub use component_service::{Component, LayerScope, Service};
pub use protocol_capability::ProtocolCapability;
pub use repo_manager_capability::{
    RepoHealthInfo, RepoInfo, RepoManagerCapability, RepoPresenceState, RepoStatus, RunningRepo,
    normalize_path_key,
};
pub use update_capability::UpdateCapability;
// Phase A ①: wiremsg threaded inbox store (設計 mem_1CbD9H1KGQykBaFG8XXVsn)。
// R2-a: store は daemon に中央化 (cross-process forward = wire_remote は撤去、
// 設計 mem_1CbvcJj4ppU3QKH9d7xMpT 決定 D1-b)。
pub use wiremsg_store::{ParticipantStatus, WireMessage, WireNotifier, WiremsgStore};
// 委譲 (delegation) の daemon 中央 store (doc 28 §4 / §6)。 委譲型は crate 内部なので pub(crate)。
pub(crate) use delegation_store::DelegationStore;
