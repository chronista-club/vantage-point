//! Vantage Point Agent - AI協働開発プラットフォーム
//!
//! Usage:
//!   vp start    # デーモンを起動（HTTP + WebSocket）
//!   vp mcp      # MCPサーバーとして起動（stdio）
//!   vp status   # デーモンの稼働状態を確認
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # デバッグ表示モード
//!   VANTAGE_PROJECT_DIR=/path/to/project  # デフォルトプロジェクトディレクトリ
//!
//! Config file: ~/.config/vantage/config.toml

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

mod agent;
mod agui;
mod config;
mod daemon;
mod mcp;
mod midi;
mod protocol;
mod tray;
mod webview;

use config::Config;
use protocol::DebugMode;

/// Health response from daemon
#[derive(serde::Deserialize)]
struct HealthResponse {
    status: String,
    version: String,
    pid: u32,
    #[serde(default)]
    project_dir: Option<String>,
}

/// Check if daemon is running on the specified port
async fn check_status(port: u16) -> Result<()> {
    let url = format!("http://localhost:{}/api/health", port);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    match client.get(&url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<HealthResponse>().await {
                    Ok(health) => {
                        println!("✓ vp is running on port {}", port);
                        println!("  Version: {}", health.version);
                        println!("  PID: {}", health.pid);
                        if let Some(ref dir) = health.project_dir {
                            println!("  Project: {}", dir);
                        }
                        println!("  Status: {}", health.status);
                    }
                    Err(_) => {
                        // Old version returning plain text
                        println!("✓ vp is running on port {}", port);
                    }
                }
            } else {
                println!("✗ vp returned error: {}", response.status());
            }
        }
        Err(e) => {
            if e.is_connect() {
                println!("✗ vp is not running on port {}", port);
            } else if e.is_timeout() {
                println!("✗ vp is not responding (timeout)");
            } else {
                println!("✗ Failed to connect: {}", e);
            }
        }
    }

    Ok(())
}

/// Stop the daemon running on the specified port
async fn stop_daemon(port: u16) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    // First, get the PID via health endpoint
    let health_url = format!("http://localhost:{}/api/health", port);
    let pid = match client.get(&health_url).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<HealthResponse>().await {
                Ok(health) => Some(health.pid),
                Err(_) => None,
            }
        }
        Ok(_) => None,
        Err(e) => {
            if e.is_connect() {
                println!("✗ vp is not running on port {}", port);
                return Ok(());
            }
            None
        }
    };

    let Some(pid) = pid else {
        println!("✗ Could not get daemon PID");
        return Ok(());
    };

    println!("Stopping vp (PID: {})...", pid);

    // Request graceful shutdown via API
    let shutdown_url = format!("http://localhost:{}/api/shutdown", port);
    let _ = client.post(&shutdown_url).send().await;

    // Wait up to 10 seconds for graceful shutdown
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(10);

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Check if process is still running
        if !is_process_running(pid) {
            println!("✓ vp stopped gracefully");
            return Ok(());
        }

        if start.elapsed() > timeout {
            println!("⚠ Graceful shutdown timed out, forcing kill...");
            force_kill(pid);
            println!("✓ vp force killed");
            return Ok(());
        }
    }
}

/// Check if a process is still running
#[cfg(unix)]
fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_process_running(_pid: u32) -> bool {
    false
}

/// Force kill a process
#[cfg(unix)]
fn force_kill(pid: u32) {
    use std::process::Command;
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}

#[cfg(not(unix))]
fn force_kill(_pid: u32) {}

/// Default port range to scan for instances
const PORT_RANGE_START: u16 = 33000;
const PORT_RANGE_END: u16 = 33010;

/// Running instance info
#[derive(Clone)]
struct Instance {
    port: u16,
    pid: u32,
    version: String,
    project_dir: Option<String>,
}

/// Scan for running vp instances
async fn scan_instances() -> Vec<Instance> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap();

    let mut instances = Vec::new();

    for port in PORT_RANGE_START..=PORT_RANGE_END {
        let url = format!("http://localhost:{}/api/health", port);
        if let Ok(response) = client.get(&url).send().await
            && response.status().is_success()
                && let Ok(health) = response.json::<HealthResponse>().await {
                    instances.push(Instance {
                        port,
                        pid: health.pid,
                        version: health.version,
                        project_dir: health.project_dir,
                    });
                }
    }

    instances
}

/// Find the first available port in the range
async fn find_available_port() -> Option<u16> {
    let used_ports: std::collections::HashSet<u16> =
        scan_instances().await.into_iter().map(|i| i.port).collect();

    for port in PORT_RANGE_START..=PORT_RANGE_END {
        if !used_ports.contains(&port) {
            return Some(port);
        }
    }
    None
}

/// List all running vp instances
async fn list_instances() -> Result<()> {
    println!("Scanning ports {}–{}...", PORT_RANGE_START, PORT_RANGE_END);

    let instances = scan_instances().await;

    if instances.is_empty() {
        println!("No running vp instances found.");
        return Ok(());
    }

    println!();
    println!("  #  PORT   PID     PROJECT");
    println!("  ─  ────   ───     ───────");
    for (i, inst) in instances.iter().enumerate() {
        let project = inst.project_dir.as_deref().unwrap_or("-");
        // Shorten long paths
        let project_display = if project.len() > 40 {
            format!("...{}", &project[project.len() - 37..])
        } else {
            project.to_string()
        };
        println!(
            "  {}  {}  {:>5}   {}",
            i + 1, inst.port, inst.pid, project_display
        );
    }
    println!();
    println!("Use `vp open <#>` to open WebUI (# starts from 1)");

    Ok(())
}

/// Open WebUI for a specific instance
async fn open_instance(index: usize) -> Result<()> {
    let instances = scan_instances().await;

    if instances.is_empty() {
        println!("No running vp instances found.");
        return Ok(());
    }

    // Convert 1-based to 0-based
    if index == 0 || index > instances.len() {
        println!(
            "✗ Invalid index {}. Available: 1–{}",
            index,
            instances.len()
        );
        return Ok(());
    }

    let inst = &instances[index - 1];
    let url = format!("http://localhost:{}", inst.port);
    println!("Opening {} (PID: {})...", url, inst.pid);

    if let Err(e) = open::that(&url) {
        println!("✗ Failed to open browser: {}", e);
    } else {
        println!("✓ Opened in browser");
    }

    Ok(())
}

/// CLIデバッグモード
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum DebugModeArg {
    /// デバッグ情報なし
    #[default]
    None,
    /// 簡易デバッグ（セッションID、タイミング）
    Simple,
    /// 詳細デバッグ（JSON全体、全イベント）
    Detail,
}

impl From<DebugModeArg> for DebugMode {
    fn from(arg: DebugModeArg) -> Self {
        match arg {
            DebugModeArg::None => DebugMode::None,
            DebugModeArg::Simple => DebugMode::Simple,
            DebugModeArg::Detail => DebugMode::Detail,
        }
    }
}

/// Parse debug mode from environment variable
fn parse_debug_env() -> Option<DebugMode> {
    std::env::var("VANTAGE_DEBUG")
        .ok()
        .and_then(|v| match v.to_lowercase().as_str() {
            "none" | "off" | "0" | "false" => Some(DebugMode::None),
            "simple" | "1" | "true" => Some(DebugMode::Simple),
            "detail" | "detailed" | "2" | "verbose" => Some(DebugMode::Detail),
            _ => None,
        })
}

#[derive(Parser)]
#[command(name = "vp")]
#[command(version)]
#[command(about = "Vantage Point Agent - AI協働開発プラットフォーム")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// デーモンを起動（HTTPサーバー + WebSocketハブ）[デフォルト]
    Start {
        /// プロジェクト番号（`vp config`で確認、1始まり）
        #[arg()]
        project_index: Option<usize>,

        /// 待ち受けポート番号
        #[arg(short, long)]
        port: Option<u16>,

        /// ビューアを開かない（ヘッドレスモード）
        #[arg(long)]
        headless: bool,

        /// ネイティブWebViewの代わりにシステムブラウザを使用
        #[arg(long)]
        browser: bool,

        /// デバッグ表示モード（VANTAGE_DEBUG環境変数より優先）
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,

        /// プロジェクトディレクトリ（project_indexより優先）
        #[arg(long, short = 'C')]
        project_dir: Option<String>,

        /// MIDI入力を有効化（ポート番号または名前パターン）
        #[arg(long, short = 'm')]
        midi: Option<String>,
    },
    /// 設定と登録済みプロジェクトを表示
    Config,
    /// MCPサーバーとして起動（stdio JSON-RPC）
    Mcp,
    /// デーモンの稼働状態を確認
    Status {
        /// 確認するポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// デーモンを停止
    Stop {
        /// 停止するデーモンのポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// デーモンを再起動（セッション状態を保持）
    Restart {
        /// 再起動するデーモンのポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,

        /// ネイティブWebViewの代わりにシステムブラウザを使用
        #[arg(long)]
        browser: bool,

        /// ビューアを開かない（ヘッドレスモード）
        #[arg(long)]
        headless: bool,
    },
    /// 稼働中のインスタンス一覧
    #[command(alias = "list")]
    Ps,
    /// 指定インスタンスのWebUIを開く
    Open {
        /// インスタンス番号（`vp ps`で確認、1始まり）
        #[arg(default_value = "1")]
        index: usize,
    },
    /// メニューバーアイコンとして起動（システムトレイ）
    Tray {
        /// MIDI入力を有効化（ポート番号または名前パターン）
        #[arg(long, short = 'm')]
        midi: Option<String>,
    },
    /// WebViewウィンドウのみを開く（デーモンは別途起動済み）
    Webview {
        /// 接続先ポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// 利用可能なMIDI入力ポート一覧
    MidiPorts,
    /// MIDI入力の監視を開始
    Midi {
        /// 接続するMIDIポート番号
        #[arg(short, long)]
        port: Option<usize>,
        /// アクション送信先のデーモンポート
        #[arg(short = 'P', long, default_value = "33000")]
        daemon_port: u16,
    },
    /// LPD8コントローラー設定
    #[command(subcommand)]
    Lpd8(Lpd8Commands),
}

/// LPD8サブコマンド
#[derive(Subcommand)]
enum Lpd8Commands {
    /// VP用設定をLPD8 Program 1に書き込む
    Write {
        /// MIDIポート名のパターン（部分一致）
        #[arg(long, default_value = "LPD8")]
        port: String,
        /// 書き込み先プログラム番号（1-4）
        #[arg(short, long, default_value = "1")]
        program: u8,
    },
    /// LPD8から現在の設定を読み取る
    Read {
        /// MIDIポート名のパターン
        #[arg(long, default_value = "LPD8")]
        port: String,
        /// 読み取り元プログラム番号（1-4）
        #[arg(short, long, default_value = "1")]
        program: u8,
    },
    /// アクティブプログラムを切り替える
    Switch {
        /// プログラム番号（1-4）
        program: u8,
        /// MIDIポート名のパターン
        #[arg(long, default_value = "LPD8")]
        port: String,
    },
    /// 利用可能なMIDI出力ポート一覧
    Ports,
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("vantage_daemon=info".parse()?),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // Load config
    let config = Config::load().unwrap_or_default();

    // Default to Start if no command given
    let command = cli.command.unwrap_or(Commands::Start {
        project_index: None,
        port: None,
        headless: false,
        browser: false,
        debug: None,
        project_dir: None,
        midi: None,
    });

    match command {
        Commands::Start {
            project_index,
            port,
            headless,
            browser,
            debug,
            project_dir,
            midi,
        } => {
            // Resolve project directory
            let resolved_project_dir = if let Some(ref dir) = project_dir {
                // Explicit --project-dir takes precedence
                dir.clone()
            } else if let Some(idx) = project_index {
                // Project index from config (convert 1-based to 0-based)
                if idx == 0 || idx > config.projects.len() {
                    eprintln!(
                        "✗ Invalid project index {}. Use `vp config` to list projects (1–{}).",
                        idx,
                        config.projects.len()
                    );
                    std::process::exit(1);
                }
                let project = &config.projects[idx - 1];
                println!("📁 Project: {} ({})", project.name, project.path);
                project.path.clone()
            } else {
                // Default: cwd
                Config::resolve_project_dir(None, &config)
            };

            // Resolve port: CLI explicit > project index based (33000 + index)
            let resolved_port = if let Some(p) = port {
                // Explicit CLI port
                p
            } else {
                // Port based on project index: project 1 → 33000, project 2 → 33001, etc.
                let idx = project_index.map(|i| i.saturating_sub(1)).unwrap_or(0) as u16;
                let p = PORT_RANGE_START + idx;
                if p > PORT_RANGE_END {
                    eprintln!(
                        "✗ Project index {} exceeds port range. Max {} projects supported.",
                        idx,
                        PORT_RANGE_END - PORT_RANGE_START + 1
                    );
                    std::process::exit(1);
                }
                println!("🔌 Using port {}", p);
                p
            };

            // Determine debug mode: CLI flag > env var > default
            let debug_mode = debug
                .map(DebugMode::from)
                .or_else(parse_debug_env)
                .unwrap_or_default();

            if debug_mode != DebugMode::None {
                tracing::info!("Debug mode: {:?}", debug_mode);
            }

            tracing::info!("Project dir: {}", resolved_project_dir);

            // Setup MIDI config if enabled
            let midi_config = midi.as_ref().map(|midi_arg| {
                let mut config = midi::MidiConfig::default();
                // LPD8 pad mappings (notes 36-43)
                config
                    .note_actions
                    .insert(36, midi::MidiAction::OpenWebUI { port: None });
                config
                    .note_actions
                    .insert(37, midi::MidiAction::CancelChat { port: None });
                config
                    .note_actions
                    .insert(38, midi::MidiAction::ResetSession { port: None });

                // Set port pattern if provided as string, or port index if numeric
                if let Ok(idx) = midi_arg.parse::<usize>() {
                    (Some(idx), config)
                } else {
                    config.port_pattern = Some(midi_arg.clone());
                    (None, config)
                }
            });

            if headless || browser {
                // Headless or browser mode - use tokio runtime
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    // Start HTTP server in background
                    let project_dir = resolved_project_dir.clone();
                    let server_handle = tokio::spawn(async move {
                        daemon::run(resolved_port, false, debug_mode, project_dir).await
                    });

                    // Start MIDI monitoring if enabled
                    if let Some((port_idx, config)) = midi_config {
                        let daemon_port = resolved_port;
                        tokio::spawn(async move {
                            if let Err(e) = midi::run_midi(port_idx, config, daemon_port).await {
                                tracing::error!("MIDI error: {}", e);
                            }
                        });
                        tracing::info!("MIDI monitoring enabled");
                    }

                    if browser {
                        // Wait for server to start, then open browser
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        let url = format!("http://localhost:{}", resolved_port);
                        tracing::info!("Opening in browser: {}", url);
                        let _ = open::that(&url);
                    }

                    server_handle.await?
                })
            } else {
                // WebView mode - run server in background thread, WebView on main thread
                let project_dir = resolved_project_dir.clone();
                let midi_config_clone = midi_config.clone();
                let server_thread = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
                    rt.block_on(async {
                        // Start MIDI monitoring if enabled
                        if let Some((port_idx, config)) = midi_config_clone {
                            let daemon_port = resolved_port;
                            tokio::spawn(async move {
                                if let Err(e) = midi::run_midi(port_idx, config, daemon_port).await
                                {
                                    tracing::error!("MIDI error: {}", e);
                                }
                            });
                            tracing::info!("MIDI monitoring enabled");
                        }

                        daemon::run(resolved_port, false, debug_mode, project_dir).await
                    })
                });

                // Wait a bit for server to start
                std::thread::sleep(std::time::Duration::from_millis(300));

                // Run WebView on main thread (required by macOS)
                let webview_result = webview::run_webview(resolved_port);

                match webview_result {
                    Ok(()) => {
                        tracing::info!("WebView closed");
                    }
                    Err(e) => {
                        tracing::error!("WebView error: {}", e);
                    }
                }

                // Server thread will be terminated when main exits
                drop(server_thread);
                Ok(())
            }
        }
        Commands::Config => {
            // Show configuration
            println!("Config file: {}", Config::config_path().display());
            println!();

            if config.projects.is_empty() {
                println!("No projects registered.");
                println!();
                println!("Add projects to your config file:");
                println!("  [[projects]]");
                println!("  name = \"my-project\"");
                println!("  path = \"/path/to/project\"");
            } else {
                println!("Registered projects:");
                println!("  #  NAME                PORT    PATH");
                println!("  ─  ────                ────    ────");
                for (i, project) in config.projects.iter().enumerate() {
                    let port_str = project
                        .port
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    // Shorten long paths
                    let path_display = if project.path.len() > 40 {
                        format!("...{}", &project.path[project.path.len() - 37..])
                    } else {
                        project.path.clone()
                    };
                    println!(
                        "  {}  {:18}  {:>5}   {}",
                        i + 1, project.name, port_str, path_display
                    );
                }
                println!();
                println!("Usage: vp start <#> or vp start -C /path/to/project (# starts from 1)");
            }

            Ok(())
        }
        Commands::Mcp => {
            // MCP mode: stdio JSON-RPC server
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(33000))
        }
        Commands::Status { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(check_status(port))
        }
        Commands::Stop { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(stop_daemon(port))
        }
        Commands::Restart {
            port,
            browser,
            headless,
        } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                // Get current instance info before stopping
                let instances = scan_instances().await;
                let instance = instances.iter().find(|i| i.port == port);

                let project_dir = match instance {
                    Some(inst) => {
                        inst.project_dir.clone().unwrap_or_else(|| {
                            std::env::current_dir()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string()
                        })
                    }
                    None => {
                        println!("✗ No daemon running on port {}. Use `vp start` instead.", port);
                        return Ok(());
                    }
                };

                println!("🔄 Restarting vp on port {}...", port);
                println!("   Project: {}", project_dir);

                // Stop the daemon
                stop_daemon(port).await?;

                // Wait a moment for port to be released
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                println!("🚀 Starting daemon...");
                Ok::<(), anyhow::Error>(())
            })?;

            // Get project_dir for starting (need to get it again outside async block)
            let rt2 = tokio::runtime::Runtime::new()?;
            let project_dir = rt2.block_on(async {
                // Read from persisted state file
                let state_path = dirs::config_dir()
                    .unwrap_or_default()
                    .join("vantage")
                    .join("state")
                    .join(format!("{}.json", port));

                if let Ok(data) = std::fs::read_to_string(&state_path) {
                    if let Ok(state) = serde_json::from_str::<serde_json::Value>(&data) {
                        if let Some(dir) = state.get("project_dir").and_then(|v| v.as_str()) {
                            return dir.to_string();
                        }
                    }
                }

                std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });

            // Determine debug mode from env
            let debug_mode = parse_debug_env().unwrap_or_default();

            if headless || browser {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    let project_dir_clone = project_dir.clone();
                    let server_handle = tokio::spawn(async move {
                        daemon::run(port, false, debug_mode, project_dir_clone).await
                    });

                    if browser {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        let url = format!("http://localhost:{}", port);
                        tracing::info!("Opening in browser: {}", url);
                        let _ = open::that(&url);
                    }

                    server_handle.await?
                })
            } else {
                // WebView mode
                let project_dir_clone = project_dir.clone();
                let server_thread = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
                    rt.block_on(async { daemon::run(port, false, debug_mode, project_dir_clone).await })
                });

                std::thread::sleep(std::time::Duration::from_millis(300));

                let webview_result = webview::run_webview(port);

                match webview_result {
                    Ok(()) => tracing::info!("WebView closed"),
                    Err(e) => tracing::error!("WebView error: {}", e),
                }

                drop(server_thread);
                Ok(())
            }
        }
        Commands::Ps => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(list_instances())
        }
        Commands::Open { index } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(open_instance(index))
        }
        Commands::Tray { midi } => {
            // Start MIDI in background thread if enabled
            if let Some(ref midi_arg) = midi {
                let mut config = midi::MidiConfig::default();
                config
                    .note_actions
                    .insert(36, midi::MidiAction::OpenWebUI { port: None });
                config
                    .note_actions
                    .insert(37, midi::MidiAction::CancelChat { port: None });
                config
                    .note_actions
                    .insert(38, midi::MidiAction::ResetSession { port: None });

                let (port_idx, config) = if let Ok(idx) = midi_arg.parse::<usize>() {
                    (Some(idx), config)
                } else {
                    let mut c = config;
                    c.port_pattern = Some(midi_arg.clone());
                    (None, c)
                };

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
                    rt.block_on(async {
                        if let Err(e) = midi::run_midi(port_idx, config, 33000).await {
                            tracing::error!("MIDI error: {}", e);
                        }
                    });
                });
                tracing::info!("MIDI monitoring enabled");
            }

            tray::run_tray()
        }
        Commands::Webview { port } => {
            // Just run WebView window pointing to existing daemon
            webview::run_webview(port)
        }
        Commands::MidiPorts => {
            midi::print_ports();
            Ok(())
        }
        Commands::Midi { port, daemon_port } => {
            // Default MIDI config with example mappings
            let mut config = midi::MidiConfig::default();

            // Example: LPD8 pad mappings (notes 36-43)
            // Pad 1 (note 36) -> Open WebUI
            config
                .note_actions
                .insert(36, midi::MidiAction::OpenWebUI { port: None });
            // Pad 2 (note 37) -> Cancel chat
            config
                .note_actions
                .insert(37, midi::MidiAction::CancelChat { port: None });
            // Pad 3 (note 38) -> Reset session
            config
                .note_actions
                .insert(38, midi::MidiAction::ResetSession { port: None });

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(midi::run_midi_interactive(port, config, daemon_port))
        }
        Commands::Lpd8(lpd8_cmd) => match lpd8_cmd {
            Lpd8Commands::Write { port, program } => {
                if program < 1 || program > 4 {
                    eprintln!("✗ プログラム番号は1-4の範囲で指定してください");
                    std::process::exit(1);
                }
                println!("LPD8 Program {} にVP設定を書き込み中...", program);
                let vp_program = midi::lpd8::Program::vp_default();
                let sysex = vp_program.to_sysex(program - 1); // 0-indexed

                match midi::send_sysex(Some(&port), &sysex) {
                    Ok(()) => {
                        println!("✓ VP設定をLPD8 Program {} に書き込みました", program);
                        println!();
                        println!("PAD設定:");
                        println!("  PAD 1-4 (Note 36-39): プロジェクト切り替え (緑LED)");
                        println!("  PAD 5   (Note 40):    チャットキャンセル (赤LED)");
                        println!("  PAD 6   (Note 41):    セッションリセット (橙LED)");
                        println!("  PAD 7-8 (Note 42-43): 未割当");
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("✗ 書き込みエラー: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Lpd8Commands::Read { port, program } => {
                if program < 1 || program > 4 {
                    eprintln!("✗ プログラム番号は1-4の範囲で指定してください");
                    std::process::exit(1);
                }
                println!("LPD8 Program {} の読み取りは未実装です", program);
                println!("(SysExリクエスト送信後の応答受信が必要)");
                // TODO: Send request and wait for response via MidiInput
                let _ = port; // suppress warning
                Ok(())
            }
            Lpd8Commands::Switch { program, port } => {
                if program < 1 || program > 4 {
                    eprintln!("✗ プログラム番号は1-4の範囲で指定してください");
                    std::process::exit(1);
                }
                println!("LPD8をProgram {} に切り替え中...", program);
                let sysex = midi::lpd8::set_active_program(program - 1);

                match midi::send_sysex(Some(&port), &sysex) {
                    Ok(()) => {
                        println!("✓ LPD8をProgram {} に切り替えました", program);
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("✗ 切り替えエラー: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Lpd8Commands::Ports => {
                midi::print_output_ports();
                Ok(())
            }
        },
    }
}
