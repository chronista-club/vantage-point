//! Unison QUIC サーバー
//!
//! MCP <-> Process 間の高速通信レイヤー。
//! Axum HTTP サーバーと並行して起動し、同じ Hub.broadcast() パターンで
//! WebSocket クライアントにメッセージを配信する。
//!
//! ポート: HTTP と同一ポート番号を使う。 HTTP は TCP・QUIC は UDP で OS レベルの
//! ポート名前空間が独立しているため衝突しない (`QUIC_PORT_OFFSET = 0`)。
//!
//! "process" チャネルですべての操作を統一:
//! - show / clear / toggle_pane / split_pane / close_pane
//! - watch_file / unwatch_file

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use unison::network::channel::UnisonChannel;
use unison::network::quic::QuicServer;
use unison::network::{CertSource, MessageType, ProtocolServer};

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

/// ProcessMessage を受け取って broadcast + Msgbox 配信する汎用ハンドラー
///
/// MCP → QUIC → ここ の経路では、MCP が ProcessMessage をそのままシリアライズして送る。
/// HTTP ハンドラ（health.rs の show_handler 等）と同じ ProcessMessage 形式を受ける。
///
/// 配信先:
/// 1. Hub broadcast → WebSocket → Canvas（既存）
/// 2. Msgbox "protocol" → PP Capability（VP-24）
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

    // NOTE: Msgbox 経由の配信は ProtocolCapability 側の受信ループ実装後に追加（VP-24）
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

/// label または pane_id からペイン ID を解決
async fn handle_tmux_resolve_pane(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
    let query = payload["query"]
        .as_str()
        .ok_or_else(|| "query が必要です".to_string())?;
    match handle.resolve_pane_id(query).await {
        Some(pane_id) => {
            let meta = handle.get_agent_meta(&pane_id).await;
            Ok(serde_json::json!({"status": "ok", "pane_id": pane_id, "meta": meta}))
        }
        None => Err(format!("ペインが見つかりません: {}", query)),
    }
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
    // [::]: dual-stack (IPv6 + IPv4) bind on all interfaces (WSL2/LAN 経由アクセス対応)
    let addr = format!("[::]:{}", quic_port);

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
                            "tmux_resolve_pane" => handle_tmux_resolve_pane(&state, payload).await,
                            // ProcessRunner
                            "process_run" => handle_process_run(&state, payload).await,
                            "process_stop" => handle_process_stop(&state, payload).await,
                            "process_inject" => handle_process_inject(&state, payload).await,
                            "process_list" => handle_process_list(&state).await,
                            // wiremsg threaded inbox (Phase A ①、 R2 で wire_thread 追加)
                            "wire_send" => handle_wire_send(&state, payload).await,
                            "wire_recv" => handle_wire_recv(&state, payload).await,
                            "wire_thread" => handle_wire_thread(&state, payload).await,
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
                            .send_response(request_id, &method, &response)
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
                                &serde_json::json!({"error": "invalid token"}),
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
                            &serde_json::json!({
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
                                                &serde_json::json!({}),
                                            ).await;
                                        }
                                        Ok(ProcessMessage::TerminalExited) => {
                                            // PTY 子プロセスが終了（EOF）
                                            tracing::info!("Terminal セッション終了 (EOF): {:?}", current_session_id);
                                            let _ = channel.send_event(
                                                "session_ended",
                                                &serde_json::json!({"session_id": current_session_id}),
                                            ).await;
                                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                            break;
                                        }
                                        Err(broadcast::error::RecvError::Closed) => {
                                            // broadcast チャネル自体がクローズ
                                            tracing::info!("Terminal broadcast closed: {:?}", current_session_id);
                                            let _ = channel.send_event(
                                                "session_ended",
                                                &serde_json::json!({"session_id": current_session_id}),
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
                                                let _ = channel.send_response(msg.id, &msg.method, &r).await;
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
                                        let _ = channel.send_response(msg.id, &msg.method, &r).await;
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
                        if channel.send_event("pane", &json).await.is_err() {
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

    // --- "lanes" チャネル: wiremsg Stage 1 — TopicRouter 購読で Lane snapshot を push ---
    // `process/star-platinum/state/#`（現状 lanes、retained）を購読。接続時に retained の
    // 現 snapshot が初期配信され、以降 LanePool 変化のたび push される（Stage 0 の
    // LanesSnapshot producer と対）。consumer は vp-app の Unison topic client（Stage 1 後続）。
    // 設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
    server
        .register_channel("lanes", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
                async move {
                    let channel = UnisonChannel::new(stream);
                    let (sub_id, mut rx) = state
                        .topic_router
                        .subscribe("process/star-platinum/state/#")
                        .await;
                    while let Some((_topic, msg)) = rx.recv().await {
                        let json = serde_json::to_value(&msg).unwrap_or_default();
                        if channel.send_event("snapshot", &json).await.is_err() {
                            break;
                        }
                    }
                    state.topic_router.unsubscribe(sub_id).await;
                    Ok(())
                }
            }
        })
        .await;

    // サーバー起動
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

    // VP-185: spawn_listen は内部で QuicServer::new() (= cert なし固定) を使うため、
    // server 側で CertSource を明示するには QuicServer::builder 経由が必須。
    // PR-2 は dev default (CertSource::dev_localhost()) を明示、 PR-3 で
    // InternalMeshKeypair の server 半分に差し替える。
    let server = std::sync::Arc::new(server);
    let mut quic = QuicServer::builder(server)
        .cert_source(CertSource::dev_localhost())
        .build();
    if let Err(e) = quic.bind(&addr).await {
        tracing::error!("Unison QUIC server failed to bind: {}", e);
        let _ = ready_tx.send(()); // エラーでも通知（ブロック防止）
        return;
    }
    let _ = ready_tx.send(()); // バインド完了通知
    tracing::info!("Unison QUIC server listening on {:?}", quic.local_addr());

    // 旧 spawn_listen の ServerHandle::shutdown 連携を自前再実装:
    // state.shutdown_token (CancellationToken) → oneshot::Receiver に bridge し、
    // start_with_shutdown に渡す (= graceful shutdown を維持)。
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    {
        let token = state.shutdown_token.clone();
        tokio::spawn(async move {
            token.cancelled().await;
            let _ = shutdown_tx.send(());
        });
    }
    if let Err(e) = quic.start_with_shutdown(shutdown_rx).await {
        tracing::error!("Unison QUIC server error: {}", e);
    }
}

// =============================================================================
// wiremsg threaded inbox ハンドラー (Phase A ①、 設計 mem_1CbD9H1KGQykBaFG8XXVsn)
// =============================================================================

/// wiremsg を送信する (= 新規 thread の root、 または `reply_to` 指定で reply)
///
/// payload: `{ from, to: [..], body, reply_to? }`
///
/// Operations (Phase A ① / R1 / R3 仕様):
/// - `reply_to` なし → `WiremsgStore::send_root` (root message を INSERT、 `prev = None`)
/// - `reply_to` あり → `WiremsgStore::send_reply` (reply message を INSERT、 reply-all 展開)
/// - 送信後、 notify 対象の待機中 `wire_recv` を `WireNotifier` で起こす:
///   - root: 受信者 (= `to`) を notify
///   - reply: reply の `to` (= carry-forward 済の参加者集合) を notify
/// - R3 (cross-process delivery): ローカル INSERT の **後**、 確定した message の `to`
///   (= reply は carry-forward 済) を `classify_recipients` で振り分け、 `agent@<other-project>`
///   宛があれば受信側 SP に best-effort で forward する ([`forward_remote_recipients`])。
pub(crate) async fn handle_wire_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let store = state
        .wiremsg_store
        .as_ref()
        .ok_or_else(|| "wiremsg_store not initialized".to_string())?;

    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_send: 'from' required".to_string())?
        .to_string();
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    // body は任意 JSON。 省略時は空 object。
    let body = payload
        .get("body")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let reply_to = payload
        .get("reply_to")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    match reply_to {
        // ---- reply ----
        Some(prev_id) => {
            let msg = store
                .send_reply(&from, &to, body, &prev_id)
                .await
                .map_err(|e| format!("wire_send (reply) failed: {e}"))?;
            // notify 対象 = reply の to。 send_reply の carry-forward で確定済の参加者集合で、
            // 送信者と left は既に除外済。 thread 全走査は不要 (prev carry-forward の帰結)。
            for agent in &msg.to {
                state.wire_notifier.notify(agent).await;
            }
            // R3: reply の to (= carry-forward 済) に remote 宛があれば best-effort forward。
            forward_remote_recipients(state, &msg).await;
            Ok(serde_json::json!({
                "status": "ok",
                "id": msg.id,
                "prev": msg.prev,
                "local_seq": msg.local_seq,
                "notified": msg.to.len(),
            }))
        }
        // ---- new thread (root) ----
        None => {
            let msg = store
                .send_root(&from, &to, body)
                .await
                .map_err(|e| format!("wire_send (root) failed: {e}"))?;
            // 受信者 (= to、 送信者自身は除く) を notify
            for agent in to.iter().filter(|a| **a != from) {
                state.wire_notifier.notify(agent).await;
            }
            // R3: root の to に remote 宛があれば best-effort forward。
            forward_remote_recipients(state, &msg).await;
            Ok(serde_json::json!({
                "status": "ok",
                "id": msg.id,
                "prev": serde_json::Value::Null,
                "local_seq": msg.local_seq,
                "notified": to.iter().filter(|a| **a != from).count(),
            }))
        }
    }
}

/// R3: 確定した wire message の `to` を分類し、 remote 宛があれば best-effort で forward する
///
/// `handle_wire_send` の root / reply 両 path から、 ローカル INSERT の **後** に呼ぶ。
/// message の `to` (reply なら carry-forward 済の参加者集合) を `classify_recipients` で
/// local / remote に振り分け、 `agent@<other-project>` のような remote 宛があれば受信側 SP に
/// HTTP forward する。
///
/// **best-effort 確定** (決定 `mem_1CbDYnc7GjZkXXWgm9PAfK`): forward 失敗は
/// `forward_to_remote` 内で log のみ。 `wire_send` 自体は成功で返る (= ローカル INSERT は
/// 既に完了済)。 retry はしない。
async fn forward_remote_recipients(state: &AppState, msg: &crate::capability::WireMessage) {
    // 自 project が未確定 (World mode 等) なら cross-process forward しない。
    if state.project_name.is_empty() {
        return;
    }
    let recipients = crate::capability::classify_recipients(&msg.to, &state.project_name);
    if recipients.has_remote() {
        crate::capability::forward_to_remote(
            crate::cli::WORLD_PORT,
            &recipients.remote_projects,
            msg,
        )
        .await;
    }
}

/// wiremsg を受信する (= 呼び出し agent の参加 thread の未読 message を long-poll で取得)
///
/// payload: `{ agent, timeout? }`
///
/// Operations (Phase A ① / R1 仕様):
/// 1. `agent` 宛 (`to_addrs ∋ agent`) で `local_seq > read_cursor` の未読 message を取得
/// 2. 未読があれば返却 → 取得した最新 message の `local_seq` まで cursor を前進
///    (R1: cursor は厳密単調な local_seq。 同一 ms 衝突や clock skew で取りこぼさない)
/// 3. 未読が無ければ `WireNotifier` で待機 (timeout あり)、 notify で起床して再 poll
///
/// 取りこぼし防止のため、 `notified()` future を **store poll の前に** 生成する
/// (= `WireNotifier` の struct doc 参照)。
pub(crate) async fn handle_wire_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let store = state
        .wiremsg_store
        .as_ref()
        .ok_or_else(|| "wiremsg_store not initialized".to_string())?;

    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?
        .to_string();
    // timeout は msg_recv と同じ default 5s / max 30s
    let timeout_secs = payload
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .min(30);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        // 取りこぼし防止: poll の前に待機 future を準備しておく
        // (poll と await の隙間に来た wire_send notify も拾えるようにする)
        let notify = state.wire_notifier.handle(&agent).await;
        let notified = notify.notified();
        tokio::pin!(notified);

        // 未読 message を取得 + read_cursor 前進 (store.recv が両方まとめて実施)。
        // cursor は取得済 message の local_seq 最大値に合わせる (R1: 厳密単調で取りこぼし無し)。
        let unread = store
            .recv(&agent)
            .await
            .map_err(|e| format!("wire_recv failed: {e}"))?;

        if !unread.is_empty() {
            let value = serde_json::to_value(&unread)
                .map_err(|e| format!("wire_recv serialize failed: {e}"))?;
            return Ok(serde_json::json!({
                "messages": value,
                "count": unread.len(),
            }));
        }

        // 未読なし: deadline まで notify を待つ
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(serde_json::json!({
                "messages": [],
                "count": 0,
                "reason": "timeout",
            }));
        }
        // notify が来たら再 poll、 timeout したら次ループで deadline 判定 → timeout 返却
        let _ = tokio::time::timeout(remaining, notified).await;
    }
}

/// wiremsg の ancestor-chain (系譜) を取得する (R2、 設計 mem_1CbDLrECNZiNEZqjySLfSB)
///
/// payload: `{ message_id }`
///
/// 指定 message から `prev` を root まで辿った **系譜** (root-first・chronological) を
/// 返す。 `wire_recv` の増分 drain とは対の read-only tool で、 cursor を一切触らない
/// (冪等)。 thread に途中参加した agent が backlog (= 文脈) を取得する用途。
///
/// 戻り値: `{ status: "ok", messages: [..], count: <n> }`。
/// `message_id` の message が存在しなければ `Err`。
pub(crate) async fn handle_wire_thread(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let store = state
        .wiremsg_store
        .as_ref()
        .ok_or_else(|| "wiremsg_store not initialized".to_string())?;

    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?
        .to_string();

    // ancestor-chain (root-first) を取得。 cursor は触らない (read-only)。
    let chain = store
        .ancestor_chain(&message_id)
        .await
        .map_err(|e| format!("wire_thread failed: {e}"))?;
    let value =
        serde_json::to_value(&chain).map_err(|e| format!("wire_thread serialize failed: {e}"))?;
    Ok(serde_json::json!({
        "status": "ok",
        "messages": value,
        "count": chain.len(),
    }))
}
