//! Vantage Point Core — AI ネイティブ開発環境のコアライブラリ
//!
//! CLI バイナリ (`vp`) や外部クレートから利用される
//! Process サーバー、MCP、Daemon 等のコアロジックを提供する。

// 開発中のスキャフォールドコードが多いため一時的に抑制
#![allow(dead_code)]

pub mod agent;
pub mod agui;
pub mod capability;
pub mod cli;
pub mod commands;
pub mod config;
pub mod creo;
pub mod daemon;
pub mod db;
#[cfg(feature = "midi")]
pub mod device_profile;
pub mod discovery;
pub mod file_watcher;
pub mod flow;
pub mod lan_discovery;
// lane lib 本体 (vp-cli の bin `vp lane` も `vantage_point::lane` を経由する)
pub mod lane;
pub mod mcp;
#[cfg(feature = "midi")]
pub mod midi;
pub mod notify;
pub mod platform;
pub mod port_layout;
pub mod process;
pub mod projects_file;
pub mod protocol;
pub mod resolve;
#[cfg(feature = "midi")]
pub mod roto_palette;
pub mod screenshot;
pub mod spawn_env;
pub mod stands;
pub mod terminal;
#[cfg(feature = "gui")]
pub mod terminal_window;
pub mod tmux;
pub mod trace_log;
#[cfg(feature = "gui")]
pub mod tray;
pub mod tui;
pub mod world_client;
