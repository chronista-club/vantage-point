//! creo との境界 — CreoUI schema（表現）と creo-memories REST client（永続）。
//!
//! 2 つの別々の関心が同居する:
//!
//! - [`client`] — **creo-memories の REST client**（doc 57 Phase 3）。ACTIONS を daemon が
//!   引いて `/api/health` で vp-app へ流す。webview は外部 HTTP を叩かない
//! - 以下の schema 群 — Agent 間を流れる Event の payload / 見せ方（VP-73 R0）
//!
//! ## CreoUI schema（VP-73 R0 skeleton）
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
//! - [`Event`] が全 Agent 間を流れる単位
//! - `payload: CreoContent` + 任意 `ui: CreoUI`

pub mod client;
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
