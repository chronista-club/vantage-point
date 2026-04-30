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
    /// vp-app GUI を起動 (background spawn、 即 exit)
    Start {
        /// プロジェクト N 番を起動時に開く（省略時はランチャー画面）
        project_id: Option<usize>,
    },
    /// vp-app を停止 (SIGTERM、 idempotent)
    Stop,
}

pub fn execute(cmd: AppCommands) -> Result<()> {
    match cmd {
        AppCommands::Start { project_id } => start(project_id),
        AppCommands::Stop => stop(),
    }
}

/// vp-app を background spawn + 親即 exit。
///
/// 設計判断: `Command::status()` (= `wait()` 相当の blocking) ではなく `spawn()` で
/// child handle を drop することで、 parent (= `vp app start`) は child の終了を
/// 待たない。 stdout/stderr は log file に redirect、 unix では `process_group(0)`
/// (`setsid` 相当) で child を新しい process group に分離 ── parent shell が
/// SIGHUP / exit しても child は生存し続ける (D12: daemon lifecycle 独立性)。
fn start(project_id: Option<usize>) -> Result<()> {
    let bin = find_vp_app_binary().context(
        "vp-app binary not found. \
         Build it first: 'cargo build --release -p vp-app' \
         or install: 'cargo install --path crates/vp-app'",
    )?;

    // Phase A: log dir 統一 (~/Library/Logs/Vantage/ on macOS)
    let log_dir = log_dir_path();
    std::fs::create_dir_all(&log_dir).ok();
    let daemon_log = log_dir.join("daemon.kdl.log");
    let stdout_log = log_dir.join("app.stdout.log");

    println!("🚀 Launching vp-app: {}", bin.display());
    println!("   daemon log: {}", daemon_log.display());
    println!("   stdout log: {}", stdout_log.display());

    let mut cmd = std::process::Command::new(&bin);
    cmd.env("VP_DAEMON_LOG_FILE", &daemon_log);
    if let Some(id) = project_id {
        cmd.env("VP_PROJECT_ID", id.to_string());
    }

    // stdout/stderr を log file に redirect (parent が exit しても child の出力を
    // 失わないため、 file descriptor を OS に渡す)。
    let stdout_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stdout_log)
        .with_context(|| format!("Failed to open stdout log: {}", stdout_log.display()))?;
    let stderr_file = stdout_file
        .try_clone()
        .context("Failed to clone stdout file for stderr")?;
    cmd.stdout(stdout_file);
    cmd.stderr(stderr_file);

    // Unix: setsid 相当 (新 process group で child を分離、 親 shell の SIGHUP から守る)。
    // Windows は process_group API がないのでそのまま spawn する。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn vp-app at {}", bin.display()))?;
    let pid = child.id();
    println!("✅ vp-app launched (PID={pid})");
    println!("   logs: vp logs (or `tail -F {}`)", log_dir.display());

    // child handle drop = parent は child の終了を wait しない (= 即 exit)。
    drop(child);
    Ok(())
}

/// vp-app を SIGTERM で停止。 process が存在しなくても error にしない (idempotent)。
fn stop() -> Result<()> {
    let status = std::process::Command::new("pkill")
        .args(["-f", "vp-app$"])
        .status()
        .context("Failed to invoke pkill")?;
    match status.code() {
        Some(0) => println!("📴 vp-app stopped (SIGTERM sent)"),
        Some(1) => println!("(no vp-app process running)"),
        Some(c) => println!("(pkill exit code {c})"),
        None => println!("(pkill terminated by signal)"),
    }
    Ok(())
}

/// vp-app binary を探す:
/// 1. `VP_APP_BIN` env (mise task / dogfood で `target/release/vp-app` を直接渡す path)
/// 2. PATH 上の `vp-app` (cargo install で入った場合)
/// 3. 自分 (vp) の隣 (`~/.cargo/bin/vp` や `target/release/vp` の同 dir)
fn find_vp_app_binary() -> Option<PathBuf> {
    // `VP_APP_BIN` env が指す path が file として存在すれば最優先。
    // cargo install を毎回挟まずに `cargo build --release -p vp-app` 直後の binary を
    // 即座に呼べる。 dogfood loop の rebuild → restart を高速化する目的 ((γ) 設計)。
    if let Some(p) = std::env::var_os("VP_APP_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
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
