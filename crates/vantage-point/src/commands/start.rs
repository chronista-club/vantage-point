//! `vp start` コマンドの実行ロジック

use anyhow::Result;

use crate::cli::{DebugModeArg, parse_debug_env};
use crate::config::{Config, RunningStands};
use crate::protocol::DebugMode;
use crate::resolve::{self, ResolvedTarget};
use crate::stand::CapabilityConfig;

/// `vp start` の起動オプション
pub struct StartOptions<'a> {
    pub target: Option<String>,
    pub port: Option<u16>,
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
        headless,
        browser,
        debug,
        project_dir,
        midi,
        config,
    } = opts;

    // --project-dir が指定されていればそれを最優先
    let (resolved_project_dir, resolved_port) = if let Some(ref dir) = project_dir {
        let dir_normalized = Config::normalize_path(std::path::Path::new(dir));

        // 既に実行中かチェック
        if let Some(stand) = RunningStands::find_by_project(&dir_normalized) {
            if headless || browser {
                let name = resolve::project_name_from_path(&stand.project_dir, config);
                println!(
                    "Already running: {} (port {}). Use `vp stop` first.",
                    name, stand.port
                );
                return Ok(());
            }
            // ターミナルモード: 既存 Stand に re-attach
            (dir_normalized, stand.port)
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
            (dir_normalized, p)
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
                // ターミナルモード: 既存 Stand に re-attach
                println!(
                    "\u{1f517} Re-attaching to: {} (port {})",
                    name, running_port
                );
                (proj_dir, running_port)
            }
            ResolvedTarget::Configured { name, path, index } => {
                println!("\u{1f4c1} Project: {}", name);
                let p = if let Some(explicit) = port {
                    explicit
                } else {
                    resolve::port_for_configured(index, config)?
                };
                (path, p)
            }
            ResolvedTarget::Cwd { path } => {
                let p = if let Some(explicit) = port {
                    explicit
                } else {
                    resolve::find_available_port()
                        .ok_or_else(|| anyhow::anyhow!("No available ports in range"))?
                };
                (path, p)
            }
        }
    };

    println!("\u{1f50c} Using port {}", resolved_port);

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
    let cap_config = crate::stand::CapabilityConfig {
        project_dir: resolved_project_dir.clone(),
        midi_config,
        bonjour_port: Some(resolved_port),
    };

    if headless || browser {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let server_handle = tokio::spawn(async move {
                crate::stand::run(resolved_port, false, debug_mode, cap_config).await
            });

            if browser {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let url = format!("http://localhost:{}", resolved_port);
                tracing::info!("Opening in browser: {}", url);
                let _ = open::that(&url);
            }

            server_handle.await?
        })
    } else {
        // ネイティブターミナルモード（Unison ブリッジ）
        // Stand が起動していなければ headless で起動
        ensure_stand_running(resolved_port, &resolved_project_dir, debug_mode, cap_config)?;

        // Canvas 自動起動
        if let Err(e) = crate::canvas::run_canvas_detached(resolved_port) {
            tracing::warn!("Canvas 自動起動失敗: {}", e);
        }

        // Unison ブリッジモードのネイティブウィンドウ
        let result = crate::terminal_window::run_terminal_unison(resolved_port);

        match result {
            Ok(()) => tracing::info!("Terminal window closed (Stand is still running)"),
            Err(e) => tracing::error!("Terminal window error: {}", e),
        }

        Ok(())
    }
}

/// Stand が起動していなければ headless で起動する
///
/// running.json + PID チェックで既存 Stand を探し、
/// 見つからなければ自プロセスを `vp start --headless` で再起動してデタッチ。
fn ensure_stand_running(
    port: u16,
    project_dir: &str,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    // running.json で既に起動済みか確認
    if let Some(stand) = RunningStands::find_by_project(project_dir) {
        if stand.port == port {
            tracing::info!("Stand already running (port={}, pid={})", port, stand.pid);
            return Ok(());
        }
    }

    // headless で Stand を起動（in-process スレッド）
    tracing::info!("Starting Stand server (port={})...", port);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
        rt.block_on(async {
            if let Err(e) = crate::stand::run(port, false, debug_mode, cap_config).await {
                tracing::error!("Stand server error: {}", e);
            }
        })
    });

    // Stand の HTTP サーバーが ready になるまでポーリング
    wait_for_stand_ready(port)?;

    Ok(())
}

/// Stand の HTTP サーバーが応答するまでポーリング（最大5秒）
pub fn wait_for_stand_ready(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/health", port);
    let max_attempts = 50; // 100ms × 50 = 5秒

    for i in 0..max_attempts {
        match std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port).parse().unwrap(),
            std::time::Duration::from_millis(100),
        ) {
            Ok(_) => {
                tracing::info!("Stand ready (attempt {})", i + 1);
                return Ok(());
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }

    // TCP 接続できなくてもタイムアウトで続行（WS 接続時にリトライするため）
    tracing::warn!("Stand readiness check timed out, proceeding anyway");
    let _ = url;
    Ok(())
}
