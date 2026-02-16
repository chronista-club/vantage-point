//! CoreText ネイティブ端末レンダラー
//!
//! NSView サブクラスで alacritty_terminal のグリッドをレンダリングする。
//! Core Graphics (CGContext) で背景矩形を描画し、
//! NSString::drawAtPoint で文字を描画する。
//!
//! ## パイプライン
//! ```text
//! GridSnapshot → TerminalView (NSView) → CGContext 描画
//! ```

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSGraphicsContext,
    NSStringDrawing, NSView,
};
// MainThreadOnly トレイト（alloc に必要）
use objc2::MainThreadOnly as _;
use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSAttributedStringKey, NSDictionary, NSRect, NSString, ns_string};

use super::state::CellSnapshot;

/// レンダリング用セルデータ（f64 RGB）
#[derive(Clone, Copy)]
struct RenderCell {
    ch: char,
    fg: (CGFloat, CGFloat, CGFloat),
    bg: (CGFloat, CGFloat, CGFloat),
    bold: bool,
    /// ワイドキャラクター（2セル幅の先頭）
    wide: bool,
    /// ワイドキャラクターのスペーサー（2セル目、描画スキップ）
    wide_spacer: bool,
}

impl Default for RenderCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: (0.847, 0.871, 0.914), // Arctic Foreground #D8DEE9
            bg: (0.043, 0.067, 0.125), // Arctic Background #0B1120
            bold: false,
            wide: false,
            wide_spacer: false,
        }
    }
}

impl From<&CellSnapshot> for RenderCell {
    fn from(cell: &CellSnapshot) -> Self {
        Self {
            ch: cell.ch,
            fg: (
                cell.fg.0 as CGFloat / 255.0,
                cell.fg.1 as CGFloat / 255.0,
                cell.fg.2 as CGFloat / 255.0,
            ),
            bg: (
                cell.bg.0 as CGFloat / 255.0,
                cell.bg.1 as CGFloat / 255.0,
                cell.bg.2 as CGFloat / 255.0,
            ),
            bold: cell.bold,
            wide: cell.wide,
            wide_spacer: cell.wide_spacer,
        }
    }
}

/// NSView のインスタンス変数
pub struct TerminalViewIvars {
    cols: Cell<usize>,
    rows: Cell<usize>,
    cell_width: Cell<CGFloat>,
    cell_height: Cell<CGFloat>,
    cells: RefCell<Vec<RenderCell>>,
    font: RefCell<Option<Retained<NSFont>>>,
    bold_font: RefCell<Option<Retained<NSFont>>>,
}

// SAFETY:
// - TerminalView は NSView > NSResponder > NSObject を継承
// - NSView は MainThreadOnly — TerminalView も MainThreadOnly
// - drawRect: と isFlipped のオーバーライドは安全
define_class!(
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = objc2::MainThreadOnly]
    #[name = "VPTerminalView"]
    #[ivars = TerminalViewIvars]
    pub struct TerminalView;

    impl TerminalView {
        /// drawRect: — macOS が再描画時に呼び出す
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let Some(gfx_ctx) = NSGraphicsContext::currentContext() else {
                return;
            };
            let ctx = gfx_ctx.CGContext();

            let ivars = self.ivars();
            let cols = ivars.cols.get();
            let rows = ivars.rows.get();
            let cw = ivars.cell_width.get();
            let ch = ivars.cell_height.get();
            let cells = ivars.cells.borrow();
            let font_ref = ivars.font.borrow();
            let bold_font_ref = ivars.bold_font.borrow();

            // フォント取得（キャッシュ済み）
            let default_font = font_ref.as_ref();
            let bold_font = bold_font_ref.as_ref();

            for row in 0..rows {
                for col in 0..cols {
                    let idx = row * cols + col;
                    if idx >= cells.len() {
                        continue;
                    }
                    let cell = &cells[idx];

                    // スペーサーセルは全スキップ
                    // （ワイド文字が2セル分の背景を描画済み）
                    if cell.wide_spacer {
                        continue;
                    }

                    // セル座標（isFlipped=true なので左上原点）
                    let x = col as CGFloat * cw;
                    let y = row as CGFloat * ch;

                    // ワイドキャラクターは2セル幅、それ以外は1セル幅
                    let cell_span = if cell.wide { 2.0 } else { 1.0 };
                    let rect =
                        CGRect::new(CGPoint::new(x, y), CGSize::new(cw * cell_span, ch));

                    // 背景矩形
                    CGContext::set_rgb_fill_color(
                        Some(&ctx),
                        cell.bg.0,
                        cell.bg.1,
                        cell.bg.2,
                        1.0,
                    );
                    CGContext::fill_rect(Some(&ctx), rect);

                    // 文字描画（空白・NULL以外）
                    if cell.ch != ' ' && cell.ch != '\0' {
                        let fg_color = NSColor::colorWithSRGBRed_green_blue_alpha(
                            cell.fg.0, cell.fg.1, cell.fg.2, 1.0,
                        );

                        // 太字フォント選択
                        let draw_font = if cell.bold {
                            bold_font.unwrap_or_else(|| default_font.unwrap())
                        } else {
                            default_font.unwrap()
                        };

                        // 属性辞書
                        // SAFETY: extern statics のアクセス + NSDictionary構築
                        let attrs = unsafe {
                            let keys: &[&NSAttributedStringKey] =
                                &[NSFontAttributeName, NSForegroundColorAttributeName];
                            let vals: &[&AnyObject] = &[
                                draw_font.as_ref(),
                                &*(fg_color.as_ref() as *const NSColor as *const AnyObject),
                            ];
                            NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(
                                keys, vals,
                            )
                        };

                        // 文字をNSStringに変換
                        let mut buf = [0u8; 4];
                        let s = cell.ch.encode_utf8(&mut buf);
                        let ns_str = NSString::from_str(s);

                        // テキスト描画位置（セル内で垂直方向にセンタリング）
                        let text_point = CGPoint::new(x, y + LINE_PADDING / 2.0);
                        unsafe {
                            ns_str.drawAtPoint_withAttributes(text_point, Some(&attrs));
                        }
                    }
                }
            }
        }

        /// 座標系を左上原点にする（ターミナル描画に最適）
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// デフォルトのフォントサイズ
const DEFAULT_FONT_SIZE: CGFloat = 14.0;

/// 行間の余白ピクセル数
const LINE_PADDING: CGFloat = 4.0;

/// NSFont メトリクスからセルサイズを計算
///
/// cell_width: maximumAdvancement の幅（モノスペースフォントの文字幅）
/// cell_height: ascender + |descender| + leading + LINE_PADDING
fn measure_cell_size(font: &NSFont) -> (CGFloat, CGFloat) {
    let adv = font.maximumAdvancement();
    let cell_width = adv.width;

    let ascent = font.ascender();
    let descent = font.descender(); // 負の値
    let leading = font.leading();
    let cell_height = (ascent - descent + leading + LINE_PADDING).ceil();

    (cell_width, cell_height)
}

impl TerminalView {
    /// 新しい TerminalView を生成
    pub fn new(mtm: MainThreadMarker, frame: NSRect, cols: usize, rows: usize) -> Retained<Self> {
        let font_size = DEFAULT_FONT_SIZE;
        let cells = vec![RenderCell::default(); cols * rows];

        // モノスペースフォント取得
        let font = NSFont::fontWithName_size(ns_string!("Menlo"), font_size)
            .or_else(|| NSFont::userFixedPitchFontOfSize(font_size));
        let bold_font = NSFont::fontWithName_size(ns_string!("Menlo-Bold"), font_size);

        // フォントメトリクスからセルサイズを計算
        let (cell_width, cell_height) = font
            .as_ref()
            .map(|f| measure_cell_size(f))
            .unwrap_or((8.0, 18.0));

        let this = Self::alloc(mtm).set_ivars(TerminalViewIvars {
            cols: Cell::new(cols),
            rows: Cell::new(rows),
            cell_width: Cell::new(cell_width),
            cell_height: Cell::new(cell_height),
            cells: RefCell::new(cells),
            font: RefCell::new(font),
            bold_font: RefCell::new(bold_font),
        });

        // NSView の initWithFrame: を呼ぶ
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// GridSnapshot のセルデータを反映
    pub fn update_cells(&self, grid_cells: &[Vec<CellSnapshot>]) {
        let ivars = self.ivars();
        let cols = ivars.cols.get();
        let mut cells = ivars.cells.borrow_mut();

        for (row_idx, row) in grid_cells.iter().enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let idx = row_idx * cols + col_idx;
                if idx < cells.len() {
                    cells[idx] = RenderCell::from(cell);
                }
            }
        }
    }

    /// グリッドサイズ変更
    pub fn resize_grid(&self, cols: usize, rows: usize) {
        let ivars = self.ivars();
        ivars.cols.set(cols);
        ivars.rows.set(rows);
        let mut cells = ivars.cells.borrow_mut();
        cells.resize(cols * rows, RenderCell::default());
    }

    /// 再描画要求
    pub fn request_redraw(&self) {
        self.setNeedsDisplay(true);
    }

    /// フォントサイズ変更
    pub fn set_font_size(&self, size: CGFloat) {
        let ivars = self.ivars();
        let font = NSFont::fontWithName_size(ns_string!("Menlo"), size)
            .or_else(|| NSFont::userFixedPitchFontOfSize(size));
        let bold_font = NSFont::fontWithName_size(ns_string!("Menlo-Bold"), size);

        // フォントメトリクスからセルサイズを再計算
        if let Some(f) = font.as_ref() {
            let (cw, ch) = measure_cell_size(f);
            ivars.cell_width.set(cw);
            ivars.cell_height.set(ch);
        }

        *ivars.font.borrow_mut() = font;
        *ivars.bold_font.borrow_mut() = bold_font;
    }

    /// セル幅を取得
    pub fn cell_width(&self) -> CGFloat {
        self.ivars().cell_width.get()
    }

    /// セル高さを取得
    pub fn cell_height(&self) -> CGFloat {
        self.ivars().cell_height.get()
    }
}
