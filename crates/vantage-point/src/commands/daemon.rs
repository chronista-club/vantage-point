//! `vp daemon` コマンドの実行ロジック
//!
//! VP Daemon のプロセス管理を行う。
//! PIDファイルベースの生存確認とシグナルによるグレースフル停止。

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::process;

/// Daemon サブコマンド
#[derive(Subcommand)]
pub enum DaemonCommands {
    /// デーモンを起動（PTYプロセス管理 + Unison Server）
    Start {
        /// 待ち受けポート番号
        #[arg(short, long, default_value = "34000")]
        port: u16,
    },
    /// デーモンを停止
    Stop,
    /// デーモンの状態確認
    Status,
}

/// `vp daemon` を実行
pub fn execute(cmd: DaemonCommands) -> Result<()> {
    match cmd {
        DaemonCommands::Start { port } => {
            println!("Starting VP Daemon on port {}...", port);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(process::run_daemon(port))
        }
        DaemonCommands::Stop => {
            match process::is_daemon_running() {
                Some(pid) => {
                    process::stop_daemon(pid)?;
                    println!("Daemon stopped (PID: {})", pid);
                }
                None => {
                    println!("Daemon is not running");
                }
            }
            Ok(())
        }
        DaemonCommands::Status => {
            match process::is_daemon_running() {
                Some(pid) => {
                    println!("Daemon is running (PID: {})", pid);
                    println!("  PID file: {}", process::pid_file().display());
                }
                None => {
                    println!("Daemon is not running");
                }
            }
            Ok(())
        }
    }
}
