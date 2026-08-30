//! Vantage Point native app — ライブラリ層
//!
//! `main.rs` から使う app モジュール一式。
//! クロスプラットフォーム (macOS / Windows / Linux) 対応を原則とする。
//!
//! ## モジュール
//! - `app`: EventLoop + window lifecycle
//! - `client`: daemon の HTTP health (`/api/health`) + wire 型
//! - `daemon_control`: daemon control plane クライアント (Unison、doc 45 段 3)
//! - `menu`: muda メニューバー
//! - `tray`: tray-icon 常駐アイコン
//!
//! ## Tokio runtime 規約 (= panic 再発防止 gate)
//!
//! vp-app は tao の event_loop が macOS main thread を専有し、 closure 内に Tokio
//! runtime context が無い。 そこで bare `tokio::spawn` を呼ぶと「no reactor running」
//! panic で即死する (= 過去事故、 board 永続化 #456241e 等)。
//!
//! 全 async work は `app::run()` で作る shared runtime の
//! `rt_handle.spawn(...)` / `rt_handle.spawn_blocking(...)` 経由で投げる。
//! `tokio::spawn` 直書きは `crates/vp-app/clippy.toml` の `disallowed-methods` +
//! 下記 `#![deny(...)]` で compile-time block (= CI fail)。

#![deny(clippy::disallowed_methods)]

pub mod app;
/// sidebar Hub 行の Login / Logout フロー (`vp auth login|logout` spawn + hub/reconnect)。
pub mod auth_flow;
pub mod client;
/// daemon control plane クライアント (Unison `daemon-control` / `registry`)。 doc 45 段 3。
pub mod daemon_control;
/// 設定ページの「daemon を再起動」フロー (確認ダイアログ → `vp daemon restart`)。doc 59 P1。
pub mod daemon_flow;
pub mod daemon_launcher;
pub mod debug_log;
/// code pane（コードブラウザ）の file 供給 — lane workdir walk + ファイル読み。
/// code:list / code:read IPC の Rust 側実装。
pub mod file_explorer;
/// club-kdl-codegen 生成物 (KDL protocol schema → Rust 型)。 VP-208 Phase A。
pub mod generated;
pub mod icon;
pub mod ink_snapshot;
pub mod lane;
pub mod log_format;
pub mod log_init;
pub mod main_area;
pub mod menu;
pub mod pane;
/// Repo add / clone ダイアログ (folder picker + git clone + daemon API)。 VP-194 R-3。
pub mod repo_dialog;
pub mod session_state;
pub mod session_title;
pub mod settings;
pub mod shell_detect;
pub mod terminal;
pub mod tray;
pub mod update_flow;
pub mod web_assets;
// ws_terminal: Phase 2.x-d で削除 (per-Lane browser-native WebSocket に移行、 Rust 中継経路は不要)
