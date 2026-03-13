//! FFI エクスポート — Swift/C から呼び出し可能な関数群
//!
//! セッション ID ベースでマルチウィンドウ対応。
//! 各セッションが独立した NativeBackend + BridgePty を保持する。
//!
//! FFI 関数は C ABI 境界のため raw pointer を受け取る。
//! 呼び出し側（Swift）が有効なポインタを保証する前提。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ratatui::backend::Backend;
use ratatui::style::Color;

use crate::backend::{FrameReadyCallback, NativeBackend};
use crate::pty::BridgePty;

/// セッションデータ（Backend + PTY のペア）
struct Session {
    backend: Arc<Mutex<NativeBackend>>,
    pty: Option<BridgePty>,
}

/// 全セッションを管理する HashMap
static SESSIONS: Mutex<Option<HashMap<u32, Session>>> = Mutex::new(None);

/// セッション ID カウンター
static NEXT_SESSION_ID: AtomicU32 = AtomicU32::new(1);

/// セッション HashMap を初期化（lazy）
fn ensure_sessions() -> std::sync::MutexGuard<'static, Option<HashMap<u32, Session>>> {
    let mut guard = SESSIONS.lock().unwrap();
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    guard
}

/// Backend の参照で操作（読み取り）
fn with_session_backend<R>(session_id: u32, f: impl FnOnce(&NativeBackend) -> R) -> Option<R> {
    let guard = ensure_sessions();
    guard.as_ref()?.get(&session_id).map(|s| {
        let be = s.backend.lock().unwrap();
        f(&be)
    })
}

/// Backend の可変参照で操作
fn with_session_backend_mut<R>(
    session_id: u32,
    f: impl FnOnce(&mut NativeBackend) -> R,
) -> Option<R> {
    let guard = ensure_sessions();
    guard.as_ref()?.get(&session_id).map(|s| {
        let mut be = s.backend.lock().unwrap();
        f(&mut be)
    })
}

// =============================================================================
// FFI 構造体
// =============================================================================

/// Cell データ（C ABI 互換）
#[repr(C)]
pub struct CellData {
    pub ch: [u8; 5],
    pub fg: u32,
    pub bg: u32,
    pub flags: u8,
}

/// カーソル情報（C ABI 互換）
#[repr(C)]
pub struct CursorInfo {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

// =============================================================================
// ヘルパー関数
// =============================================================================

fn color_to_rgba(color: Color) -> u32 {
    match color {
        Color::Rgb(r, g, b) => ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 0xFF,
        Color::Reset => 0x00000000,
        Color::Black => 0x000000FF,
        Color::Red => 0xCC0000FF,
        Color::Green => 0x00CC00FF,
        Color::Yellow => 0xCCCC00FF,
        Color::Blue => 0x0000CCFF,
        Color::Magenta => 0xCC00CCFF,
        Color::Cyan => 0x00CCCCFF,
        Color::White => 0xCCCCCCFF,
        Color::Gray => 0x888888FF,
        Color::DarkGray => 0x555555FF,
        Color::LightRed => 0xFF5555FF,
        Color::LightGreen => 0x55FF55FF,
        Color::LightYellow => 0xFFFF55FF,
        Color::LightBlue => 0x5555FFFF,
        Color::LightMagenta => 0xFF55FFFF,
        Color::LightCyan => 0x55FFFFFF,
        Color::Indexed(idx) => {
            if idx < 16 {
                0x888888FF
            } else if idx < 232 {
                let idx = idx - 16;
                let r = (idx / 36) * 51;
                let g = ((idx % 36) / 6) * 51;
                let b = (idx % 6) * 51;
                ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 0xFF
            } else {
                let gray = 8 + (idx - 232) * 10;
                ((gray as u32) << 24) | ((gray as u32) << 16) | ((gray as u32) << 8) | 0xFF
            }
        }
    }
}

fn cell_to_celldata(cell: &ratatui::buffer::Cell) -> CellData {
    let symbol = cell.symbol();
    let bytes = symbol.as_bytes();
    let mut ch = [0u8; 5];
    let len = bytes.len().min(4);
    ch[..len].copy_from_slice(&bytes[..len]);

    let modifier = cell.modifier;
    let mut flags: u8 = 0;
    if modifier.contains(ratatui::style::Modifier::BOLD) {
        flags |= 1 << 0;
    }
    if modifier.contains(ratatui::style::Modifier::ITALIC) {
        flags |= 1 << 1;
    }
    if modifier.contains(ratatui::style::Modifier::UNDERLINED) {
        flags |= 1 << 2;
    }
    if modifier.contains(ratatui::style::Modifier::REVERSED) {
        flags |= 1 << 3;
    }
    if modifier.contains(ratatui::style::Modifier::CROSSED_OUT) {
        flags |= 1 << 4;
    }
    if modifier.contains(ratatui::style::Modifier::DIM) {
        flags |= 1 << 5;
    }

    CellData {
        ch,
        fg: color_to_rgba(cell.fg),
        bg: color_to_rgba(cell.bg),
        flags,
    }
}

fn empty_cell() -> CellData {
    CellData {
        ch: [b' ', 0, 0, 0, 0],
        fg: 0,
        bg: 0,
        flags: 0,
    }
}

// =============================================================================
// ライフサイクル FFI
// =============================================================================

/// セッションを作成して ID を返す
///
/// 各ウィンドウごとに独立したセッション（Backend + PTY）を生成する。
/// 戻り値: セッション ID (0 = 失敗)
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_create(
    width: u16,
    height: u16,
    frame_callback: Option<FrameReadyCallback>,
) -> u32 {
    let mut backend = NativeBackend::new(width, height);
    if let Some(cb) = frame_callback {
        backend.set_frame_callback(cb);
    }

    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let session = Session {
        backend: Arc::new(Mutex::new(backend)),
        pty: None,
    };

    let mut guard = ensure_sessions();
    if let Some(map) = guard.as_mut() {
        map.insert(session_id, session);
    }

    session_id
}

/// セッションを破棄
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_destroy(session_id: u32) {
    let mut guard = ensure_sessions();
    if let Some(map) = guard.as_mut() {
        // Session の drop で PTY も停止される
        map.remove(&session_id);
    }
}

/// 後方互換: 旧 API（セッション ID 1 を暗黙使用）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_init(
    width: u16,
    height: u16,
    frame_callback: Option<FrameReadyCallback>,
) {
    // セッション 1 が既に存在する場合は何もしない
    let guard = ensure_sessions();
    if let Some(map) = guard.as_ref() {
        if map.contains_key(&1) {
            return;
        }
    }
    drop(guard);

    let mut backend = NativeBackend::new(width, height);
    if let Some(cb) = frame_callback {
        backend.set_frame_callback(cb);
    }

    let session = Session {
        backend: Arc::new(Mutex::new(backend)),
        pty: None,
    };

    let mut guard = ensure_sessions();
    if let Some(map) = guard.as_mut() {
        map.insert(1, session);
    }
}

/// 後方互換: 旧 API
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_deinit() {
    vp_bridge_destroy(1);
}

/// グリッドサイズを変更（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_resize_session(session_id: u32, width: u16, height: u16) {
    // PTY リサイズ（ロック順序: PTY → Backend）
    // FFI はメインスレッドからのみ呼ばれる前提
    {
        let mut guard = ensure_sessions();
        if let Some(map) = guard.as_mut() {
            if let Some(session) = map.get_mut(&session_id) {
                if let Some(ref mut pty) = session.pty {
                    let _ = pty.resize(width, height);
                }
            }
        }
    }

    // Backend リサイズ
    with_session_backend_mut(session_id, |backend| {
        backend.resize(width, height);
    });
}

/// 後方互換: 旧 API
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_resize(width: u16, height: u16) {
    vp_bridge_resize_session(1, width, height);
}

// =============================================================================
// 読み取り FFI
// =============================================================================

/// 指定座標の CellData を取得（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_cell_session(session_id: u32, x: u16, y: u16) -> CellData {
    with_session_backend(session_id, |backend| {
        let buf = backend.buffer();
        let size = backend.size().unwrap_or(ratatui::layout::Size::new(0, 0));
        if x >= size.width || y >= size.height {
            return empty_cell();
        }
        cell_to_celldata(&buf[(x, y)])
    })
    .unwrap_or_else(empty_cell)
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_cell(x: u16, y: u16) -> CellData {
    vp_bridge_get_cell_session(1, x, y)
}

/// 現在のグリッドサイズを取得（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_size_session(
    session_id: u32,
    out_width: *mut u16,
    out_height: *mut u16,
) {
    if out_width.is_null() || out_height.is_null() {
        return;
    }
    with_session_backend(session_id, |backend| {
        let size = backend.size().unwrap_or(ratatui::layout::Size::new(0, 0));
        unsafe {
            *out_width = size.width;
            *out_height = size.height;
        }
    });
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_size(out_width: *mut u16, out_height: *mut u16) {
    vp_bridge_get_size_session(1, out_width, out_height);
}

/// カーソル情報を取得（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_cursor_session(session_id: u32) -> CursorInfo {
    with_session_backend(session_id, |backend| CursorInfo {
        x: backend.cursor_position().x,
        y: backend.cursor_position().y,
        visible: backend.is_cursor_visible(),
    })
    .unwrap_or(CursorInfo {
        x: 0,
        y: 0,
        visible: false,
    })
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_cursor() -> CursorInfo {
    vp_bridge_get_cursor_session(1)
}

/// バッファ全体を一括取得（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_buffer_session(
    session_id: u32,
    dst: *mut CellData,
    max_cells: u32,
) -> u32 {
    if dst.is_null() || max_cells == 0 {
        return 0;
    }
    with_session_backend(session_id, |backend| {
        let size = backend.size().unwrap_or(ratatui::layout::Size::new(0, 0));
        let total = (size.width as u32) * (size.height as u32);
        let count = total.min(max_cells);

        let buf = backend.buffer();
        for i in 0..count {
            let x = (i % size.width as u32) as u16;
            let y = (i / size.width as u32) as u16;
            unsafe {
                *dst.add(i as usize) = cell_to_celldata(&buf[(x, y)]);
            }
        }
        count
    })
    .unwrap_or(0)
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_get_buffer(dst: *mut CellData, max_cells: u32) -> u32 {
    vp_bridge_get_buffer_session(1, dst, max_cells)
}

// =============================================================================
// PTY FFI
// =============================================================================

/// PTY を起動（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_start_session(
    session_id: u32,
    cwd: *const c_char,
    cols: u16,
    rows: u16,
) -> i32 {
    let backend_arc = {
        let guard = ensure_sessions();
        match guard.as_ref().and_then(|m| m.get(&session_id)) {
            Some(session) => session.backend.clone(),
            None => return -1,
        }
    };

    let cwd_str = if cwd.is_null() {
        std::env::var("HOME").unwrap_or_else(|_| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/".to_string())
        })
    } else {
        unsafe { CStr::from_ptr(cwd) }.to_string_lossy().to_string()
    };

    match BridgePty::spawn(&cwd_str, cols, rows, backend_arc) {
        Ok(pty) => {
            let mut guard = ensure_sessions();
            if let Some(map) = guard.as_mut() {
                if let Some(session) = map.get_mut(&session_id) {
                    session.pty = Some(pty);
                }
            }
            0
        }
        Err(_) => -1,
    }
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_start(cwd: *const c_char, cols: u16, rows: u16) -> i32 {
    vp_bridge_pty_start_session(1, cwd, cols, rows)
}

/// PTY にバイト列を送信（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_write_session(session_id: u32, data: *const u8, len: u32) -> i32 {
    if data.is_null() || len == 0 {
        return -1;
    }

    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };

    let mut guard = ensure_sessions();
    if let Some(map) = guard.as_mut() {
        if let Some(session) = map.get_mut(&session_id) {
            if let Some(ref mut pty) = session.pty {
                return match pty.write(bytes) {
                    Ok(()) => 0,
                    Err(_) => -1,
                };
            }
        }
    }
    -1
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_write(data: *const u8, len: u32) -> i32 {
    vp_bridge_pty_write_session(1, data, len)
}

/// PTY が稼働中か（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_is_running_session(session_id: u32) -> bool {
    let guard = ensure_sessions();
    guard
        .as_ref()
        .and_then(|m| m.get(&session_id))
        .and_then(|s| s.pty.as_ref().map(|p| p.is_running()))
        .unwrap_or(false)
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_is_running() -> bool {
    vp_bridge_pty_is_running_session(1)
}

/// PTY を停止（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_stop_session(session_id: u32) {
    let mut guard = ensure_sessions();
    if let Some(map) = guard.as_mut() {
        if let Some(session) = map.get_mut(&session_id) {
            session.pty = None;
        }
    }
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_stop() {
    vp_bridge_pty_stop_session(1);
}

// =============================================================================
// テスト・ユーティリティ FFI
// =============================================================================

/// テストパターンを描画
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_draw_test_pattern() {
    use ratatui::buffer::Cell;
    use ratatui::style::{Modifier, Style};

    with_session_backend_mut(1, |backend| {
        let size = backend.size().unwrap_or(ratatui::layout::Size::new(0, 0));
        let w = size.width as usize;
        let h = size.height as usize;
        if w == 0 || h == 0 {
            return;
        }

        let lines: Vec<(&str, Style)> = vec![
            ("", Style::default()),
            (
                " ⭐ Vantage Point — VP Bridge v0.2.0",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            ("", Style::default()),
            (
                " ratatui → NSView Bridge Test",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            ("", Style::default()),
            (" Color Test:", Style::default().fg(Color::White)),
            ("", Style::default()),
            ("", Style::default()),
            (
                " Bold",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            (
                " Italic",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::ITALIC),
            ),
            (
                " Underline",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::UNDERLINED),
            ),
            (
                " Dim",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::DIM),
            ),
            ("", Style::default()),
            (
                " 日本語: こんにちは世界 🌍",
                Style::default().fg(Color::Cyan),
            ),
            ("", Style::default()),
            ("      /\\      ", Style::default().fg(Color::LightBlue)),
            ("     /  \\     ", Style::default().fg(Color::LightBlue)),
            ("    /    \\    ", Style::default().fg(Color::LightBlue)),
            ("   / VP   \\   ", Style::default().fg(Color::LightBlue)),
            ("  /________\\  ", Style::default().fg(Color::LightBlue)),
            ("", Style::default()),
            (
                " ✅ Bridge is working!",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        let mut cells: Vec<(u16, u16, Cell)> = Vec::new();
        for (row, (text, style)) in lines.iter().enumerate() {
            if row >= h {
                break;
            }
            let mut col = 0usize;
            for ch in text.chars() {
                if col >= w {
                    break;
                }
                let mut cell = Cell::default();
                cell.set_char(ch);
                cell.set_style(*style);
                cells.push((col as u16, row as u16, cell));
                col += 1;
                if unicode_width_hint(ch) > 1 && col < w {
                    col += 1;
                }
            }
        }

        // カラーバー行（row 6）
        if h > 6 {
            let color_bar = [
                (" Red ", Color::Red),
                (" Green ", Color::Green),
                (" Yellow ", Color::Yellow),
                (" Blue ", Color::Blue),
                (" Magenta ", Color::Magenta),
                (" Cyan ", Color::Cyan),
            ];
            let mut col = 1usize;
            for (label, bg) in &color_bar {
                for ch in label.chars() {
                    if col >= w {
                        break;
                    }
                    let mut cell = Cell::default();
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(Color::Black).bg(*bg));
                    cells.push((col as u16, 6, cell));
                    col += 1;
                }
                if col < w {
                    col += 1;
                }
            }
        }

        let refs: Vec<(u16, u16, &Cell)> = cells.iter().map(|(x, y, c)| (*x, *y, c)).collect();
        let _ = backend.draw(refs.into_iter());
        let _ = backend.flush();
    });
}

fn unicode_width_hint(ch: char) -> usize {
    let c = ch as u32;
    if (0x3000..=0x9FFF).contains(&c)
        || (0xF900..=0xFAFF).contains(&c)
        || (0xFE30..=0xFE4F).contains(&c)
        || (0xFF00..=0xFF60).contains(&c)
        || (0x20000..=0x2FA1F).contains(&c)
    {
        2
    } else {
        1
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_version() -> *const c_char {
    c"0.2.0".as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::Backend;

    #[test]
    fn test_color_to_rgba() {
        assert_eq!(color_to_rgba(Color::Rgb(255, 0, 128)), 0xFF0080FF);
        assert_eq!(color_to_rgba(Color::Black), 0x000000FF);
        assert_eq!(color_to_rgba(Color::Reset), 0x00000000);
    }

    #[test]
    fn test_color_indexed_216_cube() {
        assert_eq!(color_to_rgba(Color::Indexed(16)), 0x000000FF);
        assert_eq!(color_to_rgba(Color::Indexed(231)), 0xFFFFFFFF);
    }

    #[test]
    fn test_color_indexed_grayscale() {
        assert_eq!(color_to_rgba(Color::Indexed(232)), 0x080808FF);
        assert_eq!(color_to_rgba(Color::Indexed(255)), 0xEEEEEEFF);
    }

    #[test]
    fn test_create_and_destroy() {
        let id = vp_bridge_create(80, 24, None);
        assert!(id > 0);

        // セッションが存在する
        let guard = ensure_sessions();
        assert!(guard.as_ref().unwrap().contains_key(&id));
        drop(guard);

        vp_bridge_destroy(id);

        // セッションが削除された
        let guard = ensure_sessions();
        assert!(!guard.as_ref().unwrap().contains_key(&id));
    }

    #[test]
    fn test_multi_session() {
        let id1 = vp_bridge_create(80, 24, None);
        let id2 = vp_bridge_create(120, 40, None);
        assert_ne!(id1, id2);

        let guard = ensure_sessions();
        assert!(guard.as_ref().unwrap().contains_key(&id1));
        assert!(guard.as_ref().unwrap().contains_key(&id2));
        drop(guard);

        vp_bridge_destroy(id1);
        vp_bridge_destroy(id2);
    }

    #[test]
    fn test_init_and_get_cell() {
        let mut backend = NativeBackend::new(80, 24);
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_char('W');
        cell.set_style(
            ratatui::style::Style::default()
                .fg(Color::Rgb(255, 128, 0))
                .bg(Color::Black)
                .add_modifier(ratatui::style::Modifier::BOLD),
        );
        backend
            .draw(vec![(10u16, 5u16, &cell)].into_iter())
            .unwrap();
        let buf = backend.buffer();
        assert_eq!(buf[(10, 5)].symbol(), "W");
    }

    #[test]
    fn test_cell_data_flags() {
        let mut flags: u8 = 0;
        flags |= 1 << 0;
        flags |= 1 << 2;
        assert_eq!(flags & (1 << 0), 1);
        assert_eq!(flags & (1 << 1), 0);
        assert_eq!(flags & (1 << 2), 4);
    }
}
