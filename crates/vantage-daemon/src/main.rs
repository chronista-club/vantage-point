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

        /// Don't auto-open browser
        #[arg(long)]
        no_browser: bool,
    },
    /// Start as MCP server (stdio JSON-RPC)
    Mcp,
    /// Check if daemon is running
    Status,
    /// Stop the running daemon
    Stop,
}

#[tokio::main]
async fn main() -> Result<()> {
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
        no_browser: false,
    });

    match command {
        Commands::Start { port, no_browser } => {
            daemon::run(port, !no_browser).await
        }
        Commands::Mcp => {
            // MCP mode: stdio JSON-RPC server
            // Note: tracing goes to stderr, which MCP clients ignore
            mcp::run_mcp_server(33000).await
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
