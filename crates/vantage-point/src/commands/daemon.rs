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
