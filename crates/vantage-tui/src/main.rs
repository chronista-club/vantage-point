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
use std::sync::mpsc::Receiver;

mod claude_cli;
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
    event_rx: Option<Receiver<ClaudeEvent>>,
    project_dir: String,
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
                    "🎯 Vantage Point Agent\n📁 Project: {}\n⏳ Claude CLI 初期化中...",
                    project_dir
                ),
            }],
            claude: ClaudeCli::new(),
            backend_mode: BackendMode::Initializing,
            is_loading: false,
            scroll: 0,
            session,
            session_store,
            event_rx: None,
            project_dir,
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

        // Send prompt to Claude CLI
        match self.claude.send_prompt(&user_message, Some(&self.project_dir)) {
            Ok(rx) => {
                self.event_rx = Some(rx);
            }
            Err(e) => {
                self.add_message("error", &format!("Claude CLI エラー: {}", e));
                self.is_loading = false;
            }
        }
    }

    /// Check for events from Claude CLI
    fn poll_events(&mut self) {
        // Collect events first to avoid borrow issues
        let events: Vec<ClaudeEvent> = if let Some(ref rx) = self.event_rx {
            let mut collected = Vec::new();
            while let Ok(event) = rx.try_recv() {
                collected.push(event);
            }
            collected
        } else {
            Vec::new()
        };

        // Process collected events
        let mut should_clear_rx = false;
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
                    // Update or append assistant message
                    if let Some(last) = self.messages.last_mut() {
                        if last.role == "assistant" {
                            last.content = text;
                        } else {
                            self.add_message("assistant", &text);
                        }
                    } else {
                        self.add_message("assistant", &text);
                    }
                }
                ClaudeEvent::ToolExecuting { name } => {
                    self.add_message("tool", &format!("🔧 {} を実行中...", name));
                }
                ClaudeEvent::ToolResult { name, preview } => {
                    self.add_message("tool", &format!("✓ {}: {}", name, preview));
                }
                ClaudeEvent::Done { result: _, cost } => {
                    self.add_message("system", &format!("💰 ${:.4}", cost));
                    self.is_loading = false;
                    should_clear_rx = true;
                }
                ClaudeEvent::Error(e) => {
                    self.add_message("error", &format!("エラー: {}", e));
                    self.is_loading = false;
                    should_clear_rx = true;
                }
            }
        }

        if should_clear_rx {
            self.event_rx = None;
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
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new()?;

    // Run app
    let res = run_app(&mut terminal, &mut app);

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

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    loop {
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
            "🎯 Vantage Point Agent | ⏳ 初期化中...".to_string()
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
    let status = Paragraph::new(format!(
        "Session: {} | 📁 {} | Ctrl+S: 保存 | Ctrl+R: 再開 | Esc: 終了",
        session_id, app.project_dir
    ))
    .style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, chunks[3]);
}
