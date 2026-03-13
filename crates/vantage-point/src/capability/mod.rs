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

pub mod agent_capability;
pub mod bonjour_capability;
pub mod core;
pub mod eventbus;
pub mod evolution;
pub mod midi_capability;
pub mod params;
pub mod process_manager_capability;
pub mod protocol_capability;
pub mod registry;
pub mod update_capability;

pub use agent_capability::AgentCapability;
pub use bonjour_capability::BonjourCapability;
pub use core::{CapabilityContext, CapabilityEvent, CapabilityInfo, CapabilityState};
pub use eventbus::EventBus;
pub use midi_capability::MidiCapability;
pub use process_manager_capability::{
    ProcessManagerCapability, ProcessStatus, ProjectInfo, RunningProcess,
};
pub use protocol_capability::ProtocolCapability;
pub use registry::CapabilityRegistry;
pub use update_capability::UpdateCapability;
