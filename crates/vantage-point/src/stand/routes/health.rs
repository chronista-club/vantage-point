//! ヘルスチェック・基本ルートハンドラー

use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    response::{Html, IntoResponse},
};

use super::super::state::AppState;
use crate::protocol::StandMessage;

/// Open browser (macOS)
pub fn open_browser(url: &str) -> anyhow::Result<()> {
    std::process::Command::new("open").arg(url).spawn()?;
    Ok(())
}

/// Index page handler
pub async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../../../../../web/index.html"))
}

/// Health check response
#[derive(serde::Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub pid: u32,
    pub project_dir: String,
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        project_dir: state.project_dir.clone(),
    })
}

/// POST /api/show - Show content in browser
pub async fn show_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/toggle-pane - Toggle side panel visibility
pub async fn toggle_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/split-pane - Split a pane
pub async fn split_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/close-pane - Close a pane
pub async fn close_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/canvas/open - Canvasウィンドウを起動
pub async fn canvas_open_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut pid_guard = state.canvas_pid.lock().await;

    // 既に起動中なら何もしない
    if let Some(pid) = *pid_guard {
        // プロセスがまだ生きてるか確認
        let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
        if alive {
            return Json(serde_json::json!({"status": "already_open", "pid": pid}));
        }
    }

    // vp webview -p <port> で起動
    match std::process::Command::new("vp")
        .args(["webview", "-p", &state.port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            *pid_guard = Some(pid);
            tracing::info!("Canvas window opened (pid={})", pid);
            Json(serde_json::json!({"status": "opened", "pid": pid}))
        }
        Err(e) => {
            tracing::error!("Failed to open canvas: {}", e);
            Json(serde_json::json!({"status": "error", "message": e.to_string()}))
        }
    }
}

/// POST /api/canvas/close - Canvasウィンドウを終了
pub async fn canvas_close_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut pid_guard = state.canvas_pid.lock().await;

    if let Some(pid) = pid_guard.take() {
        // SIGTERMで終了
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        tracing::info!("Canvas window closed (pid={})", pid);
        Json(serde_json::json!({"status": "closed", "pid": pid}))
    } else {
        Json(serde_json::json!({"status": "not_open"}))
    }
}

/// POST /api/shutdown - Graceful shutdown
pub async fn shutdown_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    tracing::info!("Shutdown requested via API");
    state.shutdown_token.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}
