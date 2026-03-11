//! ターミナル状態管理
//!
//! alacritty_terminal::Term をラップし、
//! VTバイトストリームからグリッド状態を管理する。

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags as CellFlags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte;
use vte::ansi::{Color, NamedColor, Rgb};

/// ターミナルのセルスナップショット（レンダリング用）
#[derive(Debug, Clone)]
pub struct CellSnapshot {
    /// 文字
    pub ch: char,
    /// 前景色 (R, G, B)
    pub fg: (u8, u8, u8),
    /// 背景色 (R, G, B)
    pub bg: (u8, u8, u8),
    /// 太字
    pub bold: bool,
    /// 斜体
    pub italic: bool,
    /// 下線
    pub underline: bool,
    /// ワイドキャラクター（CJK等、2セル幅の先頭）
    pub wide: bool,
    /// ワイドキャラクターのスペーサー（2セル目）
    pub wide_spacer: bool,
}

/// グリッド全体のスナップショット
pub struct GridSnapshot {
    pub cells: Vec<Vec<CellSnapshot>>,
    pub cols: usize,
    pub lines: usize,
    /// カーソル位置（行, 列）
    pub cursor: (usize, usize),
    /// カーソル可視状態（DECTCEM）
    pub cursor_visible: bool,
}

/// ターミナル状態（alacritty_terminal ラッパー）
pub struct TerminalState {
    term: Term<EventProxy>,
    parser: vte::ansi::Processor,
    cols: usize,
    lines: usize,
}

/// alacritty_terminal のイベントリスナー（空実装）
struct EventProxy;

impl EventListener for EventProxy {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {
        // イベントは無視（将来的にベル音やタイトル変更に使用可能）
    }
}

/// スクロールバック履歴行数
const SCROLLBACK_LINES: usize = 10_000;

/// Dimensions トレイト実装
struct TermDimensions {
    cols: usize,
    lines: usize,
}

impl Dimensions for TermDimensions {
    fn columns(&self) -> usize {
        self.cols
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn total_lines(&self) -> usize {
        self.lines + SCROLLBACK_LINES
    }
}

/// Arctic Nordic Ocean カラーパレット
/// NamedColor → RGB 変換テーブル
mod arctic_colors {
    use super::NamedColor;

    /// 標準16色の Arctic テーマ値
    #[allow(non_snake_case)]
    pub fn named_to_rgb(color: &NamedColor) -> (u8, u8, u8) {
        match color {
            // ノーマル
            NamedColor::Black => (11, 17, 32),    // #0B1120 — 深海
            NamedColor::Red => (191, 97, 106),    // #BF616A — Nord Red
            NamedColor::Green => (163, 190, 140), // #A3BE8C — Nord Green
            NamedColor::Yellow => (235, 203, 139), // #EBCB8B — Nord Yellow
            NamedColor::Blue => (129, 161, 193),  // #81A1C1 — Nord Blue
            NamedColor::Magenta => (180, 142, 173), // #B48EAD — Nord Purple
            NamedColor::Cyan => (136, 192, 208),  // #88C0D0 — Arctic Cyan
            NamedColor::White => (216, 222, 233), // #D8DEE9 — Snow

            // ブライト
            NamedColor::BrightBlack => (76, 86, 106), // #4C566A — Polar Night
            NamedColor::BrightRed => (208, 115, 125),
            NamedColor::BrightGreen => (183, 210, 160),
            NamedColor::BrightYellow => (245, 224, 169),
            NamedColor::BrightBlue => (155, 185, 213),
            NamedColor::BrightMagenta => (200, 167, 193),
            NamedColor::BrightCyan => (163, 214, 226),
            NamedColor::BrightWhite => (236, 239, 244), // #ECEFF4 — Snow White

            // Dim
            NamedColor::DimBlack => (7, 12, 22),
            NamedColor::DimRed => (140, 70, 77),
            NamedColor::DimGreen => (120, 140, 103),
            NamedColor::DimYellow => (172, 148, 101),
            NamedColor::DimBlue => (94, 118, 141),
            NamedColor::DimMagenta => (132, 104, 127),
            NamedColor::DimCyan => (100, 141, 152),
            NamedColor::DimWhite => (158, 163, 170),

            // システム色
            NamedColor::Foreground => (216, 222, 233),
            NamedColor::Background => (11, 17, 32),
            NamedColor::Cursor => (136, 192, 208),
            NamedColor::BrightForeground => (236, 239, 244),
            NamedColor::DimForeground => (158, 163, 170),
        }
    }
}

/// 256色パレットのRGB変換
fn indexed_to_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => {
            let named = match index {
                0 => NamedColor::Black,
                1 => NamedColor::Red,
                2 => NamedColor::Green,
                3 => NamedColor::Yellow,
                4 => NamedColor::Blue,
                5 => NamedColor::Magenta,
                6 => NamedColor::Cyan,
                7 => NamedColor::White,
                8 => NamedColor::BrightBlack,
                9 => NamedColor::BrightRed,
                10 => NamedColor::BrightGreen,
                11 => NamedColor::BrightYellow,
                12 => NamedColor::BrightBlue,
                13 => NamedColor::BrightMagenta,
                14 => NamedColor::BrightCyan,
                15 => NamedColor::BrightWhite,
                _ => unreachable!(),
            };
            arctic_colors::named_to_rgb(&named)
        }
        16..=231 => {
            // 6x6x6 カラーキューブ
            let idx = index - 16;
            let r = (idx / 36) * 51;
            let g = ((idx % 36) / 6) * 51;
            let b = (idx % 6) * 51;
            (r, g, b)
        }
        232..=255 => {
            // グレースケール 24段階
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    }
}

impl TerminalState {
    /// 新しいターミナル状態を作成
    pub fn new(cols: usize, lines: usize) -> Self {
        let config = TermConfig::default();
        let dims = TermDimensions { cols, lines };
        let term = Term::new(config, &dims, EventProxy);
        let parser = vte::ansi::Processor::new();

        Self {
            term,
            parser,
            cols,
            lines,
        }
    }

    /// VTバイトストリームを処理
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// 現在のグリッド状態をスナップショットとして取得
    ///
    /// `display_offset` を反映し、スクロールバック領域の行も正しく読み取る。
    /// alacritty_terminal の Grid では:
    /// - `Line(0)` ~ `Line(N-1)` = 現在の表示領域（display area）
    /// - `Line(-1)`, `Line(-2)`, ... = スクロールバック（履歴）領域
    /// - `display_offset` が N のとき、表示行 i は `Line(i - N)` に対応
    pub fn snapshot(&self) -> GridSnapshot {
        let grid = self.term.grid();
        let display_offset = grid.display_offset();
        let mut cells = Vec::with_capacity(self.lines);

        for line_idx in 0..self.lines {
            // display_offset > 0 のとき、表示開始行を scrollback 領域にシフト
            let line = Line(line_idx as i32 - display_offset as i32);
            let mut row = Vec::with_capacity(self.cols);

            for col_idx in 0..self.cols {
                let cell = &grid[line][Column(col_idx)];

                let mut fg = resolve_color(&cell.fg);
                let mut bg = resolve_color(&cell.bg);

                // SGR 7 (INVERSE) — fg と bg を入れ替え
                // Claude CLI 等はカーソル位置のセルに INVERSE を使う
                if cell.flags.contains(CellFlags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }

                row.push(CellSnapshot {
                    ch: cell.c,
                    fg,
                    bg,
                    bold: cell.flags.contains(CellFlags::BOLD),
                    italic: cell.flags.contains(CellFlags::ITALIC),
                    underline: cell.flags.contains(CellFlags::UNDERLINE),
                    wide: cell.flags.contains(CellFlags::WIDE_CHAR),
                    wide_spacer: cell.flags.contains(CellFlags::WIDE_CHAR_SPACER),
                });
            }

            cells.push(row);
        }

        // カーソル位置
        let cursor_point = grid.cursor.point;
        let cursor_row = cursor_point.line.0 as usize;
        let cursor_col = cursor_point.column.0;

        // スクロール中はカーソルが表示領域外にあるため非表示にする
        let cursor_visible = if display_offset > 0 {
            false
        } else {
            self.cursor_visible()
        };

        GridSnapshot {
            cells,
            cols: self.cols,
            lines: self.lines,
            cursor: (cursor_row, cursor_col),
            cursor_visible,
        }
    }

    /// リサイズ
    pub fn resize(&mut self, cols: usize, lines: usize) {
        let dims = TermDimensions { cols, lines };
        self.cols = cols;
        self.lines = lines;
        self.term.resize(dims);
    }

    /// Application Cursor Keys モード（DECCKM）
    ///
    /// TUIアプリ（cc, vim等）がこのモードを有効にすると、
    /// 矢印キーのエスケープシーケンスが `\x1b[A` → `\x1bOA` に変わる。
    pub fn app_cursor_mode(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    /// Bracketed Paste モード
    ///
    /// 有効時、ペースト内容を `\x1b[200~` ... `\x1b[201~` で囲んで送信する。
    /// これによりTUIアプリがペーストとキー入力を区別できる。
    pub fn bracketed_paste_mode(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// カーソル可視状態（DECTCEM: DEC Text Cursor Enable Mode）
    ///
    /// TUIアプリが `ESC[?25l` でカーソルを隠し、`ESC[?25h` で再表示する。
    pub fn cursor_visible(&self) -> bool {
        use alacritty_terminal::term::TermMode;
        self.term.mode().contains(TermMode::SHOW_CURSOR)
    }

    /// スクロール表示位置を変更
    pub fn scroll_display(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    /// 現在のスクロールオフセット（0 = 最下部）
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// カラム数
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// 行数
    pub fn lines(&self) -> usize {
        self.lines
    }
}

/// Color enum → RGB
fn resolve_color(color: &Color) -> (u8, u8, u8) {
    match color {
        Color::Named(named) => arctic_colors::named_to_rgb(named),
        Color::Spec(Rgb { r, g, b }) => (*r, *g, *b),
        Color::Indexed(idx) => indexed_to_rgb(*idx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_state_new() {
        let state = TerminalState::new(80, 24);
        assert_eq!(state.cols(), 80);
        assert_eq!(state.lines(), 24);
    }

    #[test]
    fn test_feed_plain_text() {
        let mut state = TerminalState::new(80, 24);
        state.feed_bytes(b"Hello, World!");

        let snap = state.snapshot();
        assert_eq!(snap.cells[0][0].ch, 'H');
        assert_eq!(snap.cells[0][1].ch, 'e');
        assert_eq!(snap.cells[0][4].ch, 'o');
    }

    #[test]
    fn test_feed_ansi_colors() {
        let mut state = TerminalState::new(80, 24);
        // ESC[31m = 前景色を赤に
        state.feed_bytes(b"\x1b[31mRed");

        let snap = state.snapshot();
        assert_eq!(snap.cells[0][0].ch, 'R');
        // Arctic Red: (191, 97, 106)
        assert_eq!(snap.cells[0][0].fg, (191, 97, 106));
    }

    #[test]
    fn test_feed_bold() {
        let mut state = TerminalState::new(80, 24);
        // ESC[1m = 太字
        state.feed_bytes(b"\x1b[1mBold");

        let snap = state.snapshot();
        assert!(snap.cells[0][0].bold);
    }

    #[test]
    fn test_resize() {
        let mut state = TerminalState::new(80, 24);
        state.resize(120, 40);
        assert_eq!(state.cols(), 120);
        assert_eq!(state.lines(), 40);
    }

    #[test]
    fn test_snapshot_default_colors() {
        let state = TerminalState::new(80, 24);
        let snap = state.snapshot();
        let default_fg = arctic_colors::named_to_rgb(&NamedColor::Foreground);
        assert_eq!(snap.cells[0][0].fg, default_fg);
    }

    #[test]
    fn test_snapshot_cursor_visible() {
        let state = TerminalState::new(80, 24);
        let snap = state.snapshot();
        // デフォルトはカーソル表示
        assert!(snap.cursor_visible);
    }

    #[test]
    fn test_cursor_hidden_by_dectcem() {
        let mut state = TerminalState::new(80, 24);
        // ESC[?25l = カーソル非表示（DECTCEM off）
        state.feed_bytes(b"\x1b[?25l");
        let snap = state.snapshot();
        assert!(!snap.cursor_visible);

        // ESC[?25h = カーソル再表示
        state.feed_bytes(b"\x1b[?25h");
        let snap = state.snapshot();
        assert!(snap.cursor_visible);
    }

    #[test]
    fn test_arctic_color_palette() {
        // 深海の底
        assert_eq!(
            arctic_colors::named_to_rgb(&NamedColor::Black),
            (11, 17, 32)
        );
        // Arctic Cyan
        assert_eq!(
            arctic_colors::named_to_rgb(&NamedColor::Cyan),
            (136, 192, 208)
        );
        // Snow White
        assert_eq!(
            arctic_colors::named_to_rgb(&NamedColor::BrightWhite),
            (236, 239, 244)
        );
    }
}
