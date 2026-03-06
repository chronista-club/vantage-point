//! CLIヘルパー関数
//!
//! インスタンス管理、デバッグ設定、ユーティリティ関数を提供する。

use anyhow::Result;
use clap::ValueEnum;

use crate::protocol::DebugMode;

/// Health response from Process
#[derive(serde::Deserialize)]
pub(crate) struct HealthResponse {
    pub status: String,
    pub version: String,
    pub pid: u32,
    #[serde(default)]
    pub project_dir: Option<String>,
}

/// Check if Process is running on the specified port
pub(crate) async fn check_status(port: u16) -> Result<()> {
    let url = format!("http://localhost:{}/api/health", port);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    match client.get(&url).send().await {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<HealthResponse>().await {
                    Ok(health) => {
                        println!("✓ vp is running on port {}", port);
                        println!("  Version: {}", health.version);
                        println!("  PID: {}", health.pid);
                        if let Some(ref dir) = health.project_dir {
                            println!("  Project: {}", dir);
                        }
                        println!("  Status: {}", health.status);
                    }
                    Err(_) => {
                        // Old version returning plain text
                        println!("✓ vp is running on port {}", port);
                    }
                }
            } else {
                println!("✗ vp returned error: {}", response.status());
            }
        }
        Err(e) => {
            if e.is_connect() {
                println!("✗ vp is not running on port {}", port);
            } else if e.is_timeout() {
                println!("✗ vp is not responding (timeout)");
            } else {
                println!("✗ Failed to connect: {}", e);
            }
        }
    }

    Ok(())
}

/// Stop the Process running on the specified port
pub(crate) async fn stop_process(port: u16) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()?;

    // First, get the PID via health endpoint
    let health_url = format!("http://localhost:{}/api/health", port);
    let pid = match client.get(&health_url).send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<HealthResponse>().await {
                Ok(health) => Some(health.pid),
                Err(_) => None,
            }
        }
        Ok(_) => None,
        Err(e) => {
            if e.is_connect() {
                println!("✗ vp is not running on port {}", port);
                return Ok(());
            }
            None
        }
    };

    let Some(pid) = pid else {
        println!("✗ Could not get Process PID");
        return Ok(());
    };

    println!("Stopping vp (PID: {})...", pid);

    // Request graceful shutdown via API
    let shutdown_url = format!("http://localhost:{}/api/shutdown", port);
    let _ = client.post(&shutdown_url).send().await;

    // Wait up to 10 seconds for graceful shutdown
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(10);

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Check if process is still running
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
pub(crate) fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn is_process_running(_pid: u32) -> bool {
    false
}

/// Force kill a process
#[cfg(unix)]
pub(crate) fn force_kill(pid: u32) {
    use std::process::Command;
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}

#[cfg(not(unix))]
pub(crate) fn force_kill(_pid: u32) {}

/// Default port range to scan for instances
pub(crate) const PORT_RANGE_START: u16 = 33000;
pub(crate) const PORT_RANGE_END: u16 = 33010;

/// Running instance info
#[derive(Clone)]
pub(crate) struct Instance {
    pub port: u16,
    pub pid: u32,
    pub version: String,
    pub project_dir: Option<String>,
}

/// Scan for running vp instances
pub(crate) async fn scan_instances() -> Vec<Instance> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap();

    let mut instances = Vec::new();

    for port in PORT_RANGE_START..=PORT_RANGE_END {
        let url = format!("http://localhost:{}/api/health", port);
        if let Ok(response) = client.get(&url).send().await
            && response.status().is_success()
            && let Ok(health) = response.json::<HealthResponse>().await
        {
            instances.push(Instance {
                port,
                pid: health.pid,
                version: health.version,
                project_dir: health.project_dir,
            });
        }
    }

    instances
}

/// Find the first available port in the range
pub(crate) async fn find_available_port() -> Option<u16> {
    let used_ports: std::collections::HashSet<u16> =
        scan_instances().await.into_iter().map(|i| i.port).collect();

    for port in PORT_RANGE_START..=PORT_RANGE_END {
        if !used_ports.contains(&port) {
            return Some(port);
        }
    }
    None
}

/// 稼働中インスタンスをプロジェクト名ベースで一覧表示
pub(crate) fn list_instances(config: &crate::config::Config) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let instances = scan_instances().await;

        if instances.is_empty() {
            println!("No running vp instances found.");
            return Ok(());
        }

        // cwd を取得して一致チェック
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| std::fs::canonicalize(&p).ok())
            .map(|p| p.display().to_string());

        println!();
        println!("  {:<18} {:<7} {:<7} STATUS", "PROJECT", "PORT", "PID");
        println!(
            "  {:<18} {:<7} {:<7} \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            "\u{2500}\u{2500}\u{2500}\u{2500}",
            "\u{2500}\u{2500}\u{2500}"
        );

        for inst in &instances {
            let name = crate::resolve::project_name_from_path(
                inst.project_dir.as_deref().unwrap_or("-"),
                config,
            );

            // cwd 一致チェック（プロジェクトディレクトリまたはそのサブディレクトリ）
            let is_cwd = if let (Some(cwd_str), Some(proj_dir)) = (&cwd, &inst.project_dir) {
                let canonical_proj = std::fs::canonicalize(proj_dir)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| proj_dir.clone());
                cwd_str == &canonical_proj || cwd_str.starts_with(&format!("{}/", canonical_proj))
            } else {
                false
            };

            let marker = if is_cwd { "  \u{2190} cwd" } else { "" };

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
pub(crate) fn open_by_target(target: Option<&str>, config: &crate::config::Config) -> Result<()> {
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
                "\u{2717} '{}' is not running. Use `vp start {}` first.",
                name, name
            );
        }
        ResolvedTarget::Cwd { .. } => {
            println!("\u{2717} No running Process found for current directory.");
            println!("  Use `vp start` to start a new Process.");
        }
    }

    Ok(())
}

/// ターゲット指定で Process を停止
pub(crate) fn stop_by_target(target: Option<&str>, config: &crate::config::Config) -> Result<()> {
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
pub(crate) fn parse_debug_env() -> Option<DebugMode> {
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
pub(crate) fn init_tracing(debug_mode: DebugMode) {
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

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();
}

/// snake_case を PascalCase に変換
pub(crate) fn to_pascal_case(s: &str) -> String {
    s.split(['_', '-'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// vpバイナリのパスを取得
pub(crate) fn which_vp() -> Option<std::path::PathBuf> {
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
pub(crate) fn find_vantage_point_app() -> Option<std::path::PathBuf> {
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
