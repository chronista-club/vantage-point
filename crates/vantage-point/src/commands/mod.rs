//! コマンド実行モジュール
//!
//! 各サブコマンドの実行ロジックを分離して管理する。

pub mod app;
pub mod auth;
pub mod config;
pub mod daemon;
pub mod db;
pub mod directmsg;
pub mod file;
pub mod flow;
pub mod hd;
pub mod lan;
#[cfg(feature = "midi")]
pub mod midi;
pub mod pane;
pub mod port;
pub mod process_client;
pub mod projects;
pub mod restart;
pub mod restart_all;
pub mod sp;
pub mod sync;
pub mod tmux;
pub mod tui;
pub mod update;
pub mod wire;
