//! Windows backend stub — 将来 `Windows.Graphics.Capture` API or BitBlt + GDI で実装予定。
//!
//! 現状: `Capture` trait の最小実装、 全 method は "not yet supported" エラー返却。
//! macOS/Linux dogfooding 中はこのファイルの内容は cfg gate で参照されない。
//! Windows サポート時にここを埋める (`screenshot/mod.rs` の `default_backend()` が dispatch)。
//!
//! 関連 memory: vantage-point Atlas の Phase 5-D dogfooding bundle (cargo fmt/check 通過用 stub)。

use std::path::PathBuf;

use super::{Capture, CaptureFilter, CaptureResult, Rect, WindowInfo};

/// Windows screenshot backend (stub)
pub struct WindowsBackend;

impl Capture for WindowsBackend {
    fn list_windows(&self, _filter: &CaptureFilter) -> Result<Vec<WindowInfo>, String> {
        Err("screenshot: Windows backend not yet implemented".into())
    }

    fn capture(
        &self,
        _filter: &CaptureFilter,
        _output: Option<PathBuf>,
    ) -> Result<CaptureResult, String> {
        Err("screenshot: Windows backend not yet implemented".into())
    }

    fn capture_rect(&self, _rect: Rect, _output: Option<PathBuf>) -> Result<CaptureResult, String> {
        Err("screenshot: Windows backend not yet implemented".into())
    }
}
