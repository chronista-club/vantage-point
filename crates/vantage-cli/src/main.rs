//! Vantage Point CLI
//!
//! 開発行為を拡張するプラットフォーム

use anyhow::Result;
use clap::{Parser, Subcommand};

mod server;

#[derive(Parser)]
#[command(name = "vantage")]
#[command(about = "開発行為を拡張する", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// サーバーを起動
    Serve {
        /// ポート番号
        #[arg(short, long, default_value = "3000")]
        port: u16,
    },
    /// バージョン情報
    Version,
}

#[tokio::main]
async fn main() -> Result<()> {
    // ロギング初期化
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vantage=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { port } => {
            server::run(port).await?;
        }
        Commands::Version => {
            println!("vantage {}", env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}
