//! Vantage Daemon - Background service for browser-based rich content display
//!
//! Usage:
//!   vantaged start    # Start the daemon (HTTP + WebSocket)
//!   vantaged mcp      # Start as MCP server (stdio)
//!   vantaged status   # Check if daemon is running
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # Debug display mode
//!   VANTAGE_PROJECT_DIR=/path/to/project  # Default project directory
//!
//! Config file: ~/.config/vantage/config.toml

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

mod agent;
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
                        println!("✓ vantaged is running on port {}", port);
                        println!("  Version: {}", health.version);
                        println!("  PID: {}", health.pid);
                        if let Some(ref dir) = health.project_dir {
                            println!("  Project: {}", dir);
                        }
                        println!("  Status: {}", health.status);
                    }
                    Err(_) => {
                        // Old version returning plain text
                        println!("✓ vantaged is running on port {}", port);
                    }
                }
            } else {
                println!("✗ vantaged returned error: {}", response.status());
            }
        }
        Err(e) => {
            if e.is_connect() {
                println!("✗ vantaged is not running on port {}", port);
            } else if e.is_timeout() {
                println!("✗ vantaged is not responding (timeout)");
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
                println!("✗ vantaged is not running on port {}", port);
                return Ok(());
            }
            None
        }
    };

    let Some(pid) = pid else {
        println!("✗ Could not get daemon PID");
        return Ok(());
    };

    println!("Stopping vantaged (PID: {})...", pid);

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
            println!("✓ vantaged stopped gracefully");
            return Ok(());
        }

        if start.elapsed() > timeout {
            println!("⚠ Graceful shutdown timed out, forcing kill...");
            force_kill(pid);
            println!("✓ vantaged force killed");
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
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
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

/// Scan for running vantaged instances
async fn scan_instances() -> Vec<Instance> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap();

    let mut instances = Vec::new();

    for port in PORT_RANGE_START..=PORT_RANGE_END {
        let url = format!("http://localhost:{}/api/health", port);
        if let Ok(response) = client.get(&url).send().await {
            if response.status().is_success() {
                if let Ok(health) = response.json::<HealthResponse>().await {
                    instances.push(Instance {
                        port,
                        pid: health.pid,
                        version: health.version,
                        project_dir: health.project_dir,
                    });
                }
            }
        }
    }

    instances
}

/// List all running vantaged instances
async fn list_instances() -> Result<()> {
    println!("Scanning ports {}–{}...", PORT_RANGE_START, PORT_RANGE_END);

    let instances = scan_instances().await;

    if instances.is_empty() {
        println!("No running vantaged instances found.");
        return Ok(());
    }

    println!();
    println!("  #  PORT   PID     PROJECT");
    println!("  ─  ────   ───     ───────");
    for (i, inst) in instances.iter().enumerate() {
        let project = inst.project_dir.as_deref().unwrap_or("-");
        // Shorten long paths
        let project_display = if project.len() > 40 {
            format!("...{}", &project[project.len()-37..])
        } else {
            project.to_string()
        };
        println!("  {}  {}  {:>5}   {}", i, inst.port, inst.pid, project_display);
    }
    println!();
    println!("Use `vantaged open <#>` to open WebUI");

    Ok(())
}

/// Open WebUI for a specific instance
async fn open_instance(index: usize) -> Result<()> {
    let instances = scan_instances().await;

    if instances.is_empty() {
        println!("No running vantaged instances found.");
        return Ok(());
    }

    if index >= instances.len() {
        println!("✗ Invalid index {}. Available: 0–{}", index, instances.len() - 1);
        return Ok(());
    }

    let inst = &instances[index];
    let url = format!("http://localhost:{}", inst.port);
    println!("Opening {} (PID: {})...", url, inst.pid);

    if let Err(e) = open::that(&url) {
        println!("✗ Failed to open browser: {}", e);
    } else {
        println!("✓ Opened in browser");
    }

    Ok(())
}

/// CLI-compatible debug mode enum
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum DebugModeArg {
    /// No debug information
    #[default]
    None,
    /// Simple debug info (session ID, timing)
    Simple,
    /// Detailed debug info (full JSON, all events)
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
    std::env::var("VANTAGE_DEBUG").ok().and_then(|v| {
        match v.to_lowercase().as_str() {
            "none" | "off" | "0" | "false" => Some(DebugMode::None),
            "simple" | "1" | "true" => Some(DebugMode::Simple),
            "detail" | "detailed" | "2" | "verbose" => Some(DebugMode::Detail),
            _ => None,
        }
    })
}

#[derive(Parser)]
#[command(name = "vantaged")]
#[command(about = "Background daemon for browser-based rich content display")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon (HTTP server + WebSocket hub) [default]
    Start {
        /// Project index from config (use `vantaged config` to list)
        #[arg()]
        project_index: Option<usize>,

        /// Port to listen on
        #[arg(short, long)]
        port: Option<u16>,

        /// Don't open any viewer (headless mode)
        #[arg(long)]
        headless: bool,

        /// Use system browser instead of native WebView
        #[arg(long)]
        browser: bool,

        /// Debug display mode (overrides VANTAGE_DEBUG env var)
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,

        /// Project directory for Claude agent (overrides project_index)
        #[arg(long, short = 'C')]
        project_dir: Option<String>,
    },
    /// Show configuration and registered projects
    Config,
    /// Start as MCP server (stdio JSON-RPC)
    Mcp,
    /// Check if daemon is running
    Status {
        /// Port to check
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// Stop the running daemon
    Stop {
        /// Port of the daemon to stop
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// List running vantaged instances
    #[command(alias = "list")]
    Ps,
    /// Open WebUI for a running instance
    Open {
        /// Index of the instance (from `vantaged ps`)
        #[arg(default_value = "0")]
        index: usize,
    },
    /// Run as menu bar icon (system tray)
    Tray,
    /// List available MIDI input ports
    MidiPorts,
    /// Start MIDI input monitoring
    Midi {
        /// MIDI port index to connect to
        #[arg(short, long)]
        port: Option<usize>,
        /// Daemon port to send actions to
        #[arg(short = 'P', long, default_value = "33000")]
        daemon_port: u16,
    },
}

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("vantage_daemon=info".parse()?)
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
    });

    match command {
        Commands::Start { project_index, port, headless, browser, debug, project_dir } => {
            // Resolve project directory
            let resolved_project_dir = if let Some(ref dir) = project_dir {
                // Explicit --project-dir takes precedence
                dir.clone()
            } else if let Some(idx) = project_index {
                // Project index from config
                if idx >= config.projects.len() {
                    eprintln!("✗ Invalid project index {}. Use `vantaged config` to list projects.", idx);
                    std::process::exit(1);
                }
                let project = &config.projects[idx];
                println!("📁 Project: {} ({})", project.name, project.path);
                project.path.clone()
            } else {
                // Default: cwd
                Config::resolve_project_dir(None, &config)
            };

            // Resolve port: CLI > project config > default config > 33000
            let resolved_port = port
                .or_else(|| {
                    project_index.and_then(|idx| config.projects.get(idx).and_then(|p| p.port))
                })
                .unwrap_or(config.default_port);

            // Determine debug mode: CLI flag > env var > default
            let debug_mode = debug
                .map(DebugMode::from)
                .or_else(parse_debug_env)
                .unwrap_or_default();

            if debug_mode != DebugMode::None {
                tracing::info!("Debug mode: {:?}", debug_mode);
            }

            tracing::info!("Project dir: {}", resolved_project_dir);

            if headless || browser {
                // Headless or browser mode - use tokio runtime
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    // Start HTTP server in background
                    let project_dir = resolved_project_dir.clone();
                    let server_handle = tokio::spawn(async move {
                        daemon::run(resolved_port, false, debug_mode, project_dir).await
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
                let project_dir = resolved_project_dir.clone();
                let server_thread = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
                    rt.block_on(async {
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
                    let port_str = project.port.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string());
                    // Shorten long paths
                    let path_display = if project.path.len() > 40 {
                        format!("...{}", &project.path[project.path.len()-37..])
                    } else {
                        project.path.clone()
                    };
                    println!("  {}  {:18}  {:>5}   {}", i, project.name, port_str, path_display);
                }
                println!();
                println!("Usage: vantaged start <#> or vantaged start -C /path/to/project");
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
        Commands::Ps => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(list_instances())
        }
        Commands::Open { index } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(open_instance(index))
        }
        Commands::Tray => {
            tray::run_tray()
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
            config.note_actions.insert(36, midi::MidiAction::OpenWebUI { port: None });
            // Pad 2 (note 37) -> Cancel chat
            config.note_actions.insert(37, midi::MidiAction::CancelChat);
            // Pad 3 (note 38) -> Reset session
            config.note_actions.insert(38, midi::MidiAction::ResetSession);

            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(midi::run_midi_interactive(port, config, daemon_port))
        }
    }
}
