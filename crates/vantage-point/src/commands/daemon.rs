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
        #[arg(short, long, default_value_t = crate::cli::WORLD_PORT)]
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
    /// `CHRONISTA_HUB_ADDR` を設定した状態で `vp daemon start` していると、World が起動時に
    /// 自身を hub に register する。本コマンドは TheWorld 経由で hub の `worlds.Discover` を叩き、
    /// 同 hub に register した他 world を列挙する。env 未設定なら federation 無効。
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
    let midi_config = midi.as_ref().map(|midi_arg| build_midi_config(midi_arg));
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::process::run_world(port, midi_config))
}

#[cfg(not(feature = "midi"))]
fn start(port: u16) -> Result<()> {
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
    let plist = process::install_launch_agent(&exe, crate::cli::WORLD_PORT)?;
    println!("👑 LaunchAgent を install しました: {}", plist.display());
    println!("   login 時に自動起動 + crash 時に自動再起動します（vp daemon uninstall で解除）。");
    // KeepAlive=true 常駐中は SIGTERM を送っても launchd が即再起動するので、
    // `vp daemon stop` は一時停止にしかならない（恒久停止は uninstall）。
    println!("   注: 常駐中の `vp daemon stop` は一時的な再起動のみ。恒久停止は uninstall。");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install() -> Result<()> {
    anyhow::bail!(
        "LaunchAgent 常駐化は macOS のみ対応です（Linux は systemd user unit、Windows は別途）"
    )
}

/// `vp daemon uninstall` — LaunchAgent 常駐を解除（macOS）。
#[cfg(target_os = "macos")]
fn uninstall() -> Result<()> {
    process::uninstall_launch_agent()?;
    println!("👑 LaunchAgent を uninstall しました（plist 削除 + launchctl unload）。");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn uninstall() -> Result<()> {
    anyhow::bail!("LaunchAgent 常駐化は macOS のみ対応です")
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
        let client = DaemonClient::connect(crate::cli::WORLD_PORT, 3)
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
                let tmux = p
                    .tmux_session
                    .as_deref()
                    .map(|s| format!(" tmux={}", s))
                    .unwrap_or_default();
                println!(
                    "  • {} (port={}, pid={}{}, path={})",
                    p.project_name, p.port, p.pid, tmux, p.project_path
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
        let client = WorldControlClient::connect(crate::cli::WORLD_PORT, 3)
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
                crate::cli::WORLD_PORT
            )) && let Ok(json) = resp.json::<serde_json::Value>()
            {
                println!(
                    "  Version: {}",
                    json.get("version").and_then(|v| v.as_str()).unwrap_or("?")
                );
                println!("  Port: {}", crate::cli::WORLD_PORT);
            }
            // Process 一覧
            if let Ok(resp) = reqwest::blocking::get(format!(
                "http://[::1]:{}/api/world/processes",
                crate::cli::WORLD_PORT
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
