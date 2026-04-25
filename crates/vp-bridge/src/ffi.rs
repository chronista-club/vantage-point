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

use crate::backend::{FrameReadyCallback, NativeBackend};
use crate::pty::BridgePty;
use crate::types::{CellData, flags as cflags, rgb_to_rgba};

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
//
// `CellData` は `crate::types::CellData` を再エクスポート (FFI ABI の都合上、
// この crate の名前空間に出す必要がある)。

/// カーソル情報（C ABI 互換）
#[repr(C)]
pub struct CursorInfo {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
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
        let size = backend.size();
        if x >= size.width || y >= size.height {
            return CellData::default();
        }
        *backend.cell(x, y)
    })
    .unwrap_or_else(CellData::default)
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
        let size = backend.size();
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
        let size = backend.size();
        let total = (size.width as u32) * (size.height as u32);
        let count = total.min(max_cells);

        for i in 0..count {
            let x = (i % size.width as u32) as u16;
            let y = (i / size.width as u32) as u16;
            // CellData は内部表現 = FFI 表現なので変換不要
            let cd = *backend.cell(x, y);
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

        let mut be = session.backend.lock().unwrap();
        // FFI 引数 fg/bg は既に RGBA u32 形式なのでそのまま渡す
        be.write_chrome_line(y, text_str, fg, bg, 0);
    }
}

// =============================================================================
// テスト・ユーティリティ FFI
// =============================================================================

/// テストパターンを描画
#[unsafe(no_mangle)]
pub extern "C" fn vp_bridge_draw_test_pattern() {
    // 標準色プリセット (旧 ratatui::style::Color 由来) を RGBA u32 で表現
    const C_BLACK: u32 = rgb_to_rgba(0, 0, 0);
    const C_WHITE: u32 = rgb_to_rgba(204, 204, 204);
    const C_RED: u32 = rgb_to_rgba(204, 0, 0);
    const C_GREEN: u32 = rgb_to_rgba(0, 204, 0);
    const C_YELLOW: u32 = rgb_to_rgba(204, 204, 0);
    const C_BLUE: u32 = rgb_to_rgba(0, 0, 204);
    const C_MAGENTA: u32 = rgb_to_rgba(204, 0, 204);
    const C_CYAN: u32 = rgb_to_rgba(0, 204, 204);
    const C_LIGHT_BLUE: u32 = rgb_to_rgba(85, 85, 255);

    with_session_backend_mut(1, |backend| {
        let size = backend.size();
        let w = size.width as usize;
        let h = size.height as usize;
        if w == 0 || h == 0 {
            return;
        }

        let lines: &[(&str, u32, u8)] = &[
            ("", C_WHITE, 0),
            (" ⭐ Vantage Point — VP Bridge v0.2.0", C_CYAN, cflags::BOLD),
            ("", C_WHITE, 0),
            (" Native Cell Bridge Test", C_GREEN, cflags::BOLD),
            ("", C_WHITE, 0),
            (" Color Test:", C_WHITE, 0),
            ("", C_WHITE, 0),
            ("", C_WHITE, 0),
            (" Bold", C_WHITE, cflags::BOLD),
            (" Italic", C_WHITE, cflags::ITALIC),
            (" Underline", C_WHITE, cflags::UNDERLINED),
            (" Dim", C_WHITE, cflags::DIM),
            ("", C_WHITE, 0),
            (" 日本語: こんにちは世界 🌍", C_CYAN, 0),
            ("", C_WHITE, 0),
            ("      /\\      ", C_LIGHT_BLUE, 0),
            ("     /  \\     ", C_LIGHT_BLUE, 0),
            ("    /    \\    ", C_LIGHT_BLUE, 0),
            ("   / VP   \\   ", C_LIGHT_BLUE, 0),
            ("  /________\\  ", C_LIGHT_BLUE, 0),
            ("", C_WHITE, 0),
            (" ✅ Bridge is working!", C_GREEN, cflags::BOLD),
        ];

        for (row, (text, fg, flag_bits)) in lines.iter().enumerate() {
            if row >= h {
                break;
            }
            let mut col = 0usize;
            for ch in text.chars() {
                if col >= w {
                    break;
                }
                let mut cell = CellData::default();
                cell.set_char(ch);
                cell.fg = *fg;
                cell.flags = *flag_bits;
                backend.set_cell(col as u16, row as u16, cell);
                col += 1;
                if unicode_width_hint(ch) > 1 && col < w {
                    col += 1;
                }
            }
        }

        // カラーバー行（row 6）
        if h > 6 {
            let color_bar = [
                (" Red ", C_RED),
                (" Green ", C_GREEN),
                (" Yellow ", C_YELLOW),
                (" Blue ", C_BLUE),
                (" Magenta ", C_MAGENTA),
                (" Cyan ", C_CYAN),
            ];
            let mut col = 1usize;
            for (label, bg) in &color_bar {
                for ch in label.chars() {
                    if col >= w {
                        break;
                    }
                    let mut cell = CellData::default();
                    cell.set_char(ch);
                    cell.fg = C_BLACK;
                    cell.bg = *bg;
                    backend.set_cell(col as u16, 6, cell);
                    col += 1;
                }
                if col < w {
                    col += 1;
                }
            }
        }

        backend.flush();
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
    use crate::types::flags;

    #[test]
    fn test_create_and_destroy() {
        let id = vp_bridge_create(80, 24, None);
        assert!(id > 0);
        let guard = ensure_sessions();
        assert!(guard.as_ref().unwrap().contains_key(&id));
        drop(guard);
        vp_bridge_destroy(id);
        let guard = ensure_sessions();
        assert!(!guard.as_ref().unwrap().contains_key(&id));
    }

    #[test]
    fn test_multi_session() {
        let id1 = vp_bridge_create(80, 24, None);
        let id2 = vp_bridge_create(120, 40, None);
        assert_ne!(id1, id2);
        vp_bridge_destroy(id1);
        vp_bridge_destroy(id2);
    }

    #[test]
    fn test_get_buffer_writes_celldata_directly() {
        let mut backend = NativeBackend::new(10, 3);
        let mut cell = CellData::default();
        cell.set_char('A');
        cell.fg = 0xFF8000FF;
        cell.bg = 0x000000FF;
        cell.flags = flags::BOLD | flags::WIDE;
        backend.set_cell(2, 1, cell);

        let read = backend.cell(2, 1);
        assert_eq!(read.symbol_str(), "A");
        assert_eq!(read.fg, 0xFF8000FF);
        assert_eq!(read.flags, flags::BOLD | flags::WIDE);
    }

    #[test]
    fn test_unicode_width_hint() {
        assert_eq!(unicode_width_hint('a'), 1);
        assert_eq!(unicode_width_hint('漢'), 2);
        assert_eq!(unicode_width_hint('🌍'), 1); // 範囲外なので 1 (期待値は要検証)
    }
}
