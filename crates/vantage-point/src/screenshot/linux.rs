//! Linux backend stub — 将来 X11 (`xwd`) / Wayland (`grim`) backend 切替で実装予定。
//!
//! 現状: `Capture` trait の最小実装、 全 method は "not yet supported" エラー返却。

use std::path::PathBuf;

use super::{Capture, CaptureFilter, CaptureResult, Rect, WindowInfo};

/// Linux screenshot backend (stub)
pub struct LinuxBackend;

impl Capture for LinuxBackend {
    fn list_windows(&self, _filter: &CaptureFilter) -> Result<Vec<WindowInfo>, String> {
        Err("screenshot: Linux backend not yet implemented".into())
    }

    fn capture(
        &self,
        _filter: &CaptureFilter,
        _output: Option<PathBuf>,
    ) -> Result<CaptureResult, String> {
        Err("screenshot: Linux backend not yet implemented".into())
    }

    fn capture_rect(&self, _rect: Rect, _output: Option<PathBuf>) -> Result<CaptureResult, String> {
        Err("screenshot: Linux backend not yet implemented".into())
    }
}
