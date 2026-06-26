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
    let query = payload["query"]
        .as_str()
        .ok_or_else(|| "query が必要です".to_string())?;

    // lane address（`<project>/conductor` / `<project>/performer/<name>`）なら実 session に解決。
    // `tmux send-keys -t <session>` で active pane に届くため、 単一 TmuxActor の束縛 session や
    // agent_metadata を介さずに任意 lane へ nudge できる（fix-tmux-session-naming 根治経路）。
    if let Some(session) = state.resolve_lane_session(query).await {
        return Ok(
            serde_json::json!({"status": "ok", "pane_id": session, "meta": serde_json::Value::Null}),
        );
    }

    let handle = state
        .ensure_tmux()
        .await
        .ok_or_else(|| "tmux 未使用環境です".to_string())?;
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

/// SP "process" channel の method dispatch（reverse-routing と共有する単一の入口）。
///
/// SP の "process" Unison channel handler と、 World reverse-routing 経由 (SP control
/// keepalive) の **両方**がこの関数を呼ぶことで、 「MCP が SP 直結」「MCP → World → SP
/// reverse」どちらの経路でも同一の dispatch ロジック・同一の AppState 操作になる
/// (L0 SP-portless: SP listen port を World 単一 endpoint に寄せても挙動不変)。
/// S2 (doc 27 §4.1): terminal demand start ハンドラー。
///
/// World の TopicRouter demand hook が `process/terminal/data/{lane}/out` の購読者を
/// 検知し、 control reverse-route で本 method を撃つ。 SP は当該 Lane の PtySlot output
/// broadcast を購読する pump を spawn し、 per-lane terminal topic に route し始める
/// (= 購読者が居る間だけ pump を回す demand-driven production)。
async fn handle_terminal_demand_start(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("terminal_demand_start: lane 未指定".to_string());
    }
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(&lane) else {
        return Err(format!("terminal_demand_start: lane パース失敗: {}", lane));
    };

    // 当該 Lane の PtySlot output broadcast を購読 (Lane 不在 / PtySlot 無 = None)。
    let rx = state.lane_pool.read().await.subscribe_output(&addr);
    let Some(rx) = rx else {
        // pump は張れないが demand 自体は受理 (Lane 起動後の再 demand 余地を残す)。
        tracing::debug!("terminal_demand_start: Lane に PtySlot 無 (lane={})", lane);
        return Ok(serde_json::json!({"status": "no_lane", "lane": lane}));
    };

    let handle = crate::process::terminal_pump::spawn_lane_terminal_pump(
        lane.clone(),
        rx,
        state.topic_router.clone(),
    );
    // 既存 pump があれば差し替え (二重 demand_start でも 1 本に収束)。
    if let Some(old) = state
        .terminal_pumps
        .write()
        .await
        .insert(lane.clone(), handle)
    {
        old.abort();
    }
    tracing::info!("terminal pump start (lane={})", lane);
    Ok(serde_json::json!({"status": "started", "lane": lane}))
}

/// S2: terminal demand stop ハンドラー。 最後の購読者が消えたら pump を abort する。
async fn handle_terminal_demand_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("terminal_demand_stop: lane 未指定".to_string());
    }
    let removed = state.terminal_pumps.write().await.remove(&lane);
    match removed {
        Some(handle) => {
            handle.abort();
            tracing::info!("terminal pump stop (lane={})", lane);
            Ok(serde_json::json!({"status": "stopped", "lane": lane}))
        }
        None => Ok(serde_json::json!({"status": "not_running", "lane": lane})),
    }
}

pub(crate) async fn dispatch_process_method(
    state: &Arc<AppState>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        // switch_lane も generic broadcast 経路に乗せる（B1: 遠隔 active Lane 制御）。
        // hub → topic `process/paisley-park/event/switch-lane`（一時コマンド=非
        // retained）→ canvas channel → vp-app が受信して active Lane を切り替える。
        "show" | "clear" | "toggle_pane" | "split_pane" | "close_pane" | "switch_lane" => {
            handle_process_message(state, payload)
        }
        "watch_file" => handle_watch_file(state, payload).await,
        "unwatch_file" => handle_unwatch_file(state, payload).await,
        // S2: demand-driven terminal pump (World demand hook → control reverse-route)
        "terminal_demand_start" => handle_terminal_demand_start(state, payload).await,
        "terminal_demand_stop" => handle_terminal_demand_stop(state, payload).await,
        "tmux_split" => handle_tmux_split(state, payload).await,
        "tmux_list" => handle_tmux_list(state).await,
        "tmux_close" => handle_tmux_close(state, payload).await,
        "tmux_capture" => handle_tmux_capture(state, payload).await,
        "tmux_capture_all" => handle_tmux_capture_all(state).await,
        // エージェントメタデータ
        "tmux_set_agent_meta" => handle_tmux_set_agent_meta(state, payload).await,
        "tmux_update_agent_status" => handle_tmux_update_agent_status(state, payload).await,
        "tmux_clear_agent_meta" => handle_tmux_clear_agent_meta(state, payload).await,
        "tmux_send_keys" => handle_tmux_send_keys(state, payload).await,
        "tmux_resolve_pane" => handle_tmux_resolve_pane(state, payload).await,
        // ProcessRunner
        "process_run" => handle_process_run(state, payload).await,
        "process_stop" => handle_process_stop(state, payload).await,
        "process_inject" => handle_process_inject(state, payload).await,
        "process_list" => handle_process_list(state).await,
        // wiremsg threaded inbox (Phase A ①、 R2 で wire_thread 追加)
        "wire_send" => handle_wire_send(state, payload).await,
        "wire_recv" => handle_wire_recv(state, payload).await,
        "wire_thread" => handle_wire_thread(state, payload).await,
        // flow_progress 用 read-only 未読 count (cursor 不触り)
        "wire_unread_count" => handle_wire_unread_count(state, payload).await,
        // flow_progress 5-state FSM derive 用 read-only 最新 wmsg
        "wire_latest_msg" => handle_wire_latest_msg(state, payload).await,
        "wire_ack" => handle_wire_ack(state, payload).await,
        _ => Err(format!("不明なメソッド: process.{}", method)),
    }
}

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

                        // L0 SP-portless (control slice): method dispatch は
                        // dispatch_process_method に抽出済 (World reverse-routing と共有)。
                        let result = dispatch_process_method(&state, &method, payload).await;

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
// wiremsg ハンドラー (R2-a: TheWorld 中央 store への proxy 層)
//
// store 直結のロジックは routes/wire.rs (TheWorld 側) に移設済。 SP の責務は
// 「アドレス正規化 (N1) → TheWorld へ HTTP relay」 のみ。 QUIC dispatch と
// HTTP wrapper (routes/health.rs) は本 proxy 群を呼ぶため signature 不変。
// =============================================================================

/// agent address を canonical (qualified) 形に正規化する (wiremsg N1、 refactor R1 PR-B)
///
/// bare `"agent"` を qualified (`agent@<project>`) に正規化する。
///
/// 現行 MCP (`SelfLane::from_address`) は conductor も canonical `agent@<project>` を
/// 自前で送るため、本関数は実質 **冪等な素通し + 後方互換 (旧 client / bare 送信者) 用の
/// 防御層**。bare を残す理由: 旧 bare 送信が来ても store 識別子を qualified 一本に揃え、
/// cross-process 返信 (`agent@<project>` 宛 forward) が bare query と完全一致せず届かない
/// バグ (B2、 レビュー mem_1CbuxQuNRwHBiZgBVUWVfN) を防ぐため。
/// bare 以外 (qualified / canvas@... / gold_experience@... 等) はそのまま返す。
///
/// ⚠️ 正規化先 `self_project` は「繋いだ SP の project」なので、bare のままだと誤 SP 接続で
/// identity が化ける (= 旧 conductor バグの根)。だから identity の SSOT は MCP 側 canonical
/// 送出に移した。本関数は qualified を受けたら何もしない (= SP 非依存) のが正常運用。
fn normalize_agent_addr(addr: &str, self_project: &str) -> String {
    if addr == "agent" {
        format!("agent@{}", self_project)
    } else {
        addr.to_string()
    }
}

/// wiremsg を送信する (R2-a: TheWorld 中央 store への proxy)
///
/// payload: `{ from, to: [..], body, reply_to? }`
///
/// SP の責務はアドレス正規化 (N1: bare `"agent"` → `"agent@<self_project>"`) のみ。
/// 保存・notify・local_seq 採番・body coerce は全て TheWorld 側
/// ([`crate::process::routes::wire`])。 cross-process forward は中央化で概念ごと消滅。
pub(crate) async fn handle_wire_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_send: 'from' required".to_string())?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| normalize_agent_addr(s, &state.project_name))
                .collect()
        })
        .unwrap_or_default();
    let mut forwarded = serde_json::json!({
        "from": from,
        "to": to,
    });
    if let Some(body) = payload.get("body") {
        forwarded["body"] = body.clone();
    }
    if let Some(reply_to) = payload.get("reply_to") {
        forwarded["reply_to"] = reply_to.clone();
    }
    super::world_wire::call("/api/wire/send", forwarded).await
}

/// wiremsg を受信する (R2-a: TheWorld 中央 store への proxy、 long-poll は TheWorld 側)
///
/// payload: `{ agent, timeout? }` — timeout の clamp (default 5s / max 30s) も TheWorld 側。
pub(crate) async fn handle_wire_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?;
    let timeout = payload.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);
    super::world_wire::call(
        "/api/wire/recv",
        serde_json::json!({ "agent": agent, "timeout": timeout }),
    )
    .await
}

/// wiremsg の ancestor-chain (系譜) を取得する (R2-a: TheWorld proxy、 read-only)
///
/// payload: `{ message_id }` — agent 文脈不要のため正規化なしで relay。
pub(crate) async fn handle_wire_thread(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _ = state; // thread は project 文脈 (正規化) 不要。 signature は他 handler と統一
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?;
    super::world_wire::call(
        "/api/wire/thread",
        serde_json::json!({ "message_id": message_id }),
    )
    .await
}

/// wiremsg の agent 関与最新 message を取得する (R2-a: TheWorld proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の 5-state FSM derive で使う。
pub(crate) async fn handle_wire_latest_msg(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_latest_msg: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/latest-msg",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の per-agent 未読 count を取得する (R2-a: TheWorld proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の集約 view / `wire_inbox` MCP tool で使う。
pub(crate) async fn handle_wire_unread_count(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_unread_count: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/unread-count",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg を ack する (R2-a 新設、 決定 D3: cursor 非破壊の ack 台帳への proxy)
///
/// payload: `{ message_id, agent }` → `{ status, acked }`
pub(crate) async fn handle_wire_ack(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::normalize_agent_addr;

    #[test]
    fn normalize_bare_agent_to_qualified() {
        assert_eq!(normalize_agent_addr("agent", "vp"), "agent@vp");
    }

    #[test]
    fn normalize_keeps_qualified_and_other_addrs() {
        assert_eq!(normalize_agent_addr("agent@vp", "vp"), "agent@vp");
        assert_eq!(normalize_agent_addr("agent@other", "vp"), "agent@other");
        assert_eq!(normalize_agent_addr("agent@vp/sub", "vp"), "agent@vp/sub");
        assert_eq!(normalize_agent_addr("canvas@vp", "vp"), "canvas@vp");
    }

    // =========================================================================
    // S2 (doc 27 §4.1): demand-driven terminal pump の SP 側 e2e
    // =========================================================================

    /// 実 PtySlot を lane_pool に仕込み、 demand_start → pump 起動 → PTY 出力が
    /// per-lane terminal topic に届く → demand_stop → pump 除去、 を 1 本で検証する
    /// (World 側 demand hook の reverse-route 先 = SP dispatch の責務範囲)。
    #[tokio::test]
    async fn terminal_demand_start_routes_pty_output_then_stop() {
        use super::dispatch_process_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::process::lanes_state::LaneAddress;
        use crate::process::state::build_test_app_state;
        use crate::protocol::ProcessMessage;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::conductor("vp");
        let lane = addr.to_string(); // "vp/conductor"

        // 実 PtySlot を attach (subscribe_output が Some を返す前提を作る)。
        {
            let (slot, rx) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), slot, rx);
        }

        // surface 相当: SP topic_router に per-lane terminal topic を購読。
        let topic = format!("process/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start → pump 起動。
        let started = dispatch_process_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(started["status"], "started");
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump が登録される"
        );

        // shell プロンプト等の PTY 出力が terminal topic に流れてくる。
        let (rtopic, msg) = tokio::time::timeout(Duration::from_secs(5), srx.recv())
            .await
            .expect("PTY 出力が terminal topic に届かない (timeout)")
            .expect("topic channel closed");
        assert_eq!(rtopic, topic);
        assert!(matches!(msg, ProcessMessage::LaneTerminalOutput { .. }));

        // demand_stop → pump abort + map 除去。
        let stopped = dispatch_process_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_stop");
        assert_eq!(stopped["status"], "stopped");
        assert!(
            !state.terminal_pumps.read().await.contains_key(&lane),
            "pump が除去される"
        );
    }

    /// PtySlot を持たない Lane への demand_start は graceful に no_lane を返し pump を張らない。
    #[tokio::test]
    async fn terminal_demand_start_without_lane_is_graceful() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": "vp/conductor" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "no_lane");
        assert!(state.terminal_pumps.read().await.is_empty());
    }
}
