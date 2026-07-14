//! TheWorld daemon の auto-launch
//!
//! vp-app 起動時、`VP_WORLD_URL` の daemon が up でなければ `vp` バイナリを
//! 同梱 (standalone distribution) から spawn して待つ。
//!
//! ## 挙動
//!
//! 1. `<world_url>/api/health` を ping (500ms timeout)
//! 2. 成功 → ready を返す
//! 3. 失敗 + URL が localhost 相当なら → 起動を試み、up まで poll (最大 `LAUNCH_TIMEOUT`)。起動経路は:
//!    - macOS で LaunchAgent job が load 済みなら `launchctl kickstart`（-k なし）で起こす
//!      （所有権一本化 2026-07-14: 直接 spawn 個体が port を握ると LaunchAgent 個体が
//!      二重起動ガードで空回りし続け、brew upgrade の `kickstart -k` が実 holder に
//!      届かない「所有権分裂」が起きる。job が居るなら起動は launchd に委譲する）
//!    - job 未 load（LaunchAgent 未 install ユーザー）なら従来どおり `vp world` を
//!      background spawn（fallback）
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

/// TheWorld LaunchAgent の label。
///
/// SSOT は `crates/vantage-point/src/daemon/process.rs` の `LAUNCH_AGENT_LABEL`。
/// vp-app は重量 crate (vantage-point) に依存しない方針（Cargo.toml 参照）のため
/// ローカル定数で相互参照する。変更時は両方を同期すること。
#[cfg(target_os = "macos")]
const LAUNCH_AGENT_LABEL: &str = "club.chronista.vantage-point.daemon";

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

/// macOS: LaunchAgent job が load 済みなら `launchctl kickstart`（-k なし）で起こす。
///
/// 戻り値 = kickstart を発行したか（false = job 未 load / dev profile → caller は
/// 従来の直接 spawn fallback へ進む）。
///
/// - `launchctl print` の exit 0 は「job が load 済み」しか意味しない（その job が
///   port holder を所有している保証はない — 2026-07-14 の教訓）。ここでは caller が
///   health ping 失敗を確認済み = port holder 不在なので、load 済み job に起動を委譲してよい
/// - `-k` なし kickstart は「停止中なら即起動 / 稼働中なら no-op」の冪等な意味論。
///   KeepAlive の crash-restart throttle (~10s) を待たずに起こせる
#[cfg(target_os = "macos")]
fn try_kickstart_launch_agent() -> bool {
    // VP_PROFILE=dev では踏まない: label は profile 非分離なので、kickstart しても起きるのは
    // brew(release) 個体 (:32000) だけで dev(:32100) は上がらず、poll timeout に落ちるだけ。
    // dev daemon は従来どおり直接 spawn で起動する。
    if vp_paths::vp_profile().is_some() {
        return false;
    }
    let uid = unsafe { libc::getuid() };
    let target = format!("gui/{uid}/{LAUNCH_AGENT_LABEL}");
    let loaded = Command::new("launchctl")
        .args(["print", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !loaded {
        return false;
    }
    match Command::new("launchctl")
        .args(["kickstart", &target])
        .output()
    {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            tracing::warn!(
                "launchctl kickstart 非ゼロ ({}): {}",
                target,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            tracing::warn!("launchctl kickstart 実行失敗 ({}): {}", target, e);
            false
        }
    }
}

/// daemon が up になるまで poll する（最大 [`LAUNCH_TIMEOUT`]）
fn wait_daemon_up(world_url: &str) -> Result<()> {
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

    // 所有権一本化: LaunchAgent job が居るなら起動を launchd に委譲する（直接 spawn 個体が
    // port を握って launchd job と所有権分裂するのを構造的に防ぐ）。
    #[cfg(target_os = "macos")]
    if try_kickstart_launch_agent() {
        tracing::info!("daemon auto-launch: LaunchAgent kickstart に委譲");
        if wait_daemon_up(world_url).is_ok() {
            return Ok(());
        }
        // kickstart したが up せず = plist 破損の疑い（ProgramArguments[0] の binary が
        // 古い/不在。この repo には「plist に dev binary を焼いた」事故の前例あり。恢復は
        // `vp daemon install` の再実行）。job が load 済なら try_kickstart は true を返すが
        // launchd は起動に失敗するため、旧実装（常に直接 spawn）の恢復経路を後退させないよう、
        // 下の直接 spawn を最後の救済として一段試す。遅延 kickstart と直接 spawn が競合しても
        // #687 の二重起動ガード + bind AddrInUse で片方が譲り自己解決する。
        tracing::warn!(
            "daemon auto-launch: LaunchAgent kickstart 後も {} が up せず — 直接 spawn で救済を試みる（plist 破損の疑い、恢復は `vp daemon install` 再実行）",
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
        // GUI/launchd 起動の最小 PATH (`/usr/bin:/bin:...`) が daemon → SP → mise → claude へ
        // 伝播するのを spawn 最上流で断つ (#498/#501 再発の根治、 補正の SSOT は下記 augment_path)。
        .env("PATH", vp_paths::spawn_env::augmented_spawn_path())
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
        // TheWorld も SIGHUP 巻き添えで死亡 → `mise run app` ごとに TheWorld 再起動 = sidebar の
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

    wait_daemon_up(world_url)
}

// PATH 補強 (`augmented_spawn_path`) の SSOT は `vp_paths::spawn_env` に一本化した。
// GUI/launchd 起動の最小 PATH (`/usr/bin:/bin:...`) が daemon → SP へ伝播し mise/claude が
// 見つからず Echoes が出ない症状 (#498/#501 再発) を、 この spawn 最上流で断つ。 vantage-point と
// vp-app が同一実装を共有し、 かつて手動同期していたレプリカ (drift 源) を解消した。
