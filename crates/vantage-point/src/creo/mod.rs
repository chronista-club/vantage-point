//! CreoUI schema — Event payload & render hint types (VP-73 R0 skeleton)
//!
//! VP 側 consumer 実装。schema の source of truth は creo-memories 側。
//! 本モジュールは co-design draft (`docs/design/06-creoui-draft.md`) の実体化。
//!
//! 3 層:
//! - [`CreoFormat`] 形式 (12 enum)
//! - [`CreoContent`] 内容 (envelope)
//! - [`CreoUI`] 見せ方 (Component 単位、2026-04-22 確定)
//!
//! Event パイプライン:
//! - [`Event`] が全 Stand 間を流れる単位
//! - `payload: CreoContent` + 任意 `ui: CreoUI`

pub mod content;
pub mod event;
pub mod format;
pub mod topic;
pub mod ui;

pub use content::{CreoCallContext, CreoContent, CreoSource, MemoryId, MemoryRef};
pub use event::{ActorRef, Event, EventId};
pub use format::CreoFormat;
pub use topic::{Topic, TopicAlias, default_aliases, looks_canonical};
pub use ui::{CreoEmphasis, CreoLayout, CreoOwnership, CreoPlacement, CreoUI};
