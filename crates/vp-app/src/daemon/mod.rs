//! daemon との線 — 接続・制御 RPC・購読・poller・起動 / 再起動 / 診断。
//!
//! 再接続の唯一の所有者は `conn`。他 directory の関数は呼ばない
//! （共有型 `crate::daemon_wire` / `crate::pane` の参照は可）。棚卸し 項目 6 / 6-0（2026-09-08）。

/// 共有 QUIC connection の manager（再接続の唯一の所有者）+ repo-proxy ask。
pub mod conn;
/// daemon control plane クライアント (Unison `daemon-control` / `registry`)。 doc 45 段 3。
pub mod control;
/// HTTP `/api/health` の probe（Unison が壊れた時の診断用、doc 45 §2）。
pub mod health_probe;
/// daemon の起動確認 / 自動起動（`vp daemon start` の spawn）。
pub mod launcher;
/// 設定ページの「daemon を再起動」フロー (確認ダイアログ → `vp daemon restart`)。doc 59 P1。
pub mod restart;

pub use control::DaemonControl;
pub use health_probe::HealthProbe;

/// daemon の既定ポート。
///
/// VP_PROFILE 分離 (dev/brew 混在根治): brew=32000 / dev=32100。 定義は
/// `vp_paths::default_daemon_port()` (全 crate 共有の SSOT)。 dev binary と brew cask が
/// 別 node port で並列常駐できるよう、 app→daemon connect もこの port を honor する。
pub fn default_daemon_port() -> u16 {
    vp_paths::default_daemon_port()
}
