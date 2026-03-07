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
use super::session::{ClaudeSession, SessionMode, list_sessions};
use super::terminal_widget::TerminalView;

// Arctic Nord カラー定数
const NORD_BG: Color = Color::Rgb(11, 17, 32); // #0B1120
const NORD_FG: Color = Color::Rgb(216, 222, 233); // #D8DEE9
const NORD_CYAN: Color = Color::Rgb(136, 192, 208); // #88C0D0
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
        // ポート解決: running.json から取得、なければ config index ベース
        let config = Config::load()?;
        let port = if let Some(running) = crate::config::RunningProcesses::find_by_project(&dir) {
            running.port
        } else if let Some(idx) = config.find_project_index(&dir) {
            crate::resolve::port_for_configured(idx, &config)?
        } else {
            33000 // フォールバック
        };
        run_claude_session(&mut terminal, &dir, &name, port)
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

                            // ポート取得
                            let port = crate::resolve::port_for_configured(idx, &config)
                                .unwrap_or(33000 + idx as u16);

                            // Claude セッション開始
                            return run_claude_session(terminal, &dir, &name, port);
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

/// バックグラウンドサービス起動（Process サーバーのみ、Canvas は Ctrl+O でオンデマンド起動）
fn start_background_services(
    project_dir: &str,
    config: &Config,
    project_index: usize,
    _project_name: &str,
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

    Ok(())
}

// =============================================================================
// セッション選択画面
// =============================================================================

/// セッション選択画面（前回の続き / 新規 / 一覧から選択）
fn run_session_select(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    project_dir: &str,
) -> Result<SessionMode> {
    let sessions = list_sessions(project_dir);

    // セッションが 0 件なら新規で開始
    if sessions.is_empty() {
        return Ok(SessionMode::New);
    }

    // 選択肢を構築: [続き] + [新規] + 過去セッション一覧
    let mut list_state = ListState::default();
    list_state.select(Some(0)); // デフォルトは「前回の続き」

    loop {
        let sessions_ref = &sessions;
        terminal.draw(|frame| {
            draw_session_select(frame, sessions_ref, &mut list_state);
        })?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Enter => {
                        let idx = list_state.selected().unwrap_or(0);
                        return Ok(match idx {
                            0 => SessionMode::Continue,
                            1 => SessionMode::New,
                            n => SessionMode::Resume(sessions[n - 2].id.clone()),
                        });
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let i = list_state.selected().unwrap_or(0);
                        let total = sessions.len() + 2; // +2 for Continue & New
                        let new = if i == 0 { total - 1 } else { i - 1 };
                        list_state.select(Some(new));
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let i = list_state.selected().unwrap_or(0);
                        let total = sessions.len() + 2;
                        let new = if i >= total - 1 { 0 } else { i + 1 };
                        list_state.select(Some(new));
                    }
                    // Esc: 前回の続き（デフォルト動作）
                    KeyCode::Esc => return Ok(SessionMode::Continue),
                    // Ctrl+Q: 終了
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        anyhow::bail!("User quit from session select");
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

/// セッション選択画面の描画
fn draw_session_select(
    frame: &mut ratatui::Frame,
    sessions: &[ClaudeSession],
    list_state: &mut ListState,
) {
    let area = frame.area();

    frame.render_widget(Block::default().style(Style::default().bg(NORD_BG)), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // タイトル
            Constraint::Min(1),    // セッションリスト
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
        Span::styled(" — セッション選択", Style::default().fg(NORD_FG)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(NORD_COMMENT)),
    );
    frame.render_widget(title, chunks[0]);

    // セッションリスト構築
    let mut items: Vec<ListItem> = Vec::new();

    // [0] 前回の続き
    items.push(ListItem::new(Line::from(vec![
        Span::styled(" ▶ ", Style::default().fg(NORD_GREEN)),
        Span::styled(
            "前回の続き",
            Style::default().fg(NORD_FG).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" (--continue)", Style::default().fg(NORD_COMMENT)),
    ])));

    // [1] 新規セッション
    items.push(ListItem::new(Line::from(vec![
        Span::styled(" + ", Style::default().fg(NORD_CYAN)),
        Span::styled("新規セッション", Style::default().fg(NORD_FG)),
    ])));

    // [2..] 過去セッション
    for session in sessions.iter().take(20) {
        let elapsed = session
            .modified
            .elapsed()
            .map(format_elapsed)
            .unwrap_or_else(|_| "?".to_string());

        let summary_text = if session.summary.is_empty() {
            "(no messages)".to_string()
        } else {
            session.summary.clone()
        };

        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!(" {} ", elapsed), Style::default().fg(NORD_COMMENT)),
            Span::styled(summary_text, Style::default().fg(NORD_FG)),
            Span::styled(
                format!("  ~{}msgs", session.message_count),
                Style::default().fg(NORD_COMMENT),
            ),
        ])));
    }

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
        Span::styled("Esc", Style::default().fg(NORD_CYAN)),
        Span::styled(": 前回の続き", Style::default().fg(NORD_COMMENT)),
    ]);
    frame.render_widget(
        Paragraph::new(help).style(Style::default().bg(NORD_POLAR)),
        chunks[2],
    );
}

/// 経過時間を人間に読みやすい形式で表示
fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

// =============================================================================
// Claude CLI セッション
// =============================================================================

/// Canvas の表示/非表示をトグル（Unison QUIC 経由）
fn toggle_canvas(port: u16, canvas_open: &mut bool) {
    let method = if *canvas_open {
        "close_canvas"
    } else {
        "open_canvas"
    };

    // 別スレッド + mini runtime で QUIC リクエスト（TUI をブロックしない）
    let method = method.to_string();
    let result = std::thread::spawn(move || -> bool {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return false;
        };
        rt.block_on(async {
            let quic_port = port + crate::process::unison_server::QUIC_PORT_OFFSET;
            let addr = format!("[::1]:{}", quic_port);

            let Ok(client) = unison::ProtocolClient::new_default() else {
                return false;
            };
            let ok = tokio::time::timeout(Duration::from_secs(2), async {
                client.connect(&addr).await.ok()?;
                let channel = client.open_channel("process").await.ok()?;
                channel.request(&method, serde_json::json!({})).await.ok()?;
                Some(())
            })
            .await;
            matches!(ok, Ok(Some(())))
        })
    })
    .join();

    if let Ok(true) = result {
        *canvas_open = !*canvas_open;
    }
}

/// Claude CLI PTY セッション
fn run_claude_session(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    project_dir: &str,
    project_name: &str,
    port: u16,
) -> Result<()> {
    // セッション選択（PTY 起動前に行う — bail しても PTY リーク しない）
    let session_mode = run_session_select(terminal, project_dir)?;

    let size = terminal.size()?;
    // 枠線（左右各1セル）+ ヘッダ（1行）+ フッター（1行）+ 枠線（上下各1セル）
    let pty_cols = (size.width.saturating_sub(2) as usize).max(1);
    let pty_lines = (size.height.saturating_sub(4) as usize).max(1);

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

    // VP が権限管理を担うため、Claude CLI 側の権限確認をスキップ
    cmd.arg("--dangerously-skip-permissions");

    // セッションモードに応じた引数
    match &session_mode {
        SessionMode::Continue => {
            cmd.arg("--continue");
        }
        SessionMode::New => {
            // 引数なし = 新規セッション
        }
        SessionMode::Resume(id) => {
            cmd.arg("--resume");
            cmd.arg(id);
        }
    }

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

    // Canvas 状態トラッキング（Ctrl+O でオンデマンド起動）
    let mut canvas_open = false;

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
                        Constraint::Length(1), // ヘッダバー
                        Constraint::Min(1),    // ターミナル pane
                        Constraint::Length(1), // フッターバー
                    ])
                    .split(frame.area());

                // ヘッダバー
                draw_header_bar(frame, chunks[0], project_name, port, canvas_open);

                // Pane 枠線 + タイトル
                let mut block = Block::default()
                    .title(" Claude CLI ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(NORD_CYAN));

                // スクロール中はオフセットインジケータ表示（右上）
                if display_offset > 0 {
                    block = block.title_top(
                        ratatui::text::Line::from(format!(" \u{2191}{} ", display_offset))
                            .alignment(ratatui::layout::Alignment::Right),
                    );
                }

                let inner = block.inner(chunks[1]);
                frame.render_widget(block, chunks[1]);
                frame.render_widget(TerminalView::new(&snapshot), inner);

                // ハードウェアカーソルを PTY のカーソル位置に配置
                let (crow, ccol) = snapshot.cursor;
                let cx = inner.x + ccol as u16;
                let cy = inner.y + crow as u16;
                if cx < inner.right() && cy < inner.bottom() {
                    frame.set_cursor_position(ratatui::layout::Position::new(cx, cy));
                }

                draw_footer_bar(frame, chunks[2]);
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

                    // Ctrl+O: Canvas 表示/非表示トグル
                    if key.code == KeyCode::Char('o')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        toggle_canvas(port, &mut canvas_open);
                        continue;
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
                    // 枠線 + ヘッダ + フッター分を引く
                    let new_cols = (cols.saturating_sub(2) as usize).max(1);
                    let new_lines = (rows.saturating_sub(4) as usize).max(1);
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

/// ヘッダバー描画
fn draw_header_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    project_name: &str,
    port: u16,
    canvas_open: bool,
) {
    let canvas_indicator = if canvas_open {
        Span::styled(
            " Canvas:ON ",
            Style::default().fg(NORD_GREEN).bg(NORD_POLAR),
        )
    } else {
        Span::styled(
            " Canvas:OFF ",
            Style::default().fg(NORD_COMMENT).bg(NORD_POLAR),
        )
    };

    let header = Line::from(vec![
        Span::styled(
            " VP ",
            Style::default()
                .fg(NORD_BG)
                .bg(NORD_CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", project_name),
            Style::default()
                .fg(NORD_FG)
                .bg(NORD_POLAR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" :{} ", port),
            Style::default().fg(NORD_COMMENT).bg(NORD_POLAR),
        ),
        Span::styled("│", Style::default().fg(NORD_COMMENT).bg(NORD_POLAR)),
        canvas_indicator,
    ]);
    let bar = Paragraph::new(header).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}

/// フッターバー描画（ショートカットのみ）
fn draw_footer_bar(frame: &mut ratatui::Frame, area: Rect) {
    let footer = Line::from(vec![
        Span::styled(" C-o", Style::default().fg(NORD_CYAN).bg(NORD_POLAR)),
        Span::styled(" canvas ", Style::default().fg(NORD_COMMENT).bg(NORD_POLAR)),
        Span::styled(" C-q", Style::default().fg(NORD_CYAN).bg(NORD_POLAR)),
        Span::styled(" quit ", Style::default().fg(NORD_COMMENT).bg(NORD_POLAR)),
        Span::styled(" PgUp/Dn", Style::default().fg(NORD_CYAN).bg(NORD_POLAR)),
        Span::styled(" scroll ", Style::default().fg(NORD_COMMENT).bg(NORD_POLAR)),
    ]);
    let bar = Paragraph::new(footer).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}
