//! Daemon management - auto-start vantaged if not running

use anyhow::Result;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const DAEMON_PORT: u16 = 33000;
const HEALTH_URL: &str = "http://localhost:33000/api/health";

/// Check if daemon is running by hitting health endpoint
pub async fn is_running() -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .ok();

    if let Some(client) = client {
        client.get(HEALTH_URL).send().await.is_ok()
    } else {
        false
    }
}

/// Start the daemon process if not already running
/// Returns the child process handle if we started it
pub async fn ensure_running() -> Result<Option<Child>> {
    if is_running().await {
        tracing::info!("vantaged already running");
        return Ok(None);
    }

    tracing::info!("Starting vantaged...");

    // Try to find vantaged binary
    let binary = find_vantaged_binary()?;

    let child = Command::new(&binary)
        .arg("start")
        .arg("--no-browser") // TUI will manage browser opening
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    // Wait a bit for daemon to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify it started
    if is_running().await {
        tracing::info!("vantaged started successfully");
        Ok(Some(child))
    } else {
        tracing::warn!("vantaged may not have started properly");
        Ok(Some(child))
    }
}

/// Find vantaged binary path
fn find_vantaged_binary() -> Result<String> {
    // 1. Check if in PATH
    if Command::new("which")
        .arg("vantaged")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok("vantaged".to_string());
    }

    // 2. Check cargo target directory (development)
    let dev_path = std::env::current_dir()?
        .join("target/debug/vantaged");
    if dev_path.exists() {
        return Ok(dev_path.to_string_lossy().to_string());
    }

    // 3. Check release build
    let release_path = std::env::current_dir()?
        .join("target/release/vantaged");
    if release_path.exists() {
        return Ok(release_path.to_string_lossy().to_string());
    }

    // 4. Fallback - assume it's in PATH
    Ok("vantaged".to_string())
}

/// Send content to daemon for display
pub async fn show_content(pane_id: &str, content: DaemonContent, append: bool) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://localhost:{}/api/show", DAEMON_PORT);

    let body = serde_json::json!({
        "type": "show",
        "pane_id": pane_id,
        "content": content,
        "append": append
    });

    client
        .post(&url)
        .json(&body)
        .send()
        .await?;

    Ok(())
}

/// Content types for daemon display
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonContent {
    Log(String),
    Markdown(String),
    Html(String),
}
