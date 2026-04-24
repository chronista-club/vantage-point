//! Vantage Point Core — AI ネイティブ開発環境のコアライブラリ
//!
//! CLI バイナリ (`vp`) や外部クレートから利用される
//! Process サーバー、MCP、Daemon 等のコアロジックを提供する。

// 開発中のスキャフォールドコードが多いため一時的に抑制
#![allow(dead_code)]

pub mod agent;
pub mod agui;
pub mod capability;
// Phase L7d: ccwire module 削除 — Mailbox Router (msgbox.rs) に統合
// pub mod ccwire;
pub mod cli;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod db;
pub mod discovery;
pub mod file_watcher;
pub mod mcp;
#[cfg(feature = "midi")]
pub mod midi;
pub mod notify;
pub mod platform;
pub mod port_layout;
pub mod process;
pub mod protocol;
pub mod resolve;
pub mod stands;
pub mod terminal;
#[cfg(feature = "gui")]
pub mod terminal_window;
pub mod tmux;
pub mod trace_log;
#[cfg(feature = "gui")]
pub mod tray;
pub mod tui;
