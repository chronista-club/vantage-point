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

use crate::terminal::StatusBarInfo;

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

/// ステータスバー上のクリック可能な領域
#[derive(Clone)]
struct ClickRegion {
    x_start: CGFloat,
    x_end: CGFloat,
    window_index: usize,
}

/// テキスト選択の範囲（セル座標）
#[derive(Clone, Copy, Default)]
struct Selection {
    /// 選択開始位置 (row, col)
    start: (usize, usize),
    /// 選択終了位置 (row, col)
    end: (usize, usize),
    /// 選択が有効か
    active: bool,
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
    /// ステータスバーに表示する tmux セッション情報
    status_info: RefCell<StatusBarInfo>,
    /// ステータスバー上のクリック可能領域（draw_rect で更新）
    click_regions: RefCell<Vec<ClickRegion>>,
    /// テキスト選択状態
    selection: Cell<Selection>,
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
            let selection = ivars.selection.get();

            // フォント取得（キャッシュ済み）
            let default_font = font_ref.as_ref();
            let bold_font = bold_font_ref.as_ref();

            // 選択範囲の正規化（start < end を保証）
            let (sel_start, sel_end) = if selection.active {
                let s = (selection.start.0, selection.start.1);
                let e = (selection.end.0, selection.end.1);
                if s <= e { (s, e) } else { (e, s) }
            } else {
                ((0, 0), (0, 0))
            };

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

                    // セルが選択範囲内かチェック
                    let is_selected = selection.active && {
                        let pos = (row, col);
                        pos >= sel_start && pos <= sel_end
                    };

                    // 背景矩形（選択中は反転色）
                    if is_selected {
                        // 選択ハイライト: Arctic Frost Blue 半透明
                        CGContext::set_rgb_fill_color(
                            Some(&ctx),
                            SELECTION_BG.0,
                            SELECTION_BG.1,
                            SELECTION_BG.2,
                            0.6,
                        );
                    } else {
                        CGContext::set_rgb_fill_color(
                            Some(&ctx),
                            cell.bg.0,
                            cell.bg.1,
                            cell.bg.2,
                            1.0,
                        );
                    }
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

            // --- ステータスバー描画（常に表示） ---
            let status_info = ivars.status_info.borrow();
            let bar_y = rows as CGFloat * ch;
            let view_width = self.frame().size.width;

            // セパレーター線（1px）
            CGContext::set_rgb_fill_color(
                Some(&ctx),
                STATUS_SEPARATOR.0,
                STATUS_SEPARATOR.1,
                STATUS_SEPARATOR.2,
                1.0,
            );
            CGContext::fill_rect(
                Some(&ctx),
                CGRect::new(CGPoint::new(0.0, bar_y), CGSize::new(view_width, 1.0)),
            );

            // 背景矩形（1セル高さ）
            CGContext::set_rgb_fill_color(
                Some(&ctx),
                STATUS_BG.0,
                STATUS_BG.1,
                STATUS_BG.2,
                1.0,
            );
            CGContext::fill_rect(
                Some(&ctx),
                CGRect::new(
                    CGPoint::new(0.0, bar_y + 1.0),
                    CGSize::new(view_width, ch),
                ),
            );

            // テキスト描画
            if let Some(font) = default_font {
                let text_y = bar_y + 1.0 + LINE_PADDING / 2.0;
                let padding_x = cw * 0.5;

                if status_info.session_name.is_empty() {
                    // データ未到着時のプレースホルダー
                    let placeholder_attrs = unsafe {
                        let fg = NSColor::colorWithSRGBRed_green_blue_alpha(
                            STATUS_INACTIVE.0,
                            STATUS_INACTIVE.1,
                            STATUS_INACTIVE.2,
                            1.0,
                        );
                        let keys: &[&NSAttributedStringKey] =
                            &[NSFontAttributeName, NSForegroundColorAttributeName];
                        let vals: &[&AnyObject] = &[
                            font.as_ref(),
                            &*(fg.as_ref() as *const NSColor as *const AnyObject),
                        ];
                        NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(keys, vals)
                    };
                    let ns_str = NSString::from_str("vantage point");
                    unsafe {
                        ns_str.drawAtPoint_withAttributes(
                            CGPoint::new(padding_x, text_y),
                            Some(&placeholder_attrs),
                        );
                    }
                } else {
                    // セッション名
                    let mut text = status_info.session_name.clone();
                    text.push_str("  ");

                    let session_attrs = unsafe {
                        let fg = NSColor::colorWithSRGBRed_green_blue_alpha(
                            STATUS_INACTIVE.0,
                            STATUS_INACTIVE.1,
                            STATUS_INACTIVE.2,
                            1.0,
                        );
                        let keys: &[&NSAttributedStringKey] =
                            &[NSFontAttributeName, NSForegroundColorAttributeName];
                        let vals: &[&AnyObject] = &[
                            font.as_ref(),
                            &*(fg.as_ref() as *const NSColor as *const AnyObject),
                        ];
                        NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(keys, vals)
                    };

                    let session_ns = NSString::from_str(&text);
                    unsafe {
                        session_ns.drawAtPoint_withAttributes(
                            CGPoint::new(padding_x, text_y),
                            Some(&session_attrs),
                        );
                    }

                    // ウィンドウ一覧（クリック領域も記録）
                    let session_width = (text.len() as CGFloat + 0.5) * cw;
                    let mut x_offset = padding_x + session_width;
                    let mut regions = Vec::new();

                    for win in &status_info.windows {
                        let active_marker = if win.is_active { "*" } else { "" };
                        let win_text =
                            format!("[{}:{}{}] ", win.index, win.name, active_marker);
                        let win_width = win_text.len() as CGFloat * cw;

                        // クリック領域を記録
                        regions.push(ClickRegion {
                            x_start: x_offset,
                            x_end: x_offset + win_width,
                            window_index: win.index,
                        });

                        let (r, g, b) = if win.is_active {
                            STATUS_ACTIVE
                        } else {
                            STATUS_INACTIVE
                        };

                        let win_attrs = unsafe {
                            let fg =
                                NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0);
                            let keys: &[&NSAttributedStringKey] =
                                &[NSFontAttributeName, NSForegroundColorAttributeName];
                            let vals: &[&AnyObject] = &[
                                font.as_ref(),
                                &*(fg.as_ref() as *const NSColor as *const AnyObject),
                            ];
                            NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(
                                keys, vals,
                            )
                        };

                        let win_ns = NSString::from_str(&win_text);
                        unsafe {
                            win_ns.drawAtPoint_withAttributes(
                                CGPoint::new(x_offset, text_y),
                                Some(&win_attrs),
                            );
                        }

                        x_offset += win_width;
                    }

                    // クリック領域を保存
                    *ivars.click_regions.borrow_mut() = regions;
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

// --- ステータスバー Arctic カラー ---
/// 背景: グリッド背景より少し明るい
const STATUS_BG: (CGFloat, CGFloat, CGFloat) = (51.0 / 255.0, 54.0 / 255.0, 67.0 / 255.0);
/// アクティブウィンドウ: Arctic Cyan #88C0D0
const STATUS_ACTIVE: (CGFloat, CGFloat, CGFloat) = (136.0 / 255.0, 192.0 / 255.0, 208.0 / 255.0);
/// 非アクティブウィンドウ: ミュート
const STATUS_INACTIVE: (CGFloat, CGFloat, CGFloat) = (132.0 / 255.0, 140.0 / 255.0, 148.0 / 255.0);
/// セパレーター線: Polar Night #4C566A
const STATUS_SEPARATOR: (CGFloat, CGFloat, CGFloat) = (76.0 / 255.0, 86.0 / 255.0, 106.0 / 255.0);
/// テキスト選択ハイライト: Arctic Frost #5E81AC
const SELECTION_BG: (CGFloat, CGFloat, CGFloat) = (94.0 / 255.0, 129.0 / 255.0, 172.0 / 255.0);

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
            status_info: RefCell::new(StatusBarInfo::default()),
            click_regions: RefCell::new(Vec::new()),
            selection: Cell::new(Selection::default()),
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

    /// 現在のセッション名を取得
    pub fn session_name(&self) -> Option<String> {
        let info = self.ivars().status_info.borrow();
        if info.session_name.is_empty() {
            None
        } else {
            Some(info.session_name.clone())
        }
    }

    /// ステータスバー情報を更新して再描画
    pub fn update_status_bar(&self, info: StatusBarInfo) {
        *self.ivars().status_info.borrow_mut() = info;
        self.setNeedsDisplay(true);
    }

    /// ピクセル座標をセル座標 (row, col) に変換
    pub fn point_to_cell(&self, x: f64, y: f64) -> (usize, usize) {
        let ivars = self.ivars();
        let cw = ivars.cell_width.get();
        let ch = ivars.cell_height.get();
        let cols = ivars.cols.get();
        let rows = ivars.rows.get();
        let col = ((x / cw) as usize).min(cols.saturating_sub(1));
        let row = ((y / ch) as usize).min(rows.saturating_sub(1));
        (row, col)
    }

    /// テキスト選択を開始
    pub fn start_selection(&self, row: usize, col: usize) {
        self.ivars().selection.set(Selection {
            start: (row, col),
            end: (row, col),
            active: true,
        });
        self.setNeedsDisplay(true);
    }

    /// テキスト選択を拡張（ドラッグ中）
    pub fn extend_selection(&self, row: usize, col: usize) {
        let mut sel = self.ivars().selection.get();
        if sel.active {
            sel.end = (row, col);
            self.ivars().selection.set(sel);
            self.setNeedsDisplay(true);
        }
    }

    /// テキスト選択をクリア
    pub fn clear_selection(&self) {
        self.ivars().selection.set(Selection::default());
        self.setNeedsDisplay(true);
    }

    /// 選択範囲が有効か
    pub fn has_selection(&self) -> bool {
        self.ivars().selection.get().active
    }

    /// 選択範囲のテキストを取得
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.ivars().selection.get();
        if !sel.active {
            return None;
        }

        let ivars = self.ivars();
        let cols = ivars.cols.get();
        let cells = ivars.cells.borrow();

        // 正規化
        let (start, end) = if sel.start <= sel.end {
            (sel.start, sel.end)
        } else {
            (sel.end, sel.start)
        };

        let mut result = String::new();
        for row in start.0..=end.0 {
            let col_start = if row == start.0 { start.1 } else { 0 };
            let col_end = if row == end.0 { end.1 } else { cols - 1 };

            for col in col_start..=col_end {
                let idx = row * cols + col;
                if idx < cells.len() && !cells[idx].wide_spacer {
                    result.push(cells[idx].ch);
                }
            }

            // 行末の空白を除去し改行を追加（最終行以外）
            if row < end.0 {
                let trimmed = result.trim_end().len();
                result.truncate(trimmed);
                result.push('\n');
            }
        }

        // 末尾の空白除去
        let trimmed = result.trim_end().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// ステータスバー上のクリックをヒットテスト
    ///
    /// クリック座標がウィンドウラベル上にあれば (window_index, session_name) を返す。
    pub fn hit_test_status_bar(&self, x: CGFloat, y: CGFloat) -> Option<(usize, String)> {
        let ivars = self.ivars();
        let rows = ivars.rows.get();
        let ch = ivars.cell_height.get();
        let bar_y = rows as CGFloat * ch;

        // ステータスバー領域外
        if y < bar_y {
            return None;
        }

        let regions = ivars.click_regions.borrow();
        let status_info = ivars.status_info.borrow();

        for region in regions.iter() {
            if x >= region.x_start && x < region.x_end {
                return Some((region.window_index, status_info.session_name.clone()));
            }
        }
        None
    }
}
