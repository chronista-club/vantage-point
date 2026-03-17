//! vp tui — ratatui コンソール
//!
//! tmux セッションをヘッダー/フッター付きで表示する TUI コンソール。
//! どのターミナル（Kitty, Ghostty, iTerm, VantagePoint.app）でも
//! 同じ見た目・操作感を提供する「ターミナル体験の標準レイヤー」。

use anyhow::Result;

use crate::config::Config;
use crate::tmux;

/// VP Shell コマンドを実行
fn execute_command(input: &str, session_name: &str) -> String {
    let parts: Vec<&str> = input.trim().splitn(2, ' ').collect();
    let cmd = parts[0];
    let args = parts.get(1).copied().unwrap_or("");

    match cmd {
        "split" | "sp" => {
            // tmux split-window
            let tmux_bin = if std::path::Path::new("/opt/homebrew/bin/tmux").exists() {
                "/opt/homebrew/bin/tmux"
            } else {
                "tmux"
            };
            let status = std::process::Command::new(tmux_bin)
                .args(["split-window", "-t", session_name, "-d"])
                .status();
            match status {
                Ok(s) if s.success() => "Split created".to_string(),
                _ => "Split failed".to_string(),
            }
        }
        "vsplit" | "vs" => {
            let tmux_bin = if std::path::Path::new("/opt/homebrew/bin/tmux").exists() {
                "/opt/homebrew/bin/tmux"
            } else {
                "tmux"
            };
            let status = std::process::Command::new(tmux_bin)
                .args(["split-window", "-h", "-t", session_name, "-d"])
                .status();
            match status {
                Ok(s) if s.success() => "Vertical split created".to_string(),
                _ => "VSplit failed".to_string(),
            }
        }
        "q" | "quit" => {
            std::process::exit(0);
        }
        "help" | "h" => ":split :vsplit :quit :help".to_string(),
        _ => {
            if args.is_empty() {
                format!("Unknown command: {}", cmd)
            } else {
                format!("Unknown command: {} {}", cmd, args)
            }
        }
    }
}

/// vp tui コマンドを実行
pub fn execute(session: Option<String>, config: &Config) -> Result<()> {
    // セッション名を解決（指定なしなら cwd から自動検出）
    let session_name = if let Some(s) = session {
        s
    } else {
        let cwd = std::env::current_dir()?;
        let project_name = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default");
        // config から名前を解決（登録済みプロジェクトなら config の名前を優先）
        let resolved_name = config
            .projects
            .iter()
            .find(|p| {
                Config::normalize_path(std::path::Path::new(&p.path))
                    == Config::normalize_path(&cwd)
            })
            .map(|p| p.name.as_str())
            .unwrap_or(project_name);
        tmux::session_name(resolved_name)
    };

    // tmux セッションが存在するか確認
    if !tmux::session_exists(&session_name) {
        eprintln!("tmux session '{}' not found.", session_name);
        eprintln!("Start a project first: vp start");
        std::process::exit(1);
    }

    // ratatui TUI を起動
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_tui_console(&session_name))?;

    Ok(())
}

/// ratatui コンソールのメインループ
async fn run_tui_console(session_name: &str) -> Result<()> {
    use crossterm::event::{self, Event, KeyModifiers};
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::{Constraint, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};
    use std::io;

    // ターミナル初期化
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    crossterm::execute!(stdout, crossterm::event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // PTY でtmux attach を起動
    // レイアウト: ヘッダー1行 + ボーダー上1行 + PTY + ボーダー下1行 + フッター1行
    let size = terminal.size()?;
    let pty_rows = size.height.saturating_sub(4).max(1); // ヘッダー + ボーダー上下 + フッター
    let pty_cols = size.width.saturating_sub(2).max(1); // ボーダー左右

    let term_state = std::sync::Arc::new(std::sync::Mutex::new(
        crate::terminal::state::TerminalState::new(pty_cols as usize, pty_rows as usize),
    ));

    // tmux attach コマンドを PTY で起動
    let tmux_bin = if std::path::Path::new("/opt/homebrew/bin/tmux").exists() {
        "/opt/homebrew/bin/tmux"
    } else {
        "tmux"
    };
    let pty_command = format!("{} attach-session -t {}", tmux_bin, session_name);

    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("/"))
        .to_string_lossy()
        .to_string();

    // PTY プロセスを起動（portable-pty）
    use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: pty_rows,
        cols: pty_cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.args(["-l", "-c", &pty_command]);
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave); // slave は子プロセスが使う

    let reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    // リサイズ用に master を保持
    let pty_master = std::sync::Arc::new(std::sync::Mutex::new(pair.master));

    // PTY 出力リーダースレッド
    let term_state_for_reader = term_state.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut state = term_state_for_reader.lock().unwrap();
                    state.feed_bytes(&buf[..n]);
                }
            }
        }
    });

    loop {
        // 描画
        let footer_text = String::new();

        terminal.draw(|frame| {
            let chunks = Layout::vertical([
                Constraint::Length(1), // ヘッダー
                Constraint::Min(1),    // ターミナル（ボーダー込み）
                Constraint::Length(1), // フッター
            ])
            .split(frame.area());

            // ヘッダー（ロール + セッション名 — ステータスは NSView 側に委譲）
            // セッション名パターンで Lead/Worker を判定:
            //   {project}-vp → proj-lead
            //   {project}-{id}-vp → proj-worker
            let role_label = {
                let without_vp = session_name.strip_suffix("-vp").unwrap_or(session_name);
                // config のプロジェクト名と一致すれば Lead、それ以外は Worker
                // 簡易判定: ハイフン分割で最後のセグメントがプロジェクト名でなければ Worker
                if without_vp.contains('-') {
                    // ccws ワーカー名は {parent}-{worker} 形式
                    // ただし vantage-point のようにプロジェクト名自体にハイフンが含まれる場合もある
                    // tmux セッション名が ccws ディレクトリ名ベースかどうかで判定
                    let ccws_dir = dirs::home_dir()
                        .map(|h| h.join(".local/share/ccws").join(without_vp))
                        .unwrap_or_default();
                    if ccws_dir.is_dir() {
                        " proj-worker "
                    } else {
                        " proj-lead "
                    }
                } else {
                    " proj-lead "
                }
            };
            let (role_fg, role_bg) = if role_label.contains("worker") {
                (Color::Rgb(11, 17, 32), Color::Rgb(163, 190, 140)) // 緑系
            } else {
                (Color::Rgb(11, 17, 32), Color::Rgb(136, 192, 208)) // 青系
            };
            let header = Paragraph::new(Line::from(vec![
                Span::styled(
                    role_label,
                    Style::default()
                        .fg(role_fg)
                        .bg(role_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}", session_name),
                    Style::default()
                        .fg(Color::Rgb(216, 222, 233))
                        .bg(Color::Rgb(46, 52, 64)),
                ),
            ]))
            .style(Style::default().bg(Color::Rgb(46, 52, 64)));
            frame.render_widget(header, chunks[0]);

            // ターミナルビューポート（ボーダー付き）
            let state = term_state.lock().unwrap();
            let snapshot = state.snapshot();
            let widget = crate::tui::terminal_widget::TerminalView::new(&snapshot);
            let border_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(76, 86, 106)));
            let inner = border_block.inner(chunks[1]);
            frame.render_widget(border_block, chunks[1]);
            frame.render_widget(widget, inner);

            // フッター
            let footer = Paragraph::new(footer_text.clone())
                .style(Style::default().fg(Color::Rgb(76, 86, 106)).bg(Color::Rgb(46, 52, 64)));
            frame.render_widget(footer, chunks[2]);
        })?;

        // イベント処理（10ms ポーリング）
        if event::poll(std::time::Duration::from_millis(10))? {
            match event::read()? {
                Event::Resize(cols, rows) => {
                    // ボーダー分を差し引いて PTY リサイズ
                    let new_cols = cols.saturating_sub(2).max(1);
                    let new_rows = rows.saturating_sub(4).max(1); // ヘッダー + ボーダー上下 + フッター
                    {
                        let mut state = term_state.lock().unwrap();
                        state.resize(new_cols as usize, new_rows as usize);
                    }
                    if let Ok(master) = pty_master.lock() {
                        let _ = master.resize(PtySize {
                            rows: new_rows,
                            cols: new_cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    continue;
                }
                Event::Key(key) => {
                    // 全キー入力を PTY にパススルー
                    let app_cursor = {
                        let state = term_state.lock().unwrap();
                        state.app_cursor_mode()
                    };
                    let bytes = crate::tui::input::key_to_pty_bytes(key, app_cursor);
                    if !bytes.is_empty() {
                        use std::io::Write;
                        let _ = writer.write_all(&bytes);
                        let _ = writer.flush();
                    }
                }
                Event::Mouse(mouse) => {
                    // Cmd+Click で URL をブラウザで開く
                    if mouse.modifiers.contains(KeyModifiers::SUPER)
                        && matches!(
                            mouse.kind,
                            event::MouseEventKind::Down(event::MouseButton::Left)
                        )
                    {
                        let col = mouse.column.saturating_sub(1) as usize;
                        let row = mouse.row.saturating_sub(2) as usize;
                        if let Some(url) = extract_url_at(&term_state, row, col) {
                            let _ = open::that(&url);
                        }
                    } else {
                        let seq = mouse_to_sgr(&mouse);
                        if !seq.is_empty() {
                            use std::io::Write;
                            let _ = writer.write_all(seq.as_bytes());
                            let _ = writer.flush();
                        }
                    }
                }
                _ => {} // FocusGained, FocusLost, Paste
            }
        }

        // 子プロセスが終了したかチェック
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
    }

    // クリーンアップ
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;

    Ok(())
}

/// ターミナルグリッドの指定位置から URL を抽出
///
/// 指定行のテキストを取得し、カラム位置を含む URL があればその URL を返す。
fn extract_url_at(
    term_state: &std::sync::Arc<std::sync::Mutex<crate::terminal::state::TerminalState>>,
    row: usize,
    col: usize,
) -> Option<String> {
    let state = term_state.lock().ok()?;
    let snapshot = state.snapshot();

    if row >= snapshot.cells.len() {
        return None;
    }

    // 行テキストを構築して URL を検索
    let line: String = snapshot.cells[row].iter().map(|c| c.ch).collect();
    find_url_at_column(&line, col)
}

/// テキスト行の指定カラム位置にある URL を抽出
///
/// URL パターン: `https?://[^\s<>"'）」\]]+`
/// 末尾の句読点（`.` `,` `)` `]`）は除去
fn find_url_at_column(line: &str, col: usize) -> Option<String> {
    static URL_REGEX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let url_pattern =
        URL_REGEX.get_or_init(|| regex::Regex::new(r#"https?://[^\s<>"'）」\]]+"#).unwrap());
    for m in url_pattern.find_iter(line) {
        let start_col = line[..m.start()].chars().count();
        let end_col = start_col + m.as_str().chars().count();

        if col >= start_col && col < end_col {
            let url = m.as_str().trim_end_matches(['.', ',', ')', ']']);
            return Some(url.to_string());
        }
    }
    None
}

/// マウスイベントを SGR エスケープシーケンスに変換
///
/// Block 枠分のオフセット: ヘッダー1行 + 上枠1行 = 2行分, 左枠1列
fn mouse_to_sgr(mouse: &crossterm::event::MouseEvent) -> String {
    use crossterm::event::MouseEventKind;

    let x = mouse.column.saturating_sub(1);
    let y = mouse.row.saturating_sub(2);
    match mouse.kind {
        MouseEventKind::ScrollUp => format!("\x1b[<64;{};{}M", x + 1, y + 1),
        MouseEventKind::ScrollDown => format!("\x1b[<65;{};{}M", x + 1, y + 1),
        MouseEventKind::Down(btn) => {
            let b = match btn {
                crossterm::event::MouseButton::Left => 0,
                crossterm::event::MouseButton::Right => 2,
                crossterm::event::MouseButton::Middle => 1,
            };
            format!("\x1b[<{};{};{}M", b, x + 1, y + 1)
        }
        MouseEventKind::Up(btn) => {
            let b = match btn {
                crossterm::event::MouseButton::Left => 0,
                crossterm::event::MouseButton::Right => 2,
                crossterm::event::MouseButton::Middle => 1,
            };
            format!("\x1b[<{};{};{}m", b, x + 1, y + 1)
        }
        MouseEventKind::Drag(btn) => {
            let b = match btn {
                crossterm::event::MouseButton::Left => 32,
                crossterm::event::MouseButton::Right => 34,
                crossterm::event::MouseButton::Middle => 33,
            };
            format!("\x1b[<{};{};{}M", b, x + 1, y + 1)
        }
        MouseEventKind::Moved => format!("\x1b[<35;{};{}M", x + 1, y + 1),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    /// テスト用ヘルパー: MouseEvent を生成
    fn make_mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn test_scroll_up() {
        // col=5, row=4 → x=5-1=4, y=4-2=2 → SGR座標 x+1=5, y+1=3
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::ScrollUp, 5, 4));
        assert_eq!(seq, "\x1b[<64;5;3M");
    }

    #[test]
    fn test_scroll_down() {
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::ScrollDown, 10, 6));
        assert_eq!(seq, "\x1b[<65;10;5M");
    }

    #[test]
    fn test_left_click_down() {
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::Down(MouseButton::Left), 3, 5));
        assert_eq!(seq, "\x1b[<0;3;4M");
    }

    #[test]
    fn test_left_click_up_uses_lowercase_m() {
        // release は小文字 'm'（SGR プロトコル仕様）
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::Up(MouseButton::Left), 3, 5));
        assert_eq!(seq, "\x1b[<0;3;4m");
    }

    #[test]
    fn test_right_click_button_code() {
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::Down(MouseButton::Right), 1, 2));
        assert_eq!(seq, "\x1b[<2;1;1M");
    }

    #[test]
    fn test_middle_click_button_code() {
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::Down(MouseButton::Middle), 1, 2));
        assert_eq!(seq, "\x1b[<1;1;1M");
    }

    #[test]
    fn test_drag_left_button_code() {
        // left drag = button 32
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::Drag(MouseButton::Left), 10, 10));
        assert_eq!(seq, "\x1b[<32;10;9M");
    }

    #[test]
    fn test_moved_button_code() {
        // move = button 35
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::Moved, 20, 15));
        assert_eq!(seq, "\x1b[<35;20;14M");
    }

    #[test]
    fn test_offset_saturating_at_zero() {
        // col=0, row=0 → saturating_sub で負にならない → SGR (1,1)
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::ScrollUp, 0, 0));
        assert_eq!(seq, "\x1b[<64;1;1M");
    }

    #[test]
    fn test_offset_row_in_header_area() {
        // row=1 → y=saturating_sub(2)=0 → SGR y=1
        let seq = mouse_to_sgr(&make_mouse(MouseEventKind::ScrollUp, 5, 1));
        assert_eq!(seq, "\x1b[<64;5;1M");
    }

    // --- URL 検出テスト ---

    #[test]
    fn test_find_url_basic() {
        let line = "See https://example.com for details";
        assert_eq!(
            find_url_at_column(line, 5),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn test_find_url_with_path() {
        let line = "Visit https://linear.app/chronista/issue/VP-15 now";
        assert_eq!(
            find_url_at_column(line, 10),
            Some("https://linear.app/chronista/issue/VP-15".to_string())
        );
    }

    #[test]
    fn test_find_url_http() {
        let line = "Check http://localhost:3000/api/health endpoint";
        assert_eq!(
            find_url_at_column(line, 8),
            Some("http://localhost:3000/api/health".to_string())
        );
    }

    #[test]
    fn test_find_url_trailing_period() {
        // 末尾のピリオドは除去
        let line = "See https://example.com.";
        assert_eq!(
            find_url_at_column(line, 5),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn test_find_url_trailing_comma() {
        let line = "URLs: https://a.com, https://b.com";
        assert_eq!(
            find_url_at_column(line, 7),
            Some("https://a.com".to_string())
        );
    }

    #[test]
    fn test_find_url_not_on_url() {
        let line = "No URL here, just text";
        assert_eq!(find_url_at_column(line, 5), None);
    }

    #[test]
    fn test_find_url_col_before_url() {
        let line = "text https://example.com rest";
        // col=0 は "text" の部分
        assert_eq!(find_url_at_column(line, 0), None);
    }

    #[test]
    fn test_find_url_col_after_url() {
        let line = "text https://example.com rest";
        // "rest" の部分（col=25 以降）
        assert_eq!(find_url_at_column(line, 26), None);
    }

    #[test]
    fn test_find_url_multiple_urls() {
        let line = "a https://first.com b https://second.com c";
        assert_eq!(
            find_url_at_column(line, 3),
            Some("https://first.com".to_string())
        );
        assert_eq!(
            find_url_at_column(line, 23),
            Some("https://second.com".to_string())
        );
    }

    #[test]
    fn test_find_url_empty_line() {
        assert_eq!(find_url_at_column("", 0), None);
    }
}
