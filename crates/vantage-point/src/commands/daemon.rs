//! `vp daemon` コマンド (alias `vp world`) — TheWorld（常駐プロセス管理）
//!
//! - `vp daemon start` — TheWorld をフォアグラウンドで起動
//! - `vp daemon stop` — TheWorld を停止 (idempotent)
//! - `vp daemon status` — TheWorld の状態確認
//!
//! restart は意図的に提供しない。 user 指示「restart いらないかも、 わかりずらい。
//! build -> start -> stop が、 まず cli で回ってからだね。」 (2026-04-30) に従い、
//! 合成は user 責任で `vp daemon stop && vp daemon start` と並べる方針。
//!
//! 注: `vp world ...` は後方互換 alias で同じ実装に dispatch される。

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::process;

/// TheWorld サブコマンド
///
/// サブコマンド省略時は `start` として扱う（後方互換: `vp daemon --port 32000`）
#[derive(Subcommand)]
pub enum DaemonCommands {
    /// TheWorld を起動（foreground blocking、 backgrounding は呼出側で `&` / nohup）
    Start {
        /// 待ち受けポート番号
        #[arg(short, long, default_value_t = crate::cli::world_port())]
        port: u16,

        /// MIDI ポート指定 — usize ならポート index、 文字列ならポート名 pattern (部分一致)。
        ///
        /// PR-α-4 (VP-114) で復活: PR-α-2/3 で MidiCapability が World daemon に移管された
        /// 経路に対する CLI 入口。 未指定なら `MidiConfig::default()` (port_index/pattern 共に
        /// None = 最初の利用可能 port を auto pick)。 例: `vp daemon start --midi 0` (index 0)、
        /// `vp daemon start --midi LPD8` (pattern マッチ)。
        ///
        /// 旧 `vp start --midi` flag は PR-α-2 で warning + ignored 化済 (本 flag に rewire)。
        #[cfg(feature = "midi")]
        #[arg(long)]
        midi: Option<String>,
    },
    /// TheWorld を停止 (idempotent)
    Stop,
    /// TheWorld の状態確認
    Status,
    /// VP-154 PR-2.5: world-process channel 経由で Process snapshot / lifecycle を観察
    ///
    /// `vp daemon processes` で list (= snapshot 1 回出力)、 `--watch` で subscribe stream
    /// に切り替えて register/unregister/disconnect を realtime に表示する。 dogfood debug 用。
    Processes {
        /// snapshot のみ表示せず、 lifecycle event を realtime stream する
        #[arg(long)]
        watch: bool,
    },
    /// chronista-hub registry に居る world 一覧を取得（federation discovery）
    ///
    /// hub addr（env `CHRONISTA_HUB_ADDR` or config.kdl `hub-addr`）を設定した状態で
    /// `vp daemon start` していると、World が起動時に自身を hub に register する。本コマンドは
    /// TheWorld 経由で hub の `worlds.Discover` を叩き、同 hub に register した他 world を
    /// 列挙する。env / config とも未設定なら federation 無効。
    Discover,
    /// L1 lifecycle: TheWorld を LaunchAgent として常駐化（macOS、login always-on + crash 自動再起動）
    ///
    /// `~/Library/LaunchAgents/club.chronista.vantage-point.daemon.plist` を配置し launchctl で
    /// 起動する。`RunAtLoad`（login 時自動起動）+ `KeepAlive`（crash 時自動再起動）。idempotent。
    Install,
    /// L1 lifecycle: LaunchAgent 常駐を解除（plist 削除 + launchctl unload、idempotent）
    Uninstall,
}

/// `vp daemon` (= `vp world`) を実行
pub fn execute(cmd: DaemonCommands) -> Result<()> {
    match cmd {
        #[cfg(feature = "midi")]
        DaemonCommands::Start { port, midi } => start(port, midi),
        #[cfg(not(feature = "midi"))]
        DaemonCommands::Start { port } => start(port),
        DaemonCommands::Stop => stop(),
        DaemonCommands::Status => status(),
        DaemonCommands::Processes { watch } => processes(watch),
        DaemonCommands::Discover => discover(),
        DaemonCommands::Install => install(),
        DaemonCommands::Uninstall => uninstall(),
    }
}

#[cfg(feature = "midi")]
fn start(port: u16, midi: Option<String>) -> Result<()> {
    // 二重起動ガード: 既に TheWorld が稼働中なら讓って正常終了する。
    // これが無いと LaunchAgent(KeepAlive) / vp-app auto-launch / 手動 `vp world` が
    // 既存 daemon を確認せず run_world に突入し、SurrealDB world lock 衝突 → :port bind
    // AddrInUse → 異常終了 → KeepAlive 再起動の無限ループに陥る (2026-07-09 二重起動事故)。
    // is_daemon_running() は pidfile + port ping の二段確認なので pidfile 不整合でも検出できる。
    if let Some(pid) = process::is_daemon_running() {
        println!("👑 TheWorld は既に稼働中 (PID: {pid})。二重起動を避けて終了します。");
        return Ok(());
    }
    let midi_config = midi.as_ref().map(|midi_arg| build_midi_config(midi_arg));
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::process::run_world(port, midi_config))
}

#[cfg(not(feature = "midi"))]
fn start(port: u16) -> Result<()> {
    // 二重起動ガード（midi 版 start と同旨）: 既存 daemon 稼働中なら讓って正常終了。
    // LaunchAgent(KeepAlive) / vp-app auto-launch / 手動 `vp world` の二重起動 →
    // world DB lock 衝突 + bind AddrInUse → crash ループを防ぐ (2026-07-09 事故)。
    if let Some(pid) = process::is_daemon_running() {
        println!("👑 TheWorld は既に稼働中 (PID: {pid})。二重起動を避けて終了します。");
        return Ok(());
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::process::run_world(port))
}

/// `--midi <arg>` から `MidiConfig` を構築する。
///
/// 旧 `vp start --midi` の logic を移植 (PR-α-4 / VP-114):
/// - default config をベースに、 標準 PAD 3 つ (note 36/37/38) に VP action を bind
///   (PAD 1 = OpenWebUI、 PAD 2 = CancelChat、 PAD 3 = ResetSession)
/// - arg を `usize::parse` で試し、 成功なら port_index、 失敗なら port_pattern (部分一致)
#[cfg(feature = "midi")]
fn build_midi_config(midi_arg: &str) -> crate::midi::MidiConfig {
    let mut config = crate::midi::MidiConfig::default();
    config
        .note_actions
        .insert(36, crate::midi::MidiAction::OpenWebUI { port: None });
    config
        .note_actions
        .insert(37, crate::midi::MidiAction::CancelChat { port: None });
    config
        .note_actions
        .insert(38, crate::midi::MidiAction::ResetSession { port: None });

    if let Ok(idx) = midi_arg.parse::<usize>() {
        config.port_index = Some(idx);
    } else {
        config.port_pattern = Some(midi_arg.to_string());
    }
    config
}

fn stop() -> Result<()> {
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

/// `vp daemon install` — TheWorld を LaunchAgent 常駐化（macOS）。
#[cfg(target_os = "macos")]
fn install() -> Result<()> {
    // plist の ProgramArguments に焼く binary = 今 install を呼んでいる vp 自身。
    let exe = std::env::current_exe()?;
    let plist = process::install_launch_agent(&exe, crate::cli::world_port())?;
    println!("👑 LaunchAgent を install しました: {}", plist.display());
    println!("   login 時に自動起動 + crash 時に自動再起動します（vp daemon uninstall で解除）。");
    // KeepAlive=true 常駐中は SIGTERM を送っても launchd が即再起動するので、
    // `vp daemon stop` は一時停止にしかならない（恒久停止は uninstall）。
    println!("   注: 常駐中の `vp daemon stop` は一時的な再起動のみ。恒久停止は uninstall。");
    Ok(())
}

/// `vp daemon install` — TheWorld を Task Scheduler 常駐化（Windows）。
#[cfg(windows)]
fn install() -> Result<()> {
    // task の action に焼く binary = 今 install を呼んでいる vp 自身。
    let exe = std::env::current_exe()?;
    let task = process::install_scheduled_task(&exe, crate::cli::world_port())?;
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
/// world-process Unison channel に接続して list (snapshot) を出力。 `--watch` 時は subscribe に
/// 進んで `register/unregister/disconnect` の lifecycle event を Ctrl-C まで stream する。
fn processes(watch: bool) -> Result<()> {
    use crate::daemon::client::DaemonClient;
    use crate::daemon::protocol::ProcessLifecycleEvent;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let client = DaemonClient::connect(crate::cli::world_port(), 3)
            .await
            .map_err(|e| {
                anyhow::anyhow!("TheWorld 接続失敗: {} (= `vp daemon start` で起動済か?)", e)
            })?;

        // snapshot を最初に出す (= list / watch 共通の冒頭)
        let snapshot = client.world_processes_list().await?;
        println!(
            "📋 World 配下 Process snapshot ({} entries)",
            snapshot.len()
        );
        if snapshot.is_empty() {
            println!("  (= まだ SP register なし)");
        } else {
            for p in &snapshot {
                println!(
                    "  • {} (port={}, pid={}, path={})",
                    p.project_name, p.port, p.pid, p.project_path
                );
            }
        }

        if !watch {
            return Ok::<(), anyhow::Error>(());
        }

        println!("\n👁️  --watch: lifecycle event を Ctrl-C まで stream します");
        let ch = client.world_processes_subscribe().await?;
        loop {
            match DaemonClient::world_processes_recv_event(ch).await {
                Ok(ProcessLifecycleEvent::Add {
                    project_path,
                    project_name,
                    port,
                    pid,
                }) => {
                    println!(
                        "➕ Add: {} (port={}, pid={}, path={})",
                        project_name, port, pid, project_path
                    );
                }
                Ok(ProcessLifecycleEvent::Remove { project_path }) => {
                    println!("➖ Remove: {}", project_path);
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

/// `vp daemon discover` — chronista-hub registry の world 一覧を TheWorld 経由で取得する。
///
/// SSOT: CLI は hub に直接接続せず、World daemon の `hub/discover` RPC を叩く。
fn discover() -> Result<()> {
    use crate::daemon::client::WorldControlClient;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let client = WorldControlClient::connect(crate::cli::world_port(), 3)
            .await
            .map_err(|e| {
                anyhow::anyhow!("TheWorld 接続失敗: {} (= `vp daemon start` で起動済か?)", e)
            })?;

        let worlds = client.hub_discover().await?;
        if worlds.is_empty() {
            println!("🌐 hub registry に world なし (hub 未到達 or 登録ゼロ)");
        } else {
            println!("🌐 hub registry の world ({} 件):", worlds.len());
            for w in &worlds {
                let handle = w.get("handle").and_then(|v| v.as_str()).unwrap_or("?");
                let name = w.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let at = w
                    .get("registered_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                println!("  • {} ({}) registered_at={}", handle, name, at);
            }
        }
        Ok::<(), anyhow::Error>(())
    })
}

fn status() -> Result<()> {
    match process::is_daemon_running() {
        Some(pid) => {
            println!("👑 TheWorld is running (PID: {})", pid);
            // ヘルスチェックで詳細情報を取得
            if let Ok(resp) = reqwest::blocking::get(format!(
                "http://[::1]:{}/api/health",
                crate::cli::world_port()
            )) && let Ok(json) = resp.json::<serde_json::Value>()
            {
                println!(
                    "  Version: {}",
                    json.get("version").and_then(|v| v.as_str()).unwrap_or("?")
                );
                println!("  Port: {}", crate::cli::world_port());
                // hub federation 状態 (disabled/connecting/connected/disconnected)。
                // dogfood で「federation が本当に ON か」を CLI だけで確認できるようにする
                // (config.kdl hub-addr 永続化とペア、旧 daemon の health に hub field は無いので if let)。
                if let Some(hub) = json.get("hub").and_then(|v| v.as_str()) {
                    println!("  Hub: {}", hub);
                }
            }
            // Process 一覧
            if let Ok(resp) = reqwest::blocking::get(format!(
                "http://[::1]:{}/api/world/processes",
                crate::cli::world_port()
            )) && let Ok(json) = resp.json::<serde_json::Value>()
                && let Some(processes) = json.as_array()
            {
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
        None => {
            println!("TheWorld is not running");
        }
    }
    Ok(())
}
