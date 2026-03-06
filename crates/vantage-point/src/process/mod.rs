//! Process module - AI Agent server (HTTP + WebSocket hub)
//!
//! Process はプロジェクトの開発プロセスを表す本体。
//! JoJo の Stand（能力）を保持し、ユーザーの開発を支援する。
//!
//! ## 構成
//! - **Process**: サーバー（開発プロセス本体）
//! - **Point**: WebView（視点/観測点）
//! - **Capability**: Process が持つ能力（Agent, MIDI, Protocol等）

pub mod capabilities;
pub(crate) mod hub;
pub mod pty;
mod routes;
pub mod ruby_vm;
mod server;
mod session;
pub(crate) mod state;
pub(crate) mod unison_server;

pub use capabilities::CapabilityConfig;
pub use server::run;
