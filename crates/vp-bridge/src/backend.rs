//! NativeBackend — ratatui Backend trait 実装
//!
//! 描画結果を内部バッファ (Buffer) に蓄積し、FFI 経由で Swift/NSView に公開する。
//! ターミナルエミュレータに依存せず、Cell グリッドをメモリ上に保持するのみ。

use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};

/// フレーム更新通知コールバック型
///
/// Swift 側が登録し、`draw()` + `flush()` 完了時に呼ばれる。
/// NSView.setNeedsDisplay() を呼び出すトリガーとして使用。
pub type FrameReadyCallback = extern "C" fn();

/// ratatui の Cell グリッドを内部バッファに蓄積する Backend
///
/// ターミナルを一切使わず、描画結果をメモリ上の Buffer として保持する。
/// Swift 側は FFI 関数 (`vp_bridge_get_cell` 等) でセルデータを読み取り、
/// Core Text や Metal で描画する。
pub struct NativeBackend {
    /// グリッドサイズ（列数）
    width: u16,
    /// グリッドサイズ（行数）
    height: u16,
    /// ratatui の Cell バッファ（全セルの内容を保持）
    buffer: Buffer,
    /// ワイドキャラクターフラグ（VT パーサー由来、Buffer と同サイズ）
    /// ratatui Cell にはワイド情報がないため、別途保持して FFI に伝搬する
    wide_flags: Vec<bool>,
    /// カーソル位置
    cursor: Position,
    /// カーソルの可視状態
    cursor_visible: bool,
    /// フレーム更新通知コールバック（Swift 側が登録）
    frame_callback: Option<FrameReadyCallback>,
    /// ダーティフラグ（draw 後〜flush 前を追跡）
    dirty: AtomicBool,
}

impl NativeBackend {
    /// 指定サイズで新規 Backend を作成
    pub fn new(width: u16, height: u16) -> Self {
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        Self {
            width,
            height,
            wide_flags: vec![false; (width as usize) * (height as usize)],
            buffer: Buffer::empty(area),
            cursor: Position::new(0, 0),
            cursor_visible: true,
            frame_callback: None,
            dirty: AtomicBool::new(false),
        }
    }

    /// フレーム更新コールバックを登録
    pub fn set_frame_callback(&mut self, callback: FrameReadyCallback) {
        self.frame_callback = Some(callback);
    }

    /// バッファへの直接参照（FFI 読み取り用）
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// グリッドサイズを変更（Swift 側のリサイズに対応）
    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        self.buffer = Buffer::empty(area);
        self.wide_flags = vec![false; (width as usize) * (height as usize)];
    }

    /// ワイドキャラクターフラグを設定
    pub fn set_wide_flag(&mut self, x: u16, y: u16, wide: bool) {
        if x < self.width && y < self.height {
            self.wide_flags[(y as usize) * (self.width as usize) + (x as usize)] = wide;
        }
    }

    /// ワイドキャラクターフラグを取得
    pub fn is_wide_char(&self, x: u16, y: u16) -> bool {
        if x < self.width && y < self.height {
            self.wide_flags[(y as usize) * (self.width as usize) + (x as usize)]
        } else {
            false
        }
    }

    /// ワイドフラグをクリア（sync_to_backend の先頭で呼ぶ）
    pub fn clear_wide_flags(&mut self) {
        self.wide_flags.fill(false);
    }

    /// カーソル可視状態を取得
    pub fn is_cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// カーソル位置を取得（&self — FFI 読み取り用）
    pub fn cursor_position(&self) -> Position {
        self.cursor
    }
}

impl Backend for NativeBackend {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (x, y, cell) in content {
            if x < self.width && y < self.height {
                self.buffer[(x, y)] = cell.clone();
            }
        }
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_visible = false;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.cursor_visible = true;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.cursor = position.into();
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.buffer.reset();
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        match clear_type {
            ClearType::All => self.clear(),
            // 部分クリアは NSView では不要 — 全体再描画で対応
            _ => Ok(()),
        }
    }

    fn size(&self) -> Result<Size, Self::Error> {
        Ok(Size::new(self.width, self.height))
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        // ピクセルサイズは Swift 側が管理するため 0 を返す
        Ok(WindowSize {
            columns_rows: Size::new(self.width, self.height),
            pixels: Size::new(0, 0),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        if self.dirty.swap(false, Ordering::AcqRel)
            && let Some(callback) = self.frame_callback
        {
            callback();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_backend() {
        let backend = NativeBackend::new(80, 24);
        assert_eq!(backend.size().unwrap(), Size::new(80, 24));
        assert!(backend.is_cursor_visible());
    }

    #[test]
    fn test_resize() {
        let mut backend = NativeBackend::new(80, 24);
        backend.resize(120, 40);
        assert_eq!(backend.size().unwrap(), Size::new(120, 40));
    }

    #[test]
    fn test_cursor_operations() {
        let mut backend = NativeBackend::new(80, 24);

        backend.set_cursor_position(Position::new(10, 5)).unwrap();
        assert_eq!(backend.get_cursor_position().unwrap(), Position::new(10, 5));

        backend.hide_cursor().unwrap();
        assert!(!backend.is_cursor_visible());

        backend.show_cursor().unwrap();
        assert!(backend.is_cursor_visible());
    }

    #[test]
    fn test_draw_and_read() {
        let mut backend = NativeBackend::new(80, 24);

        let mut cell = Cell::default();
        cell.set_char('A');

        let content = vec![(5u16, 3u16, &cell)];
        backend.draw(content.into_iter()).unwrap();

        assert_eq!(backend.buffer()[(5, 3)].symbol(), "A");
    }

    #[test]
    fn test_clear() {
        let mut backend = NativeBackend::new(80, 24);

        let mut cell = Cell::default();
        cell.set_char('X');
        backend.draw(vec![(0u16, 0u16, &cell)].into_iter()).unwrap();
        assert_eq!(backend.buffer()[(0, 0)].symbol(), "X");

        backend.clear().unwrap();
        assert_eq!(backend.buffer()[(0, 0)].symbol(), " ");
    }

    #[test]
    fn test_draw_out_of_bounds_ignored() {
        let mut backend = NativeBackend::new(10, 10);

        let mut cell = Cell::default();
        cell.set_char('Z');

        // 範囲外の座標は無視される
        backend
            .draw(vec![(20u16, 20u16, &cell)].into_iter())
            .unwrap();
        // パニックしないことを確認
    }

    #[test]
    fn test_window_size() {
        let mut backend = NativeBackend::new(80, 24);
        let ws = backend.window_size().unwrap();
        assert_eq!(ws.columns_rows, Size::new(80, 24));
        assert_eq!(ws.pixels, Size::new(0, 0));
    }

    // =========================================================================
    // wide_flags テスト
    // =========================================================================

    #[test]
    fn test_wide_flag_set_and_get() {
        let mut backend = NativeBackend::new(80, 24);
        assert!(!backend.is_wide_char(5, 3));

        backend.set_wide_flag(5, 3, true);
        assert!(backend.is_wide_char(5, 3));

        backend.set_wide_flag(5, 3, false);
        assert!(!backend.is_wide_char(5, 3));
    }

    #[test]
    fn test_wide_flag_default_false() {
        let backend = NativeBackend::new(80, 24);
        // 全セルがデフォルトで false
        for y in 0..24 {
            for x in 0..80 {
                assert!(!backend.is_wide_char(x, y));
            }
        }
    }

    #[test]
    fn test_wide_flag_boundary_values() {
        let mut backend = NativeBackend::new(10, 5);

        // 境界値（width-1, height-1）で正常動作
        backend.set_wide_flag(9, 4, true);
        assert!(backend.is_wide_char(9, 4));

        // 原点でも正常
        backend.set_wide_flag(0, 0, true);
        assert!(backend.is_wide_char(0, 0));
    }

    #[test]
    fn test_wide_flag_out_of_bounds_safe() {
        let mut backend = NativeBackend::new(10, 5);

        // 範囲外は false を返し、パニックしない
        assert!(!backend.is_wide_char(10, 0)); // x == width
        assert!(!backend.is_wide_char(0, 5)); // y == height
        assert!(!backend.is_wide_char(10, 5)); // 両方境界外
        assert!(!backend.is_wide_char(u16::MAX, u16::MAX)); // 極端な値

        // 範囲外への set も無視（パニックしない）
        backend.set_wide_flag(10, 0, true);
        backend.set_wide_flag(0, 5, true);
        backend.set_wide_flag(u16::MAX, u16::MAX, true);
    }

    #[test]
    fn test_clear_wide_flags() {
        let mut backend = NativeBackend::new(10, 5);

        // いくつかフラグを立てる
        backend.set_wide_flag(0, 0, true);
        backend.set_wide_flag(5, 2, true);
        backend.set_wide_flag(9, 4, true);
        assert!(backend.is_wide_char(0, 0));
        assert!(backend.is_wide_char(5, 2));

        // クリア後は全て false
        backend.clear_wide_flags();
        assert!(!backend.is_wide_char(0, 0));
        assert!(!backend.is_wide_char(5, 2));
        assert!(!backend.is_wide_char(9, 4));
    }

    #[test]
    fn test_clear_wide_flags_zero_size() {
        // 0x0 グリッドでもパニックしない
        let mut backend = NativeBackend::new(0, 0);
        backend.clear_wide_flags();
    }

    #[test]
    fn test_wide_flags_after_resize() {
        let mut backend = NativeBackend::new(10, 5);
        backend.set_wide_flag(5, 3, true);
        assert!(backend.is_wide_char(5, 3));

        // リサイズ後はフラグがリセットされる
        backend.resize(20, 10);
        assert!(!backend.is_wide_char(5, 3));

        // 新サイズの境界値で正常動作
        backend.set_wide_flag(19, 9, true);
        assert!(backend.is_wide_char(19, 9));

        // 縮小リサイズ
        backend.resize(5, 3);
        assert!(!backend.is_wide_char(19, 9)); // 旧座標は範囲外
        assert!(!backend.is_wide_char(4, 2)); // 新境界内はデフォルト false
    }

    #[test]
    fn test_wide_flags_independent_of_cell_content() {
        let mut backend = NativeBackend::new(80, 24);

        // セル内容を書き込み
        let mut cell = Cell::default();
        cell.set_char('漢');
        backend.draw(vec![(5u16, 3u16, &cell)].into_iter()).unwrap();

        // wide_flags はセル内容と独立（明示的に設定しないと false）
        assert!(!backend.is_wide_char(5, 3));

        // 明示的に設定
        backend.set_wide_flag(5, 3, true);
        assert!(backend.is_wide_char(5, 3));

        // clear() はセル内容をリセットするが wide_flags には影響しない
        backend.clear().unwrap();
        assert_eq!(backend.buffer()[(5, 3)].symbol(), " ");
        assert!(backend.is_wide_char(5, 3)); // wide_flags は残る
    }

    #[test]
    fn test_wide_flag_multiple_positions() {
        let mut backend = NativeBackend::new(80, 24);

        // 複数位置に設定しても干渉しない
        let positions = [(0, 0), (10, 5), (79, 23), (40, 12)];
        for &(x, y) in &positions {
            backend.set_wide_flag(x, y, true);
        }
        for &(x, y) in &positions {
            assert!(backend.is_wide_char(x, y), "({}, {}) should be wide", x, y);
        }

        // 設定していない位置は false
        assert!(!backend.is_wide_char(1, 0));
        assert!(!backend.is_wide_char(10, 6));
    }
}
