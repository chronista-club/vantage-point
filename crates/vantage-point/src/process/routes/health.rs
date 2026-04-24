//! ヘルスチェック・基本ルートハンドラー
//!
//! ## Dev モード (`VP_DEV=1`)
//!
//! Web アセット (canvas.html, mermaid.min.js) をファイルシステムから動的に読み込む。
//! HTML/JS の変更がビルド不要、ブラウザリロードだけで反映される。
//!
//! ```bash
//! VP_DEV=1 vp sp start   # ファイルシステムから読む
//! vp sp start             # バイナリ埋め込みから読む（本番）
//! ```

use std::sync::Arc;

use serde::Deserialize;

use axum::{
    Json,
    extract::{Path, State},
    http::header,
    response::{Html, IntoResponse},
};

use super::super::state::AppState;
use crate::protocol::ProcessMessage;

/// VP_DEV 環境変数が設定されているか判定
fn is_dev_mode() -> bool {
    std::env::var("VP_DEV").is_ok()
}

/// web/ ディレクトリのパスを解決する（dev モード用）
///
/// バイナリの場所から逆算して web/ を探す:
/// 1. カレントディレクトリの `web/`
/// 2. Cargo マニフェストディレクトリ（CARGO_MANIFEST_DIR コンパイル時）
fn web_dir() -> std::path::PathBuf {
    // カレントディレクトリの web/ を最優先
    let cwd_web = std::path::PathBuf::from("web");
    if cwd_web.exists() {
        return cwd_web;
    }
    // フォールバック: コンパイル時のプロジェクトルート
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::PathBuf::from(manifest).join("../../web")
}

/// canvas.html を返す（dev: ファイル読み込み / 本番: 埋め込み）
fn load_canvas_html() -> String {
    if is_dev_mode() {
        let path = web_dir().join("canvas.html");
        match std::fs::read_to_string(&path) {
            Ok(html) => {
                tracing::debug!("dev: canvas.html loaded from {}", path.display());
                html
            }
            Err(e) => {
                tracing::warn!(
                    "dev: canvas.html not found at {}: {}, falling back to embedded",
                    path.display(),
                    e
                );
                include_str!("../../../../../web/canvas.html").to_string()
            }
        }
    } else {
        include_str!("../../../../../web/canvas.html").to_string()
    }
}

/// vendor ファイルを返す（dev: ファイル読み込み / 本番: 埋め込み）
fn load_vendor_file(filename: &str) -> Option<Vec<u8>> {
    if is_dev_mode() {
        let path = web_dir().join("vendor").join(filename);
        match std::fs::read(&path) {
            Ok(bytes) => {
                tracing::debug!("dev: vendor/{} loaded from {}", filename, path.display());
                return Some(bytes);
            }
            Err(e) => {
                tracing::warn!(
                    "dev: vendor/{} not found at {}: {}, trying embedded",
                    filename,
                    path.display(),
                    e
                );
            }
        }
    }

    // 本番: コンパイル時に埋め込んだファイルを返す
    match filename {
        "mermaid.min.js" => {
            Some(include_bytes!("../../../../../web/vendor/mermaid.min.js").to_vec())
        }
        "shiki-vp.mjs" => Some(include_bytes!("../../../../../web/vendor/shiki-vp.mjs").to_vec()),
        "shiki-onig.wasm" => {
            Some(include_bytes!("../../../../../web/vendor/shiki-onig.wasm").to_vec())
        }
        _ => None,
    }
}

/// Open browser (macOS)
pub fn open_browser(url: &str) -> anyhow::Result<()> {
    std::process::Command::new("open").arg(url).spawn()?;
    Ok(())
}

/// Index page handler — Canvas を 1st ビューとして表示
pub async fn index_handler() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")],
        Html(load_canvas_html()),
    )
}

/// Canvas page handler（キャッシュ無効化ヘッダー付き）
pub async fn canvas_handler() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")],
        Html(load_canvas_html()),
    )
}

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

/// VP-83 Phase 2.5 準備: Msgbox 内部 state を dump する debug endpoint。
/// Mailbox に登録されている address 一覧を返し、Lead/Worker 間 msg flow の疎通確認に使う。
pub async fn msgbox_debug_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let addresses = state.capabilities.msgbox_router.addresses().await;
    Json(serde_json::json!({
        "addresses": addresses,
        "count": addresses.len(),
    }))
}

/// Stand 自己診断 (2026-04-25 user 発案) — ProcessCapabilities の各 Stand の
/// diagnose() を集約。side-effect-free、いつでも呼び出し可能。
///
/// NOTE: CapabilityRegistry は現状 skeleton (未使用) のため、state.capabilities
/// の field を直接 iterate する方式を採用。Mailbox address list と Stand state を
/// 1 view にまとめて観測可能に。
pub async fn diagnose_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    use crate::capability::core::Capability;
    let mut reports = Vec::new();

    // Protocol Capability (WebSocket / stdio 配信)
    {
        let protocol = state.capabilities.protocol.read().await;
        reports.push(protocol.diagnose());
    }
    // Agent Capability (Heaven's Door 📖、Claude CLI 統合)
    {
        let agent = state.capabilities.agent.read().await;
        reports.push(agent.diagnose());
    }
    // MIDI Capability (Hermit Purple 🍇、feature 有効時)
    #[cfg(feature = "midi")]
    if let Some(ref midi) = state.capabilities.midi {
        let midi = midi.read().await;
        reports.push(midi.diagnose());
    }

    // Mailbox (Router 自体は Capability trait 外だが、integration layer として
    // 診断対象に含める)
    let msgbox_addresses = state.capabilities.msgbox_router.addresses().await;

    Json(serde_json::json!({
        "count": reports.len(),
        "reports": reports,
        "msgbox": {
            "addresses": msgbox_addresses,
            "count": msgbox_addresses.len(),
        },
    }))
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let token = if state.terminal_token == "WORLD_DISABLED" {
        None
    } else {
        Some(state.terminal_token.clone())
    };

    // Stand ステータスを収集（TheWorld モードでは省略）
    let stands = if state.terminal_token != "WORLD_DISABLED" {
        let mut map = std::collections::HashMap::new();

        // 📖 Heaven's Door（Agent）— interactive_agent の有無で判定
        let hd_status = {
            let agent = state.interactive_agent.read().await;
            if agent.is_some() { "active" } else { "idle" }
        };
        map.insert(
            "heavens_door".to_string(),
            StandStatus {
                status: hd_status,
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

        // 🍇 Hermit Purple（MIDI）— Capability 有無 (feature = "midi" 有効時のみ active)
        #[cfg(feature = "midi")]
        let midi_status = if state.capabilities.midi.is_some() {
            "active"
        } else {
            "disabled"
        };
        #[cfg(not(feature = "midi"))]
        let midi_status = "disabled";
        map.insert(
            "hermit_purple".to_string(),
            StandStatus {
                status: midi_status,
                detail: None,
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
        None
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

/// POST /api/show - Show content in browser
pub async fn show_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<ProcessMessage>,
) -> impl IntoResponse {
    // TopicRouter が Hub ブリッジ経由で自動的に retained に保存するため、
    // 明示的なキャッシュは不要。Hub に broadcast するだけ。
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/msgbox/remote_deliver - Cross-Process forward 受信（Msgbox Phase 3 Step 2）
///
/// 別 Process の `RemoteRoutingClient::forward` から呼ばれる。
/// 認証: `VP_REGISTRY_TOKEN` 環境変数設定時は Bearer header 検証。
/// 配信: ローカル MsgboxRouter の `deliver_local` に渡す。
pub async fn msgbox_remote_deliver_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(msg): Json<crate::capability::Message>,
) -> impl IntoResponse {
    // Auth 検証
    if let Some(expected) = crate::capability::msgbox_remote::registry_token() {
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "));
        if provided != Some(expected.as_str()) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "invalid registry token"})),
            );
        }
    }

    // ローカル配信
    match state
        .capabilities
        .msgbox_router
        .deliver_local(msg.clone())
        .await
    {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "delivered", "to": msg.to})),
        ),
        Err(crate::capability::msgbox::Error::BoxNotFound { ref address }) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("msgbox address not found: {}", address),
                "to": msg.to,
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": format!("msgbox router unavailable: {}", e),
                "to": msg.to,
            })),
        ),
    }
}

/// POST /api/toggle-pane - Toggle side panel visibility
pub async fn toggle_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<ProcessMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/split-pane - Split a pane
pub async fn split_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<ProcessMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/close-pane - Close a pane
pub async fn close_pane_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<ProcessMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

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
/// フロントエンドから Lane/Tab/Pane の構造を JSON で受け取り、ディスクに保存。
/// ペイン内容もこのタイミングで永続化する。
pub async fn canvas_layout_save_handler(
    State(state): State<Arc<AppState>>,
    Json(layout): Json<serde_json::Value>,
) -> impl IntoResponse {
    state.save_canvas_layout(&layout).await;
    // ペイン内容も同時に保存（RetainedStore から取得）
    state.persist_pane_contents().await;
    Json(serde_json::json!({"status": "saved"}))
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

/// GET /vendor/{filename} — ローカルバンドルのベンダーライブラリ配信
///
/// CDN 依存を排除し、wry WebView でも確実に読み込めるようにする。
/// dev モードではファイルシステムから読み込む。本番はバイナリ埋め込み。
pub async fn vendor_handler(Path(filename): Path<String>) -> impl IntoResponse {
    // パストラバーサル防止
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return (axum::http::StatusCode::NOT_FOUND, "Not found").into_response();
    }

    let content_type = if filename.ends_with(".js") || filename.ends_with(".mjs") {
        "application/javascript; charset=utf-8"
    } else if filename.ends_with(".wasm") {
        "application/wasm"
    } else if filename.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    };

    let body = load_vendor_file(&filename);
    match body {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, content_type),
                (
                    header::CACHE_CONTROL,
                    if is_dev_mode() {
                        "no-store"
                    } else {
                        "public, max-age=86400"
                    },
                ),
            ],
            bytes,
        )
            .into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "Vendor file not found").into_response(),
    }
}

/// GET /wasm/{filename} — WASM モジュール配信
///
/// vp-mdast-wasm のビルド成果物を配信。
/// dev モードではファイルシステムから読み込み、本番は埋め込み。
pub async fn wasm_handler(Path(filename): Path<String>) -> impl IntoResponse {
    let content_type = if filename.ends_with(".wasm") {
        "application/wasm"
    } else if filename.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else {
        "application/octet-stream"
    };

    let body = load_wasm_file(&filename);
    match body {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, content_type),
                (
                    header::CACHE_CONTROL,
                    if is_dev_mode() {
                        "no-store"
                    } else {
                        "public, max-age=86400"
                    },
                ),
            ],
            bytes,
        )
            .into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "WASM file not found").into_response(),
    }
}

/// WASM ファイルを読み込む
fn load_wasm_file(filename: &str) -> Option<Vec<u8>> {
    // セキュリティ: パストラバーサル防止（vendor_handler と同一ルール）
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return None;
    }

    if is_dev_mode() {
        let path = web_dir().join("wasm").join(filename);
        std::fs::read(&path).ok()
    } else {
        // 本番: 埋め込み
        match filename {
            "vp_mdast_wasm_bg.wasm" => {
                Some(include_bytes!("../../../../../web/wasm/vp_mdast_wasm_bg.wasm").to_vec())
            }
            "vp_mdast_wasm.js" => {
                Some(include_bytes!("../../../../../web/wasm/vp_mdast_wasm.js").to_vec())
            }
            _ => None,
        }
    }
}

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
        Some("agent") | Some("hd") => Some("claude".to_string()),
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
pub async fn tmux_resolve_pane_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<TmuxResolvePaneParams>,
) -> impl IntoResponse {
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

// =========================================================================
// ProcessRunner 汎用 API ハンドラー
// =========================================================================

/// POST /api/process/run — 任意コマンドを起動
pub async fn process_run_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<crate::process::process_runner::RunParams>,
) -> impl IntoResponse {
    let result = crate::process::process_runner::process_run(
        &state.process_registry,
        &params,
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

/// POST /api/process/run-eval — 短命実行
pub async fn process_run_eval_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<crate::process::process_runner::RunEvalParams>,
) -> impl IntoResponse {
    let result =
        crate::process::process_runner::process_run_eval(&params, &state.project_dir, &state.hub)
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

/// POST /api/process/stop — プロセス停止
pub async fn process_stop_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<RubyStopParams>,
) -> impl IntoResponse {
    match crate::process::process_runner::process_stop(&state.process_registry, &params.process_id)
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

/// POST /api/process/inject — コード注入
pub async fn process_inject_handler(
    State(state): State<Arc<AppState>>,
    Json(params): Json<crate::process::process_runner::InjectParams>,
) -> impl IntoResponse {
    match crate::process::process_runner::process_inject(&state.process_registry, &params).await {
        Ok(()) => Json(serde_json::json!({
            "status": "ok",
            "message": format!("プロセス {} にコードを注入しました", params.process_id),
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": e,
        })),
    }
}

/// GET /api/process/list — プロセス一覧
pub async fn process_list_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let processes = state.process_registry.lock().await.list();
    Json(serde_json::json!({
        "status": "ok",
        "processes": processes,
    }))
}
