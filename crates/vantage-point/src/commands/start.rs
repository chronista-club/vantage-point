//! `vp start` コマンドの実行ロジック
//!
//! ## アーキテクチャ
//!
//! ```text
//! execute()
//!   ├── Step 1: resolve_project()   — ターゲット → (dir, port, name)
//!   ├── Step 2: route by mode
//!   │     ├── --headless  → run_headless()   SP サーバー本体（blocking）
//!   │     ├── --browser   → run_browser()    SP 確保 → ブラウザ
//!   │     ├── --gui       → run_gui()        SP 確保 → ネイティブウィンドウ
//!   │     └── default     → run_tui_mode()   SP 確保 → tmux + TUI
//!   └── 共通: ensure_sp_running()            SP 未起動なら in-process spawn
//! ```
//!
//! ## TUI アーキテクチャ（v2）
//!
//! TUI は tmux の**外**で直接 ratatui を起動する。
//! tmux セッションは Claude CLI の永続化層として HD が管理。
//! TUI 終了 = detach（Claude CLI は tmux 内で生き続ける）。
//! TUI 再起動 = 既存 tmux セッションに再接続。

use std::io::{self, Read, Write};
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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::cli::{DebugModeArg, parse_debug_env};
use crate::config::Config;
use crate::process::CapabilityConfig;
use crate::protocol::DebugMode;
use crate::resolve::{self, ResolvedTarget};
use crate::terminal::state::TerminalState;
use crate::tui::input::key_to_pty_bytes;
use crate::tui::terminal_widget::TerminalView;
use crate::tui::theme::*;

/// tmux セッション名のプレフィックス
const TMUX_PREFIX: &str = "vp-";

/// `vp start` の起動オプション
pub struct StartOptions<'a> {
    pub target: Option<String>,
    pub port: Option<u16>,
    pub gui: bool,
    pub headless: bool,
    pub browser: bool,
    pub debug: Option<DebugModeArg>,
    pub project_dir: Option<String>,
    pub midi: Option<String>,
    pub config: &'a Config,
}

/// ターゲット解決の結果
struct ResolvedProject {
    dir: String,
    port: u16,
    name: String,
    already_running: bool,
}

// =============================================================================
// メインエントリー
// =============================================================================

/// `vp start` を実行
pub fn execute(opts: StartOptions) -> Result<()> {
    let StartOptions {
        target,
        port,
        gui,
        headless,
        browser,
        debug,
        project_dir,
        midi,
        config,
    } = opts;

    // Step 1: ターゲット解決
    let resolved = resolve_project(target, port, project_dir, headless || browser, config)?;

    println!("\u{1f50c} Using port {}", resolved.port);

    // デバッグモード: CLI > env > default
    let debug_mode = debug
        .map(DebugMode::from)
        .or_else(parse_debug_env)
        .unwrap_or_default();

    if debug_mode != DebugMode::None {
        tracing::info!("Debug mode: {:?}", debug_mode);
    }

    tracing::info!("Project dir: {}", resolved.dir);

    // MIDI 設定
    let midi_config = midi.as_ref().map(|midi_arg| {
        let mut config = crate::midi::MidiConfig::default();
        config
            .note_actions
            .insert(36, crate::midi::MidiAction::OpenWebUI { port: None });
        config
            .note_actions
            .insert(37, crate::midi::MidiAction::CancelChat { port: None });
        config
            .note_actions
            .insert(38, crate::midi::MidiAction::ResetSession { port: None });

        if let Ok(idx) = midi_arg.parse::<usize>() {
            config.port_index = Some(idx);
        } else {
            config.port_pattern = Some(midi_arg.clone());
        }
        config
    });

    let cap_config = CapabilityConfig {
        project_dir: resolved.dir.clone(),
        midi_config,
        bonjour_port: Some(resolved.port),
    };

    // Step 2: モード別ルーティング
    if headless {
        run_headless(resolved.port, debug_mode, cap_config)
    } else if browser {
        run_browser(resolved.port, debug_mode, cap_config)
    } else if gui {
        run_gui(
            resolved.port,
            &resolved.name,
            debug_mode,
            cap_config,
        )
    } else {
        run_tui_mode(
            resolved.port,
            &resolved.dir,
            &resolved.name,
            debug_mode,
            cap_config,
        )
    }
}

// =============================================================================
// Step 1: ターゲット解決
// =============================================================================

/// CLI 引数からプロジェクト情報を解決する
fn resolve_project(
    target: Option<String>,
    explicit_port: Option<u16>,
    project_dir: Option<String>,
    server_only: bool,
    config: &Config,
) -> Result<ResolvedProject> {
    if let Some(ref dir) = project_dir {
        resolve_from_dir(dir, explicit_port, server_only, config)
    } else {
        resolve_from_target(target, explicit_port, server_only, config)
    }
}

/// --project-dir からの解決
fn resolve_from_dir(
    dir: &str,
    explicit_port: Option<u16>,
    server_only: bool,
    config: &Config,
) -> Result<ResolvedProject> {
    let dir = Config::normalize_path(std::path::Path::new(dir));
    let name = resolve::project_name_from_path(&dir, config).to_string();

    if let Some(running) = crate::discovery::find_by_project_blocking(&dir) {
        if server_only {
            println!(
                "Already running: {} (port {}). Use `vp stop` first.",
                name, running.port
            );
            std::process::exit(0);
        }
        return Ok(ResolvedProject {
            dir,
            port: running.port,
            name,
            already_running: true,
        });
    }

    let port = resolve_port(explicit_port, config.find_project_index(&dir), config)?;
    Ok(ResolvedProject {
        dir,
        port,
        name,
        already_running: false,
    })
}

/// target（名前/インデックス）からの解決
fn resolve_from_target(
    target: Option<String>,
    explicit_port: Option<u16>,
    server_only: bool,
    config: &Config,
) -> Result<ResolvedProject> {
    let resolved = resolve::resolve_target(target.as_deref(), config)?;

    match resolved {
        ResolvedTarget::Running {
            port,
            name,
            project_dir,
        } => {
            if server_only {
                println!(
                    "Already running: {} (port {}). Use `vp stop` first.",
                    name, port
                );
                std::process::exit(0);
            }
            println!("\u{1f517} Re-attaching to: {} (port {})", name, port);
            Ok(ResolvedProject {
                dir: project_dir,
                port,
                name: name.to_string(),
                already_running: true,
            })
        }
        ResolvedTarget::Configured { name, path, index } => {
            println!("\u{1f4c1} Project: {}", name);
            let port = resolve_port(explicit_port, Some(index), config)?;
            Ok(ResolvedProject {
                dir: path,
                port,
                name: name.to_string(),
                already_running: false,
            })
        }
        ResolvedTarget::Cwd { path } => {
            let name = resolve::project_name_from_path(&path, config).to_string();
            let port = resolve_port(explicit_port, None, config)?;
            Ok(ResolvedProject {
                dir: path,
                port,
                name,
                already_running: false,
            })
        }
    }
}

/// ポート番号を決定（明示指定 > config index > 自動検索）
fn resolve_port(
    explicit: Option<u16>,
    config_index: Option<usize>,
    config: &Config,
) -> Result<u16> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Some(i) = config_index {
        return resolve::port_for_configured(i, config);
    }
    resolve::find_available_port().ok_or_else(|| anyhow::anyhow!("No available ports in range"))
}

// =============================================================================
// Step 2: モード別実行
// =============================================================================

/// Headless モード: SP サーバー本体として blocking 実行
fn run_headless(port: u16, debug_mode: DebugMode, cap_config: CapabilityConfig) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        crate::process::run(port, false, debug_mode, cap_config).await
    })
}

/// Browser モード: SP を確保してブラウザで開く
fn run_browser(port: u16, debug_mode: DebugMode, cap_config: CapabilityConfig) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let server_handle = tokio::spawn(async move {
            crate::process::run(port, false, debug_mode, cap_config).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let url = format!("http://localhost:{}", port);
        tracing::info!("Opening in browser: {}", url);
        let _ = open::that(&url);

        server_handle.await?
    })
}

/// GUI モード: SP を確保してネイティブウィンドウ（Unison）を起動
fn run_gui(
    port: u16,
    project_name: &str,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    ensure_sp_running(port, debug_mode, cap_config)?;

    let terminal_token = crate::discovery::fetch_terminal_token_blocking(port).ok_or_else(|| {
        anyhow::anyhow!(
            "Terminal token not found for port {}. Process may not be fully started.",
            port
        )
    })?;

    let result =
        crate::terminal_window::run_terminal_unison(port, &terminal_token, project_name);

    match result {
        Ok(()) => tracing::info!("Terminal window closed (Process is still running)"),
        Err(e) => tracing::error!("Terminal window error: {}", e),
    }

    Ok(())
}

/// TUI モード: SP を確保 → tmux セッション（Claude CLI）→ TUI 描画
///
/// TUI は tmux の外で直接 ratatui を起動する。
/// tmux セッションは Claude CLI の永続化層。
/// Ctrl+Q で detach（Claude CLI は生き続ける）。
fn run_tui_mode(
    port: u16,
    project_dir: &str,
    project_name: &str,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    // SP サーバーを確保
    ensure_sp_running(port, debug_mode, cap_config)?;

    // tmux セッション管理
    let session_name = format!("{}{}", TMUX_PREFIX, project_name);
    let is_reconnect = tmux_session_exists(&session_name);

    if is_reconnect {
        println!("\u{1f504} 既存セッションに再接続: {}", session_name);
    }

    // ccwire 登録（tmux セッション作成前でも可だが、target 確定後がベスト）
    let tmux_target = format!("{}:0.0", session_name);
    if let Err(e) = crate::ccwire::register(&session_name, &tmux_target) {
        tracing::warn!("ccwire 登録失敗（続行）: {}", e);
    }

    // TUI 起動（tmux の外で直接）
    let result = run_tui(&session_name, project_dir, project_name, port, is_reconnect);

    // ccwire 解除（TUI 終了時）
    if let Err(e) = crate::ccwire::unregister(&session_name) {
        tracing::warn!("ccwire 解除失敗: {}", e);
    }

    result
}

// =============================================================================
// tmux 操作
// =============================================================================

fn tmux_session_exists(name: &str) -> bool {
    std::process::Command::new("tmux")
        .args(["has-session", "-t", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// tmux セッション作成（Claude CLI を中で起動、ステータスバー非表示）
fn create_tmux_session(name: &str, project_dir: &str, cols: u16, rows: u16) -> Result<()> {
    let status = std::process::Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
            "-c",
            project_dir,
            "claude",
            "--dangerously-skip-permissions",
            "--continue",
        ])
        .status()?;

    if !status.success() {
        anyhow::bail!("tmux セッション作成に失敗: {}", name);
    }

    // TUI が自前のヘッダー/フッターを持つため tmux ステータスバーを非表示
    let _ = std::process::Command::new("tmux")
        .args(["set-option", "-t", name, "status", "off"])
        .status();

    Ok(())
}

/// tmux セッションのリサイズ
fn resize_tmux_session(name: &str, cols: u16, rows: u16) {
    let _ = std::process::Command::new("tmux")
        .args([
            "resize-window",
            "-t",
            name,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ])
        .status();
}

// =============================================================================
// TUI メインループ
// =============================================================================

fn run_tui(
    session_name: &str,
    project_dir: &str,
    project_name: &str,
    port: u16,
    is_reconnect: bool,
) -> Result<()> {
    // ターミナル初期化
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    // ウィンドウタイトル設定
    {
        use std::io::Write as _;
        let _ = write!(io::stdout(), "\x1b]0;VP: {}\x07", project_name);
        let _ = io::stdout().flush();
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let size = terminal.size()?;
    // ヘッダー1行 + 区切り線1行 + フッター1行 + 区切り線1行 = 4行分
    let pty_cols = size.width as usize;
    let pty_lines = (size.height.saturating_sub(4)) as usize;

    // tmux セッション確保
    if !is_reconnect {
        create_tmux_session(session_name, project_dir, pty_cols as u16, pty_lines as u16)?;
    } else {
        resize_tmux_session(session_name, pty_cols as u16, pty_lines as u16);
        // 再接続時もステータスバーを非表示にする
        let _ = std::process::Command::new("tmux")
            .args(["set-option", "-t", session_name, "status", "off"])
            .status();
    }

    // ローカル PTY で tmux にアタッチ
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: pty_lines as u16,
        cols: pty_cols as u16,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("tmux");
    cmd.args(["attach", "-t", session_name]);
    let _child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // ターミナルエミュレータ状態
    let term_state = Arc::new(Mutex::new(TerminalState::new(pty_cols, pty_lines)));

    // PTY リーダースレッド
    let term_state_reader = Arc::clone(&term_state);
    let _reader_handle = std::thread::Builder::new()
        .name("tui-pty-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut state = term_state_reader.lock().unwrap();
                        state.feed_bytes(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        })?;

    // PP window 状態
    let mut pp_open = crate::canvas::find_running_canvas().is_some();
    let mut pp_check_timer = std::time::Instant::now();

    // ccwire heartbeat タイマー
    let mut ccwire_heartbeat_timer = std::time::Instant::now();

    // メインループ
    let mut needs_redraw = true;
    let result = loop {
        // ccwire heartbeat（3分間隔）
        if ccwire_heartbeat_timer.elapsed() >= crate::ccwire::HEARTBEAT_INTERVAL {
            let _ = crate::ccwire::heartbeat(session_name);
            ccwire_heartbeat_timer = std::time::Instant::now();
        }

        // PP window 状態を定期チェック
        if pp_check_timer.elapsed() >= Duration::from_secs(1) {
            let was_open = pp_open;
            pp_open = crate::canvas::find_running_canvas().is_some();
            if was_open != pp_open {
                needs_redraw = true;
            }
            pp_check_timer = std::time::Instant::now();
        }

        // 描画
        if needs_redraw {
            let pp_open_val = pp_open;
            terminal.draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1), // ヘッダー
                        Constraint::Length(1), // 上区切り線
                        Constraint::Min(3),    // ターミナル
                        Constraint::Length(1), // 下区切り線
                        Constraint::Length(1), // フッター
                    ])
                    .split(frame.area());

                draw_header(frame, chunks[0], project_name, port, is_reconnect, pp_open_val);
                draw_separator(frame, chunks[1]);

                let state = term_state.lock().unwrap();
                let snap = state.snapshot();
                let view = TerminalView::new(&snap);
                frame.render_widget(view, chunks[2]);

                draw_separator(frame, chunks[3]);
                draw_footer(frame, chunks[4]);
            })?;
        }

        needs_redraw = true;

        // イベントポーリング
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                // Ctrl+Q: detach して終了（tmux セッションは生き続ける）
                if key.code == KeyCode::Char('q')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    break Ok(());
                }

                // Home: PP window トグル
                if key.code == KeyCode::Home {
                    let (canvas_port, lanes) = crate::canvas::canvas_target(port);
                    if crate::canvas::find_running_canvas().is_some() {
                        crate::canvas::stop_canvas();
                    } else {
                        let _ = crate::canvas::ensure_canvas_running(canvas_port, lanes, None);
                    }
                    pp_open = crate::canvas::find_running_canvas().is_some();
                    continue;
                }

                // キー入力を PTY に送信
                let bytes = key_to_pty_bytes(key, false);
                if !bytes.is_empty() {
                    if writer.write_all(&bytes).is_err() {
                        break Ok(());
                    }
                    let _ = writer.flush();
                }
            }
        }
    };

    // 終了処理（tmux セッションは殺さない — detach のみ）
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    println!(
        "\u{1f44b} TUI を終了しました。tmux セッション '{}' は継続中です。",
        session_name
    );
    println!("   再接続: vp start");

    result
}

// =============================================================================
// ヘッダー / フッター描画（Nord テーマ）
// =============================================================================

/// ヘッダーバー — Nord テーマ + Stand アイコン
fn draw_header(
    frame: &mut ratatui::Frame,
    area: Rect,
    project_name: &str,
    port: u16,
    is_reconnect: bool,
    pp_open: bool,
) {
    let mut spans: Vec<Span> = Vec::new();

    // プロジェクト名（タブ風）
    spans.push(Span::styled(
        format!(" {} ", project_name),
        Style::default()
            .fg(NORD_BG)
            .bg(NORD_CYAN)
            .add_modifier(Modifier::BOLD),
    ));

    let sep = Span::styled(" ", Style::default().bg(NORD_POLAR));
    spans.push(sep.clone());

    // ⭐ Star Platinum（SP 接続）
    spans.push(Span::styled(
        "\u{2b50}",
        Style::default().fg(NORD_GREEN).bg(NORD_POLAR),
    ));
    spans.push(sep.clone());

    // 🧭 Paisley Park（Canvas）
    let pp_color = if pp_open { NORD_GREEN } else { NORD_COMMENT };
    spans.push(Span::styled(
        "\u{1f9ed}",
        Style::default().fg(pp_color).bg(NORD_POLAR),
    ));
    spans.push(sep.clone());

    // 📖 Heaven's Door（Claude CLI）
    spans.push(Span::styled(
        "\u{1f4d6}",
        Style::default().fg(NORD_GREEN).bg(NORD_POLAR),
    ));

    // 再接続マーカー
    if is_reconnect {
        spans.push(sep.clone());
        spans.push(Span::styled(
            "\u{1f504}再接続",
            Style::default().fg(NORD_YELLOW).bg(NORD_POLAR),
        ));
    }

    // 右端: ポート
    let port_span = Span::styled(
        format!(" :{} ", port),
        Style::default().fg(NORD_COMMENT).bg(NORD_POLAR),
    );

    let left_width: usize = spans.iter().map(|s| s.width()).sum();
    let right_width = port_span.width();
    let gap = (area.width as usize).saturating_sub(left_width + right_width);
    spans.push(Span::styled(
        " ".repeat(gap),
        Style::default().bg(NORD_POLAR),
    ));
    spans.push(port_span);

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}

/// フッターバー — キーバインドガイド
fn draw_footer(frame: &mut ratatui::Frame, area: Rect) {
    let key_style = Style::default().fg(NORD_CYAN).bg(NORD_POLAR);
    let desc_style = Style::default().fg(NORD_COMMENT).bg(NORD_POLAR);

    let spans = vec![
        Span::styled(" Home", key_style),
        Span::styled(" canvas ", desc_style),
        Span::styled(" C-q", key_style),
        Span::styled(" detach ", desc_style),
        Span::styled(" PgUp/Dn", key_style),
        Span::styled(" scroll ", desc_style),
    ];

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(NORD_POLAR));
    frame.render_widget(bar, area);
}

/// 区切り線 — ヘッダー/ターミナル/フッター間の薄い水平線
fn draw_separator(frame: &mut ratatui::Frame, area: Rect) {
    let line = "─".repeat(area.width as usize);
    let sep = Paragraph::new(Line::from(Span::styled(
        line,
        Style::default().fg(NORD_COMMENT),
    )));
    frame.render_widget(sep, area);
}

// =============================================================================
// SP（Star Platinum）サーバー管理
// =============================================================================

/// SP が起動していなければ in-process thread で起動する
pub fn ensure_sp_running(
    port: u16,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    // TheWorld がまだ起動していなければ自動起動
    if let Err(e) = crate::daemon::process::ensure_daemon_running(crate::cli::WORLD_PORT) {
        tracing::warn!("TheWorld 自動起動失敗（Process は続行）: {}", e);
    }

    // HTTP サーバーが実際に応答するか確認
    if is_server_responding(port) {
        tracing::info!("SP already running (port={})", port);
        return Ok(());
    }

    // in-process thread で SP を起動
    tracing::info!("Starting SP server (port={})...", port);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
        rt.block_on(async {
            if let Err(e) = crate::process::run(port, false, debug_mode, cap_config).await {
                tracing::error!("SP server error: {}", e);
            }
        })
    });

    wait_for_ready(port)
}

/// SP の HTTP サーバーが応答するまでポーリング（最大5秒）
pub fn wait_for_ready(port: u16) -> Result<()> {
    let max_attempts = 50; // 100ms × 50 = 5秒

    for i in 0..max_attempts {
        match std::net::TcpStream::connect_timeout(
            &format!("[::1]:{}", port).parse().unwrap(),
            std::time::Duration::from_millis(100),
        ) {
            Ok(_) => {
                tracing::info!("SP ready (attempt {})", i + 1);
                return Ok(());
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    tracing::warn!("SP readiness check timed out, proceeding anyway");
    Ok(())
}

/// SP サーバーが応答するかチェック（TCP 接続テスト）
fn is_server_responding(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("[::1]:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}
