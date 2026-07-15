//! Echoes 💬 — コーディングアシスタント Stand（engine 軸 × Act(surface) 軸の直交格子）
//!
//! doc 37: Echoes は「コーディングアシスタント」という能力の namespace。その中に
//! - **engine 軸**（どの頭脳か: claude / cursor / codex / agy …）= session に束縛される identity
//! - **Act(surface) 軸**（どう視るか: Act I 端末 / Act II chat GUI）= 切替可能な view
//!
//! の直交 2 軸がある。本 module は Act II のバックエンド（SP 側）+ engine 軸の語彙を持つ。
//! Act I は raw PTY（`process::stand_spawner` の床 + CLI 注入）で、本 module を通らない。
//!
//! 設計 SSOT: `docs/design/37-echoes-two-axes.md`（2 軸）/ `32-echoes-act2-gui.md`（Act II）。
//!
//! ## モジュール構成
//! - [`event`]: GUI 語彙 [`EchoesEvent`]（engine 非依存 — 全 engine をこの共通面に翻訳する）
//! - [`engine`]: engine 軸の語彙 [`EngineKind`] と chat engine 所有型 [`ChatHost`] / [`ChatEngineSlot`]
//! - [`host`]: [`EchoesAgentHost`] — headless claude を lane 単位で**常駐**駆動（stream-json stdin 連投）
//! - [`turn_host`]: [`turn_host::TurnHost`] — 常駐機構を持たない CLI の **turn-scoped** 共通 host
//! - [`translate`] / [`transcript`]: claude stream / transcript → [`EchoesEvent`] 翻訳層
//! - [`cursor_translate`] + [`cursor_host`]: cursor-agent（turn-scoped）
//! - [`codex_translate`] + [`codex_host`]: codex（turn-scoped）
//!
//! Unison 配信は `process::echoes_pump`（terminal_pump と同型）が担う。GUI 語彙 [`EchoesEvent`] は
//! engine 非依存なので、新 engine は「翻訳器 + [`turn_host::TurnEngine`] 実装（または常駐 host）」を
//! 足すだけで乗る（chatview / topic 配線は無改修）。

pub mod codex_host;
pub mod codex_translate;
pub mod cursor_host;
pub mod cursor_translate;
pub mod engine;
pub mod event;
pub mod host;
pub mod transcript;
pub mod translate;
pub mod turn_host;

pub use codex_host::CodexAgentHost;
pub use cursor_host::CursorAgentHost;
pub use engine::{ChatEngineSlot, ChatHost, EngineKind};
pub use event::{EchoesEvent, PlanEntry, QuestionOption, QuestionSpec};
pub use host::{EchoesAgentHost, EchoesHostConfig, InFlight, PermissionDecision};
pub use translate::EchoesTranslator;
pub use turn_host::TurnHostConfig;
