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
#[derive(Clone)]
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
    if let Some(map) = guard.as_ref()
        && map.contains_key(&1)
    {
        return;
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
        if let Some(map) = guard.as_mut()
            && let Some(session) = map.get_mut(&session_id)
            && let Some(ref mut pty) = session.pty
        {
            let _ = pty.resize(width, height);
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
            let mut cd = cell_to_celldata(&buf[(x, y)]);
            // bit 6: WIDE_CHAR（VT パーサー由来のワイドフラグ）
            if backend.is_wide_char(x, y) {
                cd.flags |= 1 << 6;
            }
            unsafe {
                *dst.add(i as usize) = cd;
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
    vp_bridge_pty_start_command_session(session_id, cwd, std::ptr::null(), cols, rows)
}

/// コマンド指定で PTY を起動（セッション指定）
/// command が NULL ならデフォルトシェル、それ以外なら指定コマンドを実行
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_start_command_session(
    session_id: u32,
    cwd: *const c_char,
    command: *const c_char,
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

    let cmd_args: Option<Vec<String>> = if command.is_null() {
        None
    } else {
        let cmd_str = unsafe { CStr::from_ptr(command) }
            .to_string_lossy()
            .to_string();
        // シェル経由で実行（パイプ・リダイレクト等に対応）
        Some(vec![
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()),
            "-l".to_string(),
            "-c".to_string(),
            cmd_str,
        ])
    };

    let result = if let Some(args) = &cmd_args {
        let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        BridgePty::spawn_command(&cwd_str, cols, rows, Some(&args_ref), backend_arc)
    } else {
        BridgePty::spawn(&cwd_str, cols, rows, backend_arc)
    };

    match result {
        Ok(pty) => {
            let mut guard = ensure_sessions();
            if let Some(map) = guard.as_mut()
                && let Some(session) = map.get_mut(&session_id)
            {
                session.pty = Some(pty);
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

    // VP-HD-NEWLINE-DEBUG: app quit 時の改行混入原因調査 (bug feedback_hd_input_newline_on_restart)
    // 環境変数 VP_PTY_WRITE_DEBUG=1 で有効化
    if std::env::var("VP_PTY_WRITE_DEBUG").as_deref() == Ok("1") {
        let hex = bytes
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");
        let utf8 = std::str::from_utf8(bytes)
            .unwrap_or("<non-utf8>")
            .replace('\n', "\\n")
            .replace('\r', "\\r");
        eprintln!(
            "[VP_PTY_WRITE session={} len={}] hex=[{}] utf8=[{}]",
            session_id, len, hex, utf8
        );
    }

    let mut guard = ensure_sessions();
    if let Some(map) = guard.as_mut()
        && let Some(session) = map.get_mut(&session_id)
        && let Some(ref mut pty) = session.pty
    {
        return match pty.write(bytes) {
            Ok(()) => 0,
            Err(_) => -1,
        };
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

/// Bracketed Paste モードが有効か（セッション指定）
///
/// CC 等の TUI アプリが有効化している場合 true。
/// ペースト時は `\x1b[200~` ... `\x1b[201~` で囲んで送信する必要がある。
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_bracketed_paste_session(session_id: u32) -> bool {
    let guard = ensure_sessions();
    guard
        .as_ref()
        .and_then(|m| m.get(&session_id))
        .and_then(|s| s.pty.as_ref().map(|p| p.bracketed_paste_mode()))
        .unwrap_or(false)
}

/// PTY を停止（セッション指定）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_stop_session(session_id: u32) {
    let mut guard = ensure_sessions();
    if let Some(map) = guard.as_mut()
        && let Some(session) = map.get_mut(&session_id)
    {
        session.pty = None;
    }
}

/// 後方互換
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_pty_stop() {
    vp_bridge_pty_stop_session(1);
}

/// スクロールバック表示位置を変更（セッション指定）
///
/// delta > 0: 上にスクロール（過去を遡る）
/// delta < 0: 下にスクロール（現在に戻る）
/// delta == i32::MAX: ページアップ（画面高さ分）
/// delta == i32::MIN: ページダウン（画面高さ分）
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_scroll_session(session_id: u32, delta: i32) {
    let guard = ensure_sessions();
    if let Some(session) = guard.as_ref().and_then(|m| m.get(&session_id))
        && let Some(pty) = &session.pty
    {
        pty.scroll(delta);
    }
}

// =============================================================================
// クローム（ヘッダー/フッター）FFI
// =============================================================================

/// クローム領域を設定（ヘッダー/フッターの行数）
///
/// PTY には `height - header - footer` 行のグリッドサイズが通知される。
/// PTY 出力は header 行目からオフセットされて描画される。
///
/// NOTE(VP-50): Swift 側の呼び出し元は削除済み（PaneHeader に一本化）。
/// chrome 機能を復活させる場合は TerminalView.swift の setupChrome() も復元すること。
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_set_chrome(session_id: u32, header_rows: u16, footer_rows: u16) {
    let guard = ensure_sessions();
    if let Some(session) = guard.as_ref().and_then(|m| m.get(&session_id)) {
        let mut be = session.backend.lock().unwrap();
        be.set_chrome(header_rows, footer_rows);
    }
}

/// クローム行にテキストを書き込む
///
/// `y` はグリッド上の絶対行番号（0 = 最上行）。
/// `text` は UTF-8 C 文字列。
/// `fg` / `bg` は RGBA u32（0 = デフォルト色）。
///
/// NOTE(VP-50): Swift 側の呼び出し元は削除済み。
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_write_chrome_line(
    session_id: u32,
    y: u16,
    text: *const std::ffi::c_char,
    fg: u32,
    bg: u32,
) {
    let guard = ensure_sessions();
    if let Some(session) = guard.as_ref().and_then(|m| m.get(&session_id)) {
        let text_str = if text.is_null() {
            ""
        } else {
            unsafe { std::ffi::CStr::from_ptr(text) }
                .to_str()
                .unwrap_or("")
        };

        // RGBA u32 → ratatui Style
        let style = {
            use ratatui::style::{Color, Style};
            let mut s = Style::default();
            if fg != 0 {
                let r = ((fg >> 24) & 0xFF) as u8;
                let g = ((fg >> 16) & 0xFF) as u8;
                let b = ((fg >> 8) & 0xFF) as u8;
                s = s.fg(Color::Rgb(r, g, b));
            }
            if bg != 0 {
                let r = ((bg >> 24) & 0xFF) as u8;
                let g = ((bg >> 16) & 0xFF) as u8;
                let b = ((bg >> 8) & 0xFF) as u8;
                s = s.bg(Color::Rgb(r, g, b));
            }
            s
        };

        let mut be = session.backend.lock().unwrap();
        be.write_chrome_line(y, text_str, style);
    }
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
    fn test_cell_to_celldata_flags() {
        // cell_to_celldata が ratatui Modifier を正しく flags ビットに変換するか
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_char('A');
        cell.set_style(
            ratatui::style::Style::default().add_modifier(
                ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED,
            ),
        );

        let cd = cell_to_celldata(&cell);
        assert_ne!(cd.flags & (1 << 0), 0, "bit 0 (bold) should be set");
        assert_eq!(cd.flags & (1 << 1), 0, "bit 1 (italic) should NOT be set");
        assert_ne!(cd.flags & (1 << 2), 0, "bit 2 (underline) should be set");
        assert_eq!(cd.flags & (1 << 3), 0, "bit 3 (inverse) should NOT be set");
        assert_eq!(cd.flags & (1 << 5), 0, "bit 5 (dim) should NOT be set");
        assert_eq!(
            cd.flags & (1 << 6),
            0,
            "bit 6 (wide) should NOT be set by cell_to_celldata"
        );
    }

    #[test]
    fn test_cell_to_celldata_all_modifiers() {
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_char('Z');
        cell.set_style(ratatui::style::Style::default().add_modifier(
            ratatui::style::Modifier::BOLD
                | ratatui::style::Modifier::ITALIC
                | ratatui::style::Modifier::UNDERLINED
                | ratatui::style::Modifier::REVERSED
                | ratatui::style::Modifier::CROSSED_OUT
                | ratatui::style::Modifier::DIM,
        ));

        let cd = cell_to_celldata(&cell);
        assert_ne!(cd.flags & (1 << 0), 0, "bold");
        assert_ne!(cd.flags & (1 << 1), 0, "italic");
        assert_ne!(cd.flags & (1 << 2), 0, "underline");
        assert_ne!(cd.flags & (1 << 3), 0, "inverse");
        assert_ne!(cd.flags & (1 << 4), 0, "strikethrough");
        assert_ne!(cd.flags & (1 << 5), 0, "dim");
    }

    // =========================================================================
    // WIDE_CHAR bit 6 伝搬テスト
    // =========================================================================

    #[test]
    fn test_wide_flag_bit6_in_buffer() {
        // セッション作成
        let id = vp_bridge_create(10, 5, None);
        assert!(id > 0);

        // Backend に wide フラグを設定
        with_session_backend_mut(id, |backend| {
            let mut cell = ratatui::buffer::Cell::default();
            cell.set_char('漢');
            backend.draw(vec![(3u16, 1u16, &cell)].into_iter()).unwrap();
            backend.set_wide_flag(3, 1, true);
        });

        // バッファ取得して bit 6 を確認
        let total = 10 * 5;
        let mut buffer = vec![empty_cell(); total];
        let count = vp_bridge_get_buffer_session(id, buffer.as_mut_ptr(), total as u32);
        assert_eq!(count, total as u32);

        // (3, 1) → index 13 のフラグに bit 6 が立っている
        let idx = 10 + 3;
        assert_ne!(
            buffer[idx].flags & (1 << 6),
            0,
            "bit 6 should be set for wide char"
        );

        // 隣接セル (4, 1) は wide でない
        let idx_next = 10 + 4;
        assert_eq!(
            buffer[idx_next].flags & (1 << 6),
            0,
            "bit 6 should NOT be set for non-wide"
        );

        vp_bridge_destroy(id);
    }

    #[test]
    fn test_wide_flag_combined_with_bold() {
        let id = vp_bridge_create(10, 5, None);

        with_session_backend_mut(id, |backend| {
            let mut cell = ratatui::buffer::Cell::default();
            cell.set_char('あ');
            cell.set_style(
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            );
            backend.draw(vec![(0u16, 0u16, &cell)].into_iter()).unwrap();
            backend.set_wide_flag(0, 0, true);
        });

        let mut buffer = vec![empty_cell(); 50];
        vp_bridge_get_buffer_session(id, buffer.as_mut_ptr(), 50);

        // bit 0 (bold) と bit 6 (wide) が両方立っている
        assert_ne!(buffer[0].flags & (1 << 0), 0, "bit 0 (bold) should be set");
        assert_ne!(buffer[0].flags & (1 << 6), 0, "bit 6 (wide) should be set");
        // 他のビットは影響しない
        assert_eq!(
            buffer[0].flags & (1 << 1),
            0,
            "bit 1 (italic) should NOT be set"
        );

        vp_bridge_destroy(id);
    }

    #[test]
    fn test_wide_flag_not_set_by_default() {
        let id = vp_bridge_create(10, 5, None);

        with_session_backend_mut(id, |backend| {
            let mut cell = ratatui::buffer::Cell::default();
            cell.set_char('X');
            backend.draw(vec![(0u16, 0u16, &cell)].into_iter()).unwrap();
            // wide_flag を設定しない
        });

        let mut buffer = vec![empty_cell(); 50];
        vp_bridge_get_buffer_session(id, buffer.as_mut_ptr(), 50);

        // bit 6 はデフォルトで 0
        assert_eq!(
            buffer[0].flags & (1 << 6),
            0,
            "bit 6 should NOT be set without explicit wide flag"
        );

        vp_bridge_destroy(id);
    }

    #[test]
    fn test_get_buffer_null_dst() {
        let id = vp_bridge_create(10, 5, None);
        // null ポインタで 0 を返す（クラッシュしない）
        let count = vp_bridge_get_buffer_session(id, std::ptr::null_mut(), 50);
        assert_eq!(count, 0);
        vp_bridge_destroy(id);
    }

    #[test]
    fn test_get_buffer_zero_max_cells() {
        let id = vp_bridge_create(10, 5, None);
        let mut buffer = vec![empty_cell(); 50];
        // max_cells = 0 で 0 を返す
        let count = vp_bridge_get_buffer_session(id, buffer.as_mut_ptr(), 0);
        assert_eq!(count, 0);
        vp_bridge_destroy(id);
    }

    #[test]
    fn test_get_buffer_invalid_session() {
        let mut buffer = vec![empty_cell(); 50];
        // 存在しないセッション ID で 0 を返す
        let count = vp_bridge_get_buffer_session(99999, buffer.as_mut_ptr(), 50);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_clear_wide_flags_between_frames() {
        let id = vp_bridge_create(10, 5, None);

        // フレーム 1: (3,1) を wide に
        with_session_backend_mut(id, |backend| {
            backend.set_wide_flag(3, 1, true);
        });

        let mut buf1 = vec![empty_cell(); 50];
        vp_bridge_get_buffer_session(id, buf1.as_mut_ptr(), 50);
        assert_ne!(buf1[13].flags & (1 << 6), 0);

        // フレーム 2: clear_wide_flags → (3,1) は wide でなくなる
        with_session_backend_mut(id, |backend| {
            backend.clear_wide_flags();
            // 別の位置を wide に
            backend.set_wide_flag(5, 2, true);
        });

        let mut buf2 = vec![empty_cell(); 50];
        vp_bridge_get_buffer_session(id, buf2.as_mut_ptr(), 50);
        // (3,1) の bit 6 はクリアされている
        assert_eq!(
            buf2[13].flags & (1 << 6),
            0,
            "previous frame's wide flag should be cleared"
        );
        // (5,2) の bit 6 は立っている
        let idx52 = 2 * 10 + 5;
        assert_ne!(
            buf2[idx52].flags & (1 << 6),
            0,
            "new frame's wide flag should be set"
        );

        vp_bridge_destroy(id);
    }
}
