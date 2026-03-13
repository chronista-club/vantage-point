//! PTY セッション管理 + VT パーサー → NativeBackend 統合
//!
//! portable-pty でシェルを起動し、alacritty_terminal で VT パースした結果を
//! NativeBackend のバッファに書き込む。FFI 経由で Swift から操作可能。

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags as CellFlags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use ratatui::backend::Backend;
use ratatui::buffer::Cell;
use ratatui::style::{Color, Modifier, Style};
use vte::ansi::{Color as VteColor, NamedColor, Rgb};

use crate::backend::NativeBackend;

/// PTY + VT パーサーの統合管理
pub struct BridgePty {
    /// PTY への書き込みハンドル
    writer: Box<dyn Write + Send>,
    /// PTY ペア（リサイズ用に保持）
    pair: portable_pty::PtyPair,
    /// VT パーサー状態（スレッド共有）
    term_state: Arc<Mutex<TermState>>,
    /// リーダースレッド停止フラグ
    running: Arc<AtomicBool>,
    /// リーダースレッドハンドル
    reader_handle: Option<thread::JoinHandle<()>>,
}

/// VT ターミナル状態（alacritty_terminal ラッパー）
struct TermState {
    term: Term<EventProxy>,
    parser: vte::ansi::Processor,
    cols: usize,
    lines: usize,
}

/// alacritty_terminal のイベントリスナー（空実装）
struct EventProxy;
impl EventListener for EventProxy {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {}
}

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
        self.lines + 10_000
    }
}

// =============================================================================
// Arctic Nordic Ocean カラーパレット
// =============================================================================

fn named_to_rgb(color: &NamedColor) -> (u8, u8, u8) {
    match color {
        NamedColor::Black => (11, 17, 32),
        NamedColor::Red => (191, 97, 106),
        NamedColor::Green => (163, 190, 140),
        NamedColor::Yellow => (235, 203, 139),
        NamedColor::Blue => (129, 161, 193),
        NamedColor::Magenta => (180, 142, 173),
        NamedColor::Cyan => (136, 192, 208),
        NamedColor::White => (216, 222, 233),
        NamedColor::BrightBlack => (76, 86, 106),
        NamedColor::BrightRed => (208, 115, 125),
        NamedColor::BrightGreen => (183, 210, 160),
        NamedColor::BrightYellow => (245, 224, 169),
        NamedColor::BrightBlue => (155, 185, 213),
        NamedColor::BrightMagenta => (200, 167, 193),
        NamedColor::BrightCyan => (163, 214, 226),
        NamedColor::BrightWhite => (236, 239, 244),
        NamedColor::DimBlack => (7, 12, 22),
        NamedColor::DimRed => (140, 70, 77),
        NamedColor::DimGreen => (120, 140, 103),
        NamedColor::DimYellow => (172, 148, 101),
        NamedColor::DimBlue => (94, 118, 141),
        NamedColor::DimMagenta => (132, 104, 127),
        NamedColor::DimCyan => (100, 141, 152),
        NamedColor::DimWhite => (158, 163, 170),
        NamedColor::Foreground => (216, 222, 233),
        NamedColor::Background => (11, 17, 32),
        NamedColor::Cursor => (136, 192, 208),
        NamedColor::BrightForeground => (236, 239, 244),
        NamedColor::DimForeground => (158, 163, 170),
    }
}

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
            named_to_rgb(&named)
        }
        16..=231 => {
            let idx = index - 16;
            let r = (idx / 36) * 51;
            let g = ((idx % 36) / 6) * 51;
            let b = (idx % 6) * 51;
            (r, g, b)
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    }
}

fn resolve_color(color: &VteColor) -> (u8, u8, u8) {
    match color {
        VteColor::Named(named) => named_to_rgb(named),
        VteColor::Spec(Rgb { r, g, b }) => (*r, *g, *b),
        VteColor::Indexed(idx) => indexed_to_rgb(*idx),
    }
}

// =============================================================================
// BridgePty 実装
// =============================================================================

impl BridgePty {
    /// シェルを起動して PTY セッションを開始
    ///
    /// cwd: 作業ディレクトリ
    /// backend: NativeBackend への参照（PTY 出力をバッファに書き込む）
    pub fn spawn(
        cwd: &str,
        cols: u16,
        rows: u16,
        backend: Arc<Mutex<NativeBackend>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::spawn_command(cwd, cols, rows, None, backend)
    }

    /// 指定コマンドで PTY を起動
    pub fn spawn_command(
        cwd: &str,
        cols: u16,
        rows: u16,
        command: Option<&[&str]>,
        backend: Arc<Mutex<NativeBackend>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = if let Some(args) = command {
            let mut c = CommandBuilder::new(args[0]);
            for arg in &args[1..] {
                c.arg(arg);
            }
            c
        } else {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            let mut c = CommandBuilder::new(&shell);
            c.arg("-l");
            c
        };
        cmd.cwd(cwd);

        // 環境変数設定
        // .app バンドルから起動すると環境変数が最小限のため、明示的に引き継ぐ
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // HOME / USER / SHELL / LANG を親プロセスから引き継ぎ
        for key in &["HOME", "USER", "SHELL", "LANG", "PATH", "LC_ALL", "LC_CTYPE"] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, &val);
            }
        }

        pair.slave.spawn_command(cmd)?;

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        // VT パーサー初期化
        let config = TermConfig::default();
        let dims = TermDimensions {
            cols: cols as usize,
            lines: rows as usize,
        };
        let term = Term::new(config, &dims, EventProxy);
        let parser = vte::ansi::Processor::new();

        let term_state = Arc::new(Mutex::new(TermState {
            term,
            parser,
            cols: cols as usize,
            lines: rows as usize,
        }));

        let running = Arc::new(AtomicBool::new(true));

        // リーダースレッド起動
        let reader_handle = {
            let term_state = Arc::clone(&term_state);
            let backend = Arc::clone(&backend);
            let running = Arc::clone(&running);

            thread::spawn(move || {
                Self::reader_loop(reader, term_state, backend, running);
            })
        };

        Ok(Self {
            writer,
            pair,
            term_state,
            running,
            reader_handle: Some(reader_handle),
        })
    }

    /// PTY リーダーループ
    ///
    /// PTY 出力 → VT パース → NativeBackend バッファ更新 → フレームコールバック
    fn reader_loop(
        mut reader: Box<dyn Read + Send>,
        term_state: Arc<Mutex<TermState>>,
        backend: Arc<Mutex<NativeBackend>>,
        running: Arc<AtomicBool>,
    ) {
        let mut buf = [0u8; 4096];

        while running.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF — PTY プロセス終了
                Ok(n) => {
                    // VT パース
                    // parser と term の二重借用を避けるため、一時的に取り出す
                    {
                        let mut state = term_state.lock().unwrap();
                        let TermState {
                            ref mut term,
                            ref mut parser,
                            ..
                        } = *state;
                        parser.advance(term, &buf[..n]);
                    }

                    // グリッド → NativeBackend に転写
                    Self::sync_to_backend(&term_state, &backend);
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
            }
        }

        running.store(false, Ordering::Release);
    }

    /// VT グリッド → NativeBackend バッファ同期
    fn sync_to_backend(
        term_state: &Arc<Mutex<TermState>>,
        backend: &Arc<Mutex<NativeBackend>>,
    ) {
        let state = term_state.lock().unwrap();
        let grid = state.term.grid();
        let display_offset = grid.display_offset();

        let mut cells: Vec<(u16, u16, Cell)> = Vec::new();

        for line_idx in 0..state.lines {
            let line = Line(line_idx as i32 - display_offset as i32);

            for col_idx in 0..state.cols {
                let vte_cell = &grid[line][Column(col_idx)];

                // ワイドキャラクターのスペーサーはスキップ
                if vte_cell.flags.contains(CellFlags::WIDE_CHAR_SPACER) {
                    continue;
                }

                let mut fg_rgb = resolve_color(&vte_cell.fg);
                let mut bg_rgb = resolve_color(&vte_cell.bg);

                // SGR 7 (INVERSE)
                if vte_cell.flags.contains(CellFlags::INVERSE) {
                    std::mem::swap(&mut fg_rgb, &mut bg_rgb);
                }

                let fg = Color::Rgb(fg_rgb.0, fg_rgb.1, fg_rgb.2);
                let bg = Color::Rgb(bg_rgb.0, bg_rgb.1, bg_rgb.2);

                let mut modifier = Modifier::empty();
                if vte_cell.flags.contains(CellFlags::BOLD) {
                    modifier |= Modifier::BOLD;
                }
                if vte_cell.flags.contains(CellFlags::ITALIC) {
                    modifier |= Modifier::ITALIC;
                }
                if vte_cell.flags.contains(CellFlags::UNDERLINE) {
                    modifier |= Modifier::UNDERLINED;
                }
                if vte_cell.flags.contains(CellFlags::DIM_BOLD) {
                    modifier |= Modifier::DIM;
                }
                if vte_cell.flags.contains(CellFlags::STRIKEOUT) {
                    modifier |= Modifier::CROSSED_OUT;
                }

                let style = Style::default().fg(fg).bg(bg).add_modifier(modifier);

                let mut cell = Cell::default();
                cell.set_char(vte_cell.c);
                cell.set_style(style);

                cells.push((col_idx as u16, line_idx as u16, cell));
            }
        }

        // カーソル位置も同期（負値チェック: スクロール時に負の Line になりうる）
        let cursor_point = grid.cursor.point;
        let cursor_col = cursor_point.column.0 as u16;
        let cursor_line = cursor_point.line.0;
        let cursor_visible = if display_offset > 0 {
            false
        } else {
            use alacritty_terminal::term::TermMode;
            state.term.mode().contains(TermMode::SHOW_CURSOR)
        };
        let lines = state.lines;

        // term_state ロックを先に解放してから backend をロック
        // （ロック順序の一貫性: 常に term_state → backend の順）
        drop(state);

        // Backend に書き込み
        let mut be = backend.lock().unwrap();
        // まずクリア
        let _ = be.clear();
        // セル書き込み
        let refs: Vec<(u16, u16, &Cell)> = cells.iter().map(|(x, y, c)| (*x, *y, c)).collect();
        let _ = be.draw(refs.into_iter());
        // カーソル設定（負値の場合は無視）
        if cursor_line >= 0 && (cursor_line as usize) < lines {
            let _ = be.set_cursor_position(ratatui::layout::Position::new(
                cursor_col,
                cursor_line as u16,
            ));
        }
        if cursor_visible {
            let _ = be.show_cursor();
        } else {
            let _ = be.hide_cursor();
        }
        let _ = be.flush();
    }

    /// PTY にバイト列を送信（キー入力）
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }

    /// PTY をリサイズ
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Box<dyn std::error::Error>> {
        self.pair.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut state = self.term_state.lock().unwrap();
        state.cols = cols as usize;
        state.lines = rows as usize;
        let dims = TermDimensions {
            cols: cols as usize,
            lines: rows as usize,
        };
        state.term.resize(dims);

        Ok(())
    }

    /// PTY が稼働中か
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

impl Drop for BridgePty {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        // master 側を先に閉じて reader.read() に EOF を届ける
        // （ブロッキング read を脱出させるため）
        // portable-pty の PtyPair は master を drop すると fd が閉じる
        // pair フィールドを take できないので、writer を drop して master の書き込み側を閉じる
        // reader 側は master の try_clone_reader で複製済みなので、
        // master の fd を直接閉じるには pair ごと drop する必要がある
        // → ここでは join にタイムアウト的に対応するため detach
        if let Some(handle) = self.reader_handle.take() {
            // reader.read() は PTY プロセス終了時に EOF で抜ける
            // プロセスが終了していない場合でも pair.master の drop で fd が閉じて EOF になる
            // join は pair drop 後に実行されるため、通常はすぐに戻る
            // （struct のフィールドは宣言の逆順で drop される:
            //   reader_handle → running → term_state → pair → writer）
            // ただし drop 順序上 pair がまだ生きている可能性があるので
            // ここでは短時間だけ待ち、応答がなければ detach する
            let start = std::time::Instant::now();
            loop {
                if handle.is_finished() {
                    let _ = handle.join();
                    return;
                }
                if start.elapsed() > std::time::Duration::from_millis(100) {
                    // detach: スレッドはプロセス終了時に自然終了する
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}
