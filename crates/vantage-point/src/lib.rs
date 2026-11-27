//! Vantage Point Core — AI ネイティブ開発環境のコアライブラリ
//!
//! CLI バイナリ (`vp`) や外部クレートから利用される
//! Process サーバー、MCP、Daemon 等のコアロジックを提供する。

pub mod capability;
pub mod cli;
pub mod commands;
pub mod config;
pub mod creo;
pub mod daemon;
pub mod db;
#[cfg(feature = "midi")]
pub mod devices;
#[cfg(feature = "midi")]
mod roto_midi;
#[cfg(feature = "midi")]
mod roto_setup;
#[cfg(feature = "midi")]
mod roto_setup_files;
#[cfg(feature = "midi")]
mod roto_setup_midistage;
#[cfg(feature = "midi")]
mod roto_setup_state;
#[cfg(feature = "midi")]
mod roto_setup_sync;
/// ROTO の surface（daemon が持つ見え方の正本。needs-you → LED、design 84）
#[cfg(feature = "midi")]
mod roto_surface;
// gear_input / gear_profile / roto_palette は lnx-gear に集約。
#[cfg(feature = "midi")]
pub use lnx_gear::gear_input;
#[cfg(feature = "midi")]
pub use lnx_gear::gear_profile;
pub mod conversation;
/// daemon が後から生えた MIDI port を見るための CoreMIDI 通知 thread（2026-10-14）
#[cfg(feature = "midi")]
pub mod coremidi_notify;
pub mod discovery;
pub mod file_watcher;
pub mod flow;
/// Repo Host — repo の面倒を見る決定的サービス（doc 44 D3 / §7）
pub mod host;
// lane lib 本体 (vp-cli の bin `vp lane` も `vantage_point::lane` を経由する)
pub mod lane;
pub mod mcp;
#[cfg(feature = "midi")]
pub mod midi;
pub mod panic_hook;
pub mod platform;
pub mod port_layout;
pub mod protocol;
pub mod repo;
pub mod repos_file;
pub mod resolve;
/// user の「好み」の SSOT (`~/.config/vp/settings.kdl`)。daemon が書く。doc 59。
pub mod settings_file;
#[cfg(feature = "midi")]
pub use lnx_gear::roto_palette;
pub mod daemon_client;
pub mod node;
pub mod screenshot;
pub mod spawn_env;
pub mod terminal;
/// test 専用: process-global env (`XDG_STATE_HOME`) を触る test の直列化 + RAII 復元。
#[cfg(test)]
pub(crate) mod test_env;
pub mod trace_log;
/// engine usage meter（4 エンジンのサブスク usage を machine scope で集約、spec mem_1ChMLKCMUvC73JHTLjG3xg）。
pub mod usage;

#[cfg(feature = "midi")]
mod midi_access;
#[cfg(feature = "midi")]
pub mod midi_use;
