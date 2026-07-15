//! Echoes Act II — 構造化会話 GUI のバックエンド（SP 側）
//!
//! 現行 Echoes（Act I）は claude TUI の ANSI バイト列を xterm.js へ転送するだけ。
//! Act II は engine（claude）を headless stream-json で常駐駆動し、構造化イベント
//! [`EchoesEvent`] へ翻訳して vp-app（GUI）へ配信する。
//!
//! 設計 SSOT: `docs/design/32-echoes-act2-gui.md`。
//!
//! ## モジュール構成（PR1 時点）
//! - [`event`]: GUI 語彙 [`EchoesEvent`]（engine 非依存）
//! - [`translate`]: claude stream-json → [`EchoesEvent`] 翻訳層（live）
//! - [`transcript`]: claude session transcript(jsonl) → [`EchoesEvent`] 翻訳層（replay-on-attach）
//! - [`host`]: [`EchoesAgentHost`] — headless claude を lane 単位で常駐駆動
//! - [`cursor_translate`]: cursor-agent stream-json → [`EchoesEvent`] 翻訳層（cursor engine 用）
//! - [`cursor_host`]: [`CursorAgentHost`] — cursor-agent を lane 単位で turn-scoped 駆動
//!
//! Unison 配信は `process::echoes_pump`（terminal_pump と同型）が担う。GUI 語彙 [`EchoesEvent`] は
//! engine 非依存なので、cursor は翻訳層 + host を足すだけで乗る（chatview / topic 配線は無改修）。

pub mod cursor_host;
pub mod cursor_translate;
pub mod event;
pub mod host;
pub mod transcript;
pub mod translate;

pub use cursor_host::{CursorAgentHost, CursorHostConfig};
pub use event::{EchoesEvent, PlanEntry, QuestionOption, QuestionSpec};
pub use host::{EchoesAgentHost, EchoesHostConfig, InFlight, PermissionDecision};
pub use translate::EchoesTranslator;
