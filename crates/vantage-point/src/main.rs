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
mod capability;
mod config;
mod mcp;
mod midi;
mod park;
mod protocol;
mod stand;
mod tray;
mod webview;
mod world;

use config::Config;
use protocol::DebugMode;

/// Health response from Stand
#[derive(serde::Deserialize)]
struct HealthResponse {
    status: String,
    version: String,
    pid: u32,
    #[serde(default)]
    project_dir: Option<String>,
}

/// Check if Stand is running on the specified port
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

/// Stop the Stand running on the specified port
async fn stop_stand(port: u16) -> Result<()> {
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
        println!("✗ Could not get Stand PID");
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
            && let Ok(health) = response.json::<HealthResponse>().await
        {
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
            i + 1,
            inst.port,
            inst.pid,
            project_display
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

/// Initialize tracing with VP_LOG support
/// VP_LOG環境変数またはDebugModeに基づいてログレベルを設定
/// - VP_LOG=debug|info|warn|error が優先
/// - 未設定の場合、debug_modeに基づいて設定:
///   - None -> warn
///   - Simple -> info
///   - Detail -> debug
fn init_tracing(debug_mode: DebugMode) {
    // VP_LOGが設定されていない場合、debug_modeに基づいてRUST_LOGを設定
    // SAFETY: main()開始直後、他スレッド起動前に呼ばれるため安全
    if std::env::var("VP_LOG").is_err() && std::env::var("RUST_LOG").is_err() {
        let log_level = match debug_mode {
            DebugMode::None => "warn",
            DebugMode::Simple => "info",
            DebugMode::Detail => "debug",
        };
        unsafe {
            std::env::set_var("RUST_LOG", format!("vantage_point={}", log_level));
        }
    } else if let Ok(vp_log) = std::env::var("VP_LOG") {
        // VP_LOG -> RUST_LOG に変換
        unsafe {
            std::env::set_var("RUST_LOG", format!("vantage_point={}", vp_log));
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();
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
    /// Standを起動（HTTPサーバー + WebSocketハブ）[デフォルト]
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
    /// Standの稼働状態を確認
    Status {
        /// 確認するポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// Standを停止
    Stop {
        /// 停止するStandのポート番号
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// Standを再起動（セッション状態を保持）
    Restart {
        /// 再起動するStandのポート番号
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
    /// WebViewウィンドウのみを開く（Standは別途起動済み）
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
        stand_port: u16,
    },
    /// LPD8コントローラー設定
    #[command(subcommand)]
    Lpd8(Lpd8Commands),
    /// Stand Conductorを起動（複数Standを管理）
    Conductor {
        /// 待ち受けポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
    },
    /// VantagePoint.app を起動（Conductor Standも自動起動）
    App {
        /// Conductorポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
        /// Conductor起動をスキップ（既に起動している場合）
        #[arg(long)]
        no_conductor: bool,
    },
    /// The World 管理（常駐コアプロセス）
    #[command(subcommand)]
    World(WorldCommands),
    /// Paisley Park 管理（プロジェクト単位 Agent）
    #[command(subcommand)]
    Park(ParkCommands),
    /// Gold Experience（創造・回復）
    #[command(subcommand)]
    Ge(GeCommands),
}

/// The World サブコマンド（JoJo Part 3 DIO のスタンド）
#[derive(Subcommand)]
enum WorldCommands {
    /// The World を起動「時よ止まれ」
    #[command(alias = "start")]
    Up {
        /// デバッグモード
        #[arg(long, short)]
        debug: bool,
        /// 静的ファイルディレクトリ（ViewPoint 用）
        #[arg(long, short = 's')]
        static_dir: Option<std::path::PathBuf>,
        /// MIDI 入力を有効化（ポートパターンで検索）
        #[arg(long, short = 'm')]
        midi: Option<String>,
        /// Requiem モード（GER: 自動防御強化）「真実にはたどり着けない」
        #[arg(long, short = 'r')]
        requiem: bool,
    },
    /// The World を停止「そして時は動き出す」
    #[command(alias = "stop")]
    Down,
    /// The World のステータス確認
    Status,
    /// ViewPoint を WebView で開く
    Open,
    /// MCP Server として起動（Claude CLI 連携）
    Mcp,
    /// スナップショット作成「時を止める」
    Snapshot {
        /// スナップショット名
        name: String,
        /// 説明（オプション）
        #[arg(long, short)]
        description: Option<String>,
    },
    /// スナップショットから復元「ゼロに戻す」
    Restore {
        /// スナップショット名
        name: String,
    },
    /// スナップショット一覧
    Snapshots,
    /// Guardian（自動防御）管理
    #[command(subcommand)]
    Guardian(GuardianCommands),
}

/// Paisley Park サブコマンド（JoJo Part 8 広瀬康穂のスタンド）
#[derive(Subcommand)]
enum ParkCommands {
    /// Paisley Park を起動「情報を収集します」
    #[command(alias = "start")]
    Up {
        /// プロジェクトディレクトリ
        #[arg(short = 'C', long)]
        project_dir: Option<String>,
        /// 使用するポート番号
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Paisley Park を停止
    #[command(alias = "stop")]
    Down {
        /// プロジェクトID
        project_id: Option<String>,
    },
    /// 登録済み Paisley Park 一覧
    List,
}

/// Guardian サブコマンド（GER の自動防御機能）
#[derive(Subcommand)]
enum GuardianCommands {
    /// Guardian のステータス確認
    Status,
    /// Guardian を有効化
    Enable,
    /// Guardian を無効化
    Disable,
    /// 保護ルール一覧
    Rules,
    /// 保護ルールを追加
    AddRule {
        /// ルール名
        name: String,
        /// ルールパターン（glob形式）
        pattern: String,
    },
}

/// Gold Experience サブコマンド（JoJo Part 5 ジョルノのスタンド）
#[derive(Subcommand)]
enum GeCommands {
    /// プロジェクト/コード生成「生命を与える」
    Scaffold {
        /// テンプレート名
        template: String,
        /// 出力先ディレクトリ
        #[arg(short, long, default_value = ".")]
        output: std::path::PathBuf,
        /// プロジェクト/モジュール名
        #[arg(short, long)]
        name: Option<String>,
        /// 説明
        #[arg(short, long)]
        description: Option<String>,
    },
    /// コード修復「回復」
    Heal {
        /// 修復対象ディレクトリ
        #[arg(short = 'C', long, default_value = ".")]
        dir: std::path::PathBuf,
        /// 修復アクション (format, lint-fix, all)
        #[arg(short, long, default_value = "all")]
        action: String,
    },
    /// テンプレート一覧
    Templates,
    /// プロジェクト検出
    Detect {
        /// 検出対象ディレクトリ
        #[arg(default_value = ".")]
        dir: std::path::PathBuf,
    },
    /// 成長統計
    Stats,
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
    // CLIパース（tracingより先に）
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

    // Initialize tracing
    // Startコマンドの場合はdebugフラグを取得、その他はデフォルト
    let debug_mode_for_tracing = match &command {
        Commands::Start { debug, .. } => debug
            .as_ref()
            .map(|d| DebugMode::from(d.clone()))
            .or_else(parse_debug_env)
            .unwrap_or_default(),
        Commands::Restart { .. } => parse_debug_env().unwrap_or_default(),
        _ => parse_debug_env().unwrap_or_default(),
    };
    init_tracing(debug_mode_for_tracing);

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
                    config.port_index = Some(idx);
                } else {
                    config.port_pattern = Some(midi_arg.clone());
                }
                config
            });

            // Create CapabilityConfig
            let cap_config = stand::CapabilityConfig {
                project_dir: resolved_project_dir.clone(),
                midi_config,
                bonjour_port: Some(resolved_port), // Bonjour広告を有効化
            };

            if headless || browser {
                // Headless or browser mode - use tokio runtime
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    let server_handle = tokio::spawn(async move {
                        stand::run(resolved_port, false, debug_mode, cap_config).await
                    });

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
                let server_thread = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
                    rt.block_on(async {
                        stand::run(resolved_port, false, debug_mode, cap_config).await
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
                        i + 1,
                        project.name,
                        port_str,
                        path_display
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
            rt.block_on(stop_stand(port))
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
                    Some(inst) => inst.project_dir.clone().unwrap_or_else(|| {
                        std::env::current_dir()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    }),
                    None => {
                        println!(
                            "✗ No Stand running on port {}. Use `vp start` instead.",
                            port
                        );
                        return Ok(());
                    }
                };

                println!("🔄 Restarting vp on port {}...", port);
                println!("   Project: {}", project_dir);

                // Stop the Stand
                stop_stand(port).await?;

                // Wait a moment for port to be released
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                println!("🚀 Starting Stand...");
                Ok::<(), anyhow::Error>(())
            })?;

            // Get project_dir for starting (need to get it again outside async block)
            let rt2 = tokio::runtime::Runtime::new()?;
            let project_dir = rt2.block_on(async {
                // Read from persisted state file
                let state_path = config::config_dir()
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

            // Create CapabilityConfig (no MIDI on restart)
            let cap_config = stand::CapabilityConfig {
                project_dir,
                midi_config: None,
                bonjour_port: Some(port), // Bonjour広告を有効化
            };

            if headless || browser {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    let server_handle = tokio::spawn(async move {
                        stand::run(port, false, debug_mode, cap_config).await
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
                let server_thread = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
                    rt.block_on(async { stand::run(port, false, debug_mode, cap_config).await })
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
            // Just run WebView window pointing to existing Stand
            webview::run_webview(port)
        }
        Commands::MidiPorts => {
            midi::print_ports();
            Ok(())
        }
        Commands::Midi { port, stand_port } => {
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
            rt.block_on(midi::run_midi_interactive(port, config, stand_port))
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
        Commands::Conductor { port } => {
            // Stand Conductorモードで起動
            println!("🎭 Starting Stand Conductor on port {}...", port);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(stand::run_conductor(port))
        }
        Commands::App { port, no_conductor } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                // Conductor Standが稼働しているか確認
                if !no_conductor {
                    let conductor_url = format!("http://localhost:{}/api/health", port);
                    let client = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(2))
                        .build()?;

                    let conductor_running = client
                        .get(&conductor_url)
                        .send()
                        .await
                        .map(|r| r.status().is_success())
                        .unwrap_or(false);

                    if !conductor_running {
                        println!("🎭 Starting Conductor Stand on port {}...", port);
                        // バックグラウンドでConductorを起動
                        let vp_path =
                            which_vp().ok_or_else(|| anyhow::anyhow!("vp binary not found"))?;

                        std::process::Command::new(&vp_path)
                            .args(["conductor", "-p", &port.to_string()])
                            .spawn()
                            .map_err(|e| anyhow::anyhow!("Failed to start conductor: {}", e))?;

                        // 起動を待つ
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    }
                }

                // VantagePoint.app を起動
                let app_path = find_vantage_point_app();
                match app_path {
                    Some(path) => {
                        println!("🚀 Opening VantagePoint.app...");
                        std::process::Command::new("open")
                            .arg(&path)
                            .spawn()
                            .map_err(|e| anyhow::anyhow!("Failed to open app: {}", e))?;
                        println!("✓ VantagePoint.app started");
                        Ok(())
                    }
                    None => {
                        eprintln!("✗ VantagePoint.app not found");
                        eprintln!("  Expected locations:");
                        eprintln!("    - /Applications/VantagePoint.app");
                        eprintln!("    - ~/Applications/VantagePoint.app");
                        eprintln!("    - ~/repos/vantage-point-mac/VantagePoint/VantagePoint.app (dev)");
                        std::process::exit(1);
                    }
                }
            })
        }
        Commands::World(cmd) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                match cmd {
                    WorldCommands::Up {
                        debug,
                        static_dir,
                        midi,
                        requiem,
                    } => {
                        if requiem {
                            println!(
                                "🌍 The World 起動中... 「真実にはたどり着けない」(Requiem Mode)"
                            );
                        } else {
                            println!("🌍 The World 起動中... 「時よ止まれ」");
                        }
                        if let Some(ref pattern) = midi {
                            println!(
                                "🎹 MIDI 有効: {}",
                                if pattern.is_empty() {
                                    "(全ポート)"
                                } else {
                                    pattern
                                }
                            );
                        }
                        let config = world::WorldConfig {
                            debug,
                            static_dir,
                            midi_port_pattern: midi,
                            requiem_mode: requiem,
                            ..Default::default()
                        };
                        world::run(config).await
                    }
                    WorldCommands::Down => {
                        println!("🌍 The World 停止中... 「そして時は動き出す」");
                        // TODO: HTTP POST /api/shutdown を送信
                        let client = reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(2))
                            .build()?;
                        let url = format!("http://localhost:{}/api/shutdown", world::WORLD_PORT);
                        match client.post(&url).send().await {
                            Ok(_) => {
                                println!("✓ The World 停止完了");
                                Ok(())
                            }
                            Err(e) => {
                                if e.is_connect() {
                                    println!("✗ The World は稼働していません");
                                } else {
                                    println!("✗ 停止エラー: {}", e);
                                }
                                Ok(())
                            }
                        }
                    }
                    WorldCommands::Status => {
                        let client = reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(2))
                            .build()?;
                        let url = format!("http://localhost:{}/health", world::WORLD_PORT);
                        match client.get(&url).send().await {
                            Ok(response) if response.status().is_success() => {
                                let body = response.text().await.unwrap_or_default();
                                println!("✓ The World 稼働中 (port {})", world::WORLD_PORT);
                                println!("{}", body);
                            }
                            Ok(_) => {
                                println!("✗ The World がエラー応答");
                            }
                            Err(e) => {
                                if e.is_connect() {
                                    println!("✗ The World は稼働していません");
                                } else {
                                    println!("✗ 接続エラー: {}", e);
                                }
                            }
                        }
                        Ok(())
                    }
                    WorldCommands::Open => {
                        println!("🌍 ViewPoint を開いています...");
                        // The World が稼働しているか確認
                        let client = reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(2))
                            .build()?;
                        let url = format!("http://localhost:{}/health", world::WORLD_PORT);
                        match client.get(&url).send().await {
                            Ok(response) if response.status().is_success() => {
                                // WebView を起動
                                drop(client); // クライアントを解放
                                webview::run_webview(world::WORLD_PORT)?;
                            }
                            Ok(_) => {
                                println!("✗ The World がエラー応答");
                            }
                            Err(e) => {
                                if e.is_connect() {
                                    println!("✗ The World が稼働していません");
                                    println!("  先に `vp world up -s ./web` で起動してください");
                                } else {
                                    println!("✗ 接続エラー: {}", e);
                                }
                            }
                        }
                        Ok(())
                    }
                    WorldCommands::Mcp => {
                        // MCP Server として起動（stdio JSON-RPC）
                        tracing::info!("The World MCP Server 起動");
                        world::mcp::run_mcp_server().await
                    }
                    WorldCommands::Snapshot { name, description } => {
                        println!("📸 スナップショット作成中... 「時を止める」");

                        // 現在のディレクトリをスナップショット対象とする
                        let target_dir = std::env::current_dir().unwrap_or_default();

                        // ローカルで GER を使用してスナップショット作成
                        let ger = world::GoldExperienceRequiem::default();
                        match ger
                            .create_snapshot(&name, description.as_deref(), &target_dir)
                            .await
                        {
                            Ok(snapshot) => {
                                println!("✓ スナップショット '{}' を作成しました", snapshot.name);
                                println!("  場所: {}", snapshot.path.display());
                                println!(
                                    "  作成: {}",
                                    snapshot.created_at.format("%Y-%m-%d %H:%M:%S")
                                );
                                Ok(())
                            }
                            Err(e) => {
                                println!("✗ スナップショット作成エラー: {}", e);
                                Ok(())
                            }
                        }
                    }
                    WorldCommands::Restore { name } => {
                        println!("⏪ スナップショット復元中... 「ゼロに戻す」");

                        let ger = world::GoldExperienceRequiem::default();
                        // まず既存のスナップショットを読み込む
                        if let Err(e) = ger.load_snapshots().await {
                            println!("✗ スナップショット読み込みエラー: {}", e);
                            return Ok(());
                        }

                        match ger.restore_snapshot(&name).await {
                            Ok(()) => {
                                println!("✓ スナップショット '{}' から復元しました", name);
                                Ok(())
                            }
                            Err(e) => {
                                println!("✗ 復元エラー: {}", e);
                                Ok(())
                            }
                        }
                    }
                    WorldCommands::Snapshots => {
                        println!("📸 スナップショット一覧:");

                        let ger = world::GoldExperienceRequiem::default();
                        if let Err(e) = ger.load_snapshots().await {
                            println!("✗ スナップショット読み込みエラー: {}", e);
                            return Ok(());
                        }

                        let snapshots = ger.list_snapshots().await;
                        if snapshots.is_empty() {
                            println!("  (スナップショットなし)");
                        } else {
                            println!("  NAME                CREATED              DESCRIPTION");
                            println!("  ────                ───────              ───────────");
                            for snap in snapshots {
                                let desc = snap.description.as_deref().unwrap_or("-");
                                let desc_display = if desc.len() > 30 {
                                    format!("{}...", &desc[..27])
                                } else {
                                    desc.to_string()
                                };
                                println!(
                                    "  {:18}  {}  {}",
                                    snap.name,
                                    snap.created_at.format("%Y-%m-%d %H:%M"),
                                    desc_display
                                );
                            }
                        }
                        Ok(())
                    }
                    WorldCommands::Guardian(guardian_cmd) => {
                        let ger = world::GoldExperienceRequiem::default();

                        match guardian_cmd {
                            GuardianCommands::Status => {
                                let status = ger.guardian_status().await;
                                println!("🛡️ Guardian ステータス:");
                                println!(
                                    "  状態: {}",
                                    if status.enabled {
                                        "有効 ✓"
                                    } else {
                                        "無効"
                                    }
                                );
                                println!("  ルール数: {}", status.rule_count);
                                println!("  ブロック回数: {}", status.block_count);
                                if let Some(last) = status.last_check {
                                    println!(
                                        "  最終チェック: {}",
                                        last.format("%Y-%m-%d %H:%M:%S")
                                    );
                                }
                                Ok(())
                            }
                            GuardianCommands::Enable => {
                                ger.enable_guardian().await;
                                println!("✓ Guardian を有効化しました「自動防御発動」");
                                Ok(())
                            }
                            GuardianCommands::Disable => {
                                ger.disable_guardian().await;
                                println!("✓ Guardian を無効化しました");
                                Ok(())
                            }
                            GuardianCommands::Rules => {
                                let rules = ger.list_rules().await;
                                println!("🛡️ Guardian ルール一覧:");
                                if rules.is_empty() {
                                    println!("  (ルールなし)");
                                } else {
                                    println!("  NAME                PATTERN              ACTION");
                                    println!("  ────                ───────              ──────");
                                    for rule in rules {
                                        let action = format!("{:?}", rule.action).to_lowercase();
                                        let status = if rule.enabled { "" } else { " (無効)" };
                                        println!(
                                            "  {:18}  {:20}  {}{}",
                                            rule.name, rule.pattern, action, status
                                        );
                                    }
                                }
                                Ok(())
                            }
                            GuardianCommands::AddRule { name, pattern } => {
                                match ger
                                    .add_rule(&name, &pattern, world::GuardianAction::Block)
                                    .await
                                {
                                    Ok(()) => {
                                        println!(
                                            "✓ ルール '{}' を追加しました (pattern: {})",
                                            name, pattern
                                        );
                                        Ok(())
                                    }
                                    Err(e) => {
                                        println!("✗ ルール追加エラー: {}", e);
                                        Ok(())
                                    }
                                }
                            }
                        }
                    }
                }
            })
        }
        Commands::Park(cmd) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                match cmd {
                    ParkCommands::Up { project_dir, port } => {
                        let project_path = project_dir
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                        let project_id = project_path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "unknown".to_string());
                        let port = port.unwrap_or(33100); // デフォルトポート

                        println!("🌸 Paisley Park 起動中... 「情報を収集します」");
                        println!("   Project: {} ({})", project_id, project_path.display());

                        let config = park::ParkConfig {
                            project_id,
                            project_path,
                            world_url: format!("http://localhost:{}", world::WORLD_PORT),
                            ..Default::default()
                        };

                        let park = park::PaisleyPark::new(config);
                        park.start(port).await?;

                        println!("✓ The World に登録完了");

                        // 終了シグナルを待つ
                        tokio::signal::ctrl_c().await?;
                        println!("\n🌸 シャットダウン中...");
                        park.stop().await?;
                        Ok(())
                    }
                    ParkCommands::Down { project_id: _ } => {
                        println!("🌸 Paisley Park 停止");
                        // TODO: The World 経由で特定の Park を停止
                        Ok(())
                    }
                    ParkCommands::List => {
                        let client = reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(2))
                            .build()?;
                        let url = format!("http://localhost:{}/api/parks", world::WORLD_PORT);
                        match client.get(&url).send().await {
                            Ok(response) if response.status().is_success() => {
                                let body = response.text().await.unwrap_or_default();
                                println!("登録済み Paisley Park:");
                                println!("{}", body);
                            }
                            Ok(_) => {
                                println!("✗ The World からエラー応答");
                            }
                            Err(e) => {
                                if e.is_connect() {
                                    println!("✗ The World が稼働していません");
                                    println!("  先に `vp world up` で起動してください");
                                } else {
                                    println!("✗ 接続エラー: {}", e);
                                }
                            }
                        }
                        Ok(())
                    }
                }
            })
        }
        Commands::Ge(cmd) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let ge = world::GoldExperience::default();

                match cmd {
                    GeCommands::Scaffold {
                        template,
                        output,
                        name,
                        description,
                    } => {
                        println!("🌟 Gold Experience 発動... 「生命を与える」");

                        let project_name = name.unwrap_or_else(|| {
                            output
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_else(|| "new-project".to_string())
                        });

                        let mut variables = std::collections::HashMap::new();
                        variables.insert("name".to_string(), project_name.clone());
                        variables.insert("Name".to_string(), to_pascal_case(&project_name));
                        variables.insert(
                            "description".to_string(),
                            description.unwrap_or_else(|| format!("{} project", project_name)),
                        );

                        match ge.scaffold(&template, &output, variables).await {
                            Ok(result) => {
                                println!("✓ スキャフォールド完了");
                                println!("  テンプレート: {}", template);
                                println!("  出力先: {}", result.output_dir.display());
                                println!("  生成ファイル:");
                                for file in result.files {
                                    println!("    - {}", file.display());
                                }
                                Ok(())
                            }
                            Err(e) => {
                                println!("✗ スキャフォールドエラー: {}", e);
                                Ok(())
                            }
                        }
                    }
                    GeCommands::Heal { dir, action } => {
                        println!("💚 Gold Experience 発動... 「回復」");

                        let heal_action = match action.as_str() {
                            "format" => world::HealAction::Format,
                            "lint-fix" | "lint" => world::HealAction::LintFix,
                            "deps" | "fix-deps" => world::HealAction::FixDependencies,
                            "imports" | "organize-imports" => world::HealAction::OrganizeImports,
                            _ => world::HealAction::All,
                        };

                        match ge.heal(&dir, heal_action).await {
                            Ok(result) => {
                                let status = if result.success { "✓" } else { "⚠" };
                                println!("{} 修復完了: {}", status, result.action);
                                println!("  {}", result.summary);
                                Ok(())
                            }
                            Err(e) => {
                                println!("✗ 修復エラー: {}", e);
                                Ok(())
                            }
                        }
                    }
                    GeCommands::Templates => {
                        println!("🌟 Gold Experience テンプレート一覧:");
                        let templates = ge.list_templates().await;
                        if templates.is_empty() {
                            println!("  (テンプレートなし)");
                        } else {
                            for template in templates {
                                println!("  - {}", template);
                            }
                        }
                        Ok(())
                    }
                    GeCommands::Detect { dir } => {
                        let project = world::GoldExperience::detect_project(&dir);
                        println!("🔍 プロジェクト検出結果:");
                        println!("  種類: {}", project.kind);
                        println!("  ルート: {}", project.root.display());
                        if let Some(pm) = project.package_manager {
                            println!("  パッケージマネージャ: {}", pm);
                        }
                        Ok(())
                    }
                    GeCommands::Stats => {
                        let stats = ge.growth_stats().await;
                        println!("🌱 Gold Experience 成長統計:");
                        println!("  スキャフォールド実行: {} 回", stats.total_scaffolds);
                        println!("  修復実行: {} 回", stats.total_heals);
                        println!("  修復成功率: {:.1}%", stats.heal_success_rate);
                        println!("  学習パターン: {} 件", stats.patterns_learned);
                        Ok(())
                    }
                }
            })
        }
    }
}

/// snake_case を PascalCase に変換
fn to_pascal_case(s: &str) -> String {
    s.split(|c| c == '_' || c == '-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// vpバイナリのパスを取得
fn which_vp() -> Option<std::path::PathBuf> {
    // 1. ~/.cargo/bin/vp
    if let Some(home) = dirs::home_dir() {
        let cargo_path = home.join(".cargo/bin/vp");
        if cargo_path.exists() {
            return Some(cargo_path);
        }
    }

    // 2. /usr/local/bin/vp
    let usr_local = std::path::PathBuf::from("/usr/local/bin/vp");
    if usr_local.exists() {
        return Some(usr_local);
    }

    // 3. PATH経由
    if let Ok(output) = std::process::Command::new("which").arg("vp").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }

    None
}

/// VantagePoint.app のパスを検索
fn find_vantage_point_app() -> Option<std::path::PathBuf> {
    // 1. /Applications
    let system_app = std::path::PathBuf::from("/Applications/VantagePoint.app");
    if system_app.exists() {
        return Some(system_app);
    }

    // 2. ~/Applications
    if let Some(home) = dirs::home_dir() {
        let user_app = home.join("Applications/VantagePoint.app");
        if user_app.exists() {
            return Some(user_app);
        }
    }

    // 3. 開発リポジトリ（~/repos/vantage-point-mac/）
    if let Some(home) = dirs::home_dir() {
        let dev_repo_app = home.join("repos/vantage-point-mac/VantagePoint/VantagePoint.app");
        if dev_repo_app.exists() {
            return Some(dev_repo_app);
        }
    }

    // 4. Xcode DerivedData（開発用）
    if let Some(home) = dirs::home_dir() {
        let dev_app = home
            .join("Library/Developer/Xcode/DerivedData")
            .read_dir()
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().starts_with("VantagePoint-"))
            .map(|e| e.path().join("Build/Products/Debug/VantagePoint.app"));

        if let Some(path) = dev_app {
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}
