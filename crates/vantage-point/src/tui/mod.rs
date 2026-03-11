//! TUI Core — ratatui ベースのマルチプロジェクト対話コンソール
//!
//! 複数の Claude CLI PTY セッションを同時管理し、
//! プロジェクト間の動的切替をサポートする。

mod app;
mod bridge;
mod canvas_state;
mod draw;
mod input;
mod overlay;
mod project_context;
pub(crate) mod session;
mod terminal_widget;
pub(crate) mod theme;

pub use app::{run_tui, run_tui_select};
