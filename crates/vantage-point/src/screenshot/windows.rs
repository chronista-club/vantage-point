//! Windows backend: PrintWindow + GDI で WebView2 (DirectComposition) 含む window を capture。
//!
//! ## 設計
//!
//! ### 何を解決するか
//! WebView2 / Chromium 系 webview は Direct Composition を使って GPU 直接合成するため、
//! `BitBlt(GetWindowDC(hwnd), ...)` では合成前の空 HWND DC しか取れず、 黒画面になる。
//! `PrintWindow` の `PW_RENDERFULLCONTENT` (0x02) flag は webview を含めて
//! 合成済み content を render させる WM_PRINT 経由 path で、 Chromium も対応している。
//!
//! ### 速度目標 (~50-100ms / call)
//! - DPI 設定: 一度だけ (lazy `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`、OnceLock)
//! - EnumWindows + 可視 filter + PID 単位 process name cache: ~5-10ms (top-level 200 個想定)
//! - PrintWindow: 30-50ms (window 合成サイズに依存)
//! - DIB → BGRA → RGBA swap → PNG encode: ~10-30ms (window size 依存、 zlib default)
//!
//! ### 採用しなかった代替
//! - **Windows.Graphics.Capture (WinRT)**: D3D11Device + frame pool で ~10ms 取れるが
//!   ~400 LOC 増える。 単発 ~80ms で足りるので見送り。 連続 capture (>20fps) が必要になったら検討
//! - **Desktop Duplication API**: whole desktop 単位、 multi-monitor 複雑

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible,
};

use super::{Capture, CaptureFilter, CaptureResult, Rect, WindowInfo};

const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(0x00000002);

/// プロセス全体で 1 回だけ DPI awareness を設定する。
/// vp-cli は通常 console subsystem で起動、 manifest 不在のため明示設定が必要。
fn ensure_dpi_aware() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // 失敗しても続行 (既に awareness 設定済 / 子 process 等でエラーする可能性あり)
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    });
}

/// `EnumWindows` callback の蓄積先。 raw pointer 渡しで unsafe だが、
/// EnumWindows は同期 single-thread なので race は無い。
struct EnumState {
    hwnds: Vec<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    unsafe {
        let state = &mut *(lparam.0 as *mut EnumState);
        // 可視 + 非最小化 のみ collect (process exe lookup の n を絞る)
        if IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
            state.hwnds.push(hwnd);
        }
    }
    TRUE
}

/// HWND の owning process exe basename (拡張子無し) を取得。
/// 失敗 / unknown は None。
fn process_exe_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        result.ok()?;
        let path_str: String = OsString::from_wide(&buf[..size as usize])
            .into_string()
            .ok()?;
        std::path::Path::new(&path_str)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return String::new();
        }
        OsString::from_wide(&buf[..n as usize])
            .into_string()
            .unwrap_or_default()
    }
}

fn window_rect_physical(hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some(rect)
    }
}

/// Memory DC + DIB → BGRA bytes 抜き出し → R/B swap → PNG 保存。
/// `hbm` の content は呼び出し前に PrintWindow / BitBlt で埋まってる前提。
fn bitmap_to_png(
    hbm: windows::Win32::Graphics::Gdi::HBITMAP,
    width: u32,
    height: u32,
    output: &std::path::Path,
) -> Result<(), String> {
    use windows::Win32::Graphics::Gdi::{HDC, HGDIOBJ};
    unsafe {
        let screen_dc: HDC = GetDC(None);
        if screen_dc.is_invalid() {
            return Err("GetDC(None) failed".into());
        }
        let mut bytes = vec![0u8; (width as usize) * (height as usize) * 4];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // negative → top-down DIB (上から下へ row 並び、 PNG と同じ方向)
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = GetDIBits(
            screen_dc,
            hbm,
            0,
            height,
            Some(bytes.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, screen_dc);
        if lines == 0 {
            return Err("GetDIBits returned 0 lines".into());
        }

        // GDI = BGRA、 image crate Rgba<u8> = RGBA。 inplace swap。
        for px in bytes.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        let buf: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
            image::ImageBuffer::from_raw(width, height, bytes)
                .ok_or_else(|| "ImageBuffer::from_raw size mismatch".to_string())?;
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
        }
        buf.save_with_format(output, image::ImageFormat::Png)
            .map_err(|e| format!("PNG save failed: {}", e))?;

        let _ = HGDIOBJ::from(hbm); // (clippy 静音用、 caller が DeleteObject 担当)
        Ok(())
    }
}

pub struct WindowsBackend;

impl Capture for WindowsBackend {
    fn list_windows(&self, filter: &CaptureFilter) -> Result<Vec<WindowInfo>, String> {
        ensure_dpi_aware();
        let mut state = EnumState {
            hwnds: Vec::with_capacity(64),
        };
        unsafe {
            EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut _ as isize))
                .map_err(|e| format!("EnumWindows failed: {}", e))?;
        }

        let owner_lower = filter.owner.to_lowercase();
        // PID 単位 cache: 同一 PID 複数 HWND の re-query 回避
        let mut pid_cache: std::collections::HashMap<u32, Option<String>> =
            std::collections::HashMap::new();
        let mut out = Vec::with_capacity(8);
        for hwnd in state.hwnds {
            let mut pid: u32 = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if pid == 0 {
                continue;
            }
            let exe = pid_cache
                .entry(pid)
                .or_insert_with(|| process_exe_name(pid))
                .clone();
            let exe = match exe {
                Some(e) => e,
                None => continue,
            };
            if exe.to_lowercase() != owner_lower {
                continue;
            }
            let rect = match window_rect_physical(hwnd) {
                Some(r) => r,
                None => continue,
            };
            let w = (rect.right - rect.left).max(0) as u32;
            let h = (rect.bottom - rect.top).max(0) as u32;
            if w == 0 || h == 0 {
                continue;
            }
            out.push(WindowInfo {
                id: hwnd.0 as usize as u64,
                owner: exe,
                title: window_title(hwnd),
                x: rect.left,
                y: rect.top,
                width: w,
                height: h,
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

        unsafe {
            let hwnd = HWND(target.id as *mut std::ffi::c_void);
            let screen_dc = GetDC(None);
            if screen_dc.is_invalid() {
                return Err("GetDC(None) failed".into());
            }
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            if mem_dc.is_invalid() {
                ReleaseDC(None, screen_dc);
                return Err("CreateCompatibleDC failed".into());
            }
            let hbm = CreateCompatibleBitmap(screen_dc, target.width as i32, target.height as i32);
            if hbm.is_invalid() {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err("CreateCompatibleBitmap failed".into());
            }
            let prev = SelectObject(mem_dc, hbm.into());

            let ok = PrintWindow(hwnd, mem_dc, PW_RENDERFULLCONTENT);
            if !ok.as_bool() {
                SelectObject(mem_dc, prev);
                let _ = DeleteObject(hbm.into());
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err("PrintWindow returned 0 (window unresponsive?)".into());
            }

            // bitmap → PNG (mem_dc は GetDIBits 内部で使わず screen_dc 経由)
            let r = bitmap_to_png(hbm, target.width, target.height, &path);

            SelectObject(mem_dc, prev);
            let _ = DeleteObject(hbm.into());
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            r?;
        }

        Ok(CaptureResult {
            path,
            width: target.width,
            height: target.height,
            elapsed_ms: started.elapsed().as_millis() as u64,
            window: target,
        })
    }

    fn capture_rect(&self, rect: Rect, output: Option<PathBuf>) -> Result<CaptureResult, String> {
        let started = std::time::Instant::now();
        ensure_dpi_aware();
        let path = output.unwrap_or_else(super::default_output_path);

        unsafe {
            let screen_dc = GetDC(None);
            if screen_dc.is_invalid() {
                return Err("GetDC(None) failed".into());
            }
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            if mem_dc.is_invalid() {
                ReleaseDC(None, screen_dc);
                return Err("CreateCompatibleDC failed".into());
            }
            let hbm = CreateCompatibleBitmap(screen_dc, rect.w as i32, rect.h as i32);
            if hbm.is_invalid() {
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err("CreateCompatibleBitmap failed".into());
            }
            let prev = SelectObject(mem_dc, hbm.into());

            // BitBlt 画面 DC → memory bitmap。 region が foreground window 領域なら WebView2 も
            // 画面合成済みなので問題無く取れる (背景遮蔽 / minimized 時は黒くなる)
            let blt = BitBlt(
                mem_dc,
                0,
                0,
                rect.w as i32,
                rect.h as i32,
                Some(screen_dc),
                rect.x,
                rect.y,
                SRCCOPY,
            );
            if blt.is_err() {
                SelectObject(mem_dc, prev);
                let _ = DeleteObject(hbm.into());
                let _ = DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err(format!("BitBlt failed: {:?}", blt));
            }

            let r = bitmap_to_png(hbm, rect.w, rect.h, &path);

            SelectObject(mem_dc, prev);
            let _ = DeleteObject(hbm.into());
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            r?;
        }

        let placeholder = WindowInfo {
            id: 0,
            owner: String::new(),
            title: format!("rect:{},{},{},{}", rect.x, rect.y, rect.w, rect.h),
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
