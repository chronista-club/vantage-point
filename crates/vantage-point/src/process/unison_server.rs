//! Unison QUIC サーバー
//!
//! MCP <-> Process 間の高速通信レイヤー。
//! Axum HTTP サーバーと並行して起動し、同じ Hub.broadcast() パターンで
//! WebSocket クライアントにメッセージを配信する。
//!
//! ポート: HTTP port + 1000 (例: 33000 -> 34000)
//!
//! "process" と "canvas" の2チャネルを提供:
//! - process: show / clear / toggle_pane / split_pane / close_pane
//! - canvas: open / close

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use unison::network::channel::UnisonChannel;
use unison::network::{MessageType, ProtocolServer};

use tokio::sync::broadcast;

use super::state::AppState;
use crate::protocol::{Content, ProcessMessage, SplitDirection};

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

/// UnwatchFile リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct UnwatchFileRequest {
    pane_id: String,
}

fn default_content_type() -> String {
    "markdown".to_string()
}

fn default_pane_id() -> String {
    "main".to_string()
}

// =============================================================================
// Process チャネル ハンドラー
// =============================================================================

/// show メソッドのハンドラー
fn handle_show(state: &AppState, payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let req: ShowRequest =
        serde_json::from_value(payload).map_err(|e| format!("Invalid show payload: {}", e))?;

    let content = match req.content_type.as_str() {
        "html" => Content::Html(req.content),
        "log" => Content::Log(req.content),
        _ => Content::Markdown(req.content),
    };

    let msg = ProcessMessage::Show {
        pane_id: req.pane_id.clone(),
        content,
        append: req.append,
        title: req.title,
    };
    state.hub.broadcast(msg);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

/// clear メソッドのハンドラー
fn handle_clear(state: &AppState, payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let req: ClearRequest =
        serde_json::from_value(payload).map_err(|e| format!("Invalid clear payload: {}", e))?;

    let msg = ProcessMessage::Clear {
        pane_id: req.pane_id.clone(),
    };
    state.hub.broadcast(msg);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

/// toggle_pane メソッドのハンドラー
fn handle_toggle_pane(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: TogglePaneRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid toggle_pane payload: {}", e))?;

    let msg = ProcessMessage::TogglePane {
        pane_id: req.pane_id.clone(),
        visible: req.visible,
    };
    state.hub.broadcast(msg);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

/// split_pane メソッドのハンドラー
fn handle_split_pane(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: SplitPaneRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid split_pane payload: {}", e))?;

    let direction = match req.direction.to_lowercase().as_str() {
        "horizontal" | "h" => SplitDirection::Horizontal,
        "vertical" | "v" => SplitDirection::Vertical,
        other => {
            return Err(format!(
                "Invalid direction: '{}'. Use 'horizontal' or 'vertical'",
                other
            ));
        }
    };

    let msg = ProcessMessage::Split {
        pane_id: req.source_pane_id,
        direction,
        new_pane_id: req.new_pane_id.clone(),
    };
    state.hub.broadcast(msg);

    Ok(serde_json::json!({"status": "ok", "new_pane_id": req.new_pane_id}))
}

/// close_pane メソッドのハンドラー
fn handle_close_pane(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: ClosePaneRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid close_pane payload: {}", e))?;

    let msg = ProcessMessage::Close {
        pane_id: req.pane_id.clone(),
    };
    state.hub.broadcast(msg);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

/// watch_file メソッドのハンドラー
async fn handle_watch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let config: crate::file_watcher::WatchConfig = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid watch_file payload: {}", e))?;

    let pane_id = config.pane_id.clone();

    state
        .file_watchers
        .lock()
        .await
        .start_watch(config, state.hub.clone())
        .map_err(|e| format!("watch_file 開始失敗: {}", e))?;

    Ok(serde_json::json!({"status": "ok", "pane_id": pane_id}))
}

/// unwatch_file メソッドのハンドラー
async fn handle_unwatch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: UnwatchFileRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid unwatch_file payload: {}", e))?;

    state.file_watchers.lock().await.stop_watch(&req.pane_id);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

// =============================================================================
// Canvas チャネル ハンドラー
// =============================================================================

/// canvas.open メソッドのハンドラー
async fn handle_canvas_open(state: &AppState) -> Result<serde_json::Value, String> {
    let mut pid_guard = state.canvas_pid.lock().await;

    // 既に起動中なら何もしない
    if let Some(pid) = *pid_guard {
        let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
        if alive {
            return Ok(serde_json::json!({"status": "already_open", "pid": pid}));
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
            tracing::info!("Canvas window opened via QUIC (pid={})", pid);
            Ok(serde_json::json!({"status": "opened", "pid": pid}))
        }
        Err(e) => Err(format!("Failed to open canvas: {}", e)),
    }
}

/// canvas.close メソッドのハンドラー
async fn handle_canvas_close(state: &AppState) -> Result<serde_json::Value, String> {
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
}

// =============================================================================
// サーバー起動
// =============================================================================

/// Unison QUIC サーバーを起動する
///
/// Axum HTTP サーバーと並行して動作し、MCP クライアントからの
/// QUIC リクエストを処理する。
///
/// "process" と "canvas" の2チャネルを登録し、各チャネル内で
/// メソッド名ベースのディスパッチを行う。
pub async fn start_unison_server(
    state: Arc<AppState>,
    http_port: u16,
    ready_tx: tokio::sync::oneshot::Sender<()>,
) {
    let quic_port = http_port + QUIC_PORT_OFFSET;
    let addr = format!("[::1]:{}", quic_port);

    let server =
        ProtocolServer::with_identity("vp-process", env!("CARGO_PKG_VERSION"), "vantage-point");

    // --- "process" チャネル: show / clear / toggle_pane / split_pane / close_pane ---
    server
        .register_channel("process", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
                async move {
                    use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

                    let channel = UnisonChannel::new(stream);

                    loop {
                        let msg = match channel.recv().await {
                            Ok(msg) => msg,
                            Err(_) => break,
                        };

                        if msg.msg_type != MessageType::Request {
                            continue;
                        }

                        let request_id = msg.id;
                        let method = msg.method.clone();
                        let payload = msg.payload_as_value().unwrap_or_default();

                        // リクエスト受信ログ
                        let tid = new_trace_id();
                        let start = std::time::Instant::now();
                        write_trace(
                            &TraceEntry::new(
                                "process",
                                &tid,
                                "receive",
                                "INFO",
                                format!("process.{}", method),
                            )
                            .with_data(payload.clone()),
                        );

                        let result = match method.as_str() {
                            "show" => handle_show(&state, payload),
                            "clear" => handle_clear(&state, payload),
                            "toggle_pane" => handle_toggle_pane(&state, payload),
                            "split_pane" => handle_split_pane(&state, payload),
                            "close_pane" => handle_close_pane(&state, payload),
                            "watch_file" => handle_watch_file(&state, payload).await,
                            "unwatch_file" => handle_unwatch_file(&state, payload).await,
                            _ => Err(format!("不明なメソッド: process.{}", method)),
                        };

                        let response = match &result {
                            Ok(payload) => {
                                // 処理成功ログ
                                write_trace(
                                    &TraceEntry::new(
                                        "process",
                                        &tid,
                                        "respond",
                                        "INFO",
                                        format!("process.{} OK", method),
                                    )
                                    .with_elapsed(start.elapsed().as_millis() as u64),
                                );
                                payload.clone()
                            }
                            Err(e) => {
                                // 処理失敗ログ
                                write_trace(
                                    &TraceEntry::new(
                                        "process",
                                        &tid,
                                        "respond",
                                        "ERROR",
                                        format!("process.{} 失敗: {}", method, e),
                                    )
                                    .with_elapsed(start.elapsed().as_millis() as u64),
                                );
                                serde_json::json!({"error": e})
                            }
                        };

                        if channel
                            .send_response(request_id, &method, response)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }

                    Ok(())
                }
            }
        })
        .await;

    // --- "canvas" チャネル: open / close ---
    server
        .register_channel("canvas", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
                async move {
                    use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

                    let channel = UnisonChannel::new(stream);

                    loop {
                        let msg = match channel.recv().await {
                            Ok(msg) => msg,
                            Err(_) => break,
                        };

                        if msg.msg_type != MessageType::Request {
                            continue;
                        }

                        let request_id = msg.id;
                        let method = msg.method.clone();

                        // リクエスト受信ログ
                        let tid = new_trace_id();
                        let start = std::time::Instant::now();
                        write_trace(&TraceEntry::new(
                            "process",
                            &tid,
                            "receive",
                            "INFO",
                            format!("canvas.{}", method),
                        ));

                        let result = match method.as_str() {
                            "open" => handle_canvas_open(&state).await,
                            "close" => handle_canvas_close(&state).await,
                            _ => Err(format!("不明なメソッド: canvas.{}", method)),
                        };

                        let response = match &result {
                            Ok(payload) => {
                                // 処理成功ログ
                                write_trace(
                                    &TraceEntry::new(
                                        "process",
                                        &tid,
                                        "respond",
                                        "INFO",
                                        format!("canvas.{} OK", method),
                                    )
                                    .with_elapsed(start.elapsed().as_millis() as u64),
                                );
                                payload.clone()
                            }
                            Err(e) => {
                                // 処理失敗ログ
                                write_trace(
                                    &TraceEntry::new(
                                        "process",
                                        &tid,
                                        "respond",
                                        "ERROR",
                                        format!("canvas.{} 失敗: {}", method, e),
                                    )
                                    .with_elapsed(start.elapsed().as_millis() as u64),
                                );
                                serde_json::json!({"error": e})
                            }
                        };

                        if channel
                            .send_response(request_id, &method, response)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }

                    Ok(())
                }
            }
        })
        .await;

    // --- "terminal" チャネル: raw PTY I/O + resize ---
    server
        .register_channel("terminal", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
                async move {
                    let channel = UnisonChannel::new(stream);

                    // Hub を subscribe して PTY 出力を受信
                    let mut hub_rx = state.hub.subscribe();

                    use base64::Engine;
                    let engine = base64::engine::general_purpose::STANDARD;

                    loop {
                        tokio::select! {
                            // PTY output → raw frame to client
                            msg = hub_rx.recv() => {
                                match msg {
                                    Ok(ProcessMessage::TerminalOutput { data }) => {
                                        match engine.decode(&data) {
                                            Ok(bytes) if !bytes.is_empty() => {
                                                if channel.send_raw(&bytes).await.is_err() {
                                                    break;
                                                }
                                            }
                                            Ok(_) => {} // 空データはスキップ
                                            Err(e) => {
                                                tracing::warn!("TerminalOutput base64 decode error: {}", e);
                                            }
                                        }
                                    }
                                    Ok(ProcessMessage::TerminalReady) => {
                                        // TerminalReady を protocol event として通知
                                        let _ = channel.send_event(
                                            "terminal_ready",
                                            serde_json::json!({}),
                                        ).await;
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        tracing::warn!("terminal broadcast lagged: {} messages dropped", n);
                                    }
                                    _ => {} // 他メッセージは無視
                                }
                            }
                            // Client → PTY (raw input)
                            data = channel.recv_raw() => {
                                match data {
                                    Ok(bytes) => {
                                        let mut pty = state.pty_manager.lock().await;
                                        if let Err(e) = pty.write(&bytes) {
                                            tracing::warn!("PTY write error: {}", e);
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            // Client → control (resize)
                            msg = channel.recv() => {
                                match msg {
                                    Ok(msg) if msg.method == "resize" => {
                                        let payload = msg.payload_as_value().unwrap_or_default();
                                        let cols = payload["cols"].as_u64().unwrap_or(80) as u16;
                                        let rows = payload["rows"].as_u64().unwrap_or(24) as u16;

                                        // サイズバリデーション
                                        if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
                                            tracing::warn!("Invalid resize: {}x{}", cols, rows);
                                            let _ = channel.send_response(
                                                msg.id, "resize",
                                                serde_json::json!({"error": "invalid dimensions"}),
                                            ).await;
                                            continue;
                                        }

                                        let mut pty = state.pty_manager.lock().await;
                                        if !pty.is_active() {
                                            // 初回 resize で PTY 起動
                                            if let Err(e) = pty.start(
                                                &state.project_dir, cols, rows,
                                                state.hub.sender(),
                                            ) {
                                                tracing::warn!("PTY起動失敗: {}", e);
                                            }
                                        } else {
                                            let _ = pty.resize(cols, rows);
                                        }

                                        let _ = channel.send_response(
                                            msg.id, "resize",
                                            serde_json::json!({"status": "ok"}),
                                        ).await;
                                    }
                                    Err(_) => break,
                                    _ => {}
                                }
                            }
                        }
                    }

                    Ok(())
                }
            }
        })
        .await;

    // サーバー起動（spawn_listen でバックグラウンド起動）
    tracing::info!("Starting Unison QUIC server on {}", addr);
    {
        use crate::trace_log::{TraceEntry, write_trace};
        write_trace(&TraceEntry::new(
            "process",
            "server",
            "start",
            "INFO",
            format!("QUIC server starting on {}", addr),
        ));
    }
    match server.spawn_listen(&addr).await {
        Ok(handle) => {
            let _ = ready_tx.send(()); // バインド完了通知
            tracing::info!("Unison QUIC server listening on {}", handle.local_addr());
            // Process shutdown を待ってからグレースフルシャットダウン
            state.shutdown_token.cancelled().await;
            if let Err(e) = handle.shutdown().await {
                tracing::error!("QUIC server shutdown error: {}", e);
            }
        }
        Err(e) => {
            tracing::error!("Unison QUIC server failed to start: {}", e);
            let _ = ready_tx.send(()); // エラーでも通知（ブロック防止）
        }
    }
}
