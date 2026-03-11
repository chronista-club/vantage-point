//! TUI アプリケーションメインループ
//!
//! マルチプロジェクト対応: 複数プロジェクトを動的に追加/切替しながら
//! Claude CLI PTY セッションを管理する。

use std::io;
use std::time::Duration;

use alacritty_terminal::grid::Scroll;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, ListState};

use crate::config::{Config, ProjectConfig};

use super::bridge::BridgeCommand;
use super::draw::{
    calc_pty_size, draw_footer_bar, draw_header_bar, draw_project_select, draw_session_select,
};
use super::input::key_to_pty_bytes;
use super::overlay::{OverlayKind, draw_overlay};
use super::project_context::ProjectContext;
use super::session::{SessionMode, list_sessions};
use super::terminal_widget::TerminalView;
use super::theme::*;

// =============================================================================
// MultiProjectApp — マルチプロジェクト TUI アプリ
// =============================================================================

/// マルチプロジェクト TUI アプリ
struct MultiProjectApp {
    projects: Vec<ProjectContext>,
    active_idx: usize,
    overlay: Option<OverlayKind>,
    config: Config,
    was_ai_busy: bool,
}

impl MultiProjectApp {
    fn new(config: Config) -> Self {
        Self {
            projects: Vec::new(),
            active_idx: 0,
            overlay: None,
            config,
            was_ai_busy: false,
        }
    }

    /// プロジェクトを追加して起動する
    fn add_project(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        name: String,
        dir: String,
        port: u16,
    ) -> Result<usize> {
        let session_mode = run_session_select(terminal, &dir)?;
        let size = terminal.size()?;
        let (pty_cols, pty_lines) = calc_pty_size(size.width, size.height);

        let ctx = ProjectContext::new(name, dir, port, session_mode, pty_cols, pty_lines)?;
        self.projects.push(ctx);
        Ok(self.projects.len() - 1)
    }

    /// アクティブプロジェクトを切り替える
    fn switch_to(&mut self, idx: usize) {
        if idx < self.projects.len() {
            self.active_idx = idx;
            self.projects[idx].notifications = 0;
            set_terminal_title(&format!("VP: {}", self.projects[idx].name));
        }
    }

    /// プロジェクトスイッチャーオーバーレイを開く
    fn open_project_switcher(&mut self) {
        let items: Vec<(ProjectConfig, bool)> = self
            .config
            .projects
            .iter()
            .map(|p| {
                let dir = Config::normalize_path(std::path::Path::new(&p.path));
                let active = self.projects.iter().any(|ctx| ctx.dir == dir);
                (p.clone(), active)
            })
            .collect();

        let mut list_state = ListState::default();
        if let Some(active_ctx) = self.projects.get(self.active_idx) {
            if let Some(pos) = items.iter().position(|(p, _)| {
                Config::normalize_path(std::path::Path::new(&p.path)) == active_ctx.dir
            }) {
                list_state.select(Some(pos));
            } else {
                list_state.select(Some(0));
            }
        } else {
            list_state.select(Some(0));
        }

        self.overlay = Some(OverlayKind::ProjectSwitcher { list_state, items });
    }

    // =========================================================================
    // メインループ
    // =========================================================================

    fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        let mut needs_redraw = true;

        loop {
            // 全プロジェクトのイベントをポーリング
            for i in 0..self.projects.len() {
                let changed = self.projects[i].poll_events();
                if changed {
                    if i != self.active_idx {
                        self.projects[i].notifications += 1;
                    }
                    needs_redraw = true;
                }
            }

            // 切断されたアクティブプロジェクトを除去
            if let Some(ctx) = self.projects.get(self.active_idx) {
                if ctx.disconnected {
                    self.projects.remove(self.active_idx);
                    if self.projects.is_empty() {
                        return Ok(());
                    }
                    if self.active_idx >= self.projects.len() {
                        self.active_idx = self.projects.len() - 1;
                    }
                    set_terminal_title(&format!("VP: {}", self.projects[self.active_idx].name));
                    needs_redraw = true;
                    continue;
                }
            } else {
                return Ok(());
            }

            // AI 状態遷移検知
            if let Some(ctx) = self.projects.get(self.active_idx) {
                let is_ai_busy = ctx.is_ai_busy();
                if self.was_ai_busy != is_ai_busy {
                    self.was_ai_busy = is_ai_busy;
                    needs_redraw = true;
                }
            }

            // 描画
            if needs_redraw {
                needs_redraw = false;
                self.draw(terminal)?;
            }

            // イベント処理
            if event::poll(Duration::from_millis(16))? {
                let handled = match event::read()? {
                    Event::Key(key) => {
                        needs_redraw = true;
                        self.handle_key(key, terminal)?
                    }
                    Event::Paste(text) => {
                        if self.overlay.is_none() {
                            self.handle_paste(&text);
                        }
                        true
                    }
                    Event::Mouse(mouse) => {
                        if self.overlay.is_none() {
                            self.handle_mouse(mouse)
                        } else {
                            false
                        }
                    }
                    Event::Resize(cols, rows) => {
                        self.handle_resize(cols, rows);
                        true
                    }
                    _ => false,
                };
                if handled {
                    needs_redraw = true;
                }
            }
        }
    }

    // =========================================================================
    // キー入力
    // =========================================================================

    fn handle_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<bool> {
        // オーバーレイが開いている場合
        if self.overlay.is_some() {
            return self.handle_overlay_key(key, terminal);
        }

        // Ctrl+Q: 終了
        if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
            anyhow::bail!("quit");
        }

        // Ctrl+P: プロジェクトスイッチャー
        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.open_project_switcher();
            return Ok(true);
        }

        // Ctrl+1~9: プロジェクト直接切替
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char(c) = key.code {
                if let Some(digit) = c.to_digit(10) {
                    if digit >= 1 && (digit as usize) <= self.projects.len() {
                        self.switch_to((digit - 1) as usize);
                        return Ok(true);
                    }
                }
            }
        }

        // Ctrl+←/→: プロジェクト切替
        if key.modifiers.contains(KeyModifiers::CONTROL) && self.projects.len() > 1 {
            let switch_to = match key.code {
                KeyCode::Right => Some((self.active_idx + 1) % self.projects.len()),
                KeyCode::Left => Some(if self.active_idx == 0 {
                    self.projects.len() - 1
                } else {
                    self.active_idx - 1
                }),
                _ => None,
            };
            if let Some(idx) = switch_to {
                self.switch_to(idx);
                return Ok(true);
            }
        }

        let Some(ctx) = self.projects.get_mut(self.active_idx) else {
            return Ok(false);
        };

        // Ctrl+N: 新規ペイン
        if key.code == KeyCode::Char('n') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if crate::tmux::is_inside_tmux() {
                let _ = ctx.cmd_tx.send(BridgeCommand::TmuxSplit {
                    horizontal: true,
                    command: None,
                });
            } else {
                let size = terminal.size()?;
                let (cols, rows) = calc_pty_size(size.width, size.height);
                let _ = ctx.cmd_tx.send(BridgeCommand::CreateSession {
                    cols: cols as u16,
                    rows: rows as u16,
                    command: vec![
                        "claude".to_string(),
                        "--dangerously-skip-permissions".to_string(),
                    ],
                });
            }
            return Ok(true);
        }

        // Ctrl+Shift+←/→: セッション切替（同一プロジェクト内）
        if key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
            && ctx.sessions.len() > 1
        {
            let switch_to = match key.code {
                KeyCode::Right => Some((ctx.current_session_idx + 1) % ctx.sessions.len()),
                KeyCode::Left => Some(if ctx.current_session_idx == 0 {
                    ctx.sessions.len() - 1
                } else {
                    ctx.current_session_idx - 1
                }),
                _ => None,
            };
            if let Some(idx) = switch_to {
                let sid = ctx.sessions[idx].clone();
                let _ = ctx.cmd_tx.send(BridgeCommand::SwitchSession(sid));
                return Ok(true);
            }
        }

        // Home: PP window トグル（TheWorld フォールバック付き）
        if key.code == KeyCode::Home {
            toggle_pp_window(ctx.port);
            return Ok(true);
        }

        // PageUp/PageDown: スクロールバック
        match key.code {
            KeyCode::PageUp => {
                let mut state = ctx.term_state.lock().unwrap();
                state.scroll_display(Scroll::PageUp);
                return Ok(true);
            }
            KeyCode::PageDown => {
                let mut state = ctx.term_state.lock().unwrap();
                state.scroll_display(Scroll::PageDown);
                return Ok(true);
            }
            _ => {}
        }

        // その他: PTY に転送
        let app_cursor = {
            let state = ctx.term_state.lock().unwrap();
            state.app_cursor_mode()
        };
        let bytes = key_to_pty_bytes(key, app_cursor);
        if !bytes.is_empty() {
            {
                let mut state = ctx.term_state.lock().unwrap();
                if state.display_offset() > 0 {
                    state.scroll_display(Scroll::Bottom);
                }
            }
            let _ = ctx.cmd_tx.send(BridgeCommand::Input(bytes));
        }

        Ok(true)
    }

    /// オーバーレイ内のキー入力処理
    fn handle_overlay_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<bool> {
        let overlay = self.overlay.as_mut().unwrap();
        match overlay {
            OverlayKind::ProjectSwitcher {
                list_state,
                items,
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('p')
                    if key.code == KeyCode::Esc
                        || key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.overlay = None;
                }
                KeyCode::Up | KeyCode::Char('k') if !items.is_empty() => {
                    let i = list_state.selected().unwrap_or(0);
                    let new = if i == 0 { items.len() - 1 } else { i - 1 };
                    list_state.select(Some(new));
                }
                KeyCode::Down | KeyCode::Char('j') if !items.is_empty() => {
                    let i = list_state.selected().unwrap_or(0);
                    let new = if i >= items.len() - 1 { 0 } else { i + 1 };
                    list_state.select(Some(new));
                }
                KeyCode::Enter => {
                    if let Some(idx) = list_state.selected() {
                        let (project, already_active) = &items[idx];
                        let dir = Config::normalize_path(std::path::Path::new(&project.path));
                        let name = project.name.clone();

                        if *already_active {
                            if let Some(pos) =
                                self.projects.iter().position(|ctx| ctx.dir == dir)
                            {
                                self.overlay = None;
                                self.switch_to(pos);
                                return Ok(true);
                            }
                        } else {
                            let config = self.config.clone();
                            if let Some(project_idx) = config.find_project_index(&dir) {
                                start_background_services(
                                    &dir, &config, project_idx, &name,
                                )
                                .ok();
                                let port =
                                    crate::resolve::port_for_configured(project_idx, &config)
                                        .unwrap_or(33000 + project_idx as u16);

                                self.overlay = None;

                                match self.add_project(terminal, name, dir, port) {
                                    Ok(new_idx) => {
                                        self.switch_to(new_idx);
                                    }
                                    Err(e) => {
                                        tracing::error!("プロジェクト追加失敗: {}", e);
                                    }
                                }
                                return Ok(true);
                            }
                        }
                        self.overlay = None;
                    }
                }
                _ => {}
            },
        }
        Ok(true)
    }

    // =========================================================================
    // その他の入力
    // =========================================================================

    fn handle_paste(&mut self, text: &str) {
        let Some(ctx) = self.projects.get_mut(self.active_idx) else {
            return;
        };
        let bracketed = {
            let state = ctx.term_state.lock().unwrap();
            state.bracketed_paste_mode()
        };
        let mut bytes = Vec::new();
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(text.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        if !bytes.is_empty() {
            {
                let mut state = ctx.term_state.lock().unwrap();
                if state.display_offset() > 0 {
                    state.scroll_display(Scroll::Bottom);
                }
            }
            let _ = ctx.cmd_tx.send(BridgeCommand::Input(bytes));
        }
    }

    fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        let Some(ctx) = self.projects.get_mut(self.active_idx) else {
            return false;
        };
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                let mut state = ctx.term_state.lock().unwrap();
                state.scroll_display(Scroll::Delta(3));
                true
            }
            MouseEventKind::ScrollDown => {
                let mut state = ctx.term_state.lock().unwrap();
                state.scroll_display(Scroll::Delta(-3));
                true
            }
            _ => false,
        }
    }

    fn handle_resize(&mut self, cols: u16, rows: u16) {
        if let Some(ctx) = self.projects.get_mut(self.active_idx) {
            let (new_cols, new_lines) = calc_pty_size(cols, rows);
            ctx.resize(new_cols, new_lines);
        }
    }

    // =========================================================================
    // 描画
    // =========================================================================

    fn draw(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        let active_idx = self.active_idx;

        let (snapshot, display_offset, session_count, session_idx, port, ai_busy, bridge_status) = {
            let ctx = &self.projects[active_idx];
            let state = ctx.term_state.lock().unwrap();
            (
                state.snapshot(),
                state.display_offset(),
                ctx.sessions.len(),
                ctx.current_session_idx,
                ctx.port,
                ctx.is_ai_busy(),
                ctx.bridge_status.clone(),
            )
        };

        let tab_info: Vec<(String, bool, u32)> = self
            .projects
            .iter()
            .enumerate()
            .map(|(i, ctx)| (ctx.name.clone(), i == active_idx, ctx.notifications))
            .collect();

        let project_count = self.projects.len();
        let overlay = &self.overlay;
        terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(frame.area());

            draw_header_bar(
                frame, chunks[0], &tab_info, port, ai_busy, &bridge_status,
            );

            let main_area = chunks[1];
            let pty_area = main_area;

            // PTY ペイン
            let session_label = if session_count > 1 {
                format!(" Claude CLI [{}/{}] ", session_idx + 1, session_count)
            } else {
                " Claude CLI ".to_string()
            };
            let mut pty_block = Block::default()
                .title(session_label)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(NORD_CYAN));

            if display_offset > 0 {
                pty_block = pty_block.title_top(
                    ratatui::text::Line::from(format!(" \u{2191}{} ", display_offset))
                        .alignment(ratatui::layout::Alignment::Right),
                );
            }

            let pty_inner = pty_block.inner(pty_area);
            frame.render_widget(pty_block, pty_area);
            frame.render_widget(TerminalView::new(&snapshot), pty_inner);

            draw_footer_bar(frame, chunks[2], project_count, session_count);

            // オーバーレイ
            if let Some(overlay) = overlay {
                draw_overlay(frame, main_area, overlay);
            }
        })?;

        // IME 候補ウィンドウ用にカーソル位置を設定（非表示のまま）
        // ratatui の draw 後に crossterm で直接操作することで、
        // カーソルを見せずに IME に位置情報だけ伝える
        if self.overlay.is_none() {
            let ctx = &self.projects[active_idx];
            let state = ctx.term_state.lock().unwrap();
            let snap = state.snapshot();
            let (crow, ccol) = snap.cursor;
            // pty_inner 相当の座標計算（ヘッダー1行 + ボーダー1行）
            let cx = 1 + ccol as u16;
            let cy = 2 + crow as u16;
            crossterm::execute!(
                io::stdout(),
                crossterm::cursor::MoveTo(cx, cy),
                crossterm::cursor::Hide
            )?;
        }

        Ok(())
    }
}

// =============================================================================
// パブリック API
// =============================================================================

/// TUI メインエントリー（プロジェクト指定あり）
pub fn run_tui(project_dir: &str, project_name: &str) -> Result<()> {
    run_tui_inner(Some((project_dir.to_string(), project_name.to_string())))
}

/// TUI メインエントリー（プロジェクト選択画面から開始）
pub fn run_tui_select(config: &Config) -> Result<()> {
    if config.projects.is_empty() {
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
        EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture
    )?;

    if let Some((_, ref name)) = resolved {
        set_terminal_title(&format!("VP: {}", name));
    } else {
        set_terminal_title("Vantage Point");
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let config = Config::load()?;
    let mut app = MultiProjectApp::new(config.clone());

    let result = if let Some((dir, name)) = resolved {
        let port = if let Some(running) = crate::discovery::find_by_project_blocking(&dir) {
            running.port
        } else if let Some(idx) = config.find_project_index(&dir) {
            crate::resolve::port_for_configured(idx, &config)?
        } else {
            33000
        };

        match app.add_project(&mut terminal, name, dir, port) {
            Ok(idx) => {
                app.switch_to(idx);
                app.run(&mut terminal)
            }
            Err(e) => Err(e),
        }
    } else {
        match run_project_select_for_multi(&mut terminal, &config) {
            Ok(Some((dir, name, port))) => {
                set_terminal_title(&format!("VP: {}", name));
                match app.add_project(&mut terminal, name, dir, port) {
                    Ok(idx) => {
                        app.switch_to(idx);
                        app.run(&mut terminal)
                    }
                    Err(e) => Err(e),
                }
            }
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        }
    };

    // 終了処理
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    match result {
        Err(e) if e.to_string() == "quit" => Ok(()),
        other => other,
    }
}

// =============================================================================
// 初回プロジェクト選択
// =============================================================================

fn run_project_select_for_multi(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &Config,
) -> Result<Option<(String, String, u16)>> {
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

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Enter => {
                    if let Some(idx) = list_state.selected() {
                        let project = &projects[idx];
                        let dir = Config::normalize_path(std::path::Path::new(&project.path));
                        let name = project.name.clone();
                        start_background_services(&dir, config, idx, &name).ok();
                        let port = crate::resolve::port_for_configured(idx, config)
                            .unwrap_or(33000 + idx as u16);
                        return Ok(Some((dir, name, port)));
                    }
                }
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
                KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                _ => {}
            }
        }
    }
}

// =============================================================================
// セッション選択
// =============================================================================

fn run_session_select(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    project_dir: &str,
) -> Result<SessionMode> {
    let sessions = list_sessions(project_dir);

    if sessions.is_empty() {
        return Ok(SessionMode::New);
    }

    let mut list_state = ListState::default();
    list_state.select(Some(0));

    loop {
        let sessions_ref = &sessions;
        terminal.draw(|frame| {
            draw_session_select(frame, sessions_ref, &mut list_state);
        })?;

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
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
                    let total = sessions.len() + 2;
                    let new = if i == 0 { total - 1 } else { i - 1 };
                    list_state.select(Some(new));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = list_state.selected().unwrap_or(0);
                    let total = sessions.len() + 2;
                    let new = if i >= total - 1 { 0 } else { i + 1 };
                    list_state.select(Some(new));
                }
                KeyCode::Esc => return Ok(SessionMode::Continue),
                KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    anyhow::bail!("User quit from session select");
                }
                _ => {}
            }
        }
    }
}

// =============================================================================
// ヘルパー
// =============================================================================

fn start_background_services(
    project_dir: &str,
    config: &Config,
    project_index: usize,
    _project_name: &str,
) -> Result<()> {
    let port = crate::resolve::port_for_configured(project_index, config)?;
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

/// PP Window（Paisley Park）のトグル
///
/// TheWorld 稼働中 → Lane モード、未稼働 → 個別ポートで Canvas 起動。
/// TUI インライン分割は廃止 — 常に外部ウィンドウ。
fn toggle_pp_window(sp_port: u16) {
    if crate::canvas::find_running_canvas().is_some() {
        crate::canvas::stop_canvas();
    } else {
        let (port, lanes) = crate::canvas::canvas_target(sp_port);
        let _ = crate::canvas::ensure_canvas_running(port, lanes);
    }
}
