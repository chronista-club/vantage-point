//! Unison QUIC サーバー
//!
//! MCP <-> Process 間の高速通信レイヤー。
//! Axum HTTP サーバーと並行して起動し、同じ Hub.broadcast() パターンで
//! WebSocket クライアントにメッセージを配信する。
//!
//! ポート: HTTP port + 100 (例: 33000 -> 33100)
//!
//! "process" チャネルですべての操作を統一:
//! - show / clear / toggle_pane / split_pane / close_pane
//! - watch_file / unwatch_file

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use unison::network::channel::UnisonChannel;
use unison::network::{MessageType, ProtocolServer};

use tokio::sync::broadcast;

use super::state::AppState;
use crate::protocol::ProcessMessage;

/// QUIC ポートのオフセット（HTTP ポートからの差分）
/// TCP (HTTP) と UDP (QUIC) は OS レベルで独立 → 同一ポートで共存可能
pub const QUIC_PORT_OFFSET: u16 = 0;

/// recv_raw の最大フレームサイズ（64 KiB）
const MAX_RAW_FRAME_SIZE: usize = 64 * 1024;

/// UnwatchFile リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct UnwatchFileRequest {
    pane_id: String,
}

// =============================================================================
// Process チャネル ハンドラー
// =============================================================================

/// ProcessMessage を受け取って broadcast + Mailbox 配信する汎用ハンドラー
///
/// MCP → QUIC → ここ の経路では、MCP が ProcessMessage をそのままシリアライズして送る。
/// HTTP ハンドラ（health.rs の show_handler 等）と同じ ProcessMessage 形式を受ける。
///
/// 配信先:
/// 1. Hub broadcast → WebSocket → Canvas（既存）
/// 2. Mailbox "protocol" → PP Capability（VP-24）
fn handle_process_message(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let msg: ProcessMessage = serde_json::from_value(payload.clone())
        .map_err(|e| format!("Invalid ProcessMessage: {}", e))?;

    // 1. Hub broadcast → WebSocket → Canvas（既存経路）
    // TopicRouter が Hub ブリッジ経由で自動的に retained に保存するため、
    // 明示的なキャッシュは不要。Hub に broadcast するだけ。
    state.hub.broadcast(msg);

    // NOTE: Mailbox 経由の配信は ProtocolCapability 側の受信ループ実装後に追加（VP-24）
    // 現在は Hub broadcast のみで Canvas に配信。

    Ok(serde_json::json!({"status": "ok"}))
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
// tmux Actor ハンドラー
// =============================================================================

/// tmux ペイン分割
async fn handle_tmux_split(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let horizontal = payload["horizontal"].as_bool().unwrap_or(true);
    let command = payload["command"].as_str().map(|s| s.to_string());
    let content_type = payload["content_type"].as_str();
    let command = crate::process::routes::health::resolve_content_command(content_type, command);
    let pane = handle.split(horizontal, command).await?;
    Ok(serde_json::json!({"status": "ok", "pane": pane}))
}

/// tmux ペイン一覧
async fn handle_tmux_list(state: &AppState) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let panes = handle.list().await;
    Ok(serde_json::json!({"panes": panes}))
}

/// tmux ペイン閉鎖
async fn handle_tmux_close(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let pane_id = payload["pane_id"]
        .as_str()
        .ok_or_else(|| "pane_id が必要です".to_string())?;
    handle.close(pane_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// tmux ペインキャプチャ（単一）
async fn handle_tmux_capture(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let pane_id = payload["pane_id"]
        .as_str()
        .ok_or_else(|| "pane_id が必要です".to_string())?;
    let content = handle.capture(pane_id).await?;
    Ok(serde_json::json!({"status": "ok", "pane_id": pane_id, "content": content}))
}

/// tmux 全ペインキャプチャ（ダッシュボード用）
async fn handle_tmux_capture_all(state: &AppState) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let captures = handle.capture_all().await;
    Ok(serde_json::json!({"status": "ok", "captures": captures}))
}

/// エージェントメタデータ設定
async fn handle_tmux_set_agent_meta(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let pane_id = payload["pane_id"]
        .as_str()
        .ok_or_else(|| "pane_id が必要です".to_string())?;
    let label = payload["label"]
        .as_str()
        .ok_or_else(|| "label が必要です".to_string())?;
    let status = payload["status"].as_str().unwrap_or("running");
    let task = payload["task"].as_str().map(|s| s.to_string());

    let meta = crate::process::tmux_actor::AgentMeta {
        label: label.to_string(),
        status: status.to_string(),
        task,
    };
    handle.set_agent_meta(pane_id, meta).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// エージェントステータス更新
async fn handle_tmux_update_agent_status(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let pane_id = payload["pane_id"]
        .as_str()
        .ok_or_else(|| "pane_id が必要です".to_string())?;
    let status = payload["status"]
        .as_str()
        .ok_or_else(|| "status が必要です".to_string())?;
    let task = payload["task"].as_str().map(|s| s.to_string());

    // 既存メタデータから label/task を引き継ぎ（capture_all 不要）
    let existing = handle.get_agent_meta(pane_id).await;
    let existing_label = existing
        .as_ref()
        .map(|a| a.label.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let existing_task = if task.is_none() {
        existing.and_then(|a| a.task)
    } else {
        None
    };

    let meta = crate::process::tmux_actor::AgentMeta {
        label: existing_label,
        status: status.to_string(),
        task: task.or(existing_task),
    };
    handle.set_agent_meta(pane_id, meta).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// エージェントメタデータクリア
async fn handle_tmux_clear_agent_meta(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let pane_id = payload["pane_id"]
        .as_str()
        .ok_or_else(|| "pane_id が必要です".to_string())?;
    handle.clear_agent_meta(pane_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// tmux send-keys（ペインへのテキスト送信）
async fn handle_tmux_send_keys(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let pane_id = payload["pane_id"]
        .as_str()
        .ok_or_else(|| "pane_id が必要です".to_string())?;
    let keys = payload["keys"]
        .as_str()
        .ok_or_else(|| "keys が必要です".to_string())?;
    handle.send_keys(pane_id, keys).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// ProcessRunner ハンドラー
// =============================================================================

/// プロセス起動
async fn handle_process_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::process::process_runner::RunParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    let process_id = crate::process::process_runner::process_run(
        &state.process_registry,
        &params,
        &state.project_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// プロセス停止
async fn handle_process_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::process::process_runner::process_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// コード注入
async fn handle_process_inject(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::process::process_runner::InjectParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    crate::process::process_runner::process_inject(&state.process_registry, &params).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// プロセス一覧
async fn handle_process_list(state: &AppState) -> Result<serde_json::Value, String> {
    let processes = state.process_registry.lock().await.list();
    Ok(serde_json::json!({"status": "ok", "processes": processes}))
}

// =============================================================================
// Terminal チャネル制御メッセージハンドラー
// =============================================================================

/// Terminal チャネルの制御メッセージを処理
///
/// create_session / switch_session / list_sessions / close_session / resize
async fn handle_terminal_control(
    state: &AppState,
    msg: &unison::network::ProtocolMessage,
    _channel: &UnisonChannel,
    current_session_id: &mut Option<String>,
    terminal_rx: &mut Option<broadcast::Receiver<ProcessMessage>>,
) -> Option<serde_json::Value> {
    let payload = msg.payload_as_value().unwrap_or_default();

    match msg.method.as_str() {
        "create_session" => {
            let cols = payload["cols"].as_u64().unwrap_or(80) as u16;
            let rows = payload["rows"].as_u64().unwrap_or(24) as u16;

            // コマンド指定（オプション、JSON 配列 ["claude", "--continue"] など）
            let command_parts: Option<Vec<String>> = payload["command"].as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            });
            let command_refs: Option<Vec<&str>> = command_parts
                .as_ref()
                .map(|v| v.iter().map(|s| s.as_str()).collect());

            let mut pty = state.pty_manager.lock().await;
            pty.set_project_dir(&state.project_dir);

            match pty.create_session(cols, rows, command_refs.as_deref()) {
                Ok((session_id, tx)) => {
                    // 自動的に新セッションに切替
                    *current_session_id = Some(session_id.clone());
                    *terminal_rx = Some(tx.subscribe());
                    tracing::info!("Terminal セッション作成: {}", session_id);
                    Some(serde_json::json!({
                        "status": "ok",
                        "session_id": session_id,
                    }))
                }
                Err(e) => Some(serde_json::json!({"error": format!("セッション作成失敗: {}", e)})),
            }
        }

        "switch_session" => {
            let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
            let pty = state.pty_manager.lock().await;

            if let Some(tx) = pty.get_session_tx(&session_id) {
                *current_session_id = Some(session_id.clone());
                *terminal_rx = Some(tx.subscribe());
                tracing::info!("Terminal セッション切替: {}", session_id);
                Some(serde_json::json!({"status": "ok", "session_id": session_id}))
            } else {
                Some(
                    serde_json::json!({"error": format!("セッション {} が見つかりません", session_id)}),
                )
            }
        }

        "list_sessions" => {
            let pty = state.pty_manager.lock().await;
            let sessions = pty.list_sessions();
            Some(serde_json::json!({
                "sessions": sessions,
                "current": current_session_id,
            }))
        }

        "close_session" => {
            let session_id = payload["session_id"].as_str().unwrap_or("").to_string();
            let mut pty = state.pty_manager.lock().await;

            if pty.close_session(&session_id) {
                // 現在のセッションが閉じられた場合
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    *current_session_id = None;
                    *terminal_rx = None;
                }
                tracing::info!("Terminal セッション閉鎖: {}", session_id);
                Some(serde_json::json!({"status": "ok"}))
            } else {
                Some(
                    serde_json::json!({"error": format!("セッション {} が見つかりません", session_id)}),
                )
            }
        }

        "resize" => {
            let cols = payload["cols"].as_u64().unwrap_or(80) as u16;
            let rows = payload["rows"].as_u64().unwrap_or(24) as u16;

            // サイズバリデーション
            if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
                tracing::warn!("Invalid resize: {}x{}", cols, rows);
                return Some(serde_json::json!({"error": "invalid dimensions"}));
            }

            if let Some(sid) = current_session_id.as_deref() {
                let mut pty = state.pty_manager.lock().await;
                let _ = pty.resize(sid, cols, rows);
            }

            Some(serde_json::json!({"status": "ok"}))
        }

        _ => {
            tracing::warn!("不明な terminal コマンド: {}", msg.method);
            None
        }
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
/// "process" チャネルですべての操作を統一し、
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

    // --- "process" チャネル: 全操作を統一 ---
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
                            "show" | "clear" | "toggle_pane" | "split_pane" | "close_pane" => {
                                handle_process_message(&state, payload)
                            }
                            "watch_file" => handle_watch_file(&state, payload).await,
                            "unwatch_file" => handle_unwatch_file(&state, payload).await,
                            "tmux_split" => handle_tmux_split(&state, payload).await,
                            "tmux_list" => handle_tmux_list(&state).await,
                            "tmux_close" => handle_tmux_close(&state, payload).await,
                            "tmux_capture" => handle_tmux_capture(&state, payload).await,
                            "tmux_capture_all" => handle_tmux_capture_all(&state).await,
                            // エージェントメタデータ
                            "tmux_set_agent_meta" => {
                                handle_tmux_set_agent_meta(&state, payload).await
                            }
                            "tmux_update_agent_status" => {
                                handle_tmux_update_agent_status(&state, payload).await
                            }
                            "tmux_clear_agent_meta" => {
                                handle_tmux_clear_agent_meta(&state, payload).await
                            }
                            "tmux_send_keys" => handle_tmux_send_keys(&state, payload).await,
                            // ProcessRunner
                            "process_run" => handle_process_run(&state, payload).await,
                            "process_stop" => handle_process_stop(&state, payload).await,
                            "process_inject" => handle_process_inject(&state, payload).await,
                            "process_list" => handle_process_list(&state).await,
                            // Mailbox (VP-24)
                            "mailbox_send" => handle_mailbox_send(&state, payload).await,
                            "mailbox_recv" => handle_mailbox_recv(&state, payload).await,
                            "mailbox_list" => handle_mailbox_list(&state).await,
                            "mailbox_ack" => handle_mailbox_ack(&state, payload).await,
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

    // --- "terminal" チャネル: 複数セッション管理 + raw PTY I/O + resize ---
    server
        .register_channel("terminal", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
                async move {
                    let channel = UnisonChannel::new(stream);

                    // 認証: 最初のメッセージでトークンを検証
                    let auth_msg = match channel.recv().await {
                        Ok(msg) => msg,
                        Err(_) => return Ok(()),
                    };
                    let token = auth_msg
                        .payload_as_value()
                        .unwrap_or_default()["token"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();

                    if token != state.terminal_token {
                        tracing::warn!("Terminal 認証失敗: 無効なトークン");
                        let _ = channel
                            .send_response(
                                auth_msg.id,
                                "auth",
                                serde_json::json!({"error": "invalid token"}),
                            )
                            .await;
                        return Ok(());
                    }

                    // 認証成功 — セッション一覧を返す
                    let sessions = state.pty_manager.lock().await.list_sessions();
                    let _ = channel
                        .send_response(
                            auth_msg.id,
                            "auth",
                            serde_json::json!({
                                "status": "ok",
                                "sessions": sessions,
                            }),
                        )
                        .await;
                    tracing::info!("Terminal クライアント認証成功");

                    // 現在購読中のセッション
                    let mut current_session_id: Option<String> = None;
                    // セッション出力の受信チャネル（switch 時に差し替え）
                    let mut terminal_rx: Option<broadcast::Receiver<ProcessMessage>> = None;

                    use base64::Engine;
                    let engine = base64::engine::general_purpose::STANDARD;

                    loop {
                        // terminal_rx が None なら protocol メッセージのみ待つ
                        if let Some(ref mut rx) = terminal_rx {
                            tokio::select! {
                                // PTY output → raw frame to client
                                msg = rx.recv() => {
                                    match msg {
                                        Ok(ProcessMessage::TerminalOutput { data }) => {
                                            match engine.decode(&data) {
                                                Ok(bytes) if !bytes.is_empty() => {
                                                    if channel.send_raw(&bytes).await.is_err() {
                                                        break;
                                                    }
                                                }
                                                Ok(_) => {}
                                                Err(e) => {
                                                    tracing::warn!("TerminalOutput base64 decode error: {}", e);
                                                }
                                            }
                                        }
                                        Ok(ProcessMessage::TerminalReady) => {
                                            let _ = channel.send_event(
                                                "terminal_ready",
                                                serde_json::json!({}),
                                            ).await;
                                        }
                                        Ok(ProcessMessage::TerminalExited) => {
                                            // PTY 子プロセスが終了（EOF）
                                            tracing::info!("Terminal セッション終了 (EOF): {:?}", current_session_id);
                                            let _ = channel.send_event(
                                                "session_ended",
                                                serde_json::json!({"session_id": current_session_id}),
                                            ).await;
                                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                            break;
                                        }
                                        Err(broadcast::error::RecvError::Closed) => {
                                            // broadcast チャネル自体がクローズ
                                            tracing::info!("Terminal broadcast closed: {:?}", current_session_id);
                                            let _ = channel.send_event(
                                                "session_ended",
                                                serde_json::json!({"session_id": current_session_id}),
                                            ).await;
                                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                            break;
                                        }
                                        Err(broadcast::error::RecvError::Lagged(n)) => {
                                            tracing::warn!("terminal broadcast lagged: {} messages dropped", n);
                                        }
                                        _ => {}
                                    }
                                }
                                // Client → PTY (raw input)
                                data = channel.recv_raw() => {
                                    match data {
                                        Ok(bytes) if bytes.len() > MAX_RAW_FRAME_SIZE => {
                                            tracing::warn!(
                                                "recv_raw フレームサイズ超過: {} bytes (上限 {} bytes)、ドロップ",
                                                bytes.len(), MAX_RAW_FRAME_SIZE
                                            );
                                        }
                                        Ok(bytes) => {
                                            if let Some(ref sid) = current_session_id {
                                                let mut pty = state.pty_manager.lock().await;
                                                if let Err(e) = pty.write(sid, &bytes) {
                                                    tracing::warn!("PTY write error: {}", e);
                                                }
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                                // Client → control messages
                                msg = channel.recv() => {
                                    match msg {
                                        Ok(msg) => {
                                            let resp = handle_terminal_control(
                                                &state, &msg, &channel,
                                                &mut current_session_id,
                                                &mut terminal_rx,
                                            ).await;
                                            if let Some(r) = resp {
                                                let _ = channel.send_response(msg.id, &msg.method, r).await;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                            }
                        } else {
                            // セッション未選択: protocol メッセージのみ待つ
                            match channel.recv().await {
                                Ok(msg) => {
                                    let resp = handle_terminal_control(
                                        &state, &msg, &channel,
                                        &mut current_session_id,
                                        &mut terminal_rx,
                                    ).await;
                                    if let Some(r) = resp {
                                        let _ = channel.send_response(msg.id, &msg.method, r).await;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }

                    Ok(())
                }
            }
        })
        .await;

    // --- "canvas" チャネル: TopicRouter 購読で Paisley Park メッセージを push ---
    server
        .register_channel("canvas", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
                async move {
                    let channel = UnisonChannel::new(stream);

                    // TopicRouter で paisley-park 配下を購読
                    // retained メッセージ（Show/Clear の最新値）が自動で初期配信される
                    let (sub_id, mut rx) =
                        state.topic_router.subscribe("process/paisley-park/#").await;

                    while let Some((_topic, msg)) = rx.recv().await {
                        let json = serde_json::to_value(&msg).unwrap_or_default();
                        if channel.send_event("pane", json).await.is_err() {
                            break;
                        }
                    }

                    // クリーンアップ: subscriber 登録を解除
                    state.topic_router.unsubscribe(sub_id).await;

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

// =============================================================================
// Mailbox ハンドラー (VP-24)
// =============================================================================

/// Mailbox にメッセージを送信
async fn handle_mailbox_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let msg: crate::capability::mailbox::MailboxMessage =
        serde_json::from_value(payload).map_err(|e| format!("Invalid MailboxMessage: {}", e))?;

    // 自分自身への送信を拒否
    if msg.to == msg.from {
        return Err(format!("自分自身('{}')への送信はできません", msg.to));
    }

    let to = msg.to.clone();
    let msg_id = msg.id.clone();

    // MCP 用ハンドルがあればそれを使い、なければ直接 router 経由
    if let Some(ref mcp_mailbox) = state.mcp_mailbox {
        mcp_mailbox
            .send(msg)
            .await
            .map_err(|e| format!("Mailbox send failed: {}", e))?;
    } else {
        return Err("MCP mailbox not initialized".to_string());
    }

    Ok(serde_json::json!({"status": "ok", "to": to, "id": msg_id}))
}

/// Mailbox からメッセージを受信（タイムアウト付き、Selective Receive）
///
/// from フィルタ指定時は `recv_matching` を使い、フィルタ不一致メッセージを
/// stash に退避する（メッセージロスなし）。
async fn handle_mailbox_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let timeout_secs = payload
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .min(30);
    let from_filter = payload
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mcp_mailbox = state
        .mcp_mailbox
        .as_ref()
        .ok_or_else(|| "MCP mailbox not initialized".to_string())?;

    // タイムアウト付きで受信を試行（Selective Receive でメッセージロスなし）
    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
        if let Some(filter) = from_filter {
            // Selective Receive: フィルタ不一致メッセージは stash に退避
            mcp_mailbox.recv_matching(move |m| m.from == filter).await
        } else {
            // フィルタなし: 通常の recv（stash 優先）
            mcp_mailbox.recv().await
        }
    })
    .await;

    match result {
        Ok(Some(msg)) => {
            let value =
                serde_json::to_value(&msg).map_err(|e| format!("Serialize error: {}", e))?;
            Ok(serde_json::json!({"message": value}))
        }
        Ok(None) => Ok(serde_json::json!({"message": null, "reason": "mailbox closed"})),
        Err(_) => Ok(serde_json::json!({"message": null, "reason": "timeout"})),
    }
}

/// 登録済み Mailbox アドレス一覧を返す
async fn handle_mailbox_list(state: &AppState) -> Result<serde_json::Value, String> {
    let addresses = state.capabilities.mailbox_router.addresses().await;
    Ok(serde_json::json!({"addresses": addresses}))
}

/// 明示 ack — persistent メッセージを永続ストアから削除
///
/// `manual_ack: true` で受信したメッセージに対して使う。
/// 処理完了後に呼び出すことで、途中クラッシュ時の再配信を保証。
async fn handle_mailbox_ack(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "id required".to_string())?
        .to_string();

    let mcp_mailbox = state
        .mcp_mailbox
        .as_ref()
        .ok_or_else(|| "MCP mailbox not initialized".to_string())?;
    mcp_mailbox.ack(&id).await;

    Ok(serde_json::json!({"status": "ok", "id": id}))
}
