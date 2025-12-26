//! World Server - HTTP/WebSocket サーバー
//!
//! The World の HTTP API と WebSocket エンドポイントを提供。
//!
//! ## エンドポイント
//!
//! ### HTTP API
//! - `GET /health` - ヘルスチェック
//! - `GET /api/status` - The World ステータス
//! - `GET /api/parks` - 登録済み Paisley Park 一覧
//! - `POST /api/parks/register` - Paisley Park 登録
//! - `POST /api/parks/unregister` - Paisley Park 解除
//! - `POST /api/parks/heartbeat` - ハートビート
//!
//! ### WebSocket
//! - `GET /ws` - ViewPoint との双方向通信
//!
//! ## WebSocket メッセージタイプ
//! - `workspace_switch` - ワークスペース切り替え
//! - `panel_toggle` - パネル表示/非表示
//! - `tile_split` - タイル分割
//! - `show` - コンテンツ表示

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast};
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use super::WorldConfig;
use super::conductor::{Conductor, PaisleyParkInfo, PaisleyStatus};

// ===========================================================================
// View System メッセージ型
// ===========================================================================

/// ViewPoint からのメッセージ
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewMessage {
    /// ワークスペース切り替え
    WorkspaceSwitch { workspace_id: String },
    /// パネル表示/非表示
    PanelToggle { panel_id: String },
    /// パネルリサイズ
    PanelResize { panel_id: String, width: u32 },
    /// タイル分割
    TileSplit { pane_id: String, direction: String },
    /// タイルフォーカス
    TileFocus { pane_id: String },
    /// タイルクローズ
    TileClose { pane_id: String },
    /// フローティングウィンドウ作成
    FloatingCreate { title: String, content_type: String },
    /// Ping
    Ping,
}

/// ViewPoint への配信メッセージ
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BroadcastMessage {
    /// コンテンツ表示
    Show {
        pane_id: String,
        content_type: String,
        content: String,
        append: bool,
    },
    /// ペインクリア
    Clear { pane_id: String },
    /// ワークスペース状態更新
    WorkspaceUpdate { workspace_id: String, name: String },
    /// Paisley Park ステータス更新
    ParkStatus {
        park_id: String,
        project_id: String,
        status: String,
    },
    /// Pong
    Pong,
    /// エラー
    Error { message: String },
}

/// World Server 共有状態
#[derive(Clone)]
struct WorldState {
    /// Conductor への参照
    conductor: Arc<RwLock<Conductor>>,
    /// 設定
    config: WorldConfig,
    /// View 配信チャンネル
    broadcast_tx: broadcast::Sender<BroadcastMessage>,
}

/// World Server
pub struct WorldServer;

impl WorldServer {
    /// サーバーを起動
    pub async fn run(
        config: WorldConfig,
        conductor: Arc<RwLock<Conductor>>,
        cancel: CancellationToken,
    ) -> Result<()> {
        // View 配信チャンネル（256クライアントまで）
        let (broadcast_tx, _) = broadcast::channel(256);

        // MIDI リスナー起動（設定されている場合）
        if let Some(ref midi_pattern) = config.midi_port_pattern {
            let midi_tx = broadcast_tx.clone();
            let pattern = midi_pattern.clone();
            tokio::spawn(async move {
                let pattern_ref = if pattern.is_empty() {
                    None
                } else {
                    Some(pattern.as_str())
                };
                if let Err(e) = super::midi::start_midi_listener(pattern_ref, midi_tx).await {
                    tracing::warn!("MIDI リスナー停止: {}", e);
                }
            });
        }

        let state = WorldState {
            conductor,
            config: config.clone(),
            broadcast_tx,
        };

        let mut app = Router::new()
            // ヘルスチェック
            .route("/health", get(health_check))
            // API
            .route("/api/status", get(get_status))
            .route("/api/parks", get(list_parks))
            .route("/api/parks/register", post(register_park))
            .route("/api/parks/unregister", post(unregister_park))
            .route("/api/parks/heartbeat", post(heartbeat))
            // View API (MCP Tools から呼び出し)
            .route("/api/view/show", post(show_content))
            .route("/api/view/clear", post(clear_pane))
            // WebSocket
            .route("/ws", get(ws_handler))
            .layer(CorsLayer::permissive())
            .with_state(state);

        // 静的ファイル配信（設定されている場合）
        if let Some(ref static_dir) = config.static_dir {
            tracing::info!("Static files from: {:?}", static_dir);
            app = app.fallback_service(ServeDir::new(static_dir));
        }

        let listener = tokio::net::TcpListener::bind(config.addr).await?;
        tracing::info!("World Server listening on {}", config.addr);

        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                cancel.cancelled().await;
                tracing::info!("World Server シャットダウン中...");
            })
            .await?;

        Ok(())
    }
}

// ===========================================================================
// ヘルスチェック
// ===========================================================================

async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "the-world",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ===========================================================================
// ステータス API
// ===========================================================================

#[derive(Serialize)]
struct WorldStatus {
    /// サービス名
    service: &'static str,
    /// バージョン
    version: &'static str,
    /// 登録済み Paisley Park 数
    park_count: usize,
    /// デバッグモード
    debug: bool,
}

async fn get_status(State(state): State<WorldState>) -> Json<WorldStatus> {
    let conductor = state.conductor.read().await;
    Json(WorldStatus {
        service: "the-world",
        version: env!("CARGO_PKG_VERSION"),
        park_count: conductor.park_count(),
        debug: state.config.debug,
    })
}

// ===========================================================================
// Paisley Park 管理 API
// ===========================================================================

#[derive(Serialize)]
struct ParkListResponse {
    parks: Vec<ParkInfo>,
}

#[derive(Serialize)]
struct ParkInfo {
    id: String,
    project_id: String,
    project_path: String,
    port: u16,
    status: String,
}

impl From<&PaisleyParkInfo> for ParkInfo {
    fn from(info: &PaisleyParkInfo) -> Self {
        Self {
            id: info.id.clone(),
            project_id: info.project_id.clone(),
            project_path: info.project_path.clone(),
            port: info.port,
            status: format!("{:?}", info.status),
        }
    }
}

async fn list_parks(State(state): State<WorldState>) -> Json<ParkListResponse> {
    let conductor = state.conductor.read().await;
    let parks = conductor
        .list_parks()
        .iter()
        .map(|p| ParkInfo::from(*p))
        .collect();
    Json(ParkListResponse { parks })
}

#[derive(Deserialize)]
struct RegisterRequest {
    project_id: String,
    project_path: String,
    port: u16,
}

#[derive(Serialize)]
struct RegisterResponse {
    park_id: String,
    session_token: String,
}

async fn register_park(
    State(state): State<WorldState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (axum::http::StatusCode, String)> {
    let mut conductor = state.conductor.write().await;

    match conductor.register(req.project_id, req.project_path, req.port) {
        Ok((park_id, session_token)) => Ok(Json(RegisterResponse {
            park_id,
            session_token,
        })),
        Err(e) => Err((axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

#[derive(Deserialize)]
struct UnregisterRequest {
    park_id: String,
    reason: Option<String>,
}

#[derive(Serialize)]
struct AckResponse {
    ack: bool,
}

async fn unregister_park(
    State(state): State<WorldState>,
    Json(req): Json<UnregisterRequest>,
) -> Json<AckResponse> {
    let mut conductor = state.conductor.write().await;
    let ack = conductor.unregister(&req.park_id, req.reason);
    Json(AckResponse { ack })
}

#[derive(Deserialize)]
struct HeartbeatRequest {
    park_id: String,
    status: String,
}

async fn heartbeat(
    State(state): State<WorldState>,
    Json(req): Json<HeartbeatRequest>,
) -> Json<AckResponse> {
    let status = match req.status.as_str() {
        "idle" => PaisleyStatus::Idle,
        "busy" => PaisleyStatus::Busy,
        "error" => PaisleyStatus::Error("Unknown error".to_string()),
        _ => PaisleyStatus::Idle,
    };

    let mut conductor = state.conductor.write().await;
    let ack = conductor.heartbeat(&req.park_id, status);
    Json(AckResponse { ack })
}

// ===========================================================================
// WebSocket ハンドラー
// ===========================================================================

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<WorldState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: WebSocket, state: WorldState) {
    let (mut sender, mut receiver) = socket.split();

    // Broadcast 購読
    let mut broadcast_rx = state.broadcast_tx.subscribe();

    // Welcome メッセージ送信
    let welcome = serde_json::json!({
        "type": "welcome",
        "service": "the-world",
        "version": env!("CARGO_PKG_VERSION"),
    });
    if sender
        .send(Message::Text(
            serde_json::to_string(&welcome).unwrap().into(),
        ))
        .await
        .is_err()
    {
        return;
    }

    tracing::info!("ViewPoint 接続");

    loop {
        tokio::select! {
            // クライアントからのメッセージ
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        tracing::debug!("WS受信: {}", text);

                        // メッセージをパース
                        let response = match serde_json::from_str::<ViewMessage>(&text) {
                            Ok(view_msg) => handle_view_message(view_msg, &state).await,
                            Err(e) => {
                                BroadcastMessage::Error {
                                    message: format!("Invalid message: {}", e),
                                }
                            }
                        };

                        // 応答を送信
                        if let Ok(json) = serde_json::to_string(&response) {
                            if sender.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::info!("ViewPoint 切断");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::warn!("WebSocket エラー: {}", e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            // Broadcast メッセージ
            broadcast = broadcast_rx.recv() => {
                match broadcast {
                    Ok(msg) => {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if sender.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Broadcast lagged: {} messages dropped", n);
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

/// View メッセージを処理
async fn handle_view_message(msg: ViewMessage, state: &WorldState) -> BroadcastMessage {
    match msg {
        ViewMessage::WorkspaceSwitch { workspace_id } => {
            tracing::info!("ワークスペース切り替え: {}", workspace_id);
            BroadcastMessage::WorkspaceUpdate {
                workspace_id: workspace_id.clone(),
                name: format!("Workspace {}", workspace_id),
            }
        }
        ViewMessage::PanelToggle { panel_id } => {
            tracing::info!("パネルトグル: {}", panel_id);
            // TODO: パネル状態を管理
            BroadcastMessage::Show {
                pane_id: panel_id,
                content_type: "log".to_string(),
                content: "Panel toggled".to_string(),
                append: false,
            }
        }
        ViewMessage::PanelResize { panel_id, width } => {
            tracing::info!("パネルリサイズ: {} -> {}px", panel_id, width);
            BroadcastMessage::Show {
                pane_id: panel_id,
                content_type: "log".to_string(),
                content: format!("Resized to {}px", width),
                append: false,
            }
        }
        ViewMessage::TileSplit { pane_id, direction } => {
            tracing::info!("タイル分割: {} ({})", pane_id, direction);
            BroadcastMessage::Show {
                pane_id,
                content_type: "log".to_string(),
                content: format!("Split {}", direction),
                append: false,
            }
        }
        ViewMessage::TileFocus { pane_id } => {
            tracing::info!("タイルフォーカス: {}", pane_id);
            BroadcastMessage::Show {
                pane_id,
                content_type: "log".to_string(),
                content: "Focused".to_string(),
                append: false,
            }
        }
        ViewMessage::TileClose { pane_id } => {
            tracing::info!("タイルクローズ: {}", pane_id);
            BroadcastMessage::Clear { pane_id }
        }
        ViewMessage::FloatingCreate {
            title,
            content_type,
        } => {
            tracing::info!("フローティング作成: {} ({})", title, content_type);
            BroadcastMessage::Show {
                pane_id: format!("floating-{}", uuid::Uuid::new_v4()),
                content_type,
                content: title,
                append: false,
            }
        }
        ViewMessage::Ping => BroadcastMessage::Pong,
    }
}

// ===========================================================================
// MCP Tools API（Paisley Park からの呼び出し用）
// ===========================================================================

/// Show コンテンツ API
#[derive(Deserialize)]
struct ShowRequest {
    pane_id: String,
    content_type: String,
    content: String,
    #[serde(default)]
    append: bool,
}

/// POST /api/view/show - コンテンツを表示
async fn show_content(
    State(state): State<WorldState>,
    Json(req): Json<ShowRequest>,
) -> Json<AckResponse> {
    let msg = BroadcastMessage::Show {
        pane_id: req.pane_id,
        content_type: req.content_type,
        content: req.content,
        append: req.append,
    };
    let _ = state.broadcast_tx.send(msg);
    Json(AckResponse { ack: true })
}

/// POST /api/view/clear - ペインをクリア
async fn clear_pane(
    State(state): State<WorldState>,
    Json(req): Json<ClearRequest>,
) -> Json<AckResponse> {
    let msg = BroadcastMessage::Clear {
        pane_id: req.pane_id,
    };
    let _ = state.broadcast_tx.send(msg);
    Json(AckResponse { ack: true })
}

#[derive(Deserialize)]
struct ClearRequest {
    pane_id: String,
}
