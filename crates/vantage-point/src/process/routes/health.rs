//! ヘルスチェック・基本ルートハンドラー
//!
//! UI は native vp-app (WebView) が担う。 旧 localhost browser canvas (`web/canvas.html`
//! を `/` `/canvas` `/vendor` で配信) は未使用のため撤去済 (mako/drop-web-canvas)。

use std::sync::Arc;

use serde::Deserialize;

use axum::{Json, extract::State, response::IntoResponse};

use super::super::state::AppState;

/// Stand（Capability）のステータス
#[derive(serde::Serialize)]
pub struct StandStatus {
    /// Stand の状態: "active", "idle", "connected", "disabled"
    pub status: &'static str,
    /// Stand 固有の詳細情報
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// Health check response
#[derive(serde::Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub pid: u32,
    pub project_dir: String,
    /// Terminal チャネル認証トークン（TUI 接続用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_token: Option<String>,
    /// プロセス起動時刻（ISO 8601）
    pub started_at: String,
    /// 配下の Stand（Capability）ステータス
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stands: Option<std::collections::HashMap<String, StandStatus>>,
}

/// POST /api/wire/send - wire accumulation への送信 HTTP 入口
///
/// `vp wire` CLI / `wire_*` MCP tool と同じ wire accumulation 経路の HTTP 版。
/// QUIC dispatch の `wire_send` と同一の [`handle_wire_send`] を呼ぶ薄い wrapper。
/// payload: `{from, to: [String], body: JSON, reply_to?: String}`。
pub async fn wire_send_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_send(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

/// POST /api/wire/recv - wire accumulation からの long-poll 受信 HTTP 入口
///
/// `vp wire watch` CLI / `wire_recv` MCP tool と同じ wire accumulation 経路の HTTP 版。
/// QUIC dispatch の `wire_recv` と同一の [`handle_wire_recv`] を呼ぶ薄い wrapper。
/// payload: `{agent: String, timeout?: u64}` → `{messages: [WireMessage...], count}`。
pub async fn wire_recv_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_recv(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

/// POST /api/wire/unread-count - per-agent 未読 wire count を取得 (read-only、 cursor 不触り)
///
/// `flow_progress` の集約 view に必要。 `wire_recv` を timeout=0 で叩く代替は cursor を
/// 進めてしまうため、 cursor 不触りの専用 endpoint。
/// payload: `{agent: String}` → `{status: "ok", total: u64, by_thread: {root_id: count}}`。
pub async fn wire_unread_count_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_unread_count(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

/// POST /api/wire/latest-msg - agent 関与の最新 wire message を取得 (read-only、 cursor 不触り)
///
/// 「関与」 = `from_addr == agent` OR `to_addrs CONTAINS agent`。
/// `flow_progress` の 5-state FSM derive で performer の現状態を判定するために使う。
/// payload: `{agent: String}` → `{status: "ok", message: WireMessage|null}`。
pub async fn wire_latest_msg_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_latest_msg(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

/// POST /api/wire/thread - thread 系譜取得 HTTP 入口 (read-only、 cursor 不触り)
///
/// `vp wire thread` CLI / `wire_thread` MCP tool と同じ経路の HTTP 版 (R2-a で CLI parity)。
/// payload: `{message_id: String}` → `{status: "ok", messages: [..], count}`。
pub async fn wire_thread_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_thread(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

/// POST /api/wire/ack - per-message ack HTTP 入口 (R2-a、 決定 D3)
///
/// payload: `{message_id: String, agent: String}` → `{status: "ok", acked: bool}`。
pub async fn wire_ack_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_ack(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

// L0 portless: `/api/diagnose` (Stand 自己診断 HTTP) は consumer 消滅で撤去。 必要なら将来
// World channel / mailbox query (`bastet@world` 等) 経由で再設計する。

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let token = if state.terminal_token == "WORLD_DISABLED" {
        None
    } else {
        Some(state.terminal_token.clone())
    };

    // Stand ステータスを収集（TheWorld モードでは省略）
    let stands = if state.terminal_token != "WORLD_DISABLED" {
        let mut map = std::collections::HashMap::new();

        // 💬 Echoes (Coding Assistant) — interactive_agent の有無で判定
        let echoes_status = {
            let agent = state.interactive_agent.read().await;
            if agent.is_some() { "active" } else { "idle" }
        };
        map.insert(
            "echoes".to_string(),
            StandStatus {
                status: echoes_status,
                detail: None,
            },
        );

        // 🧭 Paisley Park（Canvas）— WebSocket クライアント接続数
        let canvas_clients = state.canvas_senders.lock().await.len();
        map.insert(
            "paisley_park".to_string(),
            StandStatus {
                status: if canvas_clients > 0 {
                    "connected"
                } else {
                    "idle"
                },
                detail: Some(serde_json::json!({ "clients": canvas_clients })),
            },
        );

        // 🌿 Gold Experience（ProcessRunner）— 実行中プロセス数
        let running_processes = state.process_registry.lock().await.list().len();
        map.insert(
            "gold_experience".to_string(),
            StandStatus {
                status: if running_processes > 0 {
                    "active"
                } else {
                    "idle"
                },
                detail: Some(serde_json::json!({ "processes": running_processes })),
            },
        );

        // 🧲 Bastet（MIDI device registry）— World mode のみ host。
        // SP mode からは「disabled」として報告（α-3 で cross-process query 経由に rewire 予定）。
        #[cfg(feature = "midi")]
        let (bastet_status, bastet_detail) = {
            if let Some(wc) = state.world_capabilities.as_ref() {
                if let Some(ref bastet) = wc.bastet {
                    let b = bastet.read().await;
                    let count = b.device_count().await;
                    let discovering = b.is_discovering();
                    (
                        if count > 0 { "active" } else { "idle" },
                        Some(serde_json::json!({
                            "devices": count,
                            "discovering": discovering,
                        })),
                    )
                } else if wc.midi.is_some() {
                    ("active", None)
                } else {
                    ("disabled", None)
                }
            } else {
                ("disabled", None)
            }
        };
        #[cfg(not(feature = "midi"))]
        let (bastet_status, bastet_detail) = ("disabled", None);
        map.insert(
            "bastet".to_string(),
            StandStatus {
                status: bastet_status,
                detail: bastet_detail,
            },
        );

        // DB にも Stand ステータスを書き込み（VP-21）
        if let Some(ref db) = state.vpdb {
            for (key, s) in &map {
                if let Err(e) = db
                    .upsert_stand_status(&state.project_dir, key, s.status, s.detail.as_ref())
                    .await
                {
                    tracing::warn!("DB stand_status 書き込み失敗 ({}): {}", key, e);
                }
            }
        }

        Some(map)
    } else {
        // World mode — Bastet のみ報告（World 階層に host される唯一の observable Stand）
        #[cfg(feature = "midi")]
        {
            let mut map = std::collections::HashMap::new();
            if let Some(bastet) = state
                .world_capabilities
                .as_ref()
                .and_then(|wc| wc.bastet.as_ref())
            {
                let b = bastet.read().await;
                let count = b.device_count().await;
                let discovering = b.is_discovering();
                map.insert(
                    "bastet".to_string(),
                    StandStatus {
                        status: if count > 0 { "active" } else { "idle" },
                        detail: Some(serde_json::json!({
                            "devices": count,
                            "discovering": discovering,
                        })),
                    },
                );
            }
            if map.is_empty() { None } else { Some(map) }
        }
        #[cfg(not(feature = "midi"))]
        {
            None
        }
    };

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        project_dir: state.project_dir.clone(),
        terminal_token: token,
        started_at: state.started_at.clone(),
        stands,
    })
}

// L0 portless Group B: pane HTTP handler (show/toggle/split/close) は CLI を process-proxy ask
// (`show`/`toggle_pane`/`split_pane`/`close_pane` → `handle_process_message`) に移管し撤去。
// いずれも `state.hub.broadcast(ProcessMessage)` するだけで、 QUIC dispatch が同じ broadcast を行う。

/// POST /api/canvas/switch_lane - Canvas Lane 切り替え
///
/// canvas_senders 経由で接続中の全 Canvas WS クライアントに直接送信。
pub async fn canvas_switch_lane_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let lane = body
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if lane.is_empty() {
        return Json(serde_json::json!({"status": "error", "message": "lane is required"}));
    }
    let msg = serde_json::json!({"type": "switch_lane", "lane": lane});
    let mut senders = state.canvas_senders.lock().await;
    let mut sent = 0;
    // 送信失敗（切断済み）のチャネルを除去
    senders.retain(|tx| !tx.is_closed());
    for tx in senders.iter() {
        if tx.send(msg.clone()).await.is_ok() {
            sent += 1;
        }
    }
    tracing::info!(
        "switch_lane({}): sent to {}/{} canvas client(s)",
        lane,
        sent,
        senders.len()
    );
    Json(serde_json::json!({"status": "ok", "lane": lane, "clients": sent}))
}

/// GET /api/canvas/layout - Canvas レイアウト状態を復元
pub async fn canvas_layout_get_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match state.load_canvas_layout().await {
        Some(layout) => Json(serde_json::json!({"status": "ok", "layout": layout})),
        None => Json(serde_json::json!({"status": "empty"})),
    }
}

/// POST /api/canvas/layout - Canvas レイアウト状態を保存
///
/// フロントエンドから Lane/Tab/Pane の構造を JSON で受け取り、pane_contents (SurrealDB) に保存。
///
/// pane 内容自体は webview が `/api/pp/state` で逐次保存するので、 ここは layout のみ。
/// (旧 Whitesnake `persist_pane_contents` の conductor snapshot は冗長だったため退役)
pub async fn canvas_layout_save_handler(
    State(state): State<Arc<AppState>>,
    Json(layout): Json<serde_json::Value>,
) -> impl IntoResponse {
    state.save_canvas_layout(&layout).await;
    Json(serde_json::json!({"status": "saved"}))
}

// L0 portless Group B: file watch/unwatch HTTP handler は CLI を process-proxy ask
// (`watch_file`/`unwatch_file` → `handle_watch_file`/`handle_unwatch_file`) に移管し撤去。
// core (`state.file_watchers`) は QUIC dispatch が同じく呼ぶので維持。

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
    // 1. request_id 生成、oneshot channel 作成
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let mut waiters = state.screenshot_waiters.lock().await;
        waiters.insert(request_id.clone(), tx);
    }

    // 3. ScreenshotRequest を Canvas に broadcast
    state
        .hub
        .broadcast(crate::protocol::ProcessMessage::ScreenshotRequest {
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

            if let Err(e) = tokio::fs::write(&save_path, &bytes).await {
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

// 旧 GET /wasm/{filename} (vp-mdast-wasm 配信 endpoint) は 2026-05-25 削除。
// frontend (vp-app webview) は `marked` (npm) + `@chronista-club/creoui-editor-host`
// に markdown rendering を移行済で、 vp_mdast_wasm 関連 asset は dead 化していた。
// vp-mdast / vp-mdast-wasm crate + web/wasm/ asset (482KB) と共に撤去。

/// POST /api/shutdown - Graceful shutdown
pub async fn shutdown_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    tracing::info!("Shutdown requested via API");
    state.shutdown_token.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}

// ===== tmux ペイン操作ハンドラー =====

/// tmux split パラメータ
#[derive(Deserialize)]
pub struct TmuxSplitParams {
    #[serde(default = "default_true")]
    pub horizontal: bool,
    pub command: Option<String>,
    /// コンテンツ種別: "shell" (The Hand), "canvas" (PP), "agent" (HD)
    pub content_type: Option<String>,
}

fn default_true() -> bool {
    true
}

/// content_type からコマンドを解決する
pub fn resolve_content_command(
    content_type: Option<&str>,
    command: Option<String>,
) -> Option<String> {
    // command が直接指定されていればそちらを優先（後方互換）
    if command.is_some() {
        return command;
    }
    match content_type {
        // PR-pre2 (VP-118): "hd" → "echoes" rename。 旧 "hd" は legacy session 互換のため
        // 一時的に維持、 PR-β-4 cleanup で削除予定。
        Some("agent") | Some("hd") | Some("echoes") | Some("ec") => Some("claude".to_string()),
        Some("canvas") | Some("pp") => None, // TODO: PP ビュー起動コマンド（将来実装）
        Some("shell") | Some("th") | None => None, // デフォルトシェル
        Some(_) => None,
    }
}

/// POST /api/tmux/split - tmux ペインを分割
pub async fn tmux_split_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<TmuxSplitParams>,
) -> impl IntoResponse {
    let handle = match state.ensure_tmux().await {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({"error": "tmux 未使用環境です"}));
        }
    };
    let command = resolve_content_command(params.content_type.as_deref(), params.command);
    match handle.split(params.horizontal, command).await {
        Ok(pane) => Json(serde_json::json!({"status": "ok", "pane": pane})),
        Err(e) => Json(serde_json::json!({"error": e})),
    }
}

/// tmux close パラメータ
#[derive(Deserialize)]
pub struct TmuxCloseParams {
    pub pane_id: String,
}

/// POST /api/tmux/close - tmux ペインを閉じる
pub async fn tmux_close_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<TmuxCloseParams>,
) -> impl IntoResponse {
    let handle = match state.ensure_tmux().await {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({"error": "tmux 未使用環境です"}));
        }
    };
    match handle.close(&params.pane_id).await {
        Ok(()) => Json(serde_json::json!({"status": "ok"})),
        Err(e) => Json(serde_json::json!({"error": e})),
    }
}

// ===== tmux 追加ハンドラー（CLI 用） =====

/// tmux capture パラメータ
#[derive(Deserialize)]
pub struct TmuxCaptureParams {
    pub pane_id: Option<String>,
}

/// POST /api/tmux/capture - ペイン内容をキャプチャ
///
/// pane_id 指定で単一ペイン、省略で全ペインをキャプチャ。
pub async fn tmux_capture_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<TmuxCaptureParams>,
) -> impl IntoResponse {
    let handle = match state.ensure_tmux().await {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({"error": "tmux 未使用環境です"}));
        }
    };
    match params.pane_id {
        Some(pane_id) => match handle.capture(&pane_id).await {
            Ok(content) => {
                Json(serde_json::json!({"status": "ok", "pane_id": pane_id, "content": content}))
            }
            Err(e) => Json(serde_json::json!({"error": e})),
        },
        None => {
            let captures = handle.capture_all().await;
            Json(serde_json::json!({"status": "ok", "captures": captures}))
        }
    }
}

/// GET /api/tmux/list - ペイン一覧
pub async fn tmux_list_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let handle = match state.ensure_tmux().await {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({"error": "tmux 未使用環境です"}));
        }
    };
    let panes = handle.list().await;
    let all_meta = handle.list_all_agent_meta().await;
    // 各ペインにエージェントメタデータを付与（一括取得済み）
    let panes_with_meta: Vec<serde_json::Value> = panes
        .iter()
        .map(|pane| {
            let mut pane_json = serde_json::to_value(pane).unwrap_or_default();
            if let Some(meta) = all_meta.get(&pane.id) {
                pane_json["agent"] = serde_json::to_value(meta).unwrap_or_default();
            }
            pane_json
        })
        .collect();
    Json(serde_json::json!({"status": "ok", "panes": panes_with_meta}))
}

/// tmux send-keys パラメータ
#[derive(Deserialize)]
pub struct TmuxSendKeysParams {
    pub pane_id: String,
    pub text: String,
    /// true なら末尾に Enter を付与
    #[serde(default)]
    pub enter: bool,
}

/// POST /api/tmux/send-keys - ペインにキー入力送信
pub async fn tmux_send_keys_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<TmuxSendKeysParams>,
) -> impl IntoResponse {
    let handle = match state.ensure_tmux().await {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({"error": "tmux 未使用環境です"}));
        }
    };
    // テキスト送信
    match handle.send_keys(&params.pane_id, &params.text).await {
        Ok(()) => {}
        Err(e) => return Json(serde_json::json!({"error": e})),
    }
    // enter=true なら Enter キーを別途送信（tmux send-keys は引数単位で解釈する）
    if params.enter
        && let Err(e) = handle.send_keys(&params.pane_id, "Enter").await
    {
        return Json(serde_json::json!({"error": e}));
    }
    Json(serde_json::json!({"status": "ok"}))
}

/// tmux resolve-pane パラメータ
#[derive(Deserialize)]
pub struct TmuxResolvePaneParams {
    /// label または pane_id（%始まり）
    pub q: String,
}

/// GET /api/tmux/resolve-pane - label/pane_id からペイン ID を解決
///
/// `q` が lane address（`<project>/conductor` / `<project>/performer/<name>`）の場合は、
/// その lane の実 tmux session 名を解決して返す（`pane_id` field に session 名が入る）。
/// `tmux send-keys -t <session>` は session の active pane に届くため、 nudge は単一
/// TmuxActor の束縛 session や agent_metadata を介さずに任意 lane へ届く。
/// lane address でない場合は従来の pane_id（`%`始まり）/ agent_metadata label 解決に fallback。
pub async fn tmux_resolve_pane_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<TmuxResolvePaneParams>,
) -> impl IntoResponse {
    // lane address なら実 session に解決（nudge の正典経路）
    if let Some(session) = state.resolve_lane_session(&params.q).await {
        return Json(
            serde_json::json!({"status": "ok", "pane_id": session, "meta": serde_json::Value::Null}),
        );
    }

    let handle = match state.ensure_tmux().await {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({"error": "tmux 未使用環境です"}));
        }
    };
    match handle.resolve_pane_id(&params.q).await {
        Some(pane_id) => {
            let meta = handle.get_agent_meta(&pane_id).await;
            Json(serde_json::json!({"status": "ok", "pane_id": pane_id, "meta": meta}))
        }
        None => Json(serde_json::json!({"error": format!("ペインが見つかりません: {}", params.q)})),
    }
}

/// tmux agent-meta パラメータ
#[derive(Deserialize)]
pub struct TmuxAgentMetaParams {
    pub pane_id: String,
}

/// GET /api/tmux/agent-meta - エージェントメタデータ取得
pub async fn tmux_agent_meta_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<TmuxAgentMetaParams>,
) -> impl IntoResponse {
    let handle = match state.ensure_tmux().await {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({"error": "tmux 未使用環境です"}));
        }
    };
    let meta = handle.get_agent_meta(&params.pane_id).await;
    Json(serde_json::json!({"status": "ok", "meta": meta}))
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

    let result = crate::process::process_runner::ruby_eval(
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

    let result = crate::process::process_runner::ruby_run(
        &state.process_registry,
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
    match crate::process::process_runner::ruby_stop(&state.process_registry, &params.process_id)
        .await
    {
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
    let processes = state.process_registry.lock().await.list();
    Json(serde_json::json!({
        "status": "ok",
        "processes": processes,
    }))
}

// L0 portless: `/api/process/*` (ProcessRunner 汎用 HTTP) handler 群は consumer 消滅で撤去。
// 生きてる process 操作は QUIC `process` channel (`unison_server::handle_process_*`) が
// 同じ `process_runner` core を呼ぶので、 HTTP 入口だけ落とせば core は維持される。

#[cfg(test)]
mod tests {
    //! VP-13 sub-scope E: health.rs route の Axum oneshot smoke test。

    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_handler_returns_200_with_stands_field() {
        let state = crate::process::state::build_test_app_state(None).await;
        let app = Router::new()
            .route("/api/health", get(health_handler))
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        // HealthResponse の必須 field を verify (= 構造変更 regression net)
        assert_eq!(body.get("status").and_then(|v| v.as_str()), Some("ok"));
        assert!(body.get("version").is_some(), "version field 必須");
        assert!(body.get("pid").is_some(), "pid field 必須");
        assert!(body.get("project_dir").is_some(), "project_dir field 必須");
        assert!(body.get("started_at").is_some(), "started_at field 必須");
        // stands は test 用 AppState では terminal_token == "test" なので
        // "WORLD_DISABLED" 分岐に入らず populate される
        assert!(
            body.get("stands").is_some(),
            "stands field 必須 (= Stand status map)"
        );
    }
}
