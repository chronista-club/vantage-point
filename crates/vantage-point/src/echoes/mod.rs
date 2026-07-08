//! Echoes Act II — 構造化会話 GUI のバックエンド（SP 側）
//!
//! 現行 Echoes（Act I）は claude TUI の ANSI バイト列を xterm.js へ転送するだけ。
//! Act II は engine（claude）を headless stream-json で常駐駆動し、構造化イベント
//! [`EchoesEvent`] へ翻訳して vp-app（GUI）へ配信する。
//!
//! 設計 SSOT: `docs/design/30-echoes-act2-gui.md`。
//!
//! ## モジュール構成（PR1 時点）
//! - [`event`]: GUI 語彙 [`EchoesEvent`]（engine 非依存、PR1 で凍結）
//! - [`translate`]: claude stream-json → [`EchoesEvent`] 翻訳層
//!
//! host（常駐 spawn / stdin 送信 / respawn）と Unison 配信は PR1 の後続で追加。

pub mod event;
pub mod translate;

pub use event::{EchoesEvent, PlanEntry};
pub use translate::EchoesTranslator;
