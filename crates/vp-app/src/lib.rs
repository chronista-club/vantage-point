//! Vantage Point native app — ライブラリ層
//!
//! `main.rs` から使う app モジュール一式。
//! クロスプラットフォーム (macOS / Windows / Linux) 対応を原則とする。
//!
//! ## モジュール
//! - `app`: EventLoop + window lifecycle
//! - `client`: TheWorld daemon HTTP クライアント
//! - `menu`: muda メニューバー
//! - `tray`: tray-icon 常駐アイコン

pub mod app;
pub mod client;
pub mod daemon_launcher;
pub mod log_format;
pub mod main_area;
pub mod menu;
pub mod pane;
pub mod settings;
pub mod shell_detect;
pub mod terminal;
pub mod tray;
pub mod ws_terminal;
