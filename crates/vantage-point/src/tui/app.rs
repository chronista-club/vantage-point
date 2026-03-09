//! TUI アプリケーションメインループ
//!
//! プロジェクト選択 → Claude CLI PTY セッション の画面遷移を管理する。

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::grid::Scroll;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::config::{Config, ProjectConfig};
use crate::protocol::{Content, ProcessMessage};
use crate::terminal::state::TerminalState;
use ratatui_image::StatefulImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use tui_scrollview::{ScrollView, ScrollViewState};

use super::input::key_to_pty_bytes;
use super::session::{ClaudeSession, SessionMode, list_sessions};
use super::terminal_widget::TerminalView;

/// Canvas ペインの状態（Unison 経由でリアルタイム受信）
struct CanvasState {
    /// pane_id → (title, content)
    panes: HashMap<String, (Option<String>, Content)>,
    /// 画像プロトコル状態（pane_id → StatefulProtocol）
    images: HashMap<String, StatefulProtocol>,
    /// 画像プロトコル Picker（ターミナル検出結果をキャッシュ）
    picker: Option<Picker>,
    /// スクロール状態
    scroll_state: ScrollViewState,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            panes: HashMap::new(),
            images: HashMap::new(),
            picker: Picker::from_query_stdio().ok(),
            scroll_state: ScrollViewState::default(),
        }
    }
}

impl CanvasState {
    /// ProcessMessage を適用
    fn apply(&mut self, msg: &ProcessMessage) {
        match msg {
            ProcessMessage::Show {
                pane_id,
                content,
                append,
                title,
            } => {
                if *append {
                    if let Some((existing_title, existing_content)) = self.panes.get_mut(pane_id) {
                        *existing_content = existing_content.append_with(content);
                        if title.is_some() {
                            *existing_title = title.clone();
                        }
                    } else {
                        self.panes
                            .insert(pane_id.clone(), (title.clone(), content.clone()));
                    }
                } else {
                    self.panes
                        .insert(pane_id.clone(), (title.clone(), content.clone()));
                }

                // 画像コンテンツの場合、プロトコル状態を更新
                if let Content::ImageBase64 { data, .. } = content {
                    self.update_image(pane_id, data);
                }
            }
            ProcessMessage::Clear { pane_id } => {
                self.panes.remove(pane_id);
                self.images.remove(pane_id);
            }
            _ => {}
        }
    }

    /// Base64 画像データからプロトコル状態を生成
    fn update_image(&mut self, pane_id: &str, data: &str) {
        let Some(picker) = &mut self.picker else {
            return;
        };

        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        if let Ok(bytes) = engine.decode(data)
            && let Ok(img) = image::load_from_memory(&bytes)
        {
            let protocol = picker.new_resize_protocol(img);
            self.images.insert(pane_id.to_string(), protocol);
        }
    }

    /// スクロールアップ（3行分）
    fn scroll_up(&mut self) {
        for _ in 0..3 {
            self.scroll_state.scroll_up();
        }
    }

    /// スクロールダウン（3行分）
    fn scroll_down(&mut self) {
        for _ in 0..3 {
            self.scroll_state.scroll_down();
        }
    }
}

/// Unison QUIC で Process サーバーの "canvas" チャネルに接続し、
/// Show/Clear イベントを受信するスレッドを起動
fn spawn_canvas_receiver(
    port: u16,
    canvas_state: Arc<Mutex<CanvasState>>,
) -> Option<std::thread::JoinHandle<()>> {
    let handle = std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };

        rt.block_on(async {
            let quic_port = port + crate::process::unison_server::QUIC_PORT_OFFSET;
            let addr = format!("[::1]:{}", quic_port);

            // 接続（リトライ付き）
            let client = match unison::ProtocolClient::new_default() {
                Ok(c) => c,
                Err(_) => return,
            };

            let mut attempts = 0;
            loop {
                match client.connect(&addr).await {
                    Ok(_) => break,
                    Err(_) => {
                        attempts += 1;
                        if attempts >= 5 {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }

            // "canvas" チャネルを開く
            let channel = match client.open_channel("canvas").await {
                Ok(ch) => ch,
                Err(_) => return,
            };

            // イベント受信ループ
            while let Ok(msg) = channel.recv().await {
                let payload = msg.payload_as_value().unwrap_or_default();
                if let Ok(process_msg) = serde_json::from_value::<ProcessMessage>(payload) {
                    let mut state = canvas_state.lock().unwrap();
                    state.apply(&process_msg);
                }
            }
        });
    });
    Some(handle)
}

// Arctic Nord カラー定数
const NORD_BG: Color = Color::Rgb(11, 17, 32); // #0B1120
const NORD_FG: Color = Color::Rgb(216, 222, 233); // #D8DEE9
const NORD_CYAN: Color = Color::Rgb(136, 192, 208); // #88C0D0
const NORD_POLAR: Color = Color::Rgb(46, 52, 64); // #2E3440
const NORD_COMMENT: Color = Color::Rgb(76, 86, 106); // #4C566A
const NORD_GREEN: Color = Color::Rgb(163, 190, 140); // #A3BE8C
const NORD_RED: Color = Color::Rgb(191, 97, 106); // #BF616A
const NORD_YELLOW: Color = Color::Rgb(235, 203, 139); // #EBCB8B

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
        EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste
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
        let port = if let Some(running) = crate::discovery::find_by_project_blocking(&dir) {
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
        crossterm::event::DisableBracketedPaste,
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

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
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
            let running = crate::discovery::find_by_project_blocking(&Config::normalize_path(
                std::path::Path::new(&p.path),
            ));
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

    // ポート予約は不要 — server.rs が起動後に TheWorld に登録する

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

/// ブリッジスレッドへのコマンド
enum BridgeCommand {
    /// PTY への入力データ
    Input(Vec<u8>),
    /// PTY リサイズ
    Resize { cols: u16, rows: u16 },
    /// 新規セッション作成
    CreateSession {
        cols: u16,
        rows: u16,
        command: Vec<String>,
    },
    /// セッション切替
    SwitchSession(String),
    /// tmux ペイン分割（Process の TmuxActor 経由）
    TmuxSplit {
        horizontal: bool,
        command: Option<String>,
    },
}

/// ブリッジスレッドからのイベント
enum BridgeEvent {
    /// PTY 出力データ
    Output(Vec<u8>),
    /// セッション作成完了
    SessionCreated { session_id: String },
    /// セッション切替完了
    SessionSwitched { session_id: String },
    /// エラー
    Error(String),
    /// 接続切断
    Disconnected,
}

/// Unison terminal ブリッジスレッドを起動
fn spawn_terminal_bridge(
    port: u16,
    terminal_token: String,
    cmd_rx: std::sync::mpsc::Receiver<BridgeCommand>,
    event_tx: std::sync::mpsc::Sender<BridgeEvent>,
) {
    std::thread::Builder::new()
        .name("tui-terminal-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(async move {
                let quic_port = port + crate::process::unison_server::QUIC_PORT_OFFSET;
                let addr = format!("[::1]:{}", quic_port);

                // 接続（リトライ付き）
                let client = match unison::ProtocolClient::new_default() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = event_tx.send(BridgeEvent::Error(format!("QUIC client 作成失敗: {}", e)));
                        return;
                    }
                };

                let mut attempts = 0;
                loop {
                    match client.connect(&addr).await {
                        Ok(_) => break,
                        Err(_) => {
                            attempts += 1;
                            if attempts >= 10 {
                                let _ = event_tx.send(BridgeEvent::Error("QUIC 接続失敗".to_string()));
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(300)).await;
                        }
                    }
                }

                let channel = match client.open_channel("terminal").await {
                    Ok(ch) => ch,
                    Err(e) => {
                        let _ = event_tx.send(BridgeEvent::Error(format!("terminal チャネル開設失敗: {}", e)));
                        return;
                    }
                };

                // 認証
                match channel.request("auth", serde_json::json!({"token": terminal_token})).await {
                    Ok(resp) => {
                        if resp.get("error").is_some() {
                            let _ = event_tx.send(BridgeEvent::Error(format!("認証失敗: {:?}", resp)));
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = event_tx.send(BridgeEvent::Error(format!("認証リクエスト失敗: {}", e)));
                        return;
                    }
                }

                // sync → tokio ブリッジ
                let (bridge_tx, mut bridge_rx) = tokio::sync::mpsc::channel::<BridgeCommand>(256);
                std::thread::Builder::new()
                    .name("tui-cmd-bridge".into())
                    .spawn(move || {
                        while let Ok(cmd) = cmd_rx.recv() {
                            if bridge_tx.blocking_send(cmd).is_err() {
                                break;
                            }
                        }
                    })
                    .expect("tui-cmd-bridge spawn failed");

                // メインループ
                loop {
                    tokio::select! {
                        data = channel.recv_raw() => {
                            match data {
                                Ok(bytes) => {
                                    if event_tx.send(BridgeEvent::Output(bytes)).is_err() {
                                        break;
                                    }
                                }
                                Err(_) => {
                                    let _ = event_tx.send(BridgeEvent::Disconnected);
                                    break;
                                }
                            }
                        }
                        // 構造化イベント（session_ended 等）を受信
                        evt = channel.recv() => {
                            match evt {
                                Ok(msg) => {
                                    if msg.method == "session_ended" {
                                        tracing::info!("TUI bridge: session_ended 受信");
                                        let _ = event_tx.send(BridgeEvent::Disconnected);
                                        break;
                                    }
                                    // 他のイベントは無視
                                }
                                Err(_) => {
                                    let _ = event_tx.send(BridgeEvent::Disconnected);
                                    break;
                                }
                            }
                        }
                        cmd = bridge_rx.recv() => {
                            match cmd {
                                Some(BridgeCommand::Input(data)) => {
                                    if channel.send_raw(&data).await.is_err() {
                                        let _ = event_tx.send(BridgeEvent::Disconnected);
                                        break;
                                    }
                                }
                                Some(BridgeCommand::Resize { cols, rows }) => {
                                    let _ = channel.request(
                                        "resize",
                                        serde_json::json!({"cols": cols, "rows": rows}),
                                    ).await;
                                }
                                Some(BridgeCommand::CreateSession { cols, rows, command }) => {
                                    match channel.request(
                                        "create_session",
                                        serde_json::json!({
                                            "cols": cols,
                                            "rows": rows,
                                            "command": command,
                                        }),
                                    ).await {
                                        Ok(resp) => {
                                            if let Some(sid) = resp.get("session_id").and_then(|v| v.as_str()) {
                                                let _ = event_tx.send(BridgeEvent::SessionCreated {
                                                    session_id: sid.to_string(),
                                                });
                                            } else {
                                                let _ = event_tx.send(BridgeEvent::Error(
                                                    format!("セッション作成失敗: {:?}", resp),
                                                ));
                                            }
                                        }
                                        Err(e) => {
                                            let _ = event_tx.send(BridgeEvent::Error(
                                                format!("セッション作成リクエスト失敗: {}", e),
                                            ));
                                        }
                                    }
                                }
                                Some(BridgeCommand::SwitchSession(session_id)) => {
                                    match channel.request(
                                        "switch_session",
                                        serde_json::json!({"session_id": session_id}),
                                    ).await {
                                        Ok(resp) => {
                                            if let Some(sid) = resp.get("session_id").and_then(|v| v.as_str()) {
                                                let _ = event_tx.send(BridgeEvent::SessionSwitched {
                                                    session_id: sid.to_string(),
                                                });
                                            }
                                        }
                                        Err(e) => {
                                            let _ = event_tx.send(BridgeEvent::Error(
                                                format!("セッション切替失敗: {}", e),
                                            ));
                                        }
                                    }
                                }
                                Some(BridgeCommand::TmuxSplit { horizontal, command }) => {
                                    let payload = serde_json::json!({
                                        "horizontal": horizontal,
                                        "command": command,
                                    });
                                    match channel.request("tmux_split", payload).await {
                                        Ok(_) => {}
                                        Err(e) => {
                                            tracing::warn!("tmux_split 失敗: {}", e);
                                        }
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                }
            });
        })
        .expect("tui-terminal-bridge スレッドの起動に失敗");
}

/// Claude CLI コマンドを構築
fn build_claude_command(session_mode: &SessionMode) -> Vec<String> {
    let mut cmd = vec![
        "claude".to_string(),
        "--dangerously-skip-permissions".to_string(),
    ];

    match session_mode {
        SessionMode::Continue => {
            cmd.push("--continue".to_string());
        }
        SessionMode::New => {}
        SessionMode::Resume(id) => {
            cmd.push("--resume".to_string());
            cmd.push(id.clone());
        }
    }

    cmd
}

/// Claude CLI PTY セッション（Process サーバー経由）
fn run_claude_session(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    project_dir: &str,
    project_name: &str,
    port: u16,
) -> Result<()> {
    // セッション選択
    let session_mode = run_session_select(terminal, project_dir)?;

    let size = terminal.size()?;
    let (pty_cols, pty_lines) = calc_pty_size(size.width, size.height, false);

    // VT パーサー
    let term_state = Arc::new(Mutex::new(TerminalState::new(pty_cols, pty_lines)));

    // Health API から認証トークンを取得
    let terminal_token =
        crate::discovery::fetch_terminal_token_blocking(port).ok_or_else(|| {
            anyhow::anyhow!(
                "Terminal token not found for port {}. Process may not be fully started.",
                port
            )
        })?;

    // Unison ブリッジ起動
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<BridgeCommand>();
    let (event_tx, event_rx) = std::sync::mpsc::channel::<BridgeEvent>();
    spawn_terminal_bridge(port, terminal_token, cmd_rx, event_tx);

    // セッション作成リクエスト
    let claude_cmd = build_claude_command(&session_mode);
    let _ = cmd_tx.send(BridgeCommand::CreateSession {
        cols: pty_cols as u16,
        rows: pty_lines as u16,
        command: claude_cmd,
    });

    // セッション追跡
    let mut sessions: Vec<String> = Vec::new();
    let mut current_session_idx: usize = 0;
    let mut bridge_status: String = "接続中...".to_string();

    // Canvas 状態
    let canvas_state = Arc::new(Mutex::new(CanvasState::default()));
    let _canvas_handle = spawn_canvas_receiver(port, Arc::clone(&canvas_state));
    let mut canvas_open = false;

    // カーソル点滅制御
    let mut cursor_blink_on = true;
    let mut cursor_blink_timer = std::time::Instant::now();
    const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);

    // メインループ
    let mut needs_redraw = true;
    loop {
        // カーソル点滅タイマー
        if cursor_blink_timer.elapsed() >= CURSOR_BLINK_INTERVAL {
            cursor_blink_on = !cursor_blink_on;
            cursor_blink_timer = std::time::Instant::now();
            needs_redraw = true;
        }
        // ブリッジからのイベントを処理（ノンブロッキング）
        while let Ok(evt) = event_rx.try_recv() {
            match evt {
                BridgeEvent::Output(bytes) => {
                    let mut state = term_state.lock().unwrap();
                    state.feed_bytes(&bytes);
                    needs_redraw = true;
                }
                BridgeEvent::SessionCreated { session_id } => {
                    tracing::info!("TUI: セッション作成完了: {}", session_id);
                    sessions.push(session_id);
                    current_session_idx = sessions.len() - 1;
                    bridge_status = "接続済み".to_string();
                    needs_redraw = true;
                }
                BridgeEvent::SessionSwitched { session_id } => {
                    tracing::info!("TUI: セッション切替完了: {}", session_id);
                    if let Some(idx) = sessions.iter().position(|s| s == &session_id) {
                        current_session_idx = idx;
                    }
                    // 画面クリア（新セッションの出力で再描画される）
                    let mut state = term_state.lock().unwrap();
                    let cols = state.cols();
                    let rows = state.lines();
                    *state = TerminalState::new(cols, rows);
                    needs_redraw = true;
                }
                BridgeEvent::Error(e) => {
                    tracing::error!("TUI bridge error: {}", e);
                    bridge_status = format!("エラー: {}", e);
                    needs_redraw = true;
                }
                BridgeEvent::Disconnected => {
                    tracing::warn!("TUI: Process 接続切断");
                    return Ok(());
                }
            }
        }

        // 描画（変更があった場合のみ — IME プリエディット表示の安定化）
        if needs_redraw {
            needs_redraw = false;
            let state = term_state.lock().unwrap();
            let snapshot = state.snapshot();
            let display_offset = state.display_offset();
            drop(state);

            let session_count = sessions.len();
            let session_idx = current_session_idx;

            terminal.draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1), // ヘッダバー
                        Constraint::Min(1),    // メインエリア
                        Constraint::Length(1), // フッターバー
                    ])
                    .split(frame.area());

                // ヘッダバー
                draw_header_bar(frame, chunks[0], project_name, port, canvas_open);

                // メインエリア: Canvas ON なら左右分割
                let main_area = chunks[1];
                let (pty_area, canvas_area) = if canvas_open {
                    let panes = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                        .split(main_area);
                    (panes[0], Some(panes[1]))
                } else {
                    (main_area, None)
                };

                // PTY ペイン（セッション情報付き）
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
                frame.render_widget(
                    TerminalView::new(&snapshot).cursor_blink(cursor_blink_on),
                    pty_inner,
                );

                // ハードウェアカーソルは使わない（ソフトウェアカーソルで点滅描画）
                // cursor_visible (DECTCEM) は TerminalView 内のソフトウェアカーソルで処理

                // Canvas ペイン
                if let Some(canvas_area) = canvas_area {
                    let canvas_block = Block::default()
                        .title(" Canvas ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(NORD_GREEN));
                    let canvas_inner = canvas_block.inner(canvas_area);
                    frame.render_widget(canvas_block, canvas_area);

                    let mut cs = canvas_state.lock().unwrap();
                    render_canvas(frame, canvas_inner, &mut cs);
                }

                draw_footer_bar(frame, chunks[2], &bridge_status);
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
                    // キー入力時はカーソルを即座に表示にリセット
                    cursor_blink_on = true;
                    cursor_blink_timer = std::time::Instant::now();

                    // Ctrl+Q: 終了
                    if key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }

                    // Ctrl+N: 新規ペイン（tmux Actor 経由 or 内部 PTY セッション）
                    if key.code == KeyCode::Char('n')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if crate::tmux::is_inside_tmux() {
                            // Unison 経由で Process の TmuxActor にペイン分割を依頼
                            let _ = cmd_tx.send(BridgeCommand::TmuxSplit {
                                horizontal: true,
                                command: None,
                            });
                        } else {
                            // 従来の内部 PTY セッション作成
                            let size = terminal.size()?;
                            let (cols, rows) = calc_pty_size(size.width, size.height, canvas_open);
                            let _ = cmd_tx.send(BridgeCommand::CreateSession {
                                cols: cols as u16,
                                rows: rows as u16,
                                command: vec![
                                    "claude".to_string(),
                                    "--dangerously-skip-permissions".to_string(),
                                ],
                            });
                        }
                        continue;
                    }

                    // Ctrl+Right / Ctrl+Left: セッション切替
                    if key.modifiers.contains(KeyModifiers::CONTROL) && sessions.len() > 1 {
                        let switch_to = match key.code {
                            KeyCode::Right => Some((current_session_idx + 1) % sessions.len()),
                            KeyCode::Left => Some(if current_session_idx == 0 {
                                sessions.len() - 1
                            } else {
                                current_session_idx - 1
                            }),
                            _ => None,
                        };

                        if let Some(idx) = switch_to {
                            let sid = sessions[idx].clone();
                            let _ = cmd_tx.send(BridgeCommand::SwitchSession(sid));
                            continue;
                        }
                    }

                    // Home: Canvas 表示/非表示トグル
                    if key.code == KeyCode::Home {
                        toggle_canvas(port, &mut canvas_open);
                        let term_size = terminal.size()?;
                        let (new_cols, new_lines) =
                            calc_pty_size(term_size.width, term_size.height, canvas_open);
                        {
                            let mut state = term_state.lock().unwrap();
                            state.resize(new_cols, new_lines);
                        }
                        let _ = cmd_tx.send(BridgeCommand::Resize {
                            cols: new_cols as u16,
                            rows: new_lines as u16,
                        });
                        needs_redraw = true;
                        continue;
                    }

                    // PageUp/PageDown: TUI スクロールバック
                    match key.code {
                        KeyCode::PageUp => {
                            let mut state = term_state.lock().unwrap();
                            state.scroll_display(Scroll::PageUp);
                            needs_redraw = true;
                        }
                        KeyCode::PageDown => {
                            let mut state = term_state.lock().unwrap();
                            state.scroll_display(Scroll::PageDown);
                            needs_redraw = true;
                        }
                        _ => {
                            // その他のキー: PTY に転送
                            let bytes = key_to_pty_bytes(key, app_cursor);
                            if !bytes.is_empty() {
                                {
                                    let mut state = term_state.lock().unwrap();
                                    if state.display_offset() > 0 {
                                        state.scroll_display(Scroll::Bottom);
                                    }
                                }
                                let _ = cmd_tx.send(BridgeCommand::Input(bytes));
                            }
                        }
                    }
                }
                Event::Paste(text) => {
                    // IME 確定テキストやクリップボードペーストを PTY に転送
                    // PTY が bracketed paste モードなら囲みシーケンスを付与
                    let bracketed = {
                        let state = term_state.lock().unwrap();
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
                            let mut state = term_state.lock().unwrap();
                            if state.display_offset() > 0 {
                                state.scroll_display(Scroll::Bottom);
                            }
                        }
                        let _ = cmd_tx.send(BridgeCommand::Input(bytes));
                    }
                }
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
                    let (new_cols, new_lines) = calc_pty_size(cols, rows, canvas_open);
                    let mut state = term_state.lock().unwrap();
                    state.resize(new_cols, new_lines);
                    let _ = cmd_tx.send(BridgeCommand::Resize {
                        cols: new_cols as u16,
                        rows: new_lines as u16,
                    });
                    needs_redraw = true;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Canvas ペインの描画（tui-markdown + scrollview + image）
fn render_canvas(frame: &mut ratatui::Frame, area: Rect, state: &mut CanvasState) {
    if state.panes.is_empty() {
        // プレースホルダー
        let placeholder = Paragraph::new(vec![
            Line::from(Span::styled(
                "Canvas ready",
                Style::default().fg(NORD_COMMENT),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "MCP show で内容が表示されます",
                Style::default().fg(NORD_COMMENT),
            )),
        ]);
        frame.render_widget(placeholder, area);
        return;
    }

    // コンテンツ高さを計算
    let content_width = area.width;
    let mut total_height: u16 = 0;

    // pane_id でソートして安定した表示順に
    let mut pane_ids: Vec<_> = state.panes.keys().cloned().collect();
    pane_ids.sort();

    // まず総コンテンツ高を見積もり
    for (i, pane_id) in pane_ids.iter().enumerate() {
        if let Some((_, content)) = state.panes.get(pane_id) {
            if i > 0 {
                total_height += 3; // セパレータ
            }
            total_height += 2; // タイトル + 空行
            match content {
                Content::Markdown(text) | Content::Log(text) | Content::Html(text) => {
                    let rendered = tui_markdown::from_str(text);
                    total_height += rendered.height() as u16;
                }
                Content::ImageBase64 { .. } => {
                    total_height += area.height.saturating_sub(4).max(8);
                }
                Content::Url(_) => {
                    total_height += 1;
                }
            }
        }
    }

    // ScrollView でスクロール可能にする
    let content_size = ratatui::layout::Size::new(content_width, total_height.max(1));
    let mut scroll_view = ScrollView::new(content_size);
    let mut y: u16 = 0;

    for (i, pane_id) in pane_ids.iter().enumerate() {
        if let Some((title, content)) = state.panes.get(pane_id) {
            if i > 0 {
                let sep = Paragraph::new(Line::from(Span::styled(
                    "─".repeat(content_width as usize),
                    Style::default().fg(NORD_COMMENT),
                )));
                scroll_view.render_widget(sep, Rect::new(0, y + 1, content_width, 1));
                y += 3;
            }

            // タイトル
            let display_title = title.as_deref().unwrap_or(pane_id);
            let title_widget = Paragraph::new(Line::from(Span::styled(
                format!("▎ {}", display_title),
                Style::default().fg(NORD_CYAN).add_modifier(Modifier::BOLD),
            )));
            scroll_view.render_widget(title_widget, Rect::new(0, y, content_width, 1));
            y += 2;

            // コンテンツ
            match content {
                Content::Markdown(text) => {
                    let rendered = tui_markdown::from_str(text);
                    let h = rendered.height() as u16;
                    scroll_view.render_widget(
                        Paragraph::new(rendered).wrap(ratatui::widgets::Wrap { trim: false }),
                        Rect::new(0, y, content_width, h.max(1)),
                    );
                    y += h;
                }
                Content::Log(text) | Content::Html(text) => {
                    let rendered = tui_markdown::from_str(text);
                    let h = rendered.height() as u16;
                    scroll_view.render_widget(
                        Paragraph::new(rendered).wrap(ratatui::widgets::Wrap { trim: false }),
                        Rect::new(0, y, content_width, h.max(1)),
                    );
                    y += h;
                }
                Content::ImageBase64 { .. } => {
                    let img_height = area.height.saturating_sub(4).max(8);
                    let img_rect = Rect::new(0, y, content_width, img_height);
                    if let Some(protocol) = state.images.get_mut(pane_id) {
                        let img_widget = StatefulImage::default();
                        scroll_view.render_stateful_widget(img_widget, img_rect, protocol);
                    } else {
                        scroll_view.render_widget(
                            Paragraph::new(Span::styled(
                                "[Image loading...]",
                                Style::default().fg(NORD_GREEN),
                            )),
                            img_rect,
                        );
                    }
                    y += img_height;
                }
                Content::Url(url) => {
                    scroll_view.render_widget(
                        Paragraph::new(Span::styled(
                            format!("→ {}", url),
                            Style::default().fg(NORD_GREEN),
                        )),
                        Rect::new(0, y, content_width, 1),
                    );
                    y += 1;
                }
            }
        }
    }

    frame.render_stateful_widget(scroll_view, area, &mut state.scroll_state);
}

/// PTY サイズ計算（Canvas ON なら 55% 幅）
fn calc_pty_size(term_width: u16, term_height: u16, canvas_open: bool) -> (usize, usize) {
    // ヘッダ（1行）+ フッター（1行）+ PTY ブロック枠上下（各1セル）
    let lines = (term_height.saturating_sub(4) as usize).max(1);
    // PTY ブロック枠左右（各1セル）
    let full_cols = term_width.saturating_sub(2) as usize;
    let cols = if canvas_open {
        // 55% を PTY に割り当て、ブロック枠分(2)を引く
        let pty_area_width = (term_width as usize * 55) / 100;
        pty_area_width.saturating_sub(2).max(1)
    } else {
        full_cols.max(1)
    };
    (cols, lines)
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
fn draw_footer_bar(frame: &mut ratatui::Frame, area: Rect, status: &str) {
    let status_color = if status.starts_with("エラー") {
        NORD_RED
    } else if status == "接続済み" {
        NORD_GREEN
    } else {
        NORD_YELLOW
    };

    let footer = Line::from(vec![
        Span::styled(" Home", Style::default().fg(NORD_CYAN).bg(NORD_POLAR)),
        Span::styled(" canvas ", Style::default().fg(NORD_COMMENT).bg(NORD_POLAR)),
        Span::styled(" C-q", Style::default().fg(NORD_CYAN).bg(NORD_POLAR)),
        Span::styled(" quit ", Style::default().fg(NORD_COMMENT).bg(NORD_POLAR)),
        Span::styled(" PgUp/Dn", Style::default().fg(NORD_CYAN).bg(NORD_POLAR)),
        Span::styled(" scroll ", Style::default().fg(NORD_COMMENT).bg(NORD_POLAR)),
        Span::styled(
            format!(" {} ", status),
            Style::default().fg(status_color).bg(NORD_POLAR),
        ),
    ]);
    let bar = Paragraph::new(footer).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}
