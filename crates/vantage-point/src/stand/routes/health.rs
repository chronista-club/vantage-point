//! ヘルスチェック・基本ルートハンドラー

use std::sync::Arc;

use serde::Deserialize;

use axum::{
    Json,
    extract::State,
    http::header,
    response::{Html, IntoResponse},
};

use super::super::state::AppState;
use crate::protocol::StandMessage;

/// Open browser (macOS)
pub fn open_browser(url: &str) -> anyhow::Result<()> {
    std::process::Command::new("open").arg(url).spawn()?;
    Ok(())
}

/// Index page handler（キャッシュ無効化ヘッダー付き）
pub async fn index_handler() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")],
        Html(include_str!("../../../../../web/index.html")),
    )
}

/// Canvas page handler（キャッシュ無効化ヘッダー付き）
pub async fn canvas_handler() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")],
        Html(include_str!("../../../../../web/canvas.html")),
    )
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

    // vp canvas internal --port <port> で起動
    match std::process::Command::new("vp")
        .args(["canvas", "internal", "--port", &state.port.to_string()])
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

/// POST /api/watch-file - ファイル監視を開始
pub async fn watch_file_handler(
    State(state): State<Arc<AppState>>,
    Json(config): Json<crate::file_watcher::WatchConfig>,
) -> impl IntoResponse {
    let pane_id = config.pane_id.clone();
    match state
        .file_watchers
        .lock()
        .await
        .start_watch(config, state.hub.clone())
    {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "ok", "pane_id": pane_id})),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"status": "error", "error": e})),
        ),
    }
}

/// UnwatchFile リクエストのペイロード
#[derive(Debug, serde::Deserialize)]
pub struct UnwatchFileBody {
    pub pane_id: String,
}

/// POST /api/unwatch-file - ファイル監視を停止
pub async fn unwatch_file_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UnwatchFileBody>,
) -> impl IntoResponse {
    state.file_watchers.lock().await.stop_watch(&body.pane_id);
    Json(serde_json::json!({"status": "ok", "pane_id": body.pane_id}))
}

/// Canvas キャプチャリクエストのパラメータ
#[derive(Debug, serde::Deserialize)]
pub struct CaptureParams {
    /// 保存先パス（省略時: /tmp/vp-canvas-{timestamp}.png）
    pub path: Option<String>,
    /// 特定ペインのみキャプチャ
    pub pane_id: Option<String>,
}

/// POST /api/canvas/capture - Canvas のスクリーンショットを取得
pub async fn canvas_capture_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<CaptureParams>,
) -> impl IntoResponse {
    // 1. Canvas プロセスの生存確認
    let mut pid_guard = state.canvas_pid.lock().await;
    let canvas_alive = match *pid_guard {
        Some(pid) => unsafe { libc::kill(pid as i32, 0) == 0 },
        None => false,
    };

    // Canvas 未起動なら自動起動 + WebSocket 接続待ち
    if !canvas_alive {
        match std::process::Command::new("vp")
            .args(["canvas", "internal", "--port", &state.port.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => {
                let pid = child.id();
                *pid_guard = Some(pid);
                tracing::info!("Canvas auto-started for capture (pid={})", pid);
            }
            Err(e) => {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "status": "error",
                        "message": format!("Canvas 起動失敗: {}", e)
                    })),
                );
            }
        }
        // Canvas の WebSocket 接続を待つ（最大 5 秒）
        drop(pid_guard);
        let mut connected = false;
        for _ in 0..50 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            if state.hub.client_count().await > 0 {
                connected = true;
                break;
            }
        }
        if !connected {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "Canvas 起動後の WebSocket 接続がタイムアウト"
                })),
            );
        }
    } else {
        drop(pid_guard);
    }

    // 2. request_id 生成、oneshot channel 作成
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let mut waiters = state.screenshot_waiters.lock().await;
        waiters.insert(request_id.clone(), tx);
    }

    // 3. ScreenshotRequest を Canvas に broadcast
    state
        .hub
        .broadcast(crate::protocol::StandMessage::ScreenshotRequest {
            request_id: request_id.clone(),
            pane_id: params.pane_id,
        });

    // 4. タイムアウト付きで応答を待つ
    let result = tokio::time::timeout(tokio::time::Duration::from_secs(10), rx).await;

    match result {
        Ok(Ok(screenshot)) => {
            // width=0 はキャプチャ失敗を示す（JSからのエラー応答、data にエラーメッセージ）
            if screenshot.width == 0 {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "status": "error",
                        "message": format!("Canvas側でスクリーンショット取得に失敗: {}", screenshot.data)
                    })),
                );
            }

            // 5. base64 デコード → ファイル書き込み
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;

            let bytes = match engine.decode(&screenshot.data) {
                Ok(b) => b,
                Err(e) => {
                    return (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "status": "error",
                            "message": format!("base64 デコード失敗: {}", e)
                        })),
                    );
                }
            };

            let save_path = params.path.unwrap_or_else(|| {
                let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                format!("/tmp/vp-canvas-{}.png", ts)
            });

            if let Err(e) = std::fs::write(&save_path, &bytes) {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "status": "error",
                        "message": format!("ファイル書き込み失敗: {}", e)
                    })),
                );
            }

            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "status": "ok",
                    "path": save_path,
                    "width": screenshot.width,
                    "height": screenshot.height,
                    "size_bytes": bytes.len(),
                })),
            )
        }
        Ok(Err(_)) => {
            // oneshot sender が drop された（キャンセル）
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "スクリーンショット応答チャネルが切断"
                })),
            )
        }
        Err(_) => {
            // タイムアウト — waiter をクリーンアップ
            let mut waiters = state.screenshot_waiters.lock().await;
            waiters.remove(&request_id);
            (
                axum::http::StatusCode::GATEWAY_TIMEOUT,
                Json(serde_json::json!({
                    "status": "error",
                    "message": "スクリーンショット取得タイムアウト（10秒）"
                })),
            )
        }
    }
}

/// POST /api/shutdown - Graceful shutdown
pub async fn shutdown_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    tracing::info!("Shutdown requested via API");
    state.shutdown_token.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}

// ===== Ruby VM ハンドラー =====

/// Ruby eval パラメータ
#[derive(Deserialize)]
pub struct RubyEvalParams {
    pub code: Option<String>,
    pub file: Option<String>,
    pub pane_id: Option<String>,
}

/// POST /api/ruby/eval - Ruby コードを即座に実行
pub async fn ruby_eval_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<RubyEvalParams>,
) -> impl IntoResponse {
    let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

    let result = crate::stand::ruby_vm::ruby_eval(
        params.code.as_deref(),
        params.file.as_deref(),
        &pane_id,
        &state.project_dir,
        &state.hub,
    )
    .await;

    match result {
        Ok(r) => Json(serde_json::json!({
            "status": "ok",
            "stdout": r.stdout,
            "stderr": r.stderr,
            "exit_code": r.exit_code,
            "elapsed_ms": r.elapsed_ms,
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": e,
        })),
    }
}

/// Ruby run パラメータ
#[derive(Deserialize)]
pub struct RubyRunParams {
    pub code: Option<String>,
    pub file: Option<String>,
    pub name: Option<String>,
    pub pane_id: Option<String>,
}

/// POST /api/ruby/run - Ruby デーモンプロセスを起動
pub async fn ruby_run_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<RubyRunParams>,
) -> impl IntoResponse {
    let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

    let result = crate::stand::ruby_vm::ruby_run(
        &state.ruby_registry,
        params.code.as_deref(),
        params.file.as_deref(),
        params.name.as_deref(),
        &pane_id,
        &state.project_dir,
        &state.hub,
    )
    .await;

    match result {
        Ok(process_id) => Json(serde_json::json!({
            "status": "ok",
            "process_id": process_id,
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": e,
        })),
    }
}

/// Ruby stop パラメータ
#[derive(Deserialize)]
pub struct RubyStopParams {
    pub process_id: String,
}

/// POST /api/ruby/stop - Ruby プロセスを停止
pub async fn ruby_stop_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<RubyStopParams>,
) -> impl IntoResponse {
    match crate::stand::ruby_vm::ruby_stop(&state.ruby_registry, &params.process_id).await {
        Ok(()) => Json(serde_json::json!({
            "status": "ok",
            "message": format!("プロセス {} に停止シグナルを送信しました", params.process_id),
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": e,
        })),
    }
}

/// GET /api/ruby/list - 実行中の Ruby プロセス一覧
pub async fn ruby_list_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let processes = state.ruby_registry.lock().await.list();
    Json(serde_json::json!({
        "status": "ok",
        "processes": processes,
    }))
}
