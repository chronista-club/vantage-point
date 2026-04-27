//! `vp app` コマンドの実行ロジック
//!
//! Mac 主軸切替 (2026-04-27, mem_1CaSjv5QQUNDxsEMjAicJ7):
//! vp-app crate (Rust + wry + xterm.js + creo-ui) を spawn する。
//! 旧 Swift VantagePoint.app 起動経路は廃止。

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum AppCommands {
    /// vp-app GUI を起動 (Rust + wry + xterm.js + creo-ui)
    Run {
        /// プロジェクト N 番を起動時に開く（省略時はランチャー画面）
        project_id: Option<usize>,
    },
}

pub fn execute(cmd: AppCommands) -> Result<()> {
    match cmd {
        AppCommands::Run { project_id } => run(project_id),
    }
}

fn run(project_id: Option<usize>) -> Result<()> {
    let bin = find_vp_app_binary().context(
        "vp-app binary not found. \
         Build it first: 'cargo build --release -p vp-app' \
         or install: 'cargo install --path crates/vp-app'",
    )?;

    // Phase A: log dir 統一 (~/Library/Logs/Vantage/ on macOS)
    let log_dir = log_dir_path();
    std::fs::create_dir_all(&log_dir).ok();
    let daemon_log = log_dir.join("vp-world.kdl.log");

    println!("🚀 Launching vp-app: {}", bin.display());
    println!("   daemon log: {}", daemon_log.display());

    let mut cmd = std::process::Command::new(&bin);
    cmd.env("VP_DAEMON_LOG_FILE", &daemon_log);
    if let Some(id) = project_id {
        cmd.env("VP_PROJECT_ID", id.to_string());
    }

    // foreground spawn — ユーザの terminal に attach、Ctrl+C で一緒に終わる。
    let status = cmd
        .status()
        .with_context(|| format!("Failed to spawn vp-app at {}", bin.display()))?;

    std::process::exit(status.code().unwrap_or(1));
}

/// vp-app binary を探す:
/// 1. PATH 上の `vp-app` (cargo install で入った場合)
/// 2. 自分 (vp) の隣 (`~/.cargo/bin/vp` や `target/release/vp` の同 dir)
fn find_vp_app_binary() -> Option<PathBuf> {
    if let Some(p) = find_in_path("vp-app") {
        return Some(p);
    }
    if let Ok(self_exe) = std::env::current_exe()
        && let Some(dir) = self_exe.parent()
    {
        let candidate = dir.join("vp-app");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    path_var
        .to_str()?
        .split(':')
        .map(|d| PathBuf::from(d).join(name))
        .find(|p| p.is_file())
}

/// macOS: `~/Library/Logs/Vantage/`、その他: `~/.local/state/vantage/logs/`
fn log_dir_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Logs/Vantage")
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/state/vantage/logs")
    }
}
