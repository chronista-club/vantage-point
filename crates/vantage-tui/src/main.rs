use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

mod claude_cli;
mod daemon;
mod session;

use claude_cli::{ClaudeCli, ClaudeEvent};
use session::{Session, SessionStore};

#[derive(Debug, Clone)]
struct Message {
    role: String,
    content: String,
}

/// バックエンドモード
#[derive(Debug, Clone)]
enum BackendMode {
    ClaudeCli { model: String, tools_count: usize, mcp_count: usize },
    Initializing,
}

/// デバッグモード (Off → Simple → Detail → Off)
#[derive(Debug, Clone, Copy, PartialEq)]
enum DebugMode {
    Off,
    Simple,  // 基本情報のみ
    Detail,  // コスト、ツール詳細など
}

struct App {
    input: String,
    input_cursor: usize,
    messages: Vec<Message>,
    claude: ClaudeCli,
    backend_mode: BackendMode,
    is_loading: bool,
    scroll: u16,
    session: Session,
    session_store: SessionStore,
    project_dir: String,
    pending_prompt: Option<String>,
    debug_mode: DebugMode,
    daemon_process: Option<std::process::Child>,
}

impl App {
    fn new() -> Result<Self> {
        dotenvy::dotenv().ok();

        let session_store = SessionStore::new();
        let session = Session::new();

        // Get project directory from current directory or env
        let project_dir = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());

        Ok(Self {
            input: String::new(),
            input_cursor: 0,
            messages: vec![Message {
                role: "system".to_string(),
                content: format!(
                    "🎯 Vantage Point Agent\n📁 Project: {}\n⏳ 準備完了",
                    project_dir
                ),
            }],
            claude: ClaudeCli::new(),
            backend_mode: BackendMode::Initializing,
            is_loading: false,
            scroll: 0,
            session,
            session_store,
            project_dir,
            pending_prompt: None,
            debug_mode: DebugMode::Detail, // 開発中はDetail
            daemon_process: None,
        })
    }

    fn save_session(&mut self) -> Result<String> {
        // Sync messages to session
        for msg in self.messages.iter() {
            if !self.session.messages.iter().any(|m| m.content == msg.content) {
                self.session.add_message(&msg.role, &msg.content);
            }
        }
        let path = self.session_store.save(&self.session)?;
        Ok(path.display().to_string())
    }

    fn resume_latest(&mut self) -> Result<()> {
        if let Some(session) = self.session_store.get_latest() {
            self.session = session.clone();
            self.messages.clear();
            self.messages.push(Message {
                role: "system".to_string(),
                content: format!("セッション再開: {}", session.id),
            });
            for msg in &session.messages {
                self.messages.push(Message {
                    role: msg.role.clone(),
                    content: msg.content.clone(),
                });
            }
        }
        Ok(())
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.messages.push(Message {
            role: role.to_string(),
            content: content.to_string(),
        });
    }

    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.input_cursor.saturating_sub(1);
        self.input_cursor = self.clamp_cursor(cursor_moved_left);
    }

    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.input_cursor.saturating_add(1);
        self.input_cursor = self.clamp_cursor(cursor_moved_right);
    }

    fn enter_char(&mut self, new_char: char) {
        let index = self.byte_index();
        self.input.insert(index, new_char);
        self.move_cursor_right();
    }

    fn byte_index(&self) -> usize {
        self.input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.input_cursor)
            .unwrap_or(self.input.len())
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.input_cursor != 0;
        if is_not_cursor_leftmost {
            let current_index = self.input_cursor;
            let from_left_to_current_index = current_index - 1;
            let before_char_to_delete = self.input.chars().take(from_left_to_current_index);
            let after_char_to_delete = self.input.chars().skip(current_index);
            self.input = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.input.chars().count())
    }

    fn reset_cursor(&mut self) {
        self.input_cursor = 0;
    }

    fn submit_message(&mut self) {
        if self.input.is_empty() || self.is_loading {
            return;
        }

        let user_message = self.input.clone();
        self.add_message("user", &user_message);
        self.input.clear();
        self.reset_cursor();
        self.is_loading = true;
        self.pending_prompt = Some(user_message);
    }

    /// Check for events from Claude CLI
    fn poll_events(&mut self) {
        let events = self.claude.collect_events();

        // Debug: log received events count
        if !events.is_empty() {
            claude_cli::log_to_file(&format!("TUI RECEIVED {} events", events.len()));
        }

        // Process collected events
        let mut should_clear = false;
        for event in events {
            match event {
                ClaudeEvent::Init { model, tools, mcp_servers } => {
                    self.backend_mode = BackendMode::ClaudeCli {
                        model: model.clone(),
                        tools_count: tools.len(),
                        mcp_count: mcp_servers.len(),
                    };
                    self.add_message("system", &format!(
                        "✓ Claude CLI 接続\nModel: {}\nTools: {} / MCP: {}",
                        model, tools.len(), mcp_servers.len()
                    ));
                }
                ClaudeEvent::Text(text) => {
                    // Safe substring for logging (handle multi-byte chars)
                    let preview: String = text.chars().take(100).collect();
                    claude_cli::log_to_file(&format!("TUI TEXT EVENT: {}", preview));
                    // Update or append assistant message
                    if let Some(last) = self.messages.last_mut() {
                        if last.role == "assistant" {
                            claude_cli::log_to_file("TUI: updating existing assistant message");
                            last.content = text;
                        } else {
                            claude_cli::log_to_file("TUI: adding new assistant message");
                            self.add_message("assistant", &text);
                        }
                    } else {
                        claude_cli::log_to_file("TUI: adding first assistant message");
                        self.add_message("assistant", &text);
                    }
                    claude_cli::log_to_file(&format!("TUI: messages count = {}", self.messages.len()));
                }
                ClaudeEvent::ToolExecuting { name } => {
                    self.add_message("tool", &format!("🔧 {} を実行中...", name));
                }
                ClaudeEvent::ToolResult { name, preview } => {
                    self.add_message("tool", &format!("✓ {}: {}", name, preview));
                }
                ClaudeEvent::Done { result: _, cost } => {
                    if self.debug_mode == DebugMode::Detail {
                        self.add_message("system", &format!("✓ 完了 (${:.4})", cost));
                    }
                    self.is_loading = false;
                    should_clear = true;
                }
                ClaudeEvent::Error(e) => {
                    // stderrからのメッセージは致命的エラーではないので、クリアしない
                    if !e.starts_with("stderr:") {
                        self.add_message("error", &format!("エラー: {}", e));
                        self.is_loading = false;
                        should_clear = true;
                    }
                }
            }
        }

        if should_clear {
            self.claude.clear_channel();
        }
    }

    fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing - output to file for TUI compatibility
    let log_dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("vantage");
    std::fs::create_dir_all(&log_dir).ok();
    let log_file = std::fs::File::create(log_dir.join("vantage-tui.log")).ok();

    if let Some(file) = log_file {
        tracing_subscriber::registry()
            .with(EnvFilter::from_default_env().add_directive("vantage_tui=info".parse().unwrap()))
            .with(fmt::layer().with_writer(std::sync::Mutex::new(file)).with_ansi(false))
            .init();
    }

    // Start daemon if not running
    let daemon_process = daemon::ensure_running().await.ok().flatten();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new()?;
    app.daemon_process = daemon_process;

    // Run app
    let res = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("Error: {err:?}");
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
        // Handle pending prompt (async send)
        if let Some(prompt) = app.pending_prompt.take() {
            if let Err(e) = app.claude.send_prompt(&prompt, Some(&app.project_dir)).await {
                app.add_message("error", &format!("Claude CLI エラー: {}", e));
                app.is_loading = false;
            }
        }

        // Poll for Claude CLI events
        app.poll_events();

        terminal.draw(|f| ui(f, app))?;

        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    // Ctrl+S: Save session
                    if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('s')
                    {
                        match app.save_session() {
                            Ok(path) => {
                                app.add_message("system", &format!("セッション保存: {}", path));
                            }
                            Err(e) => {
                                app.add_message("error", &format!("保存エラー: {}", e));
                            }
                        }
                        continue;
                    }

                    // Ctrl+R: Resume latest session
                    if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('r')
                    {
                        if let Err(e) = app.resume_latest() {
                            app.add_message("error", &format!("再開エラー: {}", e));
                        }
                        continue;
                    }

                    // Ctrl+D: Toggle debug mode (Off → Simple → Detail → Off)
                    if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('d')
                    {
                        app.debug_mode = match app.debug_mode {
                            DebugMode::Off => DebugMode::Simple,
                            DebugMode::Simple => DebugMode::Detail,
                            DebugMode::Detail => DebugMode::Off,
                        };
                        let mode_str = match app.debug_mode {
                            DebugMode::Off => "OFF",
                            DebugMode::Simple => "Simple",
                            DebugMode::Detail => "Detail",
                        };
                        app.add_message("system", &format!("🔧 Debug: {}", mode_str));
                        continue;
                    }

                    match key.code {
                        KeyCode::Esc => {
                            // Auto-save on exit
                            let _ = app.save_session();
                            return Ok(());
                        }
                        KeyCode::Enter => {
                            app.submit_message();
                        }
                        KeyCode::Char(c) => {
                            app.enter_char(c);
                        }
                        KeyCode::Backspace => {
                            app.delete_char();
                        }
                        KeyCode::Left => {
                            app.move_cursor_left();
                        }
                        KeyCode::Right => {
                            app.move_cursor_right();
                        }
                        KeyCode::Up => {
                            app.scroll_up();
                        }
                        KeyCode::Down => {
                            app.scroll_down();
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Min(1),     // Messages
            Constraint::Length(3),  // Input
            Constraint::Length(1),  // Status
        ])
        .split(f.area());

    // Header with backend info
    let header_text = match &app.backend_mode {
        BackendMode::ClaudeCli { model, tools_count, mcp_count } => {
            format!("🎯 Vantage Point Agent | {} | Tools: {} | MCP: {}", model, tools_count, mcp_count)
        }
        BackendMode::Initializing => {
            "🎯 Vantage Point Agent | ⏳ 準備完了".to_string()
        }
    };
    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(header, chunks[0]);

    // Messages
    let messages: Vec<ListItem> = app.messages
        .iter()
        .map(|m| {
            let style = match m.role.as_str() {
                "user" => Style::default().fg(Color::Green),
                "assistant" => Style::default().fg(Color::White),
                "system" => Style::default().fg(Color::Yellow),
                "tool" => Style::default().fg(Color::Magenta),
                "error" => Style::default().fg(Color::Red),
                _ => Style::default(),
            };
            let prefix = match m.role.as_str() {
                "user" => "あなた: ",
                "assistant" => "Agent: ",
                "system" => "📢 ",
                "tool" => "",
                "error" => "❌ ",
                _ => "",
            };
            // Split content by newlines and wrap long lines
            let width = f.area().width.saturating_sub(4) as usize;
            let mut lines: Vec<Line> = Vec::new();

            for (i, line) in m.content.lines().enumerate() {
                let mut remaining = line;
                let mut is_first = i == 0;

                while !remaining.is_empty() {
                    let (chunk, rest) = if remaining.chars().count() > width {
                        let byte_idx = remaining
                            .char_indices()
                            .nth(width)
                            .map(|(i, _)| i)
                            .unwrap_or(remaining.len());
                        (&remaining[..byte_idx], &remaining[byte_idx..])
                    } else {
                        (remaining, "")
                    };

                    if is_first {
                        lines.push(Line::from(vec![
                            Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
                            Span::styled(chunk.to_string(), style),
                        ]));
                        is_first = false;
                    } else {
                        lines.push(Line::from(vec![
                            Span::styled("  ", style),
                            Span::styled(chunk.to_string(), style),
                        ]));
                    }
                    remaining = rest;
                }
            }

            if lines.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
                ]));
            }

            ListItem::new(lines)
        })
        .collect();

    let messages_widget = List::new(messages)
        .block(Block::default().borders(Borders::ALL).title("Chat"));
    f.render_widget(messages_widget, chunks[1]);

    // Input
    let input_title = if app.is_loading {
        "送信中...".to_string()
    } else {
        "入力 (Enter: 送信)".to_string()
    };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(input_title);

    let input = Paragraph::new(app.input.as_str())
        .style(Style::default().fg(Color::Yellow))
        .block(input_block);
    f.render_widget(input, chunks[2]);

    // Cursor position
    let cursor_x = chunks[2].x + app.input_cursor as u16 + 1;
    let cursor_y = chunks[2].y + 1;
    f.set_cursor_position((cursor_x, cursor_y));

    // Status bar
    let session_id = &app.session.id[8..].chars().take(12).collect::<String>();
    let debug_indicator = match app.debug_mode {
        DebugMode::Off => "",
        DebugMode::Simple => " [DBG]",
        DebugMode::Detail => " [DBG:detail]",
    };
    let status = Paragraph::new(format!(
        "Session: {}{} | Ctrl+D: Debug | Esc: 終了",
        session_id, debug_indicator
    ))
    .style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, chunks[3]);
}
