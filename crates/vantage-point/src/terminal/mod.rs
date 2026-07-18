//! ターミナルエミュレーション モジュール
//!
//! alacritty_terminal でVTシーケンスをパースし、
//! グリッド状態を管理する。
//!
//! ## パイプライン
//! ```text
//! PTY output (bytes) → VT parser → Grid<Cell>
//! ```

pub(crate) mod state;
pub(crate) mod term_attach;

pub use state::TerminalState;
