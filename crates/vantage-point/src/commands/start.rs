//! SP/HD 起動ユーティリティ
//!
//! `vp sp start` / `vp hd start` から共有されるヘルパー関数群。
//! - `ensure_sp_running()` — SP を detached subprocess として起動
//! - `create_tmux_session()` — Claude CLI 入りの tmux セッション作成
//! - `is_server_responding()` — TCP 疎通チェック

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::cli::{DebugModeArg, parse_debug_env};
use crate::config::Config;
use crate::process::CapabilityConfig;
use crate::protocol::DebugMode;
use crate::resolve::{self, ResolvedTarget};
use crate::terminal::state::TerminalState;
use crate::tui::input::key_to_pty_bytes;
use crate::tui::terminal_widget::TerminalView;
/// 旧 `vp start` の起動オプション（後方互換のため残存）
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

/// 旧 `vp start` エントリーポイント（現在は未使用、後方互換のため残存）
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
    };

    // Step 2: モード別ルーティング
    if headless {
        run_headless(resolved.port, debug_mode, cap_config)
    } else if browser {
        run_browser(resolved.port, debug_mode, cap_config)
    } else if gui {
        run_gui(resolved.port, &resolved.name, debug_mode, cap_config)
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
    rt.block_on(async { crate::process::run(port, false, debug_mode, cap_config).await })
}

/// Browser モード: SP を確保してブラウザで開く
fn run_browser(port: u16, debug_mode: DebugMode, cap_config: CapabilityConfig) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let server_handle =
            tokio::spawn(
                async move { crate::process::run(port, false, debug_mode, cap_config).await },
            );

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

    let terminal_token =
        crate::discovery::fetch_terminal_token_blocking(port).ok_or_else(|| {
            anyhow::anyhow!(
                "Terminal token not found for port {}. Process may not be fully started.",
                port
            )
        })?;

    let result = crate::terminal_window::run_terminal_unison(port, &terminal_token, project_name);

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
    let session_name = crate::tmux::session_name(project_name);
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
    std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
        .args(["has-session", "-t", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// tmux セッション作成（Claude CLI を中で起動、ステータスバー非表示）
///
/// `--continue` 付きで起動し、即死した場合は `--continue` なしでフォールバック。
pub fn create_tmux_session(
    name: &str,
    project_dir: &str,
    cols: u16,
    rows: u16,
    process_port: u16,
) -> Result<()> {
    let mut mise_envs = collect_mise_env(project_dir);
    // VP_PROCESS_PORT を注入 → CC が起動する MCP プロセスに自動伝播
    mise_envs.push(("VP_PROCESS_PORT".to_string(), process_port.to_string()));

    // まず --continue 付きで試行
    let created = try_create_tmux_claude(name, project_dir, cols, rows, &mise_envs, true)?;
    if !created {
        anyhow::bail!("tmux セッション作成に失敗: {}", name);
    }

    // セッションが即死していないか確認（claude --continue が壊れたセッションで落ちるケース）
    // ccws ワーカー環境ではセッション履歴がなく --continue が即死するため、十分に待つ
    std::thread::sleep(std::time::Duration::from_millis(1500));
    if !tmux_session_exists(name) {
        tracing::warn!("claude --continue が即死。--continue なしでフォールバック");
        let created = try_create_tmux_claude(name, project_dir, cols, rows, &mise_envs, false)?;
        if !created {
            anyhow::bail!("tmux セッション作成に失敗（フォールバック）: {}", name);
        }
    }

    if !mise_envs.is_empty() {
        tracing::info!("mise env: {} 変数を tmux セッションに注入", mise_envs.len());
    }

    // TUI が自前のヘッダー/フッターを持つため tmux ステータスバーを非表示
    let _ = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
        .args(["set-option", "-t", name, "status", "off"])
        .status();

    // mise 環境変数を tmux セッションにも set-environment（後続ペイン用）
    for (key, value) in &mise_envs {
        let _ = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
            .args(["set-environment", "-t", name, key, value])
            .status();
    }

    Ok(())
}

/// tmux new-session で Claude CLI を起動（成功なら true）
pub fn try_create_tmux_claude(
    name: &str,
    project_dir: &str,
    cols: u16,
    rows: u16,
    mise_envs: &[(String, String)],
    with_continue: bool,
) -> Result<bool> {
    let mut args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        name.to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
        "-c".to_string(),
        project_dir.to_string(),
    ];
    for (key, value) in mise_envs {
        args.push("-e".to_string());
        args.push(format!("{}={}", key, value));
    }
    // zsh -lc でラップ: tmux の直接 exec ではシェル初期化が走らず
    // claude が依存する PATH/環境変数が不足して即死するケースを回避
    let mut claude_cmd = "claude --dangerously-skip-permissions".to_string();
    if with_continue {
        claude_cmd.push_str(" --continue");
    }
    args.push("zsh".to_string());
    args.push("-lc".to_string());
    args.push(claude_cmd);

    let status = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux")).args(&args).status()?;

    Ok(status.success())
}

/// mise env を project_dir で評価し、環境変数の (key, value) ペアを返す
///
/// mise が未インストール or .mise.toml がなければ空 Vec を返す（ベストエフォート）。
pub fn collect_mise_env(project_dir: &str) -> Vec<(String, String)> {
    let mise_bin = dirs::home_dir()
        .map(|h| h.join(".local/bin/mise"))
        .unwrap_or_else(|| "mise".into());

    let output = std::process::Command::new(&mise_bin)
        .args(["env", "--shell", "bash"])
        .current_dir(project_dir)
        .output();

    let Ok(output) = output else {
        return vec![];
    };
    if !output.status.success() {
        return vec![];
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // "export KEY=VALUE" → (KEY, VALUE)
            let line = line.strip_prefix("export ")?;
            let (key, value) = line.split_once('=')?;
            // クォート除去
            let value = value.trim_matches('\'').trim_matches('"');
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// tmux セッションのリサイズ
fn resize_tmux_session(name: &str, cols: u16, rows: u16) {
    let _ = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
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
    // マウスキャプチャ有効化 — スクロールイベントを PTY(tmux) に転送するため
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture
    )?;

    // ウィンドウタイトル設定
    {
        use std::io::Write as _;
        let _ = write!(io::stdout(), "\x1b]0;HD: {}\x07", project_name);
        let _ = io::stdout().flush();
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let size = terminal.size()?;
    // ヘッダー1行 + 区切り線1行 + フッター1行 + 区切り線1行 = 4行分
    // Block 枠分を差し引き: ヘッダー1 + 枠上1 + 枠下1 + フッター1 = 4行, 枠左右 = 2列
    let pty_cols = size.width as usize;
    let pty_lines = size.height as usize;

    // tmux セッション確保
    if !is_reconnect {
        create_tmux_session(
            session_name,
            project_dir,
            pty_cols as u16,
            pty_lines as u16,
            port,
        )?;
    } else {
        resize_tmux_session(session_name, pty_cols as u16, pty_lines as u16);
        // 再接続時もステータスバーを非表示にする
        let _ = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
            .args(["set-option", "-t", session_name, "status", "off"])
            .status();
        // 再接続時も VP_PROCESS_PORT を注入（ポート変更に追従）
        let _ = std::process::Command::new(crate::tmux::tmux_bin().unwrap_or("tmux"))
            .args([
                "set-environment",
                "-t",
                session_name,
                "VP_PROCESS_PORT",
                &port.to_string(),
            ])
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
    // PTY master を保持（リサイズ用）
    let pty_master = pair.master;

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
            terminal.draw(|frame| {
                // PTY をフルスクリーンで描画（ヘッダ・フッタ・ボーダーなし）
                let state = term_state.lock().unwrap();
                let snap = state.snapshot();
                let view = TerminalView::new(&snap);
                frame.render_widget(view, frame.area());
            })?;
        }

        needs_redraw = true;

        // イベントポーリング
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => {
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
                Event::Paste(text) => {
                    // ブラケットペースト: tmux にそのまま転送
                    let mut bytes = Vec::new();
                    bytes.extend_from_slice(b"\x1b[200~");
                    bytes.extend_from_slice(text.as_bytes());
                    bytes.extend_from_slice(b"\x1b[201~");
                    if writer.write_all(&bytes).is_err() {
                        break Ok(());
                    }
                    let _ = writer.flush();
                }
                Event::Mouse(mouse) => {
                    // マウスイベントを SGR エスケープシーケンスで PTY(tmux) に転送
                    // tmux の `mouse on` がスクロール・ペインクリック等を処理する
                    // Block 枠分のオフセット: 上枠1行 + ヘッダー1行 = y+2, 左枠 = x+1
                    let x = mouse.column.saturating_sub(1);
                    let y = mouse.row.saturating_sub(2);
                    let seq = match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            format!("\x1b[<64;{};{}M", x + 1, y + 1)
                        }
                        MouseEventKind::ScrollDown => {
                            format!("\x1b[<65;{};{}M", x + 1, y + 1)
                        }
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
                        MouseEventKind::Moved => {
                            format!("\x1b[<35;{};{}M", x + 1, y + 1)
                        }
                        _ => String::new(),
                    };
                    if !seq.is_empty() {
                        if writer.write_all(seq.as_bytes()).is_err() {
                            break Ok(());
                        }
                        let _ = writer.flush();
                    }
                }
                Event::Resize(new_cols, new_rows) => {
                    // 親ウィンドウのリサイズに追従（Block 枠分を差し引き）
                    let new_pty_cols = new_cols as usize;
                    let new_pty_lines = new_rows as usize;

                    // PTY リサイズ
                    let _ = pty_master.resize(PtySize {
                        rows: new_pty_lines as u16,
                        cols: new_pty_cols as u16,
                        pixel_width: 0,
                        pixel_height: 0,
                    });

                    // tmux セッションもリサイズ
                    resize_tmux_session(session_name, new_pty_cols as u16, new_pty_lines as u16);

                    // ターミナルエミュレータ状態をリサイズ
                    {
                        let mut state = term_state.lock().unwrap();
                        state.resize(new_pty_cols, new_pty_lines);
                    }
                }
                _ => {}
            }
        }
    };

    // 終了処理（tmux セッションは殺さない — detach のみ）
    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    println!(
        "\u{1f44b} TUI を終了しました。tmux セッション '{}' は継続中です。",
        session_name
    );
    println!("   再接続: vp hd attach");

    result
}

// =============================================================================
// ヘッダー / フッター描画（Nord テーマ）
// =============================================================================

// =============================================================================
// SP（Star Platinum）サーバー管理
// =============================================================================

/// SP が起動していなければ detached subprocess として起動する
///
/// `vp sp start -C <dir>` を独立プロセスとして spawn し、
/// health check で起動完了を待つ。SP はこのプロセスが終了しても生存する。
pub fn ensure_sp_running(
    port: u16,
    _debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    // TheWorld がまだ起動していなければ自動起動
    if let Err(e) = crate::daemon::process::ensure_daemon_running(crate::cli::WORLD_PORT) {
        tracing::warn!("TheWorld 自動起動失敗（SP は続行）: {}", e);
    }

    // 既に起動中ならスキップ
    if is_server_responding(port) {
        tracing::info!("SP already running (port={})", port);
        return Ok(());
    }

    // detached subprocess として SP を起動
    tracing::info!("Starting SP server (port={})...", port);
    spawn_sp_detached(&cap_config.project_dir, Some(port))?;

    wait_for_ready(port)
}

/// SP を detached subprocess として spawn
///
/// `vp sp start -C <dir> [-p <port>]` を独立プロセスとして起動。
/// 呼び出し元が終了しても SP は生存する。
pub fn spawn_sp_detached(project_dir: &str, port: Option<u16>) -> Result<()> {
    let vp_bin = crate::cli::which_vp()
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| "vp".into());

    let mut args = vec!["sp".to_string(), "start".to_string()];
    args.push("-C".to_string());
    args.push(project_dir.to_string());
    if let Some(p) = port {
        args.push("-p".to_string());
        args.push(p.to_string());
    }

    std::process::Command::new(&vp_bin)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("SP spawn 失敗: {}", e))?;

    Ok(())
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
pub fn is_server_responding(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("[::1]:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}
