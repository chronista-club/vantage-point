//! TheWorld daemon の auto-launch
//!
//! vp-app 起動時、`VP_WORLD_URL` の daemon が up でなければ `vp` バイナリを
//! 同梱 (standalone distribution) から spawn して待つ。
//!
//! ## 挙動
//!
//! 1. `<world_url>/api/health` を ping (500ms timeout)
//! 2. 成功 → ready を返す
//! 3. 失敗 + URL が localhost 相当なら → `vp world` を background spawn
//!    その後 up になるまで poll (最大 `LAUNCH_TIMEOUT`)
//! 4. 失敗 + 非 localhost → auto-launch せず Err を返す (remote daemon 扱い)
//!
//! ## vp バイナリの探索
//!
//! 1. `VP_BINARY` env var (明示 override)
//! 2. `std::env::current_exe()` と同じディレクトリの `vp[.exe]`
//! 3. PATH に載った `vp[.exe]` (`Command::new("vp")` にフォールバック)

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// daemon 起動 → up 待機のタイムアウト
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
/// ping 間隔
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// `vp` バイナリの場所を特定
fn locate_vp_binary() -> PathBuf {
    if let Ok(explicit) = std::env::var("VP_BINARY") {
        return PathBuf::from(explicit);
    }
    // vp-app.exe と同じディレクトリに vp.exe がある (standalone distribution)
    if let Ok(mut exe) = std::env::current_exe() {
        exe.pop();
        #[cfg(windows)]
        let candidate = exe.join("vp.exe");
        #[cfg(unix)]
        let candidate = exe.join("vp");
        if candidate.is_file() {
            return candidate;
        }
    }
    // fallback: PATH
    #[cfg(windows)]
    {
        PathBuf::from("vp.exe")
    }
    #[cfg(unix)]
    {
        PathBuf::from("vp")
    }
}

/// URL が localhost (127.0.0.1 / [::1] / localhost) に向いているか
///
/// 判定は文字列マッチのみ。vEthernet / LAN IP (10.x / 172.x / 192.168.x) は
/// remote 扱いとし、auto-launch せず caller にエラーを返す。
fn is_localhost(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]")
}

/// 同期 ping (tokio runtime を内部で起こす)
///
/// サイズ小さい build で十分なので reqwest blocking を使う。
fn ping_health(url: &str) -> bool {
    let endpoint = format!("{}/api/health", url);
    match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(client) => client
            .get(&endpoint)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// daemon が up でなければ auto-launch してから ready まで待つ
pub fn ensure_daemon_ready(world_url: &str) -> Result<()> {
    if ping_health(world_url) {
        tracing::info!("daemon already up at {}", world_url);
        return Ok(());
    }
    if !is_localhost(world_url) {
        anyhow::bail!(
            "daemon 未起動 ({}): remote URL なので auto-launch しない",
            world_url
        );
    }

    let vp_bin = locate_vp_binary();
    tracing::info!(
        "daemon auto-launch: spawning {} world (bg)",
        vp_bin.display()
    );

    let mut cmd = Command::new(&vp_bin);
    cmd.arg("world")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS で完全に切り離す
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    #[cfg(unix)]
    {
        // Phase 5-D fix: setsid(2) で 新 session leader 化 → controlling tty / parent process group
        // から完全切り離し。 これが無いと vp-app (parent) が pkill SIGTERM で死んだ時、 child の
        // TheWorld も SIGHUP 巻き添えで死亡 → `mr app` ごとに TheWorld 再起動 = sidebar の
        // Started time が 0sec にリセットされる bug が発生していた (2026-04-29 観測)。
        //
        // Windows は CREATE_NEW_PROCESS_GROUP + DETACHED_PROCESS で同等効果を得てる (上)。
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd.spawn().with_context(|| {
        format!(
            "vp バイナリ起動失敗 ({}): 同梱 or PATH を確認してください",
            vp_bin.display()
        )
    })?;
    // Child を drop して wait しない → daemon として独立稼働
    let pid = child.id();
    drop(child);
    tracing::info!("daemon spawned (pid={})", pid);

    // up まで poll
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    while Instant::now() < deadline {
        if ping_health(world_url) {
            tracing::info!("daemon up after auto-launch");
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    anyhow::bail!(
        "daemon auto-launch: {} に {}s 以内に応答なし",
        world_url,
        LAUNCH_TIMEOUT.as_secs()
    )
}
