//! `vp daemon` コマンドの実行ロジック
//!
//! Stand Conductor をデーモンプロセスとして管理する。
//! 複数の Stand のライフサイクルとヘルスチェックを担当。

use anyhow::Result;
use clap::Subcommand;

/// Daemon サブコマンド
#[derive(Subcommand)]
pub enum DaemonCommands {
    /// デーモンを起動（Stand管理 + ヘルスチェック）
    Start {
        /// 待ち受けポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
    },
    /// デーモンを停止
    Stop {
        /// 停止するデーモンのポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
    },
    /// デーモンの状態確認
    Status {
        /// 確認するデーモンのポート番号
        #[arg(short, long, default_value = "32900")]
        port: u16,
    },
}

/// `vp daemon` を実行
pub fn execute(cmd: DaemonCommands) -> Result<()> {
    match cmd {
        DaemonCommands::Start { port } => {
            println!("Starting Stand Daemon on port {}...", port);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::stand::run_conductor(port))
        }
        DaemonCommands::Stop { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let url = format!("http://localhost:{}/api/shutdown", port);
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()?;

                match client.post(&url).send().await {
                    Ok(response) if response.status().is_success() => {
                        println!("Daemon stopped (port {})", port);
                    }
                    Ok(_) => {
                        eprintln!("Daemon returned error response");
                    }
                    Err(e) => {
                        if e.is_connect() {
                            println!("Daemon is not running (port {})", port);
                        } else {
                            eprintln!("Connection error: {}", e);
                        }
                    }
                }
                Ok(())
            })
        }
        DaemonCommands::Status { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let url = format!("http://localhost:{}/api/health", port);
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()?;

                match client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => {
                        let body = response.text().await.unwrap_or_default();
                        println!("Daemon running (port {})", port);
                        if !body.is_empty() {
                            println!("{}", body);
                        }
                    }
                    Ok(_) => {
                        eprintln!("Daemon returned error response");
                    }
                    Err(e) => {
                        if e.is_connect() {
                            println!("Daemon is not running (port {})", port);
                        } else {
                            eprintln!("Connection error: {}", e);
                        }
                    }
                }
                Ok(())
            })
        }
    }
}
