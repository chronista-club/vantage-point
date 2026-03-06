//! TUI Core — ratatui ベースの対話コンソール
//!
//! Claude CLI を PTY パススルーで表示し、VP のステータスバーを重ねる。

mod app;
mod input;
mod terminal_widget;

pub use app::run_tui;
