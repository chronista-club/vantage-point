//! Vantage Point Core — AI ネイティブ開発環境のコアライブラリ
//!
//! CLI バイナリ (`vp`) や外部クレートから利用される
//! Process サーバー、MCP、Canvas、Daemon 等のコアロジックを提供する。

// 開発中のスキャフォールドコードが多いため一時的に抑制
#![allow(dead_code)]

pub mod agent;
pub mod agui;
pub mod canvas;
pub mod capability;
pub mod cli;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod file_watcher;
pub mod mcp;
pub mod midi;
pub mod notify;
pub mod process;
pub mod protocol;
pub mod resolve;
pub mod stands;
pub mod terminal;
pub mod terminal_window;
pub mod tmux;
pub mod trace_log;
pub mod tray;
pub mod tui;
