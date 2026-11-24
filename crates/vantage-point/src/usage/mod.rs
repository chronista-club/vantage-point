//! engine usage meter — 4 エンジン（claude / codex / agy / grok）のサブスク usage
//! （使用率 %・リセット時刻・plan / tier）を **machine scope** で集約する。
//!
//! usage は repo にも lane にも依らないアカウント単位の事実なので daemon が 1 つ持ち、
//! `daemon-usage` channel（`daemon/server.rs`）で vp-app に配る。vp-app はそれを sidebar の
//! MachineStrip・sub lane 作成の engine 選択・macOS の menu bar（tray）に出す。
//!
//! spec: creo `mem_1ChMLKCMUvC73JHTLjG3xg` / 実測: `mem_1ChMJqaRP2SunfbmEd6h1n`
//!
//! - data: [`model`]（正規化済みの型。単位はここで揃っている）
//! - calculations: [`parse`]（生 JSON → model）/ [`service`] の方針・重ね方の純関数
//! - actions: [`probe`]（CLI / file に問い合わせる）/ [`service::UsageService`]（集約・配信・保存）/
//!   [`passive`]（会話中の通知を流し込む口）
//!
//! **副作用ガード**: turn を起こさない（枠を使わない）/ claude は spawn しない / grok は
//! 手動の時だけ / test は本物の CLI を起動しない。詳細は [`probe`] と [`service`] の module doc。

pub mod model;
pub mod parse;
pub mod passive;
pub mod probe;
pub mod service;

pub use model::{EngineUsage, Severity, UsageEngine, UsageSnapshot, UsageSource, UsageWindow};
pub use service::{Trigger, UsageService};
