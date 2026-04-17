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
    /// SP サーバーを起動（フォアグラウンドでブロック実行）
    Start {
        /// 待ち受けポート番号
        #[arg(short, long)]
        port: Option<u16>,
        /// デバッグモード
        #[arg(long, short = 'd', value_enum)]
        debug: Option<DebugModeArg>,
        /// プロジェクトディレクトリ（省略時は cwd）
        #[arg(long, short = 'C')]
        project_dir: Option<String>,
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
    match cmd {
        SpCommands::Start {
            port,
            debug,
            project_dir,
        } => {
            let dir = project_dir.unwrap_or_else(|| {
                std::env::current_dir()
                    .expect("cwd 取得失敗")
                    .to_string_lossy()
                    .to_string()
            });
            sp_start(&dir, port, debug, config)
        }
        SpCommands::Stop => {
            let cwd = std::env::current_dir()?;
            sp_stop(&cwd.to_string_lossy(), config)
        }
        SpCommands::Status => {
            let cwd = std::env::current_dir()?;
            sp_status(&cwd.to_string_lossy(), config)
        }
        SpCommands::Restart => {
            let cwd = std::env::current_dir()?;
            sp_restart(&cwd.to_string_lossy(), config)
        }
    }
}

/// SP サーバーを起動（フォアグラウンド、ブロック実行）
///
/// 既に起動中なら何もしない。未起動ならサーバーを起動してブロックする。
/// バックグラウンド実行が必要な場合は呼び出し元が detached subprocess として spawn する。
fn sp_start(
    project_dir: &str,
    explicit_port: Option<u16>,
    debug: Option<DebugModeArg>,
    config: &Config,
) -> Result<()> {
    let port = explicit_port.unwrap_or_else(|| resolve_port(project_dir, config));
    let debug_mode = debug.map(DebugMode::from).unwrap_or_default();

    // 既に起動中ならスキップ
    if crate::commands::start::is_server_responding(port) {
        println!("✅ SP サーバーは既に起動済み (port={})", port);
        return Ok(());
    }

    // TheWorld 自動起動
    if let Err(e) = crate::daemon::process::ensure_daemon_running(crate::cli::WORLD_PORT) {
        tracing::warn!("TheWorld 自動起動失敗（SP は続行）: {}", e);
    }

    println!("⭐ SP サーバー起動 (port={})...", port);

    let cap_config = crate::process::CapabilityConfig {
        project_dir: project_dir.to_string(),
        midi_config: None,
        whitesnake: None, // server.rs 側でポート別に注入
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async { crate::process::run(port, false, debug_mode, cap_config).await })
}

/// SP サーバーを停止
fn sp_stop(project_dir: &str, _config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));

    if let Some(running) = crate::discovery::find_by_project_blocking(&normalized) {
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

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        let url = format!("http://[::1]:{}/api/health", running.port);

        if let Ok(resp) = client.get(&url).send()
            && let Ok(json) = resp.json::<serde_json::Value>()
        {
            if let Some(started) = json.get("started_at").and_then(|s| s.as_str()) {
                println!("   起動時刻: {}", started);
            }

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
    } else {
        println!("   サーバー: ❌ not running");
    }

    // HD セッション情報も参考表示
    let sessions = crate::tmux::list_vp_sessions();
    let prefix = project_name.replace('.', "-");
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
fn sp_restart(project_dir: &str, config: &Config) -> Result<()> {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));

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
        sp_start(project_dir, Some(port), None, config)
    } else {
        sp_start(project_dir, None, None, config)
    }
}

/// プロジェクトディレクトリからポートを解決
fn resolve_port(project_dir: &str, config: &Config) -> u16 {
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));
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
