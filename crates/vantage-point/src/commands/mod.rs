//! コマンド実行モジュール
//!
//! 各サブコマンドの実行ロジックを分離して管理する。

pub mod app;
pub mod config;
pub mod daemon;
pub mod db;
pub mod file;
pub mod hd;
pub mod mailbox;
#[cfg(feature = "midi")]
pub mod midi;
pub mod pane;
pub mod port;
pub mod process_client;
pub mod restart;
pub mod restart_all;
pub mod sp;
pub mod start;
pub mod tmux;
pub mod tui;
pub mod update;
