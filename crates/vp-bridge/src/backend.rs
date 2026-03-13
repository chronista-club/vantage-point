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
        if self.dirty.swap(false, Ordering::AcqRel) {
            if let Some(callback) = self.frame_callback {
                callback();
            }
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
}
