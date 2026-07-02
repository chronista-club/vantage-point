//! CLIヘルパー関数
//!
//! インスタンス管理、デバッグ設定、ユーティリティ関数を提供する。

use anyhow::Result;
use clap::ValueEnum;

use crate::protocol::DebugMode;

/// World daemon (:32000) の `/api/health` レスポンス parser。
///
/// L0 finale: SP は HTTP listener を撤去したが、 **World daemon は HTTP を保持**するため (daemon/process.rs
/// が World 自身の health を読む)、 この struct は残す。 旧 SP /api/health 用の用途 (check_status /
/// scan_instances) は撤去済。
#[derive(serde::Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub pid: u32,
    #[serde(default)]
    pub project_dir: Option<String>,
}

/// 指定 port の SP を停止する (registry PID + World process-proxy graceful shutdown + force_kill)。
///
/// SP-portless: PID は World registry (`discovery::list`) から引き、 graceful は World :32000
/// process-proxy "shutdown" (`world_process_request`、 reverse-routing で SP control channel に届く)
/// で送る。 timeout で force_kill にフォールバック。 `restart-all` / `stop_by_target` が共有。
pub async fn stop_process(port: u16) -> Result<()> {
    let info = crate::discovery::list()
        .await
        .into_iter()
        .find(|p| p.port == port);
    let Some(info) = info else {
        println!("✗ port {} に稼働 SP が registry に居ません", port);
        return Ok(());
    };
    let pid = info.pid;

    println!("Stopping vp (PID: {})...", pid);

    // SP-portless: graceful shutdown を World :32000 process-proxy "shutdown" 経由で送る
    // (best-effort)。SP は QUIC listen を持たないため、World が reverse-routing (control
    // channel) で SP に "shutdown" を届ける。無応答でも下の force_kill fallback で確実に停止する。
    let _ = crate::commands::process_client::world_process_request(
        world_port(),
        &info.project_dir,
        "shutdown",
        serde_json::json!({}),
    )
    .await;

    // graceful 完了を待ち、 timeout で force_kill にフォールバック
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(10);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if !is_process_running(pid) {
            println!("✓ vp stopped gracefully");
            return Ok(());
        }
        if start.elapsed() > timeout {
            println!("⚠ Graceful shutdown timed out, forcing kill...");
            force_kill(pid);
            println!("✓ vp force killed");
            return Ok(());
        }
    }
}

/// Check if a process is still running
#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn is_process_running(_pid: u32) -> bool {
    false
}

/// Force kill a process
#[cfg(unix)]
pub fn force_kill(pid: u32) {
    use std::process::Command;
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}

#[cfg(not(unix))]
pub fn force_kill(_pid: u32) {}

/// Default port range to scan for instances.
///
/// VP-133 (2026-05-06) で 33010 → 33024 に拡張 (= 25 ports、 上限 max)。 旧 11 ports
/// では同時開発 project が増えると枯渇 + start_process の port allocator が「searching for
/// available port」 で別 port 選択 → multi-port spawn を誘発するリスクが上昇していた。
/// 25 まで拡張で実用 project 数 (~10-15) に対し十分な margin を確保、 reconcile dedup と
/// 組合せて安定運用へ。
pub const PORT_RANGE_START: u16 = 33000;
pub const PORT_RANGE_END: u16 = 33024;

/// TheWorld（Daemon 統合）のデフォルトポート。
///
/// VP_PROFILE 分離 (dev/brew 混在根治): brew=32000 / dev=32100。 SP portless なので実 listener は
/// world 単一 → この 1 本を profile でずらせば daemon bind / app connect / SP→world connect が
/// 芋づるで追随する。 定義は `vp_paths::default_world_port()` (全 crate 共有の SSOT)。
pub fn world_port() -> u16 {
    vp_paths::default_world_port()
}

/// 稼働中インスタンスをプロジェクト名ベースで一覧表示する。
///
/// L0 finale: SP は HTTP listener を持たないため真実源は World registry (`discovery::list` =
/// World :32000 が QUIC 自己登録から維持する稼働 SP 一覧)。 旧 TCP port-scan (`scan_instances`) は撤去。
pub fn list_instances(config: &crate::config::Config) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let instances = crate::discovery::list().await;
        if instances.is_empty() {
            println!("No running vp instances found.");
            return Ok(());
        }

        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| std::fs::canonicalize(&p).ok())
            .map(|p| p.display().to_string());

        println!();
        println!("  {:<18} {:<7} {:<7} STATUS", "PROJECT", "PORT", "PID");
        println!("  {:<18} {:<7} {:<7} ──────", "───────", "────", "───");

        for inst in &instances {
            let name = crate::resolve::project_name_from_path(&inst.project_dir, config);
            let is_cwd = if let Some(cwd_str) = &cwd {
                let canonical_proj = std::fs::canonicalize(&inst.project_dir)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| inst.project_dir.clone());
                cwd_str == &canonical_proj || cwd_str.starts_with(&format!("{}/", canonical_proj))
            } else {
                false
            };
            let marker = if is_cwd { "  ← cwd" } else { "" };
            println!(
                "  {:<18} {:<7} {:<7} running{}",
                name, inst.port, inst.pid, marker
            );
        }
        println!();
        println!("Use: vp open <project-name>");
        Ok(())
    })
}

/// ターゲット指定で WebUI を開く
pub fn open_by_target(target: Option<&str>, config: &crate::config::Config) -> Result<()> {
    use crate::resolve::{self, ResolvedTarget};

    let resolved = resolve::resolve_target(target, config)?;

    match resolved {
        ResolvedTarget::Running { port, name, .. } => {
            let url = format!("http://localhost:{}", port);
            println!("Opening {} ({})...", name, url);

            if let Err(e) = open::that(&url) {
                println!("\u{2717} Failed to open browser: {}", e);
            } else {
                println!("\u{2713} Opened in browser");
            }
        }
        ResolvedTarget::Configured { name, .. } => {
            println!(
                "\u{2717} '{}' is not running. Use `vp sp start` first.",
                name
            );
        }
        ResolvedTarget::Cwd { .. } => {
            println!("\u{2717} No running Process found for current directory.");
            println!("  Use `vp sp start` to start a new SP server.");
        }
    }

    Ok(())
}

/// ターゲット指定で Process を停止
pub fn stop_by_target(target: Option<&str>, config: &crate::config::Config) -> Result<()> {
    use crate::resolve::{self, ResolvedTarget};

    let resolved = resolve::resolve_target(target, config)?;

    match resolved {
        ResolvedTarget::Running { port, name, .. } => {
            println!("Stopping: {} (port {})", name, port);
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(stop_process(port))
        }
        ResolvedTarget::Configured { name, .. } => {
            println!("\u{2717} '{}' is not running.", name);
            Ok(())
        }
        ResolvedTarget::Cwd { .. } => {
            println!("\u{2717} No running Process found for current directory.");
            Ok(())
        }
    }
}

/// CLIデバッグモード
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum DebugModeArg {
    /// デバッグ情報なし
    #[default]
    None,
    /// 簡易デバッグ（セッションID、タイミング）
    Simple,
    /// 詳細デバッグ（JSON全体、全イベント）
    Detail,
}

impl From<DebugModeArg> for DebugMode {
    fn from(arg: DebugModeArg) -> Self {
        match arg {
            DebugModeArg::None => DebugMode::None,
            DebugModeArg::Simple => DebugMode::Simple,
            DebugModeArg::Detail => DebugMode::Detail,
        }
    }
}

/// Parse debug mode from environment variable
pub fn parse_debug_env() -> Option<DebugMode> {
    std::env::var("VANTAGE_DEBUG")
        .ok()
        .and_then(|v| match v.to_lowercase().as_str() {
            "none" | "off" | "0" | "false" => Some(DebugMode::None),
            "simple" | "1" | "true" => Some(DebugMode::Simple),
            "detail" | "detailed" | "2" | "verbose" => Some(DebugMode::Detail),
            _ => None,
        })
}

/// Initialize tracing with VP_LOG support
/// VP_LOG環境変数またはDebugModeに基づいてログレベルを設定
/// - VP_LOG=debug|info|warn|error が優先
/// - 未設定の場合、debug_modeに基づいて設定:
///   - None -> warn
///   - Simple -> info
///   - Detail -> debug
///
/// `tui_mode` が true の場合、ログ出力を stderr ではなくファイルにリダイレクト。
/// TUI (ratatui) の alternate screen にサーバーログが漏れるのを防ぐ。
pub fn init_tracing(debug_mode: DebugMode, tui_mode: bool) {
    // daemon ログローテーション設定。 vp-app 側の `log_init::LOG_MAX_BYTES` /
    // `LOG_KEEP_FILES` と同値に保つこと (2 crate に依存関係が無いため定数を物理共有できず、
    // 各 crate に複製している。 恒久的には vp-paths へ寄せる候補 = Observability Phase B)。
    const DAEMON_LOG_MAX_BYTES: usize = 10 * 1024 * 1024; // 10 MB
    const DAEMON_LOG_KEEP_FILES: usize = 5;
    // VP_LOGが設定されていない場合、debug_modeに基づいてRUST_LOGを設定
    // SAFETY: main()開始直後、他スレッド起動前に呼ばれるため安全
    if std::env::var("VP_LOG").is_err() && std::env::var("RUST_LOG").is_err() {
        let log_level = match debug_mode {
            DebugMode::None => "warn",
            DebugMode::Simple => "info",
            DebugMode::Detail => "debug",
        };
        unsafe {
            std::env::set_var("RUST_LOG", format!("vantage_point={}", log_level));
        }
    } else if let Ok(vp_log) = std::env::var("VP_LOG") {
        // VP_LOG -> RUST_LOG に変換
        unsafe {
            std::env::set_var("RUST_LOG", format!("vantage_point={}", vp_log));
        }
    }

    // d (log 整理 minimum, mem_1CaSiJkD9HATDY2srrv6D4 Phase B-2 step):
    // RUST_LOG 未指定時の default を **絞り込み** verbose を削減。
    // vantage_point=info を残しつつ、 dependency crate の chatty な debug log を抑制。
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "vantage_point=info,vp_app=info,\
             hyper=warn,hyper_util=warn,reqwest=warn,h2=warn,\
             tokio_tungstenite=warn,tungstenite=warn,\
             quinn=warn,quinn_proto=warn,quinn_udp=warn,\
             rustls=warn,rustls_post_quantum=warn",
        )
    });

    // VP-101 follow-up (Windows daemon support):
    // 環境変数 `VP_DAEMON_LOG_FILE` が指定されていれば、そのパスに直接書き込む。
    // Win-native vp-app から daemon を spawn する際に Logs/daemon.kdl.log 等を渡す想定。
    // GUI subsystem では stderr が NUL 化されるので file writer 必須。
    if let Ok(path_str) = std::env::var("VP_DAEMON_LOG_FILE")
        && !path_str.is_empty()
    {
        let path = std::path::PathBuf::from(&path_str);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // ログローテーション (file-rotate): 容量ベースで切替、 件数ベースで GC して daemon log の膨張を防ぐ。
        // 現行ファイルは `VP_DAEMON_LOG_FILE` が指す bare 名のまま (日付名にしない) なので、
        // tail -F / 既存 reader を壊さない。 rotate 世代は `daemon.kdl.log.1`.. へ逃がす。
        // Mutex で包むと tracing-subscriber が MakeWriter として扱える (WorkerGuard 不要)。
        let appender = std::sync::Mutex::new(file_rotate::FileRotate::new(
            &path,
            file_rotate::suffix::AppendCount::new(DAEMON_LOG_KEEP_FILES),
            file_rotate::ContentLimit::Bytes(DAEMON_LOG_MAX_BYTES),
            file_rotate::compression::Compression::None,
            None,
        ));
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .with_ansi(false)
            .with_writer(appender)
            .init();
        return;
    }

    if tui_mode {
        // TUI モード: ファイルに出力（stderr 汚染を防止）
        if let Some(path) = crate::trace_log::log_file_path() {
            let tracing_path = path.with_file_name("tracing.log");
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&tracing_path)
            {
                Ok(file) => {
                    tracing_subscriber::fmt()
                        .with_env_filter(env_filter)
                        .with_target(false)
                        .with_ansi(false)
                        .with_writer(file)
                        .init();
                    return;
                }
                Err(e) => {
                    eprintln!("[vp] tracing ログファイル作成失敗: {e}, stderr にフォールバック");
                }
            }
        }
    }

    // 通常モード: stderr に出力
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .init();
}

/// vpバイナリのパスを取得
pub fn which_vp() -> Option<std::path::PathBuf> {
    // 1. ~/.cargo/bin/vp
    if let Some(home) = dirs::home_dir() {
        let cargo_path = home.join(".cargo/bin/vp");
        if cargo_path.exists() {
            return Some(cargo_path);
        }
    }

    // 2. /usr/local/bin/vp
    let usr_local = std::path::PathBuf::from("/usr/local/bin/vp");
    if usr_local.exists() {
        return Some(usr_local);
    }

    // 3. PATH経由
    if let Ok(output) = std::process::Command::new("which").arg("vp").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }

    None
}

/// VantagePoint.app のパスを検索
pub fn find_vantage_point_app() -> Option<std::path::PathBuf> {
    // 1. /Applications
    let system_app = std::path::PathBuf::from("/Applications/VantagePoint.app");
    if system_app.exists() {
        return Some(system_app);
    }

    // 2. ~/Applications
    if let Some(home) = dirs::home_dir() {
        let user_app = home.join("Applications/VantagePoint.app");
        if user_app.exists() {
            return Some(user_app);
        }
    }

    // 3. Xcode DerivedData（Xcodeビルド優先）
    if let Some(home) = dirs::home_dir() {
        let derived_data = home.join("Library/Developer/Xcode/DerivedData");
        if let Ok(entries) = derived_data.read_dir() {
            for entry in entries.filter_map(|e| e.ok()) {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("VantagePoint-")
                {
                    let app_path = entry.path().join("Build/Products/Debug/VantagePoint.app");
                    if app_path.exists() {
                        return Some(app_path);
                    }
                }
            }
        }
    }

    // 4. 開発リポジトリ（~/repos/vantage-point-mac/）
    if let Some(home) = dirs::home_dir() {
        let dev_repo_app = home.join("repos/vantage-point-mac/VantagePoint/VantagePoint.app");
        if dev_repo_app.exists() {
            return Some(dev_repo_app);
        }
    }

    None
}
