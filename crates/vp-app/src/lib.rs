//! Vantage Point native app — ライブラリ層
//!
//! `main.rs` から使う app モジュール一式。
//! クロスプラットフォーム (macOS / Windows / Linux) 対応を原則とする。
//!
//! ## 構成（棚卸し 項目 6 / 6-0、2026-09-08。SSOT は docs/design/60-vp-app-layout.md）
//!
//! - crate root = 共有型・共有 state（`events` / `daemon_wire` / `lane_address` / `pane` / `session_state` / `settings`）
//!   と基盤（log / icon / menu / tray）。どの directory からも参照してよい
//! - `app/` = UI thread の世界（EventLoop、state 遷移、効果の実行）
//! - `daemon/` = daemon との線 / `lane/` = lane ごとの session / `webview/` = IPC decode と投影 /
//!   `flows/` = 別 thread の対話 flow
//! - 処理の呼び出しは `app` → 各 directory の一方向（`flows` → `daemon` は許可）。
//!   directory 同士は互いの関数を呼ばない
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

/// UI thread の世界 — EventLoop + window lifecycle + state 遷移と効果の実行。
/// （6-0 で `app/mod.rs` に。6-2 で boot / state / on_* に分解予定）
pub mod app;
/// chat submit の応答分類（純粋関数、`tests/` から参照）。
pub mod conversation_submission;
/// daemon との線（接続・制御 RPC・health probe・起動 / 再起動）。
pub mod daemon;
/// daemon ↔ vp-app の wire 型（共有型。webview の wire 型は `generated/`）。
pub mod daemon_wire;
pub mod debug_log;
/// tao EventLoop に流す app 全体の event（`AppEvent`）。送り手は各 sibling、受け手は `app::run()`。
pub mod events;
/// 別 thread で走る対話 flow（auth / update / repo dialog）。
pub mod flows;
/// club-kdl-codegen 生成物 (KDL protocol schema → Rust 型)。 VP-208 Phase A。
pub mod generated;
pub mod icon;
/// lane ごとの session と見え方（title。session 本体は 6-1 で移設）。
pub mod lane;
/// lane address の wire 型（`LaneAddressWire`、共有型）。
pub mod lane_address;
pub mod log_format;
pub mod log_init;
pub mod menu;
pub mod pane;
pub mod session_state;
pub mod settings;
/// test 専用: `$XDG_STATE_HOME` を差し替える test の直列化 + 復元（server crate と同型）。
#[cfg(test)]
mod test_env;
pub mod tray;
/// webview との線（IPC decode / asset / main-area / ink snapshot / code pane）。
pub mod webview;
// ws_terminal: Phase 2.x-d で削除 (per-Lane browser-native WebSocket に移行、 Rust 中継経路は不要)
