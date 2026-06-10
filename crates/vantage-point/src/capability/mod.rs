//! Capability Module - Stand能力の拡張システム
//!
//! このモジュールはProcessに拡張可能な「能力（Capability）」システムを提供します。
//! JoJoスタンドの世界観から着想を得て、各能力が独立しながらも協調動作します。
//!
//! ## モジュール構成
//!
//! - `core`: Capabilityトレイトとライフサイクル管理（REQ-CAP-001）
//! - `params`: 能力のパラメータ評価（A〜Eランク、6パラメータ）
//! - `evolution`: 能力の成長・進化システム（ACT進化、レクイエム、覚醒）
//!
//! ## 関連ドキュメント
//!
//! - [docs/spec/05-process-capability.md](../../../docs/spec/05-process-capability.md)

pub mod actor_registry;
pub mod agent_capability;
pub mod core;
pub mod eventbus;
pub mod evolution;
#[cfg(feature = "midi")]
pub mod midi_capability;
pub mod params;
pub mod process_manager_capability;
pub mod protocol_capability;
pub mod stand_service;
pub mod update_capability;
pub mod whitesnake;
pub mod wire_remote;
pub mod wiremsg_store;

pub use actor_registry::{ActorKind, ActorRegistry, ActorRegistryEntry};
pub use agent_capability::AgentCapability;
pub use core::{
    CapabilityContext, CapabilityEvent, CapabilityInfo, CapabilityState, DiagnosticReport,
};
pub use eventbus::EventBus;
#[cfg(feature = "midi")]
pub use midi_capability::MidiCapability;
// wiremsg R5-4: 旧 msgbox の registry サブシステム (`msgbox_registry` / `msgbox_remote`) を
// 完全撤去。 msg messaging は wiremsg (`wiremsg_store` / `wire_remote`) に移行済。
pub use process_manager_capability::{
    ProcessManagerCapability, ProcessStatus, ProjectInfo, RunningProcess, normalize_path_key,
};
pub use protocol_capability::ProtocolCapability;
pub use stand_service::{LayerScope, Service, Stand};
pub use update_capability::UpdateCapability;
pub use whitesnake::Whitesnake;
// Phase A ①: wiremsg threaded inbox store (msgs table と並存、 設計 mem_1CbD9H1KGQykBaFG8XXVsn)
pub use wiremsg_store::{ParticipantStatus, WireMessage, WireNotifier, WiremsgStore};
// R3: wire cross-process delivery (宛先分類 + best-effort forward、 設計 mem_1CbDLrECNZiNEZqjySLfSB)
pub use wire_remote::{WireRecipients, classify_recipients, forward_to_remote};
