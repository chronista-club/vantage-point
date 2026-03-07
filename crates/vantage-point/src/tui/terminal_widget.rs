//! GridSnapshot → ratatui Widget 変換
//!
//! TerminalState の VT パース結果を ratatui の Buffer に描画する。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use crate::terminal::state::GridSnapshot;

/// PTY 出力を ratatui Widget として描画するウィジェット
pub struct TerminalView<'a> {
    snapshot: &'a GridSnapshot,
}

impl<'a> TerminalView<'a> {
    pub fn new(snapshot: &'a GridSnapshot) -> Self {
        Self { snapshot }
    }
}

impl Widget for TerminalView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let snap = self.snapshot;

        for row in 0..area.height.min(snap.lines as u16) {
            let line_idx = row as usize;
            if line_idx >= snap.cells.len() {
                break;
            }
            let line = &snap.cells[line_idx];

            for col in 0..area.width.min(snap.cols as u16) {
                let col_idx = col as usize;
                if col_idx >= line.len() {
                    break;
                }
                let cell = &line[col_idx];

                // ワイドキャラクターのスペーサーはスキップ（先頭セルが2幅で描画済み）
                if cell.wide_spacer {
                    continue;
                }

                let fg = Color::Rgb(cell.fg.0, cell.fg.1, cell.fg.2);
                let bg = Color::Rgb(cell.bg.0, cell.bg.1, cell.bg.2);

                let mut modifier = Modifier::empty();
                if cell.bold {
                    modifier |= Modifier::BOLD;
                }
                if cell.italic {
                    modifier |= Modifier::ITALIC;
                }
                if cell.underline {
                    modifier |= Modifier::UNDERLINED;
                }

                let style = Style::default().fg(fg).bg(bg).add_modifier(modifier);

                let x = area.x + col;
                let y = area.y + row;

                if x < area.right() && y < area.bottom() {
                    let buf_cell = &mut buf[(x, y)];
                    buf_cell.set_char(cell.ch);
                    buf_cell.set_style(style);
                }
            }
        }

        // カーソル描画（反転色ブロック）
        // Claude CLI 等の TUI アプリは DECTCEM でカーソルを非表示にするが、
        // PTY パススルーでは常にカーソル位置を可視化する
        let (crow, ccol) = snap.cursor;
        let cx = area.x + ccol as u16;
        let cy = area.y + crow as u16;
        if cx < area.right() && cy < area.bottom() {
            let buf_cell = &mut buf[(cx, cy)];
            let current_fg = buf_cell.fg;
            let current_bg = buf_cell.bg;
            buf_cell.set_fg(current_bg);
            buf_cell.set_bg(current_fg);
        }
    }
}
