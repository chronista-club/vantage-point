//! `vp daemon` コマンドの実行ロジック
//!
//! 後方互換のため残存。実体は `vp world` に委譲。
//! PIDファイルベースの生存確認とシグナルによるグレースフル停止。

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::process;

/// Daemon サブコマンド（`vp world` のエイリアス）
#[derive(Subcommand)]
pub enum DaemonCommands {
    /// TheWorld を起動（`vp world` と同等）
    Start {
        /// 待ち受けポート番号
        #[arg(short, long, default_value_t = crate::cli::WORLD_PORT)]
        port: u16,
    },
    /// TheWorld を停止
    Stop,
    /// TheWorld の状態確認
    Status,
}

/// `vp daemon` を実行（`vp world` に委譲）
pub fn execute(cmd: DaemonCommands) -> Result<()> {
    eprintln!("Note: `vp daemon` is deprecated, use `vp world`");
    match cmd {
        DaemonCommands::Start { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::process::run_world(port))
        }
        DaemonCommands::Stop => {
            match process::is_daemon_running() {
                Some(pid) => {
                    process::stop_daemon(pid)?;
                    println!("TheWorld stopped (PID: {})", pid);
                }
                None => {
                    println!("TheWorld is not running");
                }
            }
            Ok(())
        }
        DaemonCommands::Status => {
            match process::is_daemon_running() {
                Some(pid) => {
                    println!("TheWorld is running (PID: {})", pid);
                    println!("  PID file: {}", process::pid_file().display());
                }
                None => {
                    println!("TheWorld is not running");
                }
            }
            Ok(())
        }
    }
}
