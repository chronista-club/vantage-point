//! vp sp — Star Platinum サーバー管理
//!
//! SP（HTTP/QUIC サーバー）のライフサイクル管理。
//! tmux/ccwire の管理は hd_cmd.rs に分離。

use anyhow::Result;
use clap::Subcommand;

use crate::cli::DebugModeArg;
use crate::config::Config;
use crate::protocol::DebugMode;

#[derive(Subcommand)]
pub enum SpCommands {
    /// SP サーバーを起動
    Start {
        /// 待ち受けポート番号
        #[arg(short, long)]
        port: Option<u16>,
        /// デバッグモード
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,
    },
    /// SP サーバーを停止
    Stop,
    /// SP サーバーの状態 + Stand 一覧を表示
    Status,
    /// SP サーバーを再起動
    Restart,
}

/// vp sp コマンドを実行
pub fn execute(cmd: SpCommands, config: &Config) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_dir = cwd.to_string_lossy().to_string();

    match cmd {
        SpCommands::Start { port, debug } => sp_start(&project_dir, port, debug, config),
        SpCommands::Stop => sp_stop(&project_dir, config),
        SpCommands::Status => sp_status(&project_dir, config),
        SpCommands::Restart => sp_restart(&project_dir, config),
    }
}

/// SP サーバーを起動（tmux/ccwire なし）
fn sp_start(
    project_dir: &str,
    explicit_port: Option<u16>,
    debug: Option<DebugModeArg>,
    config: &Config,
) -> Result<()> {
    let port = explicit_port.unwrap_or_else(|| resolve_port(project_dir, config));
    let debug_mode = debug.map(DebugMode::from).unwrap_or_default();

    let cap_config = crate::process::CapabilityConfig {
        project_dir: project_dir.to_string(),
        midi_config: None,
    };

    match crate::commands::start::ensure_sp_running(port, debug_mode, cap_config) {
        Ok(()) => {
            println!("✅ SP サーバー起動済み (port={})", port);
            Ok(())
        }
        Err(e) => {
            anyhow::bail!("SP サーバー起動失敗: {}", e);
        }
    }
}

/// SP サーバーを停止
fn sp_stop(project_dir: &str, _config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));

    if let Some(running) = crate::discovery::find_by_project_blocking(&normalized) {
        // /api/shutdown エンドポイントを叩く
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        let url = format!("http://[::1]:{}/api/shutdown", running.port);
        match client.post(&url).send() {
            Ok(resp) if resp.status().is_success() => {
                println!("✅ SP サーバーを停止しました (port={})", running.port);
            }
            _ => {
                // shutdown エンドポイントがなければプロセスを kill
                eprintln!("⚠️  shutdown API が応答しません。プロセスを終了します。");
                let _ = kill_process_on_port(running.port);
                println!("✅ SP サーバーを停止しました (port={})", running.port);
            }
        }
    } else {
        println!("ℹ️  SP サーバーは稼働していません");
    }

    Ok(())
}

/// SP サーバーの状態 + Stand 一覧を表示
fn sp_status(project_dir: &str, config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));
    let project_name = crate::resolve::project_name_from_path(&normalized, config);

    println!("⭐ SP ステータス: {}", project_name);
    println!();

    if let Some(running) = crate::discovery::find_by_project_blocking(&normalized) {
        println!("   サーバー: ✅ running (port={})", running.port);

        // /api/health から詳細情報を取得
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        let url = format!("http://[::1]:{}/api/health", running.port);

        if let Ok(resp) = client.get(&url).send() {
            if let Ok(json) = resp.json::<serde_json::Value>() {
                // 起動時刻
                if let Some(started) = json.get("started_at").and_then(|s| s.as_str()) {
                    println!("   起動時刻: {}", started);
                }

                // Stand ステータス
                if let Some(stands) = json.get("stands").and_then(|s| s.as_object()) {
                    println!();
                    println!("   Stand 一覧:");
                    for (name, info) in stands {
                        let status = info
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown");
                        let icon = match name.as_str() {
                            "heavens_door" => "📖 Heaven's Door (HD)",
                            "paisley_park" => "🧭 Paisley Park (PP)",
                            "gold_experience" => "🌿 Gold Experience (GE)",
                            "hermit_purple" => "🍇 Hermit Purple (HP)",
                            _ => name.as_str(),
                        };
                        let status_icon = match status {
                            "active" => "✅",
                            "disabled" => "⏸️",
                            _ => "❓",
                        };
                        println!("     {} {} {}", status_icon, icon, status);
                    }
                }
            }
        }
    } else {
        println!("   サーバー: ❌ not running");
    }

    // HD セッション情報も参考表示
    let sessions = crate::tmux::list_vp_sessions();
    let prefix = project_name.replace('.', "-");
    // 他プロジェクト名一覧（prefix フィルタ用）
    let other_prefixes: Vec<String> = config
        .projects
        .iter()
        .map(|p| {
            let name = crate::resolve::project_name_from_path(&p.path, config);
            name.replace('.', "-")
        })
        .filter(|name| *name != prefix)
        .collect();
    let hd_sessions: Vec<_> = sessions
        .iter()
        .filter(|s| crate::commands::hd_cmd::is_own_session(s, &prefix, &other_prefixes))
        .collect();
    if !hd_sessions.is_empty() {
        println!();
        println!("   HD セッション:");
        for s in &hd_sessions {
            let registered = crate::ccwire::is_registered(s);
            let ccwire_icon = if registered { "✅" } else { "❌" };
            println!("     {} (ccwire: {})", s, ccwire_icon);
        }
    }

    Ok(())
}

/// SP サーバーを再起動
///
/// 停止前にポートを保存し、再起動時に同じポートを使用する。
/// ポート開放をポーリングで待つ（最大5秒）。
fn sp_restart(project_dir: &str, config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));

    // 停止前にポートを保存
    let saved_port = crate::discovery::find_by_project_blocking(&normalized).map(|r| r.port);

    println!("🔄 SP サーバーを再起動します...");
    sp_stop(project_dir, config)?;

    // ポート開放をポーリング（最大5秒）
    if let Some(port) = saved_port {
        for _ in 0..50 {
            if !crate::commands::start::is_server_responding(port) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // 保存したポートで再起動
        sp_start(project_dir, Some(port), None, config)
    } else {
        sp_start(project_dir, None, None, config)
    }
}

/// プロジェクトディレクトリからポートを解決
fn resolve_port(project_dir: &str, config: &Config) -> u16 {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));
    // 既に稼働中のプロセスがあればそのポートを返す
    if let Some(running) = crate::discovery::find_by_project_blocking(&normalized) {
        return running.port;
    }
    if let Some(idx) = config.find_project_index(&normalized) {
        crate::resolve::port_for_configured(idx, config).unwrap_or(33000 + idx as u16)
    } else {
        crate::resolve::find_available_port().unwrap_or(33005)
    }
}

/// ポートを使用しているプロセスを終了
fn kill_process_on_port(port: u16) -> Result<()> {
    let output = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{}", port)])
        .output()?;

    if output.status.success() {
        let pids = String::from_utf8_lossy(&output.stdout);
        for pid in pids.lines() {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }

    Ok(())
}
