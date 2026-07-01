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

// L0 portless Group B-3: 旧 SP HTTP `/api/ruby/*` を process-proxy ask に移管。 HTTP handler と同じ
// `process_runner::ruby_*` core を呼ぶ薄い adapter (payload からフィールド抽出)。 ruby_list は
// `process_registry.list()` = `handle_process_list` と同一なので dispatch 側で再利用する。

/// ruby_eval: 短命 Ruby 実行
async fn handle_ruby_eval(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let r = crate::process::process_runner::ruby_eval(
        code,
        file,
        pane_id,
        &state.project_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({
        "status": "ok",
        "stdout": r.stdout,
        "stderr": r.stderr,
        "exit_code": r.exit_code,
        "elapsed_ms": r.elapsed_ms,
    }))
}

/// ruby_run: 長命 Ruby daemon 起動
async fn handle_ruby_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let name = payload.get("name").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let process_id = crate::process::process_runner::ruby_run(
        &state.process_registry,
        code,
        file,
        name,
        pane_id,
        &state.project_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// ruby_stop: Ruby daemon 停止
async fn handle_ruby_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::process::process_runner::ruby_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// Terminal チャネル制御メッセージハンドラー
// =============================================================================

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
    if crate::process::lanes_state::LanePool::parse_address(&lane).is_none() {
        return Err(format!("terminal_demand_start: lane パース失敗: {}", lane));
    }

    // 当該 Lane の現行 PtySlot に pump を張る。 Lane 不在 / PtySlot 無 = pump 張れず
    // (demand 自体は受理 = Lane 起動後の再 demand 余地を残す)。
    if respawn_terminal_pump(state, &lane).await {
        Ok(serde_json::json!({"status": "started", "lane": lane}))
    } else {
        Ok(serde_json::json!({"status": "no_lane", "lane": lane}))
    }
}

/// 指定 Lane の **現時点の** PtySlot output に terminal pump を張り直す (idempotent)。
///
/// `subscribe_output` で今の PtySlot broadcast を取得して pump を spawn、 既存 pump handle が
/// あれば `abort()` して差し替える (二重 demand_start / restart 後の付け替えでも 1 本に収束)。
/// Lane に PtySlot が無ければ `false` (pump は張れない)。
///
/// demand hook (購読 0→1) の start 経路と、 restart_lane 後の pump 付け替え (BUG#1: restart で
/// slot を差し替えても World 側 subscriber は張りっぱなしで demand が再発火しない) が、
/// この単一経路を共有する。 `lane` は LaneAddress の Display 形。
pub(crate) async fn respawn_terminal_pump(state: &AppState, lane: &str) -> bool {
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return false;
    };
    let rx = state.lane_pool.read().await.subscribe_output(&addr);
    let Some(rx) = rx else {
        tracing::debug!("respawn_terminal_pump: Lane に PtySlot 無 (lane={})", lane);
        return false;
    };
    let handle = crate::process::terminal_pump::spawn_lane_terminal_pump(
        lane.to_string(),
        rx,
        state.topic_router.clone(),
    );
    if let Some(old) = state
        .terminal_pumps
        .write()
        .await
        .insert(lane.to_string(), handle)
    {
        old.abort();
    }
    tracing::info!("terminal pump start (lane={})", lane);
    true
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

/// S3 (doc 27 §4.1, 経路 B): terminal 入力。
///
/// surface (vp-app) → World canvas channel (upstream request) → SP control → 本 dispatch。
/// `data` は base64 (出力 pump の encoding と対称、 任意バイトを JSON で運ぶため)。 decode して
/// 当該 Lane の PtySlot に書き込む。
async fn handle_terminal_write(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_write: lane 未指定".to_string());
    }
    let data_b64 = payload.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| format!("terminal_write: base64 decode 失敗: {}", e))?;
    vp_paths::term_trace("B:sp-recv", lane, &bytes);
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_write: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .write_to_lane(&addr, &bytes)
        .map_err(|e| format!("terminal_write 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// S3: terminal resize。 PtySlot (+ TermAttach grid) を cols×rows に同期する。
async fn handle_terminal_resize(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_resize: lane 未指定".to_string());
    }
    // bounds check: 0 / 極大値で PTY に不正 dims を渡さない (u64→u16 silent wrap も防ぐ)。
    // 旧 daemon "terminal" channel の resize 経路と同じ範囲 (1..=1000)。
    let cols = payload.get("cols").and_then(|v| v.as_u64()).unwrap_or(80);
    let rows = payload.get("rows").and_then(|v| v.as_u64()).unwrap_or(24);
    if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
        return Err(format!(
            "terminal_resize: 不正な dims (cols={cols} rows={rows})"
        ));
    }
    let (cols, rows) = (cols as u16, rows as u16);
    let Some(addr) = crate::process::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_resize: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .resize_lane(&addr, cols, rows)
        .map_err(|e| format!("terminal_resize 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "cols": cols, "rows": rows}))
}

/// F6 (doc 27 §3.4.5/§6): PP Canvas state save。 旧 SP HTTP `POST /api/pp/state` を
/// process-proxy ask に移管（surface→SP 直結 HTTP を撤去、 World 経由の ask に統一）。
/// logic は旧 `pp_state_save_handler` から移設（HTTP route は削除）。
async fn handle_pp_state_save(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("pp_state_save: vpdb 未初期化".to_string());
    };
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("pp_state_save: pane_id 必須")?
        .to_string();
    let content_type = payload
        .get("content_type")
        .and_then(|v| v.as_str())
        .unwrap_or("markdown")
        .to_string();
    let content = payload
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // lane: null/空/"conductor" は None(=lane IS NULL) に正規化（load 側と key 一致）。
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "conductor")
        .map(|s| s.to_string());
    let stack = payload.get("stack").filter(|v| !v.is_null()).cloned();
    let ui_state = payload.get("ui_state").filter(|v| !v.is_null()).cloned();
    vpdb.upsert_pp_state(
        &state.project_dir,
        lane.as_deref(),
        &pane_id,
        &content_type,
        &content,
        title.as_deref(),
        stack.as_ref(),
        ui_state.as_ref(),
    )
    .await
    .map_err(|e| format!("pp_state upsert 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "saved"}))
}

/// F6: PP Canvas state load。 旧 SP HTTP `GET /api/pp/state` を process-proxy ask に移管。
async fn handle_pp_state_load(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("pp_state_load: vpdb 未初期化".to_string());
    };
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("paisley-park")
        .to_string();
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "conductor")
        .map(|s| s.to_string());
    match vpdb
        .load_pp_state(&state.project_dir, lane.as_deref(), &pane_id)
        .await
    {
        Ok(Some(rec)) => Ok(serde_json::json!({"status": "ok", "record": rec})),
        Ok(None) => Ok(serde_json::json!({"status": "empty"})),
        Err(e) => Err(format!("pp_state load 失敗: {}", e)),
    }
}

/// F6② (doc 27 §3.4.5/§6): Lane delete。 旧 SP HTTP `DELETE /api/lanes` を process-proxy ask に
/// 移管（surface→SP 直結 HTTP を撤去、 World 経由の ask に統一）。 logic は旧 `delete_handler`
/// から移設し、 core の `delete_lane_orchestrated` を再利用（HTTP route + handler は削除）。
async fn handle_lane_delete(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_delete: address 必須")?;
    // cleanup default = true (旧 DeleteLaneQuery default_cleanup と一致、 dir も rm する)。
    let cleanup = payload
        .get("cleanup")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let addr = crate::process::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_delete: invalid lane address: {}", address))?;
    match super::routes::lanes::delete_lane_orchestrated(state, addr, cleanup).await {
        Ok(info) => Ok(serde_json::json!({
            "deleted": info.address,
            "pid": info.pid,
            "tmux_killed": info.tmux_killed,
            "cleanup": info.cleanup_status,
        })),
        Err(e) => Err(e.to_string()),
    }
}

/// F6③ (doc 27 §3.4.5/§6): Lane restart。 旧 SP HTTP `POST /api/lanes/restart` を process-proxy
/// ask に移管。 core の `restart_lane_orchestrated` (VP-131 透過 retry loop) を呼ぶ薄い adapter。
async fn handle_lane_restart(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_restart: address 必須")?;
    // fresh default=false (旧 RestartLaneQuery の #[serde(default)] と一致)。
    let fresh = payload
        .get("fresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let addr = crate::process::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_restart: invalid lane address: {}", address))?;
    super::routes::lanes::restart_lane_orchestrated(state, addr, fresh).await
}

/// lanes portless (doc 27 §3.4.5): Lane create。 旧 SP HTTP `POST /api/lanes` を process-proxy ask に
/// 移管。 core の `create_performer_orchestrated` (lane clone + PtySlot spawn) を呼ぶ薄い adapter。
/// payload は `CreateLaneReq` 互換 JSON (kind/name/stand?/cwd?/branch?)。 成功は LaneInfo JSON、
/// 失敗は core が返す String error (旧 HTTP の CONFLICT="already exists" 等を保持)。
async fn handle_lane_create(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: super::routes::lanes::CreateLaneReq = serde_json::from_value(payload)
        .map_err(|e| format!("lane_create: invalid payload: {}", e))?;
    let info = super::routes::lanes::create_performer_orchestrated(state, req).await?;
    serde_json::to_value(&info).map_err(|e| format!("lane_create: LaneInfo serialize 失敗: {}", e))
}

/// lanes portless (doc 27 §3.4.5): Lane list。 旧 SP HTTP `GET /api/lanes` を process-proxy ask に
/// 移管。 core の `build_lanes_snapshot` を呼び `{lanes:[...]}` で wrap (旧 HTTP `LanesResponse` 互換)。
async fn handle_lanes_list(state: &Arc<AppState>) -> Result<serde_json::Value, String> {
    let lanes = super::routes::lanes::build_lanes_snapshot(state).await;
    Ok(serde_json::json!({ "lanes": lanes }))
}

/// F6④ (doc 27 §3.4.5/§6): Stand 一覧。 旧 SP HTTP `GET /api/stands` を process-proxy ask に移管。
/// install root の mise task scan は process-global (TTL cache、 per-project state 不要) なので
/// state/payload 不問。 wire wrapping (`{stands:[...]}`) は旧 HTTP handler と互換で本 dispatch 側が担う。
async fn handle_stands_list() -> Result<serde_json::Value, String> {
    let stands = super::routes::stands::list_stands_cached().await;
    Ok(serde_json::json!({ "stands": stands }))
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
        // S3: terminal 入力/resize (surface → canvas channel upstream → control reverse-route)
        "terminal_write" => handle_terminal_write(state, payload).await,
        "terminal_resize" => handle_terminal_resize(state, payload).await,
        // F6: PP Canvas state (旧 SP HTTP /api/pp/state を process-proxy ask に移管)
        "pp_state_save" => handle_pp_state_save(state, payload).await,
        "pp_state_load" => handle_pp_state_load(state, payload).await,
        // lanes portless: Lane create/list (旧 SP HTTP POST/GET /api/lanes を process-proxy ask に移管)
        "lane_create" => handle_lane_create(state, payload).await,
        "lanes_list" => handle_lanes_list(state).await,
        // F6②: Lane delete (旧 SP HTTP DELETE /api/lanes を process-proxy ask に移管)
        "lane_delete" => handle_lane_delete(state, payload).await,
        // F6③: Lane restart (旧 SP HTTP POST /api/lanes/restart を process-proxy ask に移管)
        "lane_restart" => handle_lane_restart(state, payload).await,
        // F6④: Stand 一覧 (旧 SP HTTP GET /api/stands を process-proxy ask に移管)
        "stands_list" => handle_stands_list().await,
        // L0 finale: SP graceful shutdown を QUIC で (旧 SP HTTP POST /api/shutdown を置換、
        // World stop_process / restart_process 用)。 shutdown_token.cancel() で graceful 停止
        // (DB close 等)。 SP が即 QUIC server を畳むため応答が返らない事もあるが best-effort。
        "shutdown" => {
            tracing::info!("Shutdown requested via QUIC dispatch");
            state.shutdown_token.cancel();
            Ok(serde_json::json!({"status": "shutting_down"}))
        }
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
        // L0 portless Group B-3: Ruby VM (旧 SP HTTP /api/ruby/* を process-proxy ask に移管)。
        // ruby_list は process_registry.list() = process_list と同一なので handle_process_list 再利用。
        "ruby_eval" => handle_ruby_eval(state, payload).await,
        "ruby_run" => handle_ruby_run(state, payload).await,
        "ruby_stop" => handle_ruby_stop(state, payload).await,
        "ruby_list" => handle_process_list(state).await,
        // wiremsg threaded inbox (Phase A ①、 R2 で wire_thread 追加)
        "wire_send" => handle_wire_send(state, payload).await,
        "wire_recv" => handle_wire_recv(state, payload).await,
        "wire_thread" => handle_wire_thread(state, payload).await,
        // flow_progress 用 read-only 未読 count (cursor 不触り)
        "wire_unread_count" => handle_wire_unread_count(state, payload).await,
        // flow_progress 5-state FSM derive 用 read-only 最新 wmsg
        "wire_latest_msg" => handle_wire_latest_msg(state, payload).await,
        "wire_ack" => handle_wire_ack(state, payload).await,
        // Agent 委譲 (doc 28 §4): delegate=B を wake / complete=A を wake /
        // respond=NeedsInput(Reborn) に A が回答して B を再 wake (Active へ loop)。
        "delegate" => super::delegation::handle_delegate(state, payload).await,
        "complete" => super::delegation::handle_complete(state, payload).await,
        "respond" => super::delegation::handle_respond(state, payload).await,
        _ => Err(format!("不明なメソッド: process.{}", method)),
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

    /// S3: terminal_write の base64 入力が実 PTY に届き (echo 出力で確認)、 terminal_resize が
    /// status ok を返す。 surface→World→SP control の終端 = SP dispatch の責務範囲を検証する。
    #[tokio::test]
    async fn terminal_write_reaches_pty_and_resize_ok() {
        use super::dispatch_process_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::process::lanes_state::LaneAddress;
        use crate::process::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::conductor("vp");
        let lane = addr.to_string();

        {
            let (slot, rx) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), slot, rx);
        }

        // PTY 出力を write 前に購読 (echo を取りこぼさない)。
        let mut out = state
            .lane_pool
            .read()
            .await
            .subscribe_output(&addr)
            .expect("subscribe_output");

        // シェル初期化待ち。
        tokio::time::sleep(Duration::from_millis(500)).await;

        // terminal_write: base64 の "echo VP_S3_OK\n" を PtySlot に届ける。
        let data = base64::engine::general_purpose::STANDARD.encode(b"echo VP_S3_OK\n");
        let res = dispatch_process_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": lane, "data": data }),
        )
        .await
        .expect("terminal_write");
        assert_eq!(res["status"], "ok");

        // 出力に "VP_S3_OK" が現れる (= 入力が実 PTY に届いた)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), out.recv()).await {
                Ok(Ok(bytes)) => {
                    if String::from_utf8_lossy(&bytes).contains("VP_S3_OK") {
                        found = true;
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert!(found, "terminal_write の入力が PTY 出力に反映されない");

        // terminal_resize: status ok + cols/rows echo。
        let res = dispatch_process_method(
            &state,
            "terminal_resize",
            serde_json::json!({ "lane": lane, "cols": 120, "rows": 40 }),
        )
        .await
        .expect("terminal_resize");
        assert_eq!(res["status"], "ok");
        assert_eq!(res["cols"], 120);
        assert_eq!(res["rows"], 40);
    }

    /// PtySlot を持たない Lane への terminal_write は Err (lane 不在を上位に伝える)。
    #[tokio::test]
    async fn terminal_write_unknown_lane_errs() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;
        use base64::Engine;

        let state = build_test_app_state(None).await;
        let data = base64::engine::general_purpose::STANDARD.encode(b"x");
        let res = dispatch_process_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": "vp/conductor", "data": data }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への write は Err");
    }

    /// F6②: lane_delete dispatch e2e — performer lane を pool に作り、 lane_delete で除去できる。
    /// 二度目の delete は LaneNotFound で Err (= idempotent re-call の契約)。 Err message が
    /// "Lane not found" を含むことも固定する (MCP/CLI の idempotent 判定がこの文字列に依存)。
    #[tokio::test]
    async fn lane_delete_removes_performer_and_idempotent() {
        use super::dispatch_process_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneKind, LaneState};
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::performer("vp", "chore");
        let address = addr.to_string();

        {
            let (slot, rx) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24).expect("PTY spawn");
            let mut pool = state.lane_pool.write().await;
            // delete は lanes map (LaneInfo) を remove するので LaneInfo + PtySlot 両方を登録する。
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                kind: LaneKind::Performer,
                name: Some("chore".to_string()),
                state: LaneState::Running,
                stand: "echoes".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                performer_status: None,
                tmux: Vec::new(),
                cc_session_id: None,
            });
            pool.insert_pty_slot(addr.clone(), slot, rx);
        }

        // cleanup=false: test lane に実 workspace dir はないので Phase 2b (fs rm) をスキップ。
        let res = dispatch_process_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect("lane_delete");
        assert_eq!(res["deleted"], address);

        // pool から PtySlot が消えている (subscribe_output が None)。
        assert!(
            state
                .lane_pool
                .read()
                .await
                .subscribe_output(&addr)
                .is_none(),
            "lane_delete 後も PtySlot が pool に残っている"
        );

        // 二度目の delete は LaneNotFound で Err (idempotent re-call の契約)。
        let err = dispatch_process_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect_err("既に消えた lane の delete は Err (LaneNotFound)");
        assert!(
            err.contains("Lane not found"),
            "Err message に LaneNotFound が含まれる (MCP/CLI の idempotent 判定が依存): {err}"
        );
    }

    /// F6②: Conductor lane は lane_delete で拒否される (architecture rule: project lifetime 紐付き)。
    #[tokio::test]
    async fn lane_delete_rejects_conductor() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // delete_lane_orchestrated は LanePool の有無に関係なく kind=Conductor を最初に弾く。
        let err = dispatch_process_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": "vp/conductor" }),
        )
        .await
        .expect_err("Conductor の delete は Err");
        assert!(
            err.contains("Conductor"),
            "Conductor delete は ConductorCannotBeDeleted: {err}"
        );
    }

    /// F6③: lane_restart dispatch — 存在しない lane の restart は透過 retry (3 attempts) 後 Err。
    /// dispatch 配線 + restart_lane_orchestrated 到達を確認 (respawn 成功 path は実機検証で担保)。
    #[tokio::test]
    async fn lane_restart_unknown_lane_errs() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(
            &state,
            "lane_restart",
            serde_json::json!({ "address": "vp/performer/ghost" }),
        )
        .await;
        assert!(res.is_err(), "存在しない lane の restart は Err");
    }

    /// F6④: stands_list dispatch — process-proxy ask が `{stands:[...]}` 形で返る。
    /// list_stands_cached は mise 不在 (CI) でも空 Vec に graceful degrade するので、 配線 +
    /// wire shape (stands array 常在) を CI でも固定できる (実 stand 内容は stands.rs の
    /// mise-gated test が担保)。
    #[tokio::test]
    async fn stands_list_returns_stands_array() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(&state, "stands_list", serde_json::json!({}))
            .await
            .expect("stands_list dispatch");
        assert!(
            res.get("stands").map(|s| s.is_array()).unwrap_or(false),
            "stands_list は {{stands:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lanes_list` dispatch arm が `{lanes:[...]}` 形で返る (build_lanes_snapshot 経由)。
    #[tokio::test]
    async fn lanes_list_returns_lanes_array() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_process_method(&state, "lanes_list", serde_json::json!({}))
            .await
            .expect("lanes_list dispatch");
        assert!(
            res.get("lanes").map(|s| s.is_array()).unwrap_or(false),
            "lanes_list は {{lanes:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lane_create` dispatch arm が validation error (kind != performer) を
    /// unison error frame (= Err) として返す (core の create_performer_orchestrated に到達している証)。
    #[tokio::test]
    async fn lane_create_rejects_non_performer() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let err = dispatch_process_method(
            &state,
            "lane_create",
            serde_json::json!({ "kind": "worker", "name": "x" }),
        )
        .await
        .expect_err("kind='worker' は Err");
        assert!(
            err.contains("kind must be 'performer'"),
            "error は kind 制約を含む: {err}"
        );
    }

    // =========================================================================
    // Agent 委譲 (doc 28 §4) の SP dispatch — early validation のみ。
    // 状態遷移ロジックは World 中央 store に移管したため (doc 28 §6)、その単体 test は
    // `capability::delegation_store` が担う。SP handler は必須 field 検証後に World へ proxy
    // する (world_wire::call) ので、ここでは World 不要な早期 Err 経路だけを固定する。
    // =========================================================================

    /// delegate/complete/respond の必須 field 欠落 / 不正 outcome は World 到達前に Err。
    #[tokio::test]
    async fn delegation_dispatch_validates_before_proxy() {
        use super::dispatch_process_method;
        use crate::process::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // delegate: doer 欠落 → Err (proxy 前)。
        assert!(
            dispatch_process_method(
                &state,
                "delegate",
                serde_json::json!({ "task": "x", "requester": "agent@vp" }),
            )
            .await
            .is_err(),
            "delegate doer 欠落は Err"
        );
        // complete: id 欠落 → Err。
        assert!(
            dispatch_process_method(
                &state,
                "complete",
                serde_json::json!({ "outcome": { "kind": "done", "result": "x" } }),
            )
            .await
            .is_err(),
            "complete id 欠落は Err"
        );
        // complete: outcome の kind が未知 → from_value で Err (proxy 前)。
        assert!(
            dispatch_process_method(
                &state,
                "complete",
                serde_json::json!({ "id": "dlg-x", "outcome": { "kind": "weird" } }),
            )
            .await
            .is_err(),
            "complete 不正 outcome は Err"
        );
        // respond: answer 欠落 → Err。
        assert!(
            dispatch_process_method(&state, "respond", serde_json::json!({ "id": "dlg-x" }))
                .await
                .is_err(),
            "respond answer 欠落は Err"
        );
    }
}
