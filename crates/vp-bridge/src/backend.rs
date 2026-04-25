//! NativeBackend — `CellData` グリッドバッファ
//!
//! 描画結果を `Vec<CellData>` に蓄積し、FFI 経由で Swift/NSView に公開する。
//! ターミナルエミュレータには依存せず、CellData グリッドをメモリ上に保持するのみ。
//!
//! 設計方針 (2026-04-25 ratatui 脱却 + ultrathink simplification):
//! ratatui::Backend trait + ratatui::Buffer + 中間 Cell 型をすべて排除。
//! `CellData` (#[repr(C)] FFI 互換) を内部バッファの要素型としても使い、
//! 中間変換を完全に廃止して直線パスにする。

use std::sync::atomic::{AtomicBool, Ordering};

use crate::types::{flags, CellData, Position, Size};

/// フレーム更新通知コールバック型
pub type FrameReadyCallback = extern "C" fn();

/// CellData グリッドの内部バッファ
pub struct NativeBackend {
    width: u16,
    height: u16,
    /// CellData バッファ (row-major: index = y * width + x)
    cells: Vec<CellData>,
    cursor: Position,
    cursor_visible: bool,
    frame_callback: Option<FrameReadyCallback>,
    dirty: AtomicBool,
    chrome_header_rows: u16,
    chrome_footer_rows: u16,
}

impl NativeBackend {
    pub fn new(width: u16, height: u16) -> Self {
        let total = (width as usize) * (height as usize);
        Self {
            width,
            height,
            cells: vec![CellData::default(); total],
            cursor: Position::new(0, 0),
            cursor_visible: true,
            frame_callback: None,
            dirty: AtomicBool::new(false),
            chrome_header_rows: 0,
            chrome_footer_rows: 0,
        }
    }

    /// クローム（ヘッダー/フッター）の行数を設定
    pub fn set_chrome(&mut self, header_rows: u16, footer_rows: u16) {
        self.chrome_header_rows = header_rows;
        self.chrome_footer_rows = footer_rows;
    }

    /// PTY に通知すべきグリッドサイズ（クローム分を除いた行数）
    pub fn pty_rows(&self) -> u16 {
        self.height
            .saturating_sub(self.chrome_header_rows)
            .saturating_sub(self.chrome_footer_rows)
    }

    /// PTY 出力の Y オフセット（ヘッダー行数分だけ下にずらす）
    pub fn pty_y_offset(&self) -> u16 {
        self.chrome_header_rows
    }

    /// クローム行に色 + テキストを書き込む
    ///
    /// fg/bg は `0xRRGGBBAA` (alpha=0 でデフォルト透明扱い)、`flags` は `flags::BOLD` 等の OR。
    pub fn write_chrome_line(&mut self, y: u16, text: &str, fg: u32, bg: u32, modifier: u8) {
        if y >= self.height {
            return;
        }
        let width = self.width;
        // 行全体をクリアしてスタイル適用
        for x in 0..width {
            let idx = (y as usize) * (width as usize) + (x as usize);
            self.cells[idx] = CellData {
                ch: [b' ', 0, 0, 0, 0],
                fg,
                bg,
                flags: modifier,
            };
        }
        // テキスト書き込み
        for (i, ch) in text.chars().enumerate() {
            let x = i as u16;
            if x >= width {
                break;
            }
            let idx = (y as usize) * (width as usize) + (x as usize);
            self.cells[idx].set_char(ch);
        }
        self.dirty.store(true, Ordering::Release);
    }

    pub fn set_frame_callback(&mut self, callback: FrameReadyCallback) {
        self.frame_callback = Some(callback);
    }

    pub fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// セル参照 (FFI 読み取り用)
    pub fn cell(&self, x: u16, y: u16) -> &CellData {
        debug_assert!(x < self.width && y < self.height);
        &self.cells[(y as usize) * (self.width as usize) + (x as usize)]
    }

    /// 単一セルを書き込む (範囲外は無視)
    pub fn set_cell(&mut self, x: u16, y: u16, cell: CellData) {
        if x < self.width && y < self.height {
            self.cells[(y as usize) * (self.width as usize) + (x as usize)] = cell;
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// グリッドサイズを変更（Swift 側のリサイズに対応）
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        let total = (width as usize) * (height as usize);
        self.cells = vec![CellData::default(); total];
    }

    /// 全セルを default 値（空白）にクリア
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            *cell = CellData::default();
        }
        self.dirty.store(true, Ordering::Release);
    }

    /// ワイドキャラクターフラグを設定 (`CellData.flags` の WIDE bit を直接操作)
    pub fn set_wide_flag(&mut self, x: u16, y: u16, wide: bool) {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            if wide {
                self.cells[idx].flags |= flags::WIDE;
            } else {
                self.cells[idx].flags &= !flags::WIDE;
            }
        }
    }

    /// ワイドキャラクターフラグを取得
    pub fn is_wide_char(&self, x: u16, y: u16) -> bool {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            (self.cells[idx].flags & flags::WIDE) != 0
        } else {
            false
        }
    }

    /// 全セルから WIDE フラグだけクリア（sync_to_backend の先頭で呼ぶ）
    pub fn clear_wide_flags(&mut self) {
        for cell in &mut self.cells {
            cell.flags &= !flags::WIDE;
        }
    }

    pub fn set_cursor_position(&mut self, position: Position) {
        self.cursor = position;
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }

    pub fn is_cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn cursor_position(&self) -> Position {
        self.cursor
    }

    /// バッファのダーティ状態を flush し、コールバックを発火
    pub fn flush(&mut self) {
        if self.dirty.swap(false, Ordering::AcqRel)
            && let Some(callback) = self.frame_callback
        {
            callback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cell(c: char) -> CellData {
        let mut cell = CellData::default();
        cell.set_char(c);
        cell
    }

    #[test]
    fn test_new_backend() {
        let backend = NativeBackend::new(80, 24);
        assert_eq!(backend.size(), Size::new(80, 24));
        assert!(backend.is_cursor_visible());
    }

    #[test]
    fn test_resize() {
        let mut backend = NativeBackend::new(80, 24);
        backend.resize(120, 40);
        assert_eq!(backend.size(), Size::new(120, 40));
    }

    #[test]
    fn test_cursor_operations() {
        let mut backend = NativeBackend::new(80, 24);
        backend.set_cursor_position(Position::new(10, 5));
        assert_eq!(backend.cursor_position(), Position::new(10, 5));
        backend.set_cursor_visible(false);
        assert!(!backend.is_cursor_visible());
        backend.set_cursor_visible(true);
        assert!(backend.is_cursor_visible());
    }

    #[test]
    fn test_set_cell_and_read() {
        let mut backend = NativeBackend::new(80, 24);
        backend.set_cell(5, 3, make_cell('A'));
        assert_eq!(backend.cell(5, 3).symbol_str(), "A");
    }

    #[test]
    fn test_clear() {
        let mut backend = NativeBackend::new(80, 24);
        backend.set_cell(0, 0, make_cell('X'));
        backend.clear();
        assert_eq!(backend.cell(0, 0).symbol_str(), " ");
    }

    #[test]
    fn test_set_cell_out_of_bounds_ignored() {
        let mut backend = NativeBackend::new(10, 10);
        backend.set_cell(20, 20, make_cell('Z'));
    }

    #[test]
    fn test_chrome_line() {
        let mut backend = NativeBackend::new(80, 24);
        backend.write_chrome_line(0, "Hello", 0xFF0000FF, 0, 0);
        assert_eq!(backend.cell(0, 0).symbol_str(), "H");
        assert_eq!(backend.cell(4, 0).symbol_str(), "o");
        assert_eq!(backend.cell(0, 0).fg, 0xFF0000FF);
    }

    #[test]
    fn test_wide_flag_via_cell_flags() {
        let mut backend = NativeBackend::new(80, 24);
        assert!(!backend.is_wide_char(5, 3));

        backend.set_wide_flag(5, 3, true);
        assert!(backend.is_wide_char(5, 3));
        // CellData.flags の WIDE bit が立っている
        assert!(backend.cell(5, 3).flags & flags::WIDE != 0);

        backend.set_wide_flag(5, 3, false);
        assert!(!backend.is_wide_char(5, 3));
    }

    #[test]
    fn test_wide_flag_default_false() {
        let backend = NativeBackend::new(10, 5);
        for y in 0..5 {
            for x in 0..10 {
                assert!(!backend.is_wide_char(x, y));
            }
        }
    }

    #[test]
    fn test_wide_flag_out_of_bounds_safe() {
        let mut backend = NativeBackend::new(10, 5);
        assert!(!backend.is_wide_char(10, 0));
        assert!(!backend.is_wide_char(0, 5));
        backend.set_wide_flag(10, 0, true);
        backend.set_wide_flag(u16::MAX, u16::MAX, true);
    }

    #[test]
    fn test_clear_wide_flags_preserves_other_flags() {
        let mut backend = NativeBackend::new(10, 5);
        // BOLD + WIDE を立てる
        let mut cell = make_cell('A');
        cell.flags = flags::BOLD;
        backend.set_cell(0, 0, cell);
        backend.set_wide_flag(0, 0, true);

        // WIDE だけ消える、BOLD は残る
        backend.clear_wide_flags();
        assert!(!backend.is_wide_char(0, 0));
        assert!(backend.cell(0, 0).flags & flags::BOLD != 0);
    }
}
