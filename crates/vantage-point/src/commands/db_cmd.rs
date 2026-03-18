//! `vp db` コマンド — SurrealDB デーモン管理
//!
//! - `vp db start` — SurrealDB を起動（未起動なら）
//! - `vp db stop` — SurrealDB を停止
//! - `vp db restart` — SurrealDB を再起動
//! - `vp db status` — SurrealDB の状態確認

use anyhow::Result;
use clap::Subcommand;

use crate::db;

/// SurrealDB サブコマンド
#[derive(Subcommand)]
pub enum DbCommands {
    /// SurrealDB を起動（未起動なら）
    Start,
    /// SurrealDB を停止
    Stop,
    /// SurrealDB を再起動
    Restart,
    /// SurrealDB の状態確認
    Status,
}

/// `vp db` を実行
pub fn execute(cmd: DbCommands) -> Result<()> {
    let password = db::ensure_db_password();

    match cmd {
        DbCommands::Start => {
            match db::ensure_surreal_running(db::SURREAL_PORT, &password) {
                Ok(pid) => {
                    println!(
                        "SurrealDB running (pid: {}, port: {})",
                        pid,
                        db::SURREAL_PORT
                    );
                }
                Err(e) => {
                    eprintln!("SurrealDB 起動失敗: {}", e);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        DbCommands::Stop => {
            match db::stop_surreal() {
                Some(pid) => {
                    println!("SurrealDB stopped (pid: {})", pid);
                }
                None => {
                    println!("SurrealDB is not running");
                }
            }
            Ok(())
        }
        DbCommands::Restart => {
            match db::restart_surreal(db::SURREAL_PORT, &password) {
                Ok(pid) => {
                    println!(
                        "SurrealDB restarted (pid: {}, port: {})",
                        pid,
                        db::SURREAL_PORT
                    );
                }
                Err(e) => {
                    eprintln!("SurrealDB 再起動失敗: {}", e);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        DbCommands::Status => {
            match db::is_surreal_running() {
                Some(pid) => {
                    println!(
                        "SurrealDB is running (pid: {}, port: {})",
                        pid,
                        db::SURREAL_PORT
                    );
                }
                None => {
                    println!("SurrealDB is not running");
                }
            }
            Ok(())
        }
    }
}
