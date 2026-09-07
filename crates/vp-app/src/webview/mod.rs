//! webview との線 — IPC の decode と Rust→JS の投影。
//!
//! state 遷移はここに置かない（`app/` の責務）。push 系は 6-1 で app から移設予定。
//! 棚卸し 項目 6 / 6-0（2026-09-08）。

/// `vp-asset://` custom protocol の asset 供給（bundle の disk-read / embed）。
pub mod assets;
/// editor bridge / fleet の JS 式 builder（純 calculation）。
pub mod editor_bridge;
/// code pane（コードブラウザ）の file 供給 — lane workdir walk + ファイル読み。
pub mod file_explorer;
/// ink snapshot（WKWebView の PNG capture、macOS のみ実体）。
pub mod ink_snapshot;
/// main-area の HTML / bundle 埋め込みと active pane script。
pub mod main_area;
/// Rust → main webview の投影（typed push、`vp-push.kdl` envelope）。
pub mod push_main;
/// main_area webview からの IPC handler（decode → `AppEvent`）。
pub mod terminal_ipc;
