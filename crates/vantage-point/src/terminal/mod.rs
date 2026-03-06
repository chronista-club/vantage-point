//! ターミナルエミュレーション モジュール
//!
//! alacritty_terminal でVTシーケンスをパースし、
//! グリッド状態を管理する。
//!
//! ## パイプライン
//! ```text
//! PTY output (bytes) → VT parser → Grid<Cell> → renderer
//! ```

pub(crate) mod state;

#[cfg(target_os = "macos")]
pub mod renderer;

pub use state::TerminalState;

/// ステータスバーに表示するウィンドウ情報
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub index: usize,
    pub name: String,
    pub is_active: bool,
}

/// ステータスバーに表示する情報
#[derive(Clone, Debug, Default)]
pub struct StatusBarInfo {
    pub session_name: String,
    pub windows: Vec<WindowInfo>,
}
