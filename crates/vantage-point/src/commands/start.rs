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
//!   │     └── default     → run_tui_mode()   SP 確保 → tmux/TUI
//!   └── 共通: ensure_sp_running()            SP 未起動なら in-process spawn
//! ```

use anyhow::Result;

use crate::cli::{DebugModeArg, parse_debug_env};
use crate::config::Config;
use crate::process::CapabilityConfig;
use crate::protocol::DebugMode;
use crate::resolve::{self, ResolvedTarget};

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
        run_gui(resolved.port, &resolved.dir, &resolved.name, debug_mode, cap_config, config)
    } else {
        run_tui_mode(resolved.port, &resolved.dir, &resolved.name, debug_mode, cap_config)
    }
}

// =============================================================================
// Step 1: ターゲット解決
// =============================================================================

/// CLI 引数からプロジェクト情報を解決する
///
/// --project-dir 指定 → 正規化して使用
/// それ以外 → resolve_target() でプロジェクト名/インデックス/cwd から解決
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

    // 既に実行中？
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
    _project_dir: &str,
    project_name: &str,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
    _config: &Config,
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

/// TUI モード: SP を確保 → tmux セッション管理 → TUI 起動
fn run_tui_mode(
    port: u16,
    project_dir: &str,
    project_name: &str,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    // tmux が使える場合はセッション管理を先に行う
    // （tmux exec で自プロセスが置き換わる場合があるため、SP 起動より先）
    if crate::tmux::is_tmux_available() {
        match enter_tmux_session(project_name, project_dir)? {
            TmuxAction::RunTuiHere => {
                // 自セッション内 or tmux なし → SP 起動して TUI 実行
            }
            TmuxAction::Switched => {
                // 別セッションに switch-client した → このプロセスは終了
                return Ok(());
            }
            // ExecNeverReturns は型上到達しないが、! 型の代わりに明示
        }
    }

    // SP サーバーを確保（未起動なら in-process thread で起動）
    ensure_sp_running(port, debug_mode, cap_config)?;

    // TUI 起動（Canvas は Ctrl+O で随時 toggle）
    crate::tui::run_tui(project_dir, project_name)
}

// =============================================================================
// tmux セッション管理
// =============================================================================

/// tmux セッション管理の結果
enum TmuxAction {
    /// 自セッション内にいる → このプロセスで TUI を起動
    RunTuiHere,
    /// 別セッションに切り替えた → このプロセスは終了してよい
    Switched,
    // Note: attach_and_exec / create_and_exec は exec() で戻らないため enum 不要
}

/// tmux セッションに入る
///
/// 3 パターンを処理:
/// 1. tmux 外 → セッション作成 or アタッチ（exec で戻らない）
/// 2. tmux 内 + 別セッション → switch-client
/// 3. tmux 内 + 自セッション → RunTuiHere（TUI をここで起動）
fn enter_tmux_session(project_name: &str, project_dir: &str) -> Result<TmuxAction> {
    let session = crate::tmux::session_name(project_name);

    if !crate::tmux::is_inside_tmux() {
        // パターン 1: tmux 外 → exec でプロセス置換（戻らない）
        if crate::tmux::session_exists(&session) {
            crate::tmux::attach_and_exec(&session); // never returns
        } else {
            let vp_bin = std::env::current_exe()?;
            crate::tmux::create_and_exec(
                &session,
                &vp_bin,
                &["start", "--project-dir", project_dir],
            ); // never returns
        }
    }

    // tmux 内
    if crate::tmux::is_in_session(&session) {
        // パターン 3: 自セッション → TUI をここで起動
        return Ok(TmuxAction::RunTuiHere);
    }

    // パターン 2: 別セッション内 → ターゲットセッションに切り替え
    if !crate::tmux::session_exists(&session) {
        let vp_bin = std::env::current_exe()?;
        crate::tmux::create_detached(
            &session,
            &vp_bin,
            &["start", "--project-dir", project_dir],
        )?;
    }
    println!("\u{1f500} Switching to tmux session: {}", session);
    crate::tmux::switch_client(&session);
    Ok(TmuxAction::Switched)
}

// =============================================================================
// SP（Star Platinum）サーバー管理
// =============================================================================

/// SP が起動していなければ in-process thread で起動する
///
/// TheWorld も自動起動（Lane ビュー等に必要）。
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

    // タイムアウトでも続行（WS 接続時にリトライするため）
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
