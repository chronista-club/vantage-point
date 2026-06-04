//! Vantage Point Core — AI ネイティブ開発環境のコアライブラリ
//!
//! CLI バイナリ (`vp`) や外部クレートから利用される
//! Process サーバー、MCP、Daemon 等のコアロジックを提供する。

// 開発中のスキャフォールドコードが多いため一時的に抑制
#![allow(dead_code)]

pub mod agent;
pub mod agui;
pub mod capability;
// VP-172: 旧 vp-db crate を本 crate に物理 merge (2026-05-14)。 旧 `vp_db::` import は `crate::db::` で参照。
pub mod db;
// Phase 4-X (2026-04-27): lane lib を vp-cli から移動。 server (lanes.rs) から直接
// lib call、 subprocess 経路を撤去。 vp-cli の bin (vp lane) は `vantage_point::lane` を経由する。
pub mod lane;
// Phase L7d: ccwire module 削除 — Mailbox Router (msgbox.rs) に統合
// pub mod ccwire;
pub mod cli;
pub mod commands;
pub mod config;
pub mod creo;
pub mod daemon;
pub mod discovery;
pub mod file_watcher;
pub mod flow;
pub mod lan_discovery;
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
