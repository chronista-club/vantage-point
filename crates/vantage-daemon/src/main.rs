//! Vantage Daemon - Background service for browser-based rich content display
//!
//! Usage:
//!   vantaged start    # Start the daemon (HTTP + WebSocket)
//!   vantaged mcp      # Start as MCP server (stdio)
//!   vantaged status   # Check if daemon is running

use anyhow::Result;
use clap::{Parser, Subcommand};

mod daemon;
mod mcp;
mod protocol;
mod webview;

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
    },
    /// Start as MCP server (stdio JSON-RPC)
    Mcp,
    /// Check if daemon is running
    Status,
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
    });

    match command {
        Commands::Start { port, headless, browser } => {
            if headless || browser {
                // Headless or browser mode - use tokio runtime
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    // Start HTTP server in background
                    let server_handle = tokio::spawn(async move {
                        daemon::run(port, false).await
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
                        daemon::run(port, false).await
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
        Commands::Status => {
            // TODO: Check daemon status via health endpoint
            tracing::info!("Status check not yet implemented");
            Ok(())
        }
        Commands::Stop => {
            // TODO: Send stop signal to daemon
            tracing::info!("Stop not yet implemented");
            Ok(())
        }
    }
}
