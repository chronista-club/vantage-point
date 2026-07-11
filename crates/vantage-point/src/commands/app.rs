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
    Start,
    /// vp-app を停止 (SIGTERM、 idempotent)
    Stop,
}

pub fn execute(cmd: AppCommands) -> Result<()> {
    match cmd {
        AppCommands::Start => start(),
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
fn start() -> Result<()> {
    // ghost project の除去のみ実行（cwd の自動登録はしない）。
    // 以前は cwd を project として自動登録していたが、~/等の意図しないディレクトリが
    // 登録されてしまう問題があったため、sync は登録なし(None)で呼ぶ。
    let outcome = match crate::world_client::notify_world_sync() {
        Some(o) => Ok(o),
        None => crate::projects_file::ProjectsFile::sync(),
    };
    if let Ok(outcome) = outcome {
        for name in &outcome.removed {
            println!("🧹 ghost project を除去: {name}");
        }
    }

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
    // Windows: CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS で console を切り離し、
    //          親 (vp.exe) が exit しても vp-app GUI が独立稼働する。
    //          daemon_launcher.rs と同パターン。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
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

/// vp-app を停止 (Unix: SIGTERM via pkill、 Windows: taskkill /F /IM)。
/// process が存在しなくても error にしない (idempotent)。
fn stop() -> Result<()> {
    #[cfg(unix)]
    {
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
    #[cfg(windows)]
    {
        // Windows: `taskkill /F /IM vp-app.exe`。 SIGTERM 相当の graceful 経路は
        // window message (WM_CLOSE) 送信が必要だが、 まずは /F (hard kill) で
        // idempotent 停止を提供 (pkill -f 相当の挙動)。
        let status = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "vp-app.exe"])
            .status()
            .context("Failed to invoke taskkill")?;
        match status.code() {
            Some(0) => println!("📴 vp-app stopped (taskkill /F)"),
            Some(128) => println!("(no vp-app process running)"),
            Some(c) => println!("(taskkill exit code {c})"),
            None => println!("(taskkill terminated by signal)"),
        }
        Ok(())
    }
}

/// vp-app binary を探す:
/// 1. `VP_APP_BIN` env (mise task / dogfood で `target/release/vp-app` を直接渡す path)
/// 2. PATH 上の `vp-app` (cargo install で入った場合)
/// 3. 自分 (vp) の隣 (`~/.cargo/bin/vp` や `target/release/vp`、
///    Homebrew cask 配布なら `VantagePoint.app/Contents/MacOS/vp` の同 dir)
///
/// Windows では `.exe` 拡張子のついた binary を併せて探す。
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
    for name in binary_candidates("vp-app") {
        if let Some(p) = find_in_path(&name) {
            return Some(p);
        }
        // 自分 (vp) の隣を探す。Homebrew cask 配布では vp が
        // `/opt/homebrew/bin/vp` → `VantagePoint.app/Contents/MacOS/vp` の symlink
        // として実行され、macOS の `current_exe()` は symlink を解決せず link path を
        // そのまま返す。そのまま parent を見ると `/opt/homebrew/bin/` (vp-app 不在) を
        // 指してしまうため、canonicalize で実体 (bundle 内) に解決してから隣を見る。
        // canonicalize 失敗時は raw path の隣に fallback。
        if let Ok(self_exe) = std::env::current_exe() {
            let resolved = self_exe.canonicalize().unwrap_or(self_exe);
            if let Some(dir) = resolved.parent() {
                let candidate = dir.join(&name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// platform に応じた binary 名候補。 Windows は `.exe` 付きも試す。
fn binary_candidates(name: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![format!("{name}.exe"), name.to_string()]
    }
    #[cfg(unix)]
    {
        vec![name.to_string()]
    }
}

/// PATH を OS の区切り (`:` Unix / `;` Windows) で split して name を含む path を返す。
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    // `std::env::split_paths` は OS の PATH 区切り文字を正しく扱う (Unix=`:`, Windows=`;`)。
    std::env::split_paths(&path_var)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// log 出力先 — XDG state zone 配下 (`vp_log_dir()` = `~/.local/state/vp/log/`)。
fn log_dir_path() -> PathBuf {
    crate::config::vp_log_dir()
}
