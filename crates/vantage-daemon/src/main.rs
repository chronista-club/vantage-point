//! Vantage Daemon - Background service for browser-based rich content display
//!
//! Usage:
//!   vantaged start    # Start the daemon (HTTP + WebSocket)
//!   vantaged mcp      # Start as MCP server (stdio)
//!   vantaged status   # Check if daemon is running
//!
//! Environment variables:
//!   VANTAGE_DEBUG=none|simple|detail  # Debug display mode

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

mod agent;
mod daemon;
mod mcp;
mod protocol;
mod webview;

use protocol::DebugMode;

/// Health response from daemon
#[derive(serde::Deserialize)]
struct HealthResponse {
    status: String,
    version: String,
    pid: u32,
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
        /// Port to listen on
        #[arg(short, long, default_value = "33000")]
        port: u16,

        /// Don't open any viewer (headless mode)
        #[arg(long)]
        headless: bool,

        /// Use system browser instead of native WebView
        #[arg(long)]
        browser: bool,

        /// Debug display mode (overrides VANTAGE_DEBUG env var)
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,
    },
    /// Start as MCP server (stdio JSON-RPC)
    Mcp,
    /// Check if daemon is running
    Status {
        /// Port to check
        #[arg(short, long, default_value = "33000")]
        port: u16,
    },
    /// Stop the running daemon
    Stop,
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

    // Default to Start if no command given
    let command = cli.command.unwrap_or(Commands::Start {
        port: 33000,
        headless: false,
        browser: false,
        debug: None,
    });

    match command {
        Commands::Start { port, headless, browser, debug } => {
            // Determine debug mode: CLI flag > env var > default
            let debug_mode = debug
                .map(DebugMode::from)
                .or_else(parse_debug_env)
                .unwrap_or_default();

            if debug_mode != DebugMode::None {
                tracing::info!("Debug mode: {:?}", debug_mode);
            }

            if headless || browser {
                // Headless or browser mode - use tokio runtime
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    // Start HTTP server in background
                    let server_handle = tokio::spawn(async move {
                        daemon::run(port, false, debug_mode).await
                    });

                    if browser {
                        // Wait for server to start, then open browser
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        let url = format!("http://localhost:{}", port);
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
                        daemon::run(port, false, debug_mode).await
                    })
                });

                // Wait a bit for server to start
                std::thread::sleep(std::time::Duration::from_millis(300));

                // Run WebView on main thread (required by macOS)
                let webview_result = webview::run_webview(port);

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
        Commands::Mcp => {
            // MCP mode: stdio JSON-RPC server
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(mcp::run_mcp_server(33000))
        }
        Commands::Status { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(check_status(port))
        }
        Commands::Stop => {
            // TODO: Send stop signal to daemon
            tracing::info!("Stop not yet implemented");
            Ok(())
        }
    }
}
