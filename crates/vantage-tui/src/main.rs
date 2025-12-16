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
use std::sync::{Arc, Mutex};

mod api;
mod session;
mod tools;

use api::{AnthropicClient, ToolEvent};
use session::{Session, SessionStore};

#[derive(Debug, Clone)]
struct Message {
    role: String,
    content: String,
}

struct App {
    input: String,
    input_cursor: usize,
    messages: Arc<Mutex<Vec<Message>>>,
    client: AnthropicClient,
    is_loading: bool,
    scroll: u16,
    session: Session,
    session_store: SessionStore,
}

impl App {
    fn new() -> Result<Self> {
        dotenvy::dotenv().ok();
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .expect("ANTHROPIC_API_KEY must be set");

        let session_store = SessionStore::new();
        let session = Session::new();

        Ok(Self {
            input: String::new(),
            input_cursor: 0,
            messages: Arc::new(Mutex::new(vec![Message {
                role: "system".to_string(),
                content: format!("Vantage Point Agent へようこそ！\nセッション: {}", session.id),
            }])),
            client: AnthropicClient::new(api_key),
            is_loading: false,
            scroll: 0,
            session,
            session_store,
        })
    }

    fn save_session(&mut self) -> Result<String> {
        // Sync messages to session
        if let Ok(messages) = self.messages.lock() {
            for msg in messages.iter() {
                // Only add if not already in session
                if !self.session.messages.iter().any(|m| m.content == msg.content) {
                    self.session.add_message(&msg.role, &msg.content);
                }
            }
        }
        let path = self.session_store.save(&self.session)?;
        Ok(path.display().to_string())
    }

    fn resume_latest(&mut self) -> Result<()> {
        if let Some(session) = self.session_store.get_latest() {
            self.session = session.clone();
            if let Ok(mut messages) = self.messages.lock() {
                messages.clear();
                messages.push(Message {
                    role: "system".to_string(),
                    content: format!("セッション再開: {}", session.id),
                });
                for msg in &session.messages {
                    messages.push(Message {
                        role: msg.role.clone(),
                        content: msg.content.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn add_message(&self, role: &str, content: &str) {
        if let Ok(mut messages) = self.messages.lock() {
            messages.push(Message {
                role: role.to_string(),
                content: content.to_string(),
            });
        }
    }

    fn get_messages(&self) -> Vec<Message> {
        self.messages.lock().map(|m| m.clone()).unwrap_or_default()
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

    async fn submit_message(&mut self) {
        if self.input.is_empty() || self.is_loading {
            return;
        }

        let user_message = self.input.clone();
        self.add_message("user", &user_message);
        self.input.clear();
        self.reset_cursor();
        self.is_loading = true;

        let messages = self.messages.clone();

        // Call API with tool event callback
        let result = self.client.chat(&user_message, |event| {
            match event {
                ToolEvent::Executing(tool_name) => {
                    if let Ok(mut msgs) = messages.lock() {
                        msgs.push(Message {
                            role: "tool".to_string(),
                            content: format!("🔧 {} を実行中...", tool_name),
                        });
                    }
                }
                ToolEvent::Result(tool_name, preview) => {
                    if let Ok(mut msgs) = messages.lock() {
                        msgs.push(Message {
                            role: "tool".to_string(),
                            content: format!("✓ {}: {}", tool_name, preview),
                        });
                    }
                }
            }
        }).await;

        match result {
            Ok(response) => {
                self.add_message("assistant", &response);
            }
            Err(e) => {
                self.add_message("error", &format!("エラー: {}", e));
            }
        }
        self.is_loading = false;
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
        terminal.draw(|f| ui(f, app))?;

        if event::poll(std::time::Duration::from_millis(100))? {
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
                            app.submit_message().await;
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

    // Header
    let header = Paragraph::new("🎯 Vantage Point Agent (Phase 0)")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(header, chunks[0]);

    // Messages
    let all_messages = app.get_messages();
    let messages: Vec<ListItem> = all_messages
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
            let width = f.area().width.saturating_sub(4) as usize; // Account for borders
            let mut lines: Vec<Line> = Vec::new();

            for (i, line) in m.content.lines().enumerate() {
                // Wrap each line to fit width
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
                            Span::styled("  ", style), // Indent continuation
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
        "Session: {} | Ctrl+S: 保存 | Ctrl+R: 再開 | Esc: 終了",
        session_id
    ))
    .style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, chunks[3]);
}
