//! macOS backend: `swift -e` で window 列挙 (CGWindowList 経由) + `screencapture` CLI で capture。
//!
//! ## 性能 (~250-300ms / call、 cold start)
//!
//! - swift -e JIT compile: ~150-200ms
//! - CGWindowList traverse: ~10ms
//! - screencapture spawn: ~50-70ms
//! - 合計: ~250-300ms
//!
//! ## 将来の高速化 (Phase 2+)
//!
//! - **objc2-core-graphics で pure Rust 化**: swift JIT 排除で ~50ms に短縮
//! - **ScreenCaptureKit (macOS 14+)**: Apple 推奨の最新 API、 ~30ms 可能
//! - **persistent helper binary**: swift binary を `target/release/vp-window-finder`
//!   等に pre-compile して再利用、 JIT 排除のみで ~80ms
//!
//! 今は trait 抽象が確立してるので、 backend 実装の差し替えは ~1 file の変更で済む。

use std::path::PathBuf;
use std::process::Command;

use super::{Capture, CaptureFilter, CaptureResult, Rect, WindowInfo};

pub struct MacOsBackend;

impl Capture for MacOsBackend {
    fn list_windows(&self, filter: &CaptureFilter) -> Result<Vec<WindowInfo>, String> {
        let owner_filter = filter.owner.clone();
        let swift_script = format!(
            r#"
import CoreGraphics
import Foundation
let windows = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as? [[String: Any]] ?? []
for w in windows {{
    let owner = w["kCGWindowOwnerName"] as? String ?? ""
    let layer = w["kCGWindowLayer"] as? Int ?? -1
    if owner == "{owner}" && layer == 0 {{
        let id = w["kCGWindowNumber"] as? Int ?? 0
        let title = w["kCGWindowName"] as? String ?? ""
        let bounds = w["kCGWindowBounds"] as? [String: Any] ?? [:]
        let x = bounds["X"] as? Int ?? 0
        let y = bounds["Y"] as? Int ?? 0
        let width = bounds["Width"] as? Int ?? 0
        let height = bounds["Height"] as? Int ?? 0
        // tab-separated: id\towner\tx\ty\twidth\theight\ttitle
        print("\(id)\t\(owner)\t\(x)\t\(y)\t\(width)\t\(height)\t\(title)")
    }}
}}
"#,
            owner = owner_filter.replace('"', "\\\"")
        );
        let output = Command::new("swift")
            .args(["-e", &swift_script])
            .output()
            .map_err(|e| format!("swift spawn failed: {} (is swift available on PATH?)", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("swift exited non-zero: {}", stderr.trim()));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut out = Vec::new();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(7, '\t').collect();
            if parts.len() < 6 {
                continue;
            }
            let id: u64 = parts[0].parse().unwrap_or(0);
            if id == 0 {
                continue;
            }
            let x: i32 = parts[2].parse().unwrap_or(0);
            let y: i32 = parts[3].parse().unwrap_or(0);
            let width: u32 = parts[4].parse().unwrap_or(0);
            let height: u32 = parts[5].parse().unwrap_or(0);
            let title = parts.get(6).copied().unwrap_or("").to_string();
            out.push(WindowInfo {
                id,
                owner: parts[1].to_string(),
                title,
                x,
                y,
                width,
                height,
                layer: 0,
            });
        }
        Ok(out)
    }

    fn capture(
        &self,
        filter: &CaptureFilter,
        output: Option<PathBuf>,
    ) -> Result<CaptureResult, String> {
        let started = std::time::Instant::now();
        let windows = self.list_windows(filter)?;
        if windows.is_empty() {
            return Err(format!(
                "no window with owner = {:?}. is the app running?",
                filter.owner
            ));
        }
        let target = super::pick_window(&windows, filter)?;
        let path = output.unwrap_or_else(super::default_output_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
        }
        let status = Command::new("screencapture")
            .args(["-x", "-o", "-l"])
            .arg(target.id.to_string())
            .arg(&path)
            .status()
            .map_err(|e| format!("screencapture spawn failed: {}", e))?;
        if !status.success() {
            return Err(format!("screencapture exit status: {:?}", status.code()));
        }
        Ok(CaptureResult {
            path,
            width: target.width,
            height: target.height,
            elapsed_ms: started.elapsed().as_millis() as u64,
            window: target,
        })
    }

    fn capture_rect(
        &self,
        rect: Rect,
        output: Option<PathBuf>,
    ) -> Result<CaptureResult, String> {
        let started = std::time::Instant::now();
        let path = output.unwrap_or_else(super::default_output_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
        }
        // screencapture -R <x>,<y>,<w>,<h> path
        let region_arg = format!("{},{},{},{}", rect.x, rect.y, rect.w, rect.h);
        let status = Command::new("screencapture")
            .args(["-x", "-o", "-R", &region_arg])
            .arg(&path)
            .status()
            .map_err(|e| format!("screencapture spawn failed: {}", e))?;
        if !status.success() {
            return Err(format!("screencapture exit status: {:?}", status.code()));
        }
        // capture_rect は WindowInfo 持たないので zero-id placeholder で返す
        let placeholder = WindowInfo {
            id: 0,
            owner: String::new(),
            title: format!("rect:{}", region_arg),
            x: rect.x,
            y: rect.y,
            width: rect.w,
            height: rect.h,
            layer: 0,
        };
        Ok(CaptureResult {
            path,
            width: rect.w,
            height: rect.h,
            elapsed_ms: started.elapsed().as_millis() as u64,
            window: placeholder,
        })
    }
}
