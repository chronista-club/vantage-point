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
/// club-kdl-codegen 生成物 (KDL protocol schema → Rust 型)。 VP-208 Phase A。
pub mod generated;
pub mod lane;
pub mod log_format;
pub mod log_init;
pub mod main_area;
pub mod menu;
pub mod pane;
pub mod paths;
/// Project add / clone ダイアログ (folder picker + git clone + TheWorld API)。 VP-194 R-3。
pub mod project_dialog;
pub mod session_state;
pub mod session_title;
pub mod settings;
pub mod shell_detect;
pub mod terminal;
pub mod tray;
pub mod web_assets;
// ws_terminal: Phase 2.x-d で削除 (per-Lane browser-native WebSocket に移行、 Rust 中継経路は不要)
