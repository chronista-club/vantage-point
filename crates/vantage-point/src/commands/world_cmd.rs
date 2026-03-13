//! `vp world` コマンド — TheWorld（常駐プロセス管理）
//!
//! - `vp world start` — TheWorld をフォアグラウンドで起動
//! - `vp world stop` — TheWorld を停止
//! - `vp world status` — TheWorld の状態確認

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::process;

/// TheWorld サブコマンド
///
/// サブコマンド省略時は `start` として扱う（後方互換: `vp world --port 32000`）
#[derive(Subcommand)]
pub enum WorldCommands {
    /// TheWorld を起動（フォアグラウンド）
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

/// `vp world` を実行
pub fn execute(cmd: WorldCommands) -> Result<()> {
    match cmd {
        WorldCommands::Start { port } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::process::run_world(port))
        }
        WorldCommands::Stop => {
            match process::is_daemon_running() {
                Some(pid) => {
                    process::stop_daemon(pid)?;
                    println!("👑 TheWorld stopped (PID: {})", pid);
                }
                None => {
                    println!("TheWorld is not running");
                }
            }
            Ok(())
        }
        WorldCommands::Status => {
            match process::is_daemon_running() {
                Some(pid) => {
                    println!("👑 TheWorld is running (PID: {})", pid);
                    // ヘルスチェックで詳細情報を取得
                    if let Ok(resp) = reqwest::blocking::get(format!(
                        "http://[::1]:{}/api/health",
                        crate::cli::WORLD_PORT
                    )) {
                        if let Ok(json) = resp.json::<serde_json::Value>() {
                            println!(
                                "  Version: {}",
                                json.get("version").and_then(|v| v.as_str()).unwrap_or("?")
                            );
                            println!("  Port: {}", crate::cli::WORLD_PORT);
                        }
                    }
                    // Process 一覧
                    if let Ok(resp) = reqwest::blocking::get(format!(
                        "http://[::1]:{}/api/world/processes",
                        crate::cli::WORLD_PORT
                    )) {
                        if let Ok(json) = resp.json::<serde_json::Value>() {
                            if let Some(processes) = json.as_array() {
                                println!("  Processes: {}", processes.len());
                                for p in processes {
                                    let name = p
                                        .get("project_name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    let port = p.get("port").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let pid = p.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                                    println!("    - {} (port:{}, pid:{})", name, port, pid);
                                }
                            }
                        }
                    }
                }
                None => {
                    println!("TheWorld is not running");
                }
            }
            Ok(())
        }
    }
}
