//! `vp daemon` コマンド (alias `vp daemon`) — daemon（常駐プロセス管理）
//!
//! - `vp daemon start` — daemon をフォアグラウンドで起動
//! - `vp daemon stop` — daemon を停止 (idempotent)
//! - `vp daemon status` — daemon の状態確認
//! - `vp daemon restart` — daemon を ownership-agnostic に再起動
//!
//! restart は長らく意図的に提供しなかった（user 指示 2026-04-30「restart いらないかも」、
//! 合成は `stop && start` で足りる想定）が、brew upgrade 後に daemon が旧 binary のまま残る
//! 事象の根治（2026-07-14）で新設した。真因は「所有権分裂」— vp-app が直接 spawn した個体が
//! daemon port を握ると、LaunchAgent(KeepAlive) の job は二重起動ガードで空回りし続け、
//! cask postflight の `launchctl kickstart -k` は launchd job の個体しか叩けず実 holder に
//! 届かない。`vp daemon restart` は pidfile や launchd の見立てではなく **実 port holder**
//! （`/api/health`）を真実源に停止し、LaunchAgent 優先で立て直す（in-app update のエンジン兼務）。
//!
//! 注: `vp daemon ...` は後方互換 alias で同じ実装に dispatch される。

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::process;

/// daemon サブコマンド
///
/// サブコマンド省略時は `start` として扱う（後方互換: `vp daemon --port 32000`）
#[derive(Subcommand)]
pub enum DaemonCommands {
    /// daemon を起動（foreground blocking、 backgrounding は呼出側で `&` / nohup）
    Start {
        /// 待ち受けポート番号
        #[arg(short, long, default_value_t = crate::cli::daemon_port())]
        port: u16,
        // 旧 `--midi <arg>` flag は MidiCapability hosting 退役（fleet #877 系）で削除。
        // device 管理は DeviceRegistry 🧲 が担い、単一 port の pick 指定は不要になった。
    },
    /// daemon を停止 (idempotent)
    Stop,
    /// daemon を再起動（ownership-agnostic: 実 port holder を停止 → LaunchAgent 優先で起動）
    Restart {
        /// 稼働中の場合のみ再起動する（不在なら何もせず正常終了。brew cask postflight 用）
        #[arg(long)]
        if_running: bool,
    },
    /// daemon の状態確認
    Status,
    /// VP-154 PR-2.5: daemon-repo channel 経由で Process snapshot / lifecycle を観察
    ///
    /// `vp daemon processes` で list (= snapshot 1 回出力)、 `--watch` で subscribe stream
    /// に切り替えて register/unregister/disconnect を realtime に表示する。 dogfood debug 用。
    Processes {
        /// snapshot のみ表示せず、 lifecycle event を realtime stream する
        #[arg(long)]
        watch: bool,
    },
    /// chronista-hub registry に居る daemon 一覧を取得（federation discovery）
    ///
    /// hub addr（env `CHRONISTA_HUB_ADDR` or config.kdl `hub-addr`）を設定した状態で
    /// `vp daemon start` していると、daemon が起動時に自身を hub に register する。本コマンドは
    /// daemon 経由で hub の `nodes.Discover` を叩き、同 hub に register した他 daemon を
    /// 列挙する。env / config とも未設定なら federation 無効。
    Discover,
    /// L1 lifecycle: daemon を LaunchAgent として常駐化（macOS、login always-on + crash 自動再起動）
    ///
    /// `~/Library/LaunchAgents/club.chronista.vantage-point.daemon.plist` を配置し launchctl で
    /// 起動する。`RunAtLoad`（login 時自動起動）+ `KeepAlive`（crash 時自動再起動）。idempotent。
    Install,
    /// L1 lifecycle: LaunchAgent 常駐を解除（plist 削除 + launchctl unload、idempotent）
    Uninstall,
}

/// `vp daemon` (= `vp daemon`) を実行
pub fn execute(cmd: DaemonCommands) -> Result<()> {
    match cmd {
        DaemonCommands::Start { port } => start(port),
        DaemonCommands::Stop => stop(),
        DaemonCommands::Restart { if_running } => restart(if_running),
        DaemonCommands::Status => status(),
        DaemonCommands::Processes { watch } => processes(watch),
        DaemonCommands::Discover => discover(),
        DaemonCommands::Install => install(),
        DaemonCommands::Uninstall => uninstall(),
    }
}

fn start(port: u16) -> Result<()> {
    // 二重起動ガード: 既に daemon が稼働中なら讓って正常終了する。
    // これが無いと LaunchAgent(KeepAlive) / vp-app auto-launch / 手動 `vp daemon` が
    // 既存 daemon を確認せず run_daemon に突入し、SurrealDB daemon lock 衝突 → :port bind
    // AddrInUse → 異常終了 → KeepAlive 再起動の無限ループに陥る (2026-07-09 二重起動事故)。
    // is_daemon_running() は pidfile + port ping の二段確認なので pidfile 不整合でも検出できる。
    if let Some(pid) = process::is_daemon_running() {
        println!("👑 daemon は既に稼働中 (PID: {pid})。二重起動を避けて終了します。");
        return Ok(());
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::repo::run_daemon(port))
}

fn stop() -> Result<()> {
    match process::is_daemon_running() {
        Some(pid) => {
            process::stop_daemon(pid)?;
            println!("👑 daemon stopped (PID: {})", pid);
        }
        None => {
            println!("daemon is not running");
        }
    }
    Ok(())
}

/// restart: 起動確認 / 停止遷移の待ち上限（秒）。health 応答・pid 変化・停止完了の共通期限。
const RESTART_HEALTH_TIMEOUT_SECS: u64 = 15;

/// daemon port の実 holder に `/api/health` を問い合わせる（2s timeout、失敗 = 不在扱い）。
///
/// port は [`crate::cli::daemon_port`]（= `vp_paths::default_daemon_port()`、VP_PROFILE 準拠）
/// を caller が渡す。pidfile ではなく HTTP を真実源にするのは、所有権分裂（pidfile の指す
/// 個体 ≠ 実 port holder）でも正しい相手を掴むため。
fn fetch_health(port: u16) -> Option<crate::cli::HealthResponse> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    client
        .get(format!("http://[::1]:{port}/api/health"))
        .send()
        .ok()?
        .json()
        .ok()
}

/// stop 後の daemon の遷移結果。
enum StopTransition {
    /// old_pid と別 pid の holder が既に応答している（LaunchAgent(KeepAlive) が clean case で
    /// 即 respawn 済み）。同梱の health をそのまま成功として使える（明示起動は不要）。
    Respawned(crate::cli::HealthResponse),
    /// old_pid が holder として消えた（health 応答なし）。後継が居ないので caller が明示起動する。
    Stopped,
}

/// stop 後の遷移を待つ（「port が free になる」を待たないのが要点）。
///
/// 旧実装は「port が free になる」を待っていたが、所有権が既に clean（分裂していない）な場合、
/// SIGTERM 直後に LaunchAgent(KeepAlive) が fresh を即 respawn して port を再占有するため、
/// free window が空かず spurious timeout で restart が Err に落ちた。ここでは代わりに:
/// - health.pid が old_pid と変化 → [`StopTransition::Respawned`]（launchd が既に立て直し済み）
/// - health 応答なし → [`StopTransition::Stopped`]（後継未起動 → caller が kickstart / spawn）
/// - `RESTART_HEALTH_TIMEOUT_SECS` まで old_pid が holder のまま → Err（停止しきれず）
fn wait_stop_transition(port: u16, old_pid: u32) -> Result<StopTransition> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(RESTART_HEALTH_TIMEOUT_SECS);
    loop {
        match fetch_health(port) {
            None => return Ok(StopTransition::Stopped),
            Some(health) if health.pid != old_pid => {
                return Ok(StopTransition::Respawned(health));
            }
            Some(_) => {
                // まだ old_pid が holder。停止完了 or respawn を待つ。
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!(
                        "daemon (PID {old_pid}) が {RESTART_HEALTH_TIMEOUT_SECS}s 以内に停止しませんでした (port {port}、`vp daemon status` で確認してください)"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
    }
}

/// 起動した daemon が health 応答するまで待ち、レスポンスを返す。
fn wait_health(port: u16) -> Result<crate::cli::HealthResponse> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(RESTART_HEALTH_TIMEOUT_SECS);
    while std::time::Instant::now() < deadline {
        if let Some(health) = fetch_health(port) {
            return Ok(health);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    anyhow::bail!("daemon が {RESTART_HEALTH_TIMEOUT_SECS}s 以内に応答しませんでした (port {port})")
}

/// macOS: LaunchAgent job が load 済みなら `launchctl kickstart`（-k なし）で起こす。
///
/// 戻り値 = 起動を launchd に委譲したか（false = job 未 load / dev profile / kickstart
/// 失敗 → caller は detached spawn fallback へ進む）。
///
/// - `-k` なし kickstart は「停止中なら即起動 / 稼働中なら no-op」の冪等な意味論。
///   KeepAlive の crash-restart throttle (~10s) を待たずに起こせる
/// - `launchctl print` の exit 0 は「job が load 済み」しか意味しない（その job が port
///   holder を所有している保証はない — 2026-07-14 の教訓）。caller が port 解放を確認して
///   から呼ぶこと
/// - 対 vp-app: `crates/vp-app/src/daemon_launcher.rs` の `try_kickstart_launch_agent` と
///   同型（vp-app は vantage-point 非依存のため実装を共有できない。変更時は両方を同期）
#[cfg(target_os = "macos")]
fn try_kickstart_launch_agent() -> bool {
    // VP_PROFILE=dev では踏まない: label は profile 非分離で、kickstart で起きるのは
    // brew(release) 個体 (:32000) — dev(:32100) の再起動にはならない。dev は detached spawn で。
    if vp_paths::vp_profile().is_some() {
        return false;
    }
    let uid = unsafe { libc::getuid() };
    let target = format!("gui/{uid}/{}", process::LAUNCH_AGENT_LABEL);
    let loaded = std::process::Command::new("launchctl")
        .args(["print", &target])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !loaded {
        return false;
    }
    std::process::Command::new("launchctl")
        .args(["kickstart", &target])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 非 macOS: LaunchAgent は存在しないため常に false（detached spawn 経路へ）。
#[cfg(not(target_os = "macos"))]
fn try_kickstart_launch_agent() -> bool {
    false
}

/// `vp daemon restart [--if-running]` — daemon を ownership-agnostic に再起動する。
///
/// 1. 実 holder の稼働確認（`/api/health` — pidfile / launchctl の見立てではなく port holder が真実源）
/// 2. 稼働中なら graceful stop（`vp daemon stop` と同一実装 = SIGTERM → SIGKILL fallback）。停止後は
///    「old_pid の消滅 or pid 変化」を待つ:
///      - pid 変化 = LaunchAgent(KeepAlive) が clean case で即 respawn 済 → kickstart 不要で
///        その health を成功報告して終了（`vp daemon` binary は既に入れ替わり済み）
///      - health 応答なし = 後継未起動 → 手順3 で明示起動
/// 3. macOS で LaunchAgent load 済みなら `launchctl kickstart`（-k なし）で即起こす / それ以外は
///    detached spawn（`ensure_daemon_running` = repo auto-spawn と同じ既存経路）
/// 4. health ping で起動確認し、起動した daemon の version を表示
pub(crate) fn restart(if_running: bool) -> Result<()> {
    let port = crate::cli::daemon_port();

    let holder = fetch_health(port);
    if holder.is_none() && if_running {
        println!("daemon は稼働していません（--if-running 指定のため何もしません）");
        return Ok(());
    }

    if let Some(health) = &holder {
        let old_pid = health.pid;
        println!(
            "⏹ daemon を停止中 (PID: {}, version: {})...",
            old_pid, health.version
        );
        process::stop_daemon(old_pid)?;
        // 「port free」ではなく「old_pid の消滅 / pid 変化」を待つ（clean-ownership case で
        // KeepAlive の即時 respawn と競合しないため。詳細は wait_stop_transition の doc）。
        if let StopTransition::Respawned(new_health) = wait_stop_transition(port, old_pid)? {
            // LaunchAgent が既に fresh を立て直した。kickstart せずそのまま成功。
            println!(
                "👑 daemon restarted by LaunchAgent (PID: {}, version: {}, port: {})",
                new_health.pid, new_health.version, port
            );
            return Ok(());
        }
    }

    let kicked = try_kickstart_launch_agent();
    if kicked {
        println!("🚀 LaunchAgent kickstart で起動中...");
    } else {
        println!("🚀 daemon を起動中 (detached spawn)...");
        process::ensure_daemon_running(port)?;
    }

    // 起動確認。kickstart に委譲したのに up しない場合 = plist 破損の疑い（ProgramArguments[0]
    // の binary が古い/不在。この repo には「plist に dev binary を焼いた」事故の前例あり。恢復は
    // `vp daemon install` の再実行）。job が load 済なら try_kickstart は true を返すが launchd は
    // 起動に失敗するため、旧経路（常に spawn）の後退を防ぐべく detached spawn を最後の救済として
    // 一段試す。遅延 kickstart と detached spawn が競合しても #687 の二重起動ガード + bind
    // AddrInUse で片方が譲り自己解決する。
    let health = match wait_health(port) {
        Ok(health) => health,
        Err(_) if kicked => {
            eprintln!(
                "⚠️ LaunchAgent kickstart 後も up せず — detached spawn で救済を試みます（plist 破損の疑い、恢復は `vp daemon install` 再実行）"
            );
            process::ensure_daemon_running(port)?;
            wait_health(port)?
        }
        Err(e) => return Err(e),
    };
    println!(
        "👑 daemon restarted (PID: {}, version: {}, port: {})",
        health.pid, health.version, port
    );
    Ok(())
}

/// `vp daemon install` — daemon を LaunchAgent 常駐化（macOS）。
#[cfg(target_os = "macos")]
fn install() -> Result<()> {
    // plist の ProgramArguments に焼く binary = 今 install を呼んでいる vp 自身。
    let exe = std::env::current_exe()?;
    let plist = process::install_launch_agent(&exe, crate::cli::daemon_port())?;
    println!("👑 LaunchAgent を install しました: {}", plist.display());
    println!("   login 時に自動起動 + crash 時に自動再起動します（vp daemon uninstall で解除）。");
    // KeepAlive=true 常駐中は SIGTERM を送っても launchd が即再起動するので、
    // `vp daemon stop` は一時停止にしかならない（恒久停止は uninstall）。
    println!("   注: 常駐中の `vp daemon stop` は一時的な再起動のみ。恒久停止は uninstall。");
    Ok(())
}

/// `vp daemon install` — daemon を Task Scheduler 常駐化（Windows）。
#[cfg(windows)]
fn install() -> Result<()> {
    // task の action に焼く binary = 今 install を呼んでいる vp 自身。
    let exe = std::env::current_exe()?;
    let task = process::install_scheduled_task(&exe, crate::cli::daemon_port())?;
    println!("👑 Task Scheduler task を install しました: {task}");
    println!(
        "   ログオン時に自動起動 + crash 時に自動再起動します（vp daemon uninstall で解除）。"
    );

    // 即時有効化: 既存 daemon が無ければ今すぐ task を走らせる。
    // 稼働中なら二重起動（port bind 衝突 → RestartOnFailure loop）を避けて skip。
    if process::is_daemon_running().is_none() {
        let ran = std::process::Command::new("schtasks")
            .args(["/run", "/tn", &task])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ran {
            println!("   今すぐ起動しました（schtasks /run）。");
        } else {
            println!(
                "   注: 次回ログオンから自動起動します（今すぐは再ログオン or `schtasks /run /tn {task}`）。"
            );
        }
    } else {
        // KeepAlive 相当（RestartOnFailure）常駐化後は、TerminateProcess で落としても
        // 次回ログオン以降 task が拾う。恒久停止は uninstall。
        println!(
            "   注: 既に daemon 稼働中。常駐は次回ログオンから。恒久停止は `vp daemon uninstall`。"
        );
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn install() -> Result<()> {
    anyhow::bail!(
        "常駐化は macOS(LaunchAgent) / Windows(Task Scheduler) のみ対応です（Linux は systemd user unit 予定）"
    )
}

/// `vp daemon uninstall` — LaunchAgent 常駐を解除（macOS）。
#[cfg(target_os = "macos")]
fn uninstall() -> Result<()> {
    process::uninstall_launch_agent()?;
    println!("👑 LaunchAgent を uninstall しました（plist 削除 + launchctl unload）。");
    Ok(())
}

/// `vp daemon uninstall` — Task Scheduler 常駐を解除（Windows）。
#[cfg(windows)]
fn uninstall() -> Result<()> {
    let task = process::uninstall_scheduled_task()?;
    println!("👑 Task Scheduler task を uninstall しました（schtasks /delete）: {task}");
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn uninstall() -> Result<()> {
    anyhow::bail!("常駐化は macOS / Windows のみ対応です")
}

/// VP-154 PR-2.5: `vp daemon processes [--watch]` 実装。
///
/// daemon-repo Unison channel に接続して list (snapshot) を出力。 `--watch` 時は subscribe に
/// 進んで `register/unregister/disconnect` の lifecycle event を Ctrl-C まで stream する。
fn processes(watch: bool) -> Result<()> {
    use crate::daemon::client::DaemonClient;
    use crate::daemon::protocol::ProcessLifecycleEvent;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let client = DaemonClient::connect(crate::cli::daemon_port(), 3)
            .await
            .map_err(|e| {
                anyhow::anyhow!("daemon 接続失敗: {} (= `vp daemon start` で起動済か?)", e)
            })?;

        // snapshot を最初に出す (= list / watch 共通の冒頭)
        let snapshot = client.daemon_processes_list().await?;
        println!(
            "📋 Daemon 配下 Process snapshot ({} entries)",
            snapshot.len()
        );
        if snapshot.is_empty() {
            println!("  (= まだ repo register なし)");
        } else {
            for p in &snapshot {
                println!(
                    "  • {} (port={}, pid={}, path={})",
                    p.repo_name, p.port, p.pid, p.repo_path
                );
            }
        }

        if !watch {
            return Ok::<(), anyhow::Error>(());
        }

        println!("\n👁️  --watch: lifecycle event を Ctrl-C まで stream します");
        let ch = client.daemon_processes_subscribe().await?;
        loop {
            match DaemonClient::daemon_processes_recv_event(ch).await {
                Ok(ProcessLifecycleEvent::Add {
                    repo_path,
                    repo_name,
                    port,
                    pid,
                }) => {
                    println!(
                        "➕ Add: {} (port={}, pid={}, path={})",
                        repo_name, port, pid, repo_path
                    );
                }
                Ok(ProcessLifecycleEvent::Remove { repo_path }) => {
                    println!("➖ Remove: {}", repo_path);
                }
                Err(e) => {
                    eprintln!("⚠️  stream 終了: {}", e);
                    break;
                }
            }
        }
        Ok(())
    })
}

/// `vp daemon discover` — chronista-hub registry の daemon 一覧を daemon 経由で取得する。
///
/// SSOT: CLI は hub に直接接続せず、daemon の `hub/discover` RPC を叩く。
fn discover() -> Result<()> {
    use crate::daemon::client::DaemonControlClient;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let client = DaemonControlClient::connect(crate::cli::daemon_port(), 3)
            .await
            .map_err(|e| {
                anyhow::anyhow!("daemon 接続失敗: {} (= `vp daemon start` で起動済か?)", e)
            })?;

        let nodes = client.hub_discover().await?;
        if nodes.is_empty() {
            println!("🌐 hub registry に daemon なし (hub 未到達 or 登録ゼロ)");
        } else {
            println!("🌐 hub registry の daemon ({} 件):", nodes.len());
            for w in &nodes {
                let handle = w.get("handle").and_then(|v| v.as_str()).unwrap_or("?");
                let name = w.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let at = w
                    .get("registered_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // connected = hub の relay registry に常駐接続が生きているか (hub v0.6.0)。
                // 🟢 = relay 到達可 / ⚪ = registry には居るが offline (stale / 切断中)。
                let dot = if w
                    .get("connected")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    "🟢"
                } else {
                    "⚪"
                };
                println!("  {} {} ({}) registered_at={}", dot, handle, name, at);
            }
        }
        Ok::<(), anyhow::Error>(())
    })
}

fn status() -> Result<()> {
    match process::is_daemon_running() {
        Some(pid) => {
            println!("👑 daemon is running (PID: {})", pid);
            // ヘルスチェックで詳細情報を取得
            if let Ok(resp) = reqwest::blocking::get(format!(
                "http://[::1]:{}/api/health",
                crate::cli::daemon_port()
            )) && let Ok(json) = resp.json::<serde_json::Value>()
            {
                println!(
                    "  Version: {}",
                    json.get("version").and_then(|v| v.as_str()).unwrap_or("?")
                );
                println!("  Port: {}", crate::cli::daemon_port());
                // hub federation 状態 (disabled/connecting/connected/disconnected)。
                // dogfood で「federation が本当に ON か」を CLI だけで確認できるようにする
                // (config.kdl hub-addr 永続化とペア、旧 daemon の health に hub field は無いので if let)。
                if let Some(hub) = json.get("hub").and_then(|v| v.as_str()) {
                    println!("  Hub: {}", hub);
                }
            }
            // Process 一覧
            //
            // doc 45 段 2: 旧 `GET /api/daemon/processes` から Unison `registry.list` に差し替え。
            // ⚠️ 旧経路はこの行を**一度も表示していなかった**: response は
            // `{"processes": [...]}` なのに `json.as_array()` で受けていて、常に None に
            // 落ちて丸ごと skip されていた（transport 移行のついでに直る類の取りこぼし）。
            if let Some(processes) = crate::daemon_client::list_processes_blocking() {
                println!("  Processes: {}", processes.len());
                for p in &processes {
                    let name = p.get("repo_name").and_then(|v| v.as_str()).unwrap_or("?");
                    let path = p.get("repo_path").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("    - {} ({})", name, path);
                }
            }
        }
        None => {
            println!("daemon is not running");
            // script から `vp daemon status` の成否で分岐できるよう、停止中は非ゼロで返す
            // (launchctl list / systemctl status と同じ流儀。従来は稼働中/停止中どちらも
            //  exit 0 で判別不能だった)。表示は出し終えているので即 exit で問題ない。
            std::process::exit(1);
        }
    }
    Ok(())
}
