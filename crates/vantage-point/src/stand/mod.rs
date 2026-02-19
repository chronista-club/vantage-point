//! Stand module - AI Agent server (HTTP + WebSocket hub)
//!
//! "Stand" is named after JoJo's Bizarre Adventure - an entity that stands by
//! the user's side and wields unique capabilities.
//!
//! ## 構成
//! - **Stand**: サーバー（傍らに立つ存在）
//! - **Point**: WebView（視点/観測点）
//! - **Capability**: Stand が持つ能力（Agent, MIDI, Protocol等）

pub mod capabilities;
mod hub;
pub mod pty;
mod routes;
mod server;
mod session;
pub(crate) mod state;
pub mod tmux;
pub(crate) mod unison_server;

pub use capabilities::CapabilityConfig;
pub use server::{run, run_conductor};
