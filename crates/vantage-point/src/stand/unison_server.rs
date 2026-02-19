//! Unison QUIC サーバー
//!
//! MCP ↔ Stand 間の高速通信レイヤー。
//! Axum HTTP サーバーと並行して起動し、同じ Hub.broadcast() パターンで
//! WebSocket クライアントにメッセージを配信する。
//!
//! ポート: HTTP port + 1000 (例: 33000 → 34000)

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use unison::network::{NetworkError, ProtocolServer, UnisonServer, UnisonServerExt};

use super::state::AppState;
use crate::protocol::{Content, SplitDirection, StandMessage};

/// QUIC ポートのオフセット（HTTP ポートからの差分）
pub const QUIC_PORT_OFFSET: u16 = 1000;

/// Show リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct ShowRequest {
    content: String,
    #[serde(default = "default_content_type")]
    content_type: String,
    #[serde(default = "default_pane_id")]
    pane_id: String,
    #[serde(default)]
    append: bool,
    #[serde(default)]
    title: Option<String>,
}

/// Clear リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct ClearRequest {
    #[serde(default = "default_pane_id")]
    pane_id: String,
}

/// TogglePane リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct TogglePaneRequest {
    pane_id: String,
    #[serde(default)]
    visible: Option<bool>,
}

/// SplitPane リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct SplitPaneRequest {
    direction: String,
    #[serde(default = "default_pane_id")]
    source_pane_id: String,
    new_pane_id: String,
}

/// ClosePane リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct ClosePaneRequest {
    pane_id: String,
}

fn default_content_type() -> String {
    "markdown".to_string()
}

fn default_pane_id() -> String {
    "main".to_string()
}

/// Unison QUIC サーバーを起動する
///
/// Axum HTTP サーバーと並行して動作し、MCP クライアントからの
/// QUIC リクエストを処理する。
pub async fn start_unison_server(state: Arc<AppState>, http_port: u16) {
    let quic_port = http_port + QUIC_PORT_OFFSET;
    let addr = format!("[::1]:{}", quic_port);

    let mut server = ProtocolServer::new();

    // --- show ハンドラー ---
    {
        let state = state.clone();
        server.register_handler("show", move |payload| {
            let req: ShowRequest = serde_json::from_value(payload)
                .map_err(|e| NetworkError::Protocol(format!("Invalid show payload: {}", e)))?;

            let content = match req.content_type.as_str() {
                "html" => Content::Html(req.content),
                "log" => Content::Log(req.content),
                _ => Content::Markdown(req.content),
            };

            let msg = StandMessage::Show {
                pane_id: req.pane_id.clone(),
                content,
                append: req.append,
                title: req.title,
            };
            state.hub.broadcast(msg);

            Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
        });
    }

    // --- clear ハンドラー ---
    {
        let state = state.clone();
        server.register_handler("clear", move |payload| {
            let req: ClearRequest = serde_json::from_value(payload)
                .map_err(|e| NetworkError::Protocol(format!("Invalid clear payload: {}", e)))?;

            let msg = StandMessage::Clear {
                pane_id: req.pane_id.clone(),
            };
            state.hub.broadcast(msg);

            Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
        });
    }

    // --- toggle_pane ハンドラー ---
    {
        let state = state.clone();
        server.register_handler("toggle_pane", move |payload| {
            let req: TogglePaneRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid toggle_pane payload: {}", e))
            })?;

            let msg = StandMessage::TogglePane {
                pane_id: req.pane_id.clone(),
                visible: req.visible,
            };
            state.hub.broadcast(msg);

            Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
        });
    }

    // --- split_pane ハンドラー ---
    {
        let state = state.clone();
        server.register_handler("split_pane", move |payload| {
            let req: SplitPaneRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid split_pane payload: {}", e))
            })?;

            let direction = match req.direction.to_lowercase().as_str() {
                "horizontal" | "h" => SplitDirection::Horizontal,
                "vertical" | "v" => SplitDirection::Vertical,
                other => {
                    return Err(NetworkError::Protocol(format!(
                        "Invalid direction: '{}'. Use 'horizontal' or 'vertical'",
                        other
                    )));
                }
            };

            let msg = StandMessage::Split {
                pane_id: req.source_pane_id,
                direction,
                new_pane_id: req.new_pane_id.clone(),
            };
            state.hub.broadcast(msg);

            Ok(serde_json::json!({"status": "ok", "new_pane_id": req.new_pane_id}))
        });
    }

    // --- close_pane ハンドラー ---
    {
        let state = state.clone();
        server.register_handler("close_pane", move |payload| {
            let req: ClosePaneRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid close_pane payload: {}", e))
            })?;

            let msg = StandMessage::Close {
                pane_id: req.pane_id.clone(),
            };
            state.hub.broadcast(msg);

            Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
        });
    }

    // --- canvas.open ハンドラー ---
    {
        let state = state.clone();
        server.register_handler("canvas.open", move |_payload| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                let mut pid_guard = state.canvas_pid.lock().await;

                // 既に起動中なら何もしない
                if let Some(pid) = *pid_guard {
                    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
                    if alive {
                        return Ok(serde_json::json!({"status": "already_open", "pid": pid}));
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
                        tracing::info!("Canvas window opened via QUIC (pid={})", pid);
                        Ok(serde_json::json!({"status": "opened", "pid": pid}))
                    }
                    Err(e) => Err(NetworkError::Protocol(format!(
                        "Failed to open canvas: {}",
                        e
                    ))),
                }
            })
        });
    }

    // --- canvas.close ハンドラー ---
    {
        let state = state.clone();
        server.register_handler("canvas.close", move |_payload| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                let mut pid_guard = state.canvas_pid.lock().await;

                if let Some(pid) = pid_guard.take() {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                    tracing::info!("Canvas window closed via QUIC (pid={})", pid);
                    Ok(serde_json::json!({"status": "closed", "pid": pid}))
                } else {
                    Ok(serde_json::json!({"status": "not_open"}))
                }
            })
        });
    }

    // サーバー起動
    tracing::info!("Starting Unison QUIC server on {}", addr);
    if let Err(e) = server.listen(&addr).await {
        tracing::error!("Unison QUIC server failed to start: {}", e);
    }
}
