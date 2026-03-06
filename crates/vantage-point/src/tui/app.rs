//! TUI アプリケーションメインループ
//!
//! Claude CLI を PTY 子プロセスとして起動し、ratatui で描画する。

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::terminal::state::TerminalState;

use super::input::key_to_pty_bytes;
use super::terminal_widget::TerminalView;

/// TUI メインエントリー
///
/// `project_dir` のコンテキストで Claude CLI を PTY 起動し、
/// ratatui でターミナル出力を描画する。
pub fn run_tui(project_dir: &str, project_name: &str) -> Result<()> {
    // crossterm raw モード + alternate screen
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, project_dir, project_name);

    // 終了処理（必ず実行）
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// アプリケーションループ
fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    project_dir: &str,
    project_name: &str,
) -> Result<()> {
    // ターミナルサイズ取得（ステータスバー1行分を引く）
    let size = terminal.size()?;
    let pty_cols = size.width as usize;
    let pty_lines = (size.height.saturating_sub(1)) as usize; // ステータスバー分

    // VT パーサー（共有状態）
    let term_state = Arc::new(Mutex::new(TerminalState::new(pty_cols, pty_lines)));

    // PTY 起動
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: pty_lines as u16,
        cols: pty_cols as u16,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Claude CLI を PTY 子プロセスとして起動
    let mut cmd = CommandBuilder::new("claude");
    cmd.cwd(project_dir);
    // TERM 設定（Claude CLI が色を出力するため）
    cmd.env("TERM", "xterm-256color");

    let mut child = pair.slave.spawn_command(cmd)?;
    // slave 側は spawn 後に不要
    drop(pair.slave);

    // PTY reader → TerminalState にフィード
    let reader = pair.master.try_clone_reader()?;
    let term_state_reader = Arc::clone(&term_state);
    let reader_handle = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break, // EOF（プロセス終了）
                Ok(n) => {
                    let mut state = term_state_reader.lock().unwrap();
                    state.feed_bytes(&buf[..n]);
                }
                Err(_) => break,
            }
        }
    });

    // PTY writer（キー入力を書き込む）
    let mut writer = pair.master.take_writer()?;

    // メインループ
    loop {
        // プロセス終了チェック
        if let Ok(Some(_status)) = child.try_wait() {
            break;
        }

        // 描画
        {
            let state = term_state.lock().unwrap();
            let snapshot = state.snapshot();
            let app_cursor = state.app_cursor_mode();

            terminal.draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Min(1),    // PTY エリア
                        Constraint::Length(1), // ステータスバー
                    ])
                    .split(frame.area());

                // PTY ターミナル描画
                frame.render_widget(TerminalView::new(&snapshot), chunks[0]);

                // ステータスバー
                let status = Line::from(vec![
                    Span::styled(
                        format!(" {} ", project_name),
                        Style::default()
                            .fg(Color::Rgb(11, 17, 32))
                            .bg(Color::Rgb(136, 192, 208))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        " Claude CLI ",
                        Style::default()
                            .fg(Color::Rgb(216, 222, 233))
                            .bg(Color::Rgb(46, 52, 64)),
                    ),
                    Span::styled(
                        " Ctrl+Q: quit ",
                        Style::default()
                            .fg(Color::Rgb(76, 86, 106))
                            .bg(Color::Rgb(46, 52, 64)),
                    ),
                ]);
                let status_bar =
                    Paragraph::new(status).style(Style::default().bg(Color::Rgb(46, 52, 64)));
                frame.render_widget(status_bar, chunks[1]);
            })?;

            drop(state);

            // イベント処理
            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    Event::Key(key) => {
                        // Ctrl+Q で TUI 終了（Claude CLI は Process サーバー上で継続）
                        if key.code == KeyCode::Char('q')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            break;
                        }

                        let bytes = key_to_pty_bytes(key, app_cursor);
                        if !bytes.is_empty() {
                            let _ = std::io::Write::write_all(&mut writer, &bytes);
                        }
                    }
                    Event::Resize(cols, rows) => {
                        let new_lines = rows.saturating_sub(1) as usize;
                        let new_cols = cols as usize;
                        let mut state = term_state.lock().unwrap();
                        state.resize(new_cols, new_lines);
                        pair.master
                            .resize(PtySize {
                                rows: new_lines as u16,
                                cols: new_cols as u16,
                                pixel_width: 0,
                                pixel_height: 0,
                            })
                            .ok();
                    }
                    _ => {}
                }
            }
        }
    }

    // PTY reader スレッドの終了を待つ
    let _ = reader_handle.join();

    Ok(())
}
