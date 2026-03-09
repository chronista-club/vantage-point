//! `vp start` コマンドの実行ロジック

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

    // --project-dir が指定されていればそれを最優先
    // already_running: 既存 Process に re-attach する場合 true（pre-registration をスキップ）
    let (resolved_project_dir, resolved_port, _already_running) = if let Some(ref dir) = project_dir
    {
        let dir_normalized = Config::normalize_path(std::path::Path::new(dir));

        // 既に実行中かチェック
        if let Some(running) = crate::discovery::find_by_project_blocking(&dir_normalized) {
            if headless || browser {
                let name = resolve::project_name_from_path(&running.project_dir, config);
                println!(
                    "Already running: {} (port {}). Use `vp stop` first.",
                    name, running.port
                );
                return Ok(());
            }
            // ターミナルモード: 既存 Process に re-attach
            (dir_normalized, running.port, true)
        } else {
            let idx = config.find_project_index(&dir_normalized);
            let p = if let Some(explicit) = port {
                explicit
            } else if let Some(i) = idx {
                resolve::port_for_configured(i, config)?
            } else {
                resolve::find_available_port()
                    .ok_or_else(|| anyhow::anyhow!("No available ports in range"))?
            };
            (dir_normalized, p, false)
        }
    } else {
        // target ベースの解決
        let resolved = resolve::resolve_target(target.as_deref(), config)?;
        match resolved {
            ResolvedTarget::Running {
                port: running_port,
                name,
                project_dir: proj_dir,
            } => {
                if headless || browser {
                    println!(
                        "Already running: {} (port {}). Use `vp stop` first.",
                        name, running_port
                    );
                    return Ok(());
                }
                // ターミナルモード: 既存 Process に re-attach
                println!(
                    "\u{1f517} Re-attaching to: {} (port {})",
                    name, running_port
                );
                (proj_dir, running_port, true)
            }
            ResolvedTarget::Configured { name, path, index } => {
                println!("\u{1f4c1} Project: {}", name);
                let p = if let Some(explicit) = port {
                    explicit
                } else {
                    resolve::port_for_configured(index, config)?
                };
                (path, p, false)
            }
            ResolvedTarget::Cwd { path } => {
                let p = if let Some(explicit) = port {
                    explicit
                } else {
                    resolve::find_available_port()
                        .ok_or_else(|| anyhow::anyhow!("No available ports in range"))?
                };
                (path, p, false)
            }
        }
    };

    println!("\u{1f50c} Using port {}", resolved_port);

    // tmux exec 判定: TUI モードかつ tmux 外なら、この後 exec で tmux に置き換わる
    // → pre-registration すると tmux の PID が登録されてしまうのでスキップ
    let _will_exec_tmux = !headless
        && !browser
        && !gui
        && crate::tmux::is_tmux_available()
        && !crate::tmux::is_inside_tmux();

    // ポート予約は不要 — server.rs が起動後に TheWorld に登録する。
    // ポート衝突防止は is_port_available() のバインドテストで十分。

    // デバッグモード: CLI > env > default
    let debug_mode = debug
        .map(DebugMode::from)
        .or_else(parse_debug_env)
        .unwrap_or_default();

    if debug_mode != DebugMode::None {
        tracing::info!("Debug mode: {:?}", debug_mode);
    }

    tracing::info!("Project dir: {}", resolved_project_dir);

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

    // CapabilityConfig
    let cap_config = crate::process::CapabilityConfig {
        project_dir: resolved_project_dir.clone(),
        midi_config,
        bonjour_port: Some(resolved_port),
    };

    if headless || browser {
        // Headless / Browser モード: HTTP サーバーのみ
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let server_handle = tokio::spawn(async move {
                crate::process::run(resolved_port, false, debug_mode, cap_config).await
            });

            if browser {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let url = format!("http://localhost:{}", resolved_port);
                tracing::info!("Opening in browser: {}", url);
                let _ = open::that(&url);
            }

            server_handle.await?
        })
    } else if gui {
        // GUI モード: ネイティブウィンドウ（Unison ブリッジ）
        if let Err(e) =
            ensure_process_running(resolved_port, &resolved_project_dir, debug_mode, cap_config)
        {
            return Err(e);
        }

        let project_name = resolve::project_name_from_path(&resolved_project_dir, config);

        // Health API から認証トークンを取得
        let terminal_token = crate::discovery::fetch_terminal_token_blocking(resolved_port)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Terminal token not found for port {}. Process may not be fully started.",
                    resolved_port
                )
            })?;

        let result = crate::terminal_window::run_terminal_unison(
            resolved_port,
            &terminal_token,
            &project_name,
        );

        match result {
            Ok(()) => tracing::info!("Terminal window closed (Process is still running)"),
            Err(e) => tracing::error!("Terminal window error: {}", e),
        }

        Ok(())
    } else {
        // デフォルト: TUI モード（ratatui ベースの対話コンソール）
        let project_name =
            resolve::project_name_from_path(&resolved_project_dir, config).to_string();

        // tmux が利用可能で、まだ tmux 内でなければ、tmux session を作って再 exec
        if crate::tmux::is_tmux_available() && !crate::tmux::is_inside_tmux() {
            let session = crate::tmux::session_name(&project_name);
            if crate::tmux::session_exists(&session) {
                // 既存セッションにアタッチ（Process は既に動いている）
                crate::tmux::attach_and_exec(&session); // never returns
            } else {
                // tmux new-session で自分自身を再実行
                let vp_bin = std::env::current_exe()?;
                crate::tmux::create_and_exec(
                    &session,
                    &vp_bin,
                    &["start", "--project-dir", &resolved_project_dir],
                ); // never returns
            }
        }

        // tmux 内 or tmux なし: 従来通り TUI 起動
        // Process サーバーを headless で起動（Canvas / API 用）
        if let Err(e) =
            ensure_process_running(resolved_port, &resolved_project_dir, debug_mode, cap_config)
        {
            return Err(e);
        }

        // TUI 起動（Canvas は Ctrl+O で随時 toggle）
        crate::tui::run_tui(&resolved_project_dir, &project_name)
    }
}

/// Process が起動していなければ headless で起動する
///
/// running.json + PID チェックで既存 Process を探し、
/// 見つからなければ自プロセスを `vp start --headless` で再起動してデタッチ。
pub fn ensure_process_running(
    port: u16,
    _project_dir: &str,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    // HTTP サーバーが実際に応答するか確認（PID だけでは TUI プロセスと区別できない）
    if is_server_responding(port) {
        tracing::info!("Process already running and responding (port={})", port);
        return Ok(());
    }

    // headless で Process を起動（in-process スレッド）
    tracing::info!("Starting Process server (port={})...", port);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
        rt.block_on(async {
            if let Err(e) = crate::process::run(port, false, debug_mode, cap_config).await {
                tracing::error!("Process server error: {}", e);
            }
        })
    });

    // Process の HTTP サーバーが ready になるまでポーリング
    wait_for_process_ready(port)?;

    Ok(())
}

/// Process の HTTP サーバーが応答するまでポーリング（最大5秒）
pub fn wait_for_process_ready(port: u16) -> Result<()> {
    let url = format!("http://[::1]:{}/health", port);
    let max_attempts = 50; // 100ms × 50 = 5秒

    for i in 0..max_attempts {
        match std::net::TcpStream::connect_timeout(
            &format!("[::1]:{}", port).parse().unwrap(),
            std::time::Duration::from_millis(100),
        ) {
            Ok(_) => {
                tracing::info!("Process ready (attempt {})", i + 1);
                return Ok(());
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    // TCP 接続できなくてもタイムアウトで続行（WS 接続時にリトライするため）
    tracing::warn!("Process readiness check timed out, proceeding anyway");
    let _ = url;
    Ok(())
}

/// Process サーバーが実際に HTTP 応答するかチェック（TCP 接続のみ）
fn is_server_responding(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("[::1]:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
}
