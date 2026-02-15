//! ターミナルエミュレーション モジュール
//!
//! alacritty_terminal でVTシーケンスをパースし、
//! グリッド状態を管理する。
//!
//! ## パイプライン
//! ```text
//! tmux output (bytes) → VT parser → Grid<Cell> → renderer
//! ```

mod state;

#[cfg(target_os = "macos")]
pub mod renderer;

pub use state::TerminalState;
