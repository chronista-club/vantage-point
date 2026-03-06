//! TUI アプリケーションメインループ
//!
//! プロジェクト選択 → Claude CLI PTY セッション の画面遷移を管理する。

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::grid::Scroll;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::config::{Config, ProjectConfig};
use crate::terminal::state::TerminalState;

use super::input::key_to_pty_bytes;
use super::terminal_widget::TerminalView;

// Arctic Nord カラー定数
const NORD_BG: Color = Color::Rgb(11, 17, 32); // #0B1120
const NORD_FG: Color = Color::Rgb(216, 222, 233); // #D8DEE9
const NORD_CYAN: Color = Color::Rgb(136, 192, 208); // #88C0D0
const NORD_BLUE: Color = Color::Rgb(129, 161, 193); // #81A1C1
const NORD_POLAR: Color = Color::Rgb(46, 52, 64); // #2E3440
const NORD_COMMENT: Color = Color::Rgb(76, 86, 106); // #4C566A
const NORD_GREEN: Color = Color::Rgb(163, 190, 140); // #A3BE8C

/// TUI メインエントリー（プロジェクト指定あり）
///
/// プロジェクトが既に解決済みの場合、直接 Claude セッションを起動する。
pub fn run_tui(project_dir: &str, project_name: &str) -> Result<()> {
    run_tui_inner(Some((project_dir.to_string(), project_name.to_string())))
}

/// TUI メインエントリー（プロジェクト選択画面から開始）
pub fn run_tui_select(config: &Config) -> Result<()> {
    if config.projects.is_empty() {
        // プロジェクト未登録 → cwd で直接起動
        let cwd = std::env::current_dir()?;
        let name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return run_tui(&cwd.display().to_string(), &name);
    }

    run_tui_inner(None)
}

/// OSC シーケンスでターミナルウィンドウタイトルを設定
fn set_terminal_title(title: &str) {
    use std::io::Write;
    let _ = write!(io::stdout(), "\x1b]0;{}\x07", title);
    let _ = io::stdout().flush();
}

/// 内部エントリー
fn run_tui_inner(resolved: Option<(String, String)>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::event::EnableMouseCapture,
        EnterAlternateScreen
    )?;

    // ターミナルタイトル設定
    if let Some((_, ref name)) = resolved {
        set_terminal_title(&format!("VP: {}", name));
    } else {
        set_terminal_title("Vantage Point");
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = if let Some((dir, name)) = resolved {
        run_claude_session(&mut terminal, &dir, &name)
    } else {
        run_project_select(&mut terminal)
    };

    // 終了処理（必ず実行）
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}

// =============================================================================
// プロジェクト選択画面
// =============================================================================

/// プロジェクト選択画面
fn run_project_select(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let config = Config::load()?;
    let projects = config.projects.clone();

    if projects.is_empty() {
        anyhow::bail!("プロジェクトが登録されていません。config.toml に追加してください。");
    }

    let mut list_state = ListState::default();
    list_state.select(Some(0));

    loop {
        terminal.draw(|frame| {
            draw_project_select(frame, &projects, &mut list_state);
        })?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    // 選択決定
                    KeyCode::Enter => {
                        if let Some(idx) = list_state.selected() {
                            let project = &projects[idx];
                            let dir = Config::normalize_path(std::path::Path::new(&project.path));
                            let name = project.name.clone();

                            // ターミナルタイトル更新
                            set_terminal_title(&format!("VP: {}", name));

                            // Process サーバー + Canvas 起動
                            start_background_services(&dir, &config, idx, &name).ok();

                            // Claude セッション開始
                            return run_claude_session(terminal, &dir, &name);
                        }
                    }
                    // カーソル移動
                    KeyCode::Up | KeyCode::Char('k') => {
                        let i = list_state.selected().unwrap_or(0);
                        let new = if i == 0 { projects.len() - 1 } else { i - 1 };
                        list_state.select(Some(new));
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let i = list_state.selected().unwrap_or(0);
                        let new = if i >= projects.len() - 1 { 0 } else { i + 1 };
                        list_state.select(Some(new));
                    }
                    // 終了
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// プロジェクト選択画面の描画
fn draw_project_select(
    frame: &mut ratatui::Frame,
    projects: &[ProjectConfig],
    list_state: &mut ListState,
) {
    let area = frame.area();

    // 背景
    frame.render_widget(Block::default().style(Style::default().bg(NORD_BG)), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // タイトル
            Constraint::Min(1),    // プロジェクトリスト
            Constraint::Length(1), // ヘルプ
        ])
        .margin(1)
        .split(area);

    // タイトル
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " Vantage Point ",
            Style::default().fg(NORD_CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" — プロジェクト選択", Style::default().fg(NORD_FG)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(NORD_COMMENT)),
    );
    frame.render_widget(title, chunks[0]);

    // プロジェクトリスト
    let items: Vec<ListItem> = projects
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let running = crate::config::RunningProcesses::find_by_project(
                &Config::normalize_path(std::path::Path::new(&p.path)),
            );
            let status = if running.is_some() {
                Span::styled(" [running] ", Style::default().fg(NORD_GREEN))
            } else {
                Span::raw("")
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", i + 1), Style::default().fg(NORD_COMMENT)),
                Span::styled(&p.name, Style::default().fg(NORD_FG)),
                status,
                Span::styled(format!("  {}", p.path), Style::default().fg(NORD_COMMENT)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(NORD_POLAR)
                .fg(NORD_CYAN)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, chunks[1], list_state);

    // ヘルプバー
    let help = Line::from(vec![
        Span::styled(" Enter", Style::default().fg(NORD_CYAN)),
        Span::styled(": 選択  ", Style::default().fg(NORD_COMMENT)),
        Span::styled("j/k", Style::default().fg(NORD_CYAN)),
        Span::styled(": 移動  ", Style::default().fg(NORD_COMMENT)),
        Span::styled("q", Style::default().fg(NORD_CYAN)),
        Span::styled(": 終了", Style::default().fg(NORD_COMMENT)),
    ]);
    frame.render_widget(
        Paragraph::new(help).style(Style::default().bg(NORD_POLAR)),
        chunks[2],
    );
}

/// バックグラウンドサービス起動（Process サーバー + Canvas）
fn start_background_services(
    project_dir: &str,
    config: &Config,
    project_index: usize,
    project_name: &str,
) -> Result<()> {
    let port = crate::resolve::port_for_configured(project_index, config)?;

    // ポート予約: Process サーバー起動前に running.json へ仮登録
    let my_pid = std::process::id();
    if let Err(e) = crate::config::RunningProcesses::register(port, project_dir, my_pid, None) {
        tracing::warn!("Failed to pre-register port in running.json: {}", e);
    }

    let cap_config = crate::process::CapabilityConfig {
        project_dir: project_dir.to_string(),
        midi_config: None,
        bonjour_port: Some(port),
    };

    crate::commands::start::ensure_process_running(
        port,
        project_dir,
        crate::protocol::DebugMode::None,
        cap_config,
    )?;

    if let Err(e) = crate::canvas::run_canvas_detached(port, project_name) {
        tracing::warn!("Canvas 自動起動失敗: {}", e);
    }

    Ok(())
}

// =============================================================================
// Claude CLI セッション
// =============================================================================

/// Claude CLI PTY セッション
fn run_claude_session(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    project_dir: &str,
    project_name: &str,
) -> Result<()> {
    let size = terminal.size()?;
    // 枠線（左右各1セル）+ ステータスバー（1行）+ 枠線（上下各1セル）
    let pty_cols = (size.width.saturating_sub(2) as usize).max(1);
    let pty_lines = (size.height.saturating_sub(3) as usize).max(1);

    // VT パーサー
    let term_state = Arc::new(Mutex::new(TerminalState::new(pty_cols, pty_lines)));

    // PTY 起動
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: pty_lines as u16,
        cols: pty_cols as u16,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Claude CLI コマンド構築
    let mut cmd = CommandBuilder::new("claude");
    cmd.cwd(project_dir);
    cmd.env("TERM", "xterm-256color");

    // セッション復帰: --continue で前回セッションを自動復帰
    cmd.arg("--continue");

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    // PTY reader → TerminalState
    let reader = pair.master.try_clone_reader()?;
    let term_state_reader = Arc::clone(&term_state);
    let reader_handle = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let mut state = term_state_reader.lock().unwrap();
                    state.feed_bytes(&buf[..n]);
                }
                Err(_) => break,
            }
        }
    });

    let mut writer = pair.master.take_writer()?;

    // メインループ
    loop {
        if let Ok(Some(_status)) = child.try_wait() {
            break;
        }

        // 描画
        {
            let state = term_state.lock().unwrap();
            let snapshot = state.snapshot();
            let display_offset = state.display_offset();
            drop(state);

            terminal.draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Min(1),    // ターミナル pane
                        Constraint::Length(1), // ステータスバー
                    ])
                    .split(frame.area());

                // Pane 枠線 + タイトル
                let focus_icon = "\u{1F7E2}"; // 🟢
                let pane_title = format!(" {} Claude CLI ", focus_icon);

                let mut block = Block::default()
                    .title(pane_title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(NORD_CYAN));

                // スクロール中はオフセットインジケータ表示（右上）
                if display_offset > 0 {
                    block = block.title_top(
                        ratatui::text::Line::from(format!(" \u{2191}{} ", display_offset))
                            .alignment(ratatui::layout::Alignment::Right),
                    );
                }

                let inner = block.inner(chunks[0]);
                frame.render_widget(block, chunks[0]);
                frame.render_widget(TerminalView::new(&snapshot), inner);

                draw_status_bar(frame, chunks[1], project_name);
            })?;
        }

        // イベント処理
        if event::poll(Duration::from_millis(16))? {
            let app_cursor = {
                let state = term_state.lock().unwrap();
                state.app_cursor_mode()
            };

            match event::read()? {
                Event::Key(key) => {
                    // Ctrl+Q: 終了
                    if key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }

                    // PageUp/PageDown: TUI スクロールバック
                    match key.code {
                        KeyCode::PageUp => {
                            let mut state = term_state.lock().unwrap();
                            state.scroll_display(Scroll::PageUp);
                        }
                        KeyCode::PageDown => {
                            let mut state = term_state.lock().unwrap();
                            state.scroll_display(Scroll::PageDown);
                        }
                        _ => {
                            // その他のキー: PTY に転送 + スクロール位置を底に戻す
                            let bytes = key_to_pty_bytes(key, app_cursor);
                            if !bytes.is_empty() {
                                {
                                    let mut state = term_state.lock().unwrap();
                                    if state.display_offset() > 0 {
                                        state.scroll_display(Scroll::Bottom);
                                    }
                                }
                                let _ = std::io::Write::write_all(&mut writer, &bytes);
                            }
                        }
                    }
                }
                // マウスホイール → TUI スクロールバック制御
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        let mut state = term_state.lock().unwrap();
                        state.scroll_display(Scroll::Delta(3));
                    }
                    MouseEventKind::ScrollDown => {
                        let mut state = term_state.lock().unwrap();
                        state.scroll_display(Scroll::Delta(-3));
                    }
                    _ => {}
                },
                Event::Resize(cols, rows) => {
                    // 枠線 + ステータスバー分を引く
                    let new_cols = (cols.saturating_sub(2) as usize).max(1);
                    let new_lines = (rows.saturating_sub(3) as usize).max(1);
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

    // PTY クリーンアップ: master fd を閉じて reader スレッドに EOF を通知
    drop(writer);
    drop(pair.master);
    child.kill().ok();
    child.wait().ok();
    let _ = reader_handle.join();
    Ok(())
}

/// ステータスバー描画
fn draw_status_bar(frame: &mut ratatui::Frame, area: Rect, project_name: &str) {
    let status = Line::from(vec![
        Span::styled(
            format!(" {} ", project_name),
            Style::default()
                .fg(NORD_BG)
                .bg(NORD_CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Claude CLI ", Style::default().fg(NORD_FG).bg(NORD_POLAR)),
        Span::styled(
            " --continue ",
            Style::default().fg(NORD_GREEN).bg(NORD_POLAR),
        ),
        Span::styled(
            " Ctrl+Q: quit ",
            Style::default().fg(NORD_COMMENT).bg(NORD_POLAR),
        ),
    ]);
    let bar = Paragraph::new(status).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}
