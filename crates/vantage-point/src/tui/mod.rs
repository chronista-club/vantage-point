//! TUI Core — ratatui ベースの対話コンソール
//!
//! Claude CLI を PTY パススルーで表示し、VP のステータスバーを重ねる。
//! プロジェクト選択画面 → Claude セッション の画面遷移を管理。

mod app;
mod input;
mod terminal_widget;

pub use app::{run_tui, run_tui_select};
