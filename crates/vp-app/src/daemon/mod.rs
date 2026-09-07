//! daemon との線 — 接続・制御 RPC・購読・poller・起動 / 再起動 / 診断。
//!
//! 再接続の唯一の所有者は `conn`（6-1 で app から移設予定）。他 directory の関数は呼ばない
//! （共有型 `crate::client` / `crate::pane` の参照は可）。棚卸し 項目 6 / 6-0（2026-09-08）。

/// daemon control plane クライアント (Unison `daemon-control` / `registry`)。 doc 45 段 3。
pub mod control;
/// daemon の起動確認 / 自動起動（`vp daemon start` の spawn）。
pub mod launcher;
/// 設定ページの「daemon を再起動」フロー (確認ダイアログ → `vp daemon restart`)。doc 59 P1。
pub mod restart;

pub use control::DaemonControl;
