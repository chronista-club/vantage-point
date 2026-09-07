//! 別 thread で走る対話 flow（blocking / picker）。結果は `AppEvent` か callback で UI へ返す。
//!
//! `daemon/` の関数を呼ぶことは許す（repo picker → add / start、update → binary 位置）。
//! 棚卸し 項目 6 / 6-0（2026-09-08）。

/// sidebar Hub 行の Login / Logout フロー (`vp auth login|logout` spawn + hub/reconnect)。
pub mod auth;
/// Repo add / clone ダイアログ (folder picker + git clone + daemon API)。 VP-194 R-3。
pub mod repo_dialog;
/// in-app update フロー（`vp update` → daemon restart → GUI relaunch）。
pub mod update;
