//! Canvas Lane 集約 WebSocket ハンドラー
//!
//! World Process が各 Project Process の Hub を subscribe し、
//! Canvas クライアントに Lane（プロジェクト単位）でラップして中継する。
//!
//! ## プロトコル
//!
//! ### サーバー → クライアント
//! ```json
//! // Lane 一覧（接続時 + 変更時）
//! {"type": "lanes", "lanes": [{"name": "creo", "port": 33000, "status": "connected"}]}
//!
//! // Process メッセージ（Lane ラップ）
//! {"type": "lane_message", "lane": "creo", "port": 33000, "message": { ...ProcessMessage... }}
//! ```
//!
//! ### クライアント → サーバー
//! ```json
//! // Lane の追加/削除リクエスト（将来用）
//! {"type": "refresh"}
//! ```

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

use super::super::state::AppState;
use crate::protocol::ProcessMessage;

/// Lane 情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneInfo {
    /// プロジェクト名
    pub name: String,
    /// Process ポート番号
    pub port: u16,
    /// 接続状態
    pub status: LaneStatus,
}

/// Lane 接続状態
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneStatus {
    /// Process に接続中
    Connected,
    /// 接続試行中
    Connecting,
    /// 切断（Process 停止等）
    Disconnected,
}

/// サーバー → Canvas クライアントへのメッセージ
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LaneEvent {
    /// Lane 一覧の更新
    Lanes { lanes: Vec<LaneInfo> },
    /// Process メッセージ（Lane ラップ）
    LaneMessage {
        lane: String,
        port: u16,
        message: serde_json::Value,
    },
    /// Lane 接続状態の変更
    LaneStatusChanged {
        lane: String,
        port: u16,
        status: LaneStatus,
    },
}

/// Canvas クライアント → サーバーへのメッセージ
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LaneCommand {
    /// Process 一覧を再スキャン
    Refresh,
}

/// Lane ブリッジ内部メッセージ
enum BridgeMsg {
    /// Process からのメッセージ
    ProcessMessage {
        lane: String,
        port: u16,
        message: serde_json::Value,
    },
    /// Canvas レベルの直接メッセージ（ScreenshotRequest 等）
    DirectMessage(serde_json::Value),
    /// Lane 状態変更
    StatusChanged {
        lane: String,
        port: u16,
        status: LaneStatus,
    },
    /// Lane 一覧更新
    LanesUpdated(Vec<LaneInfo>),
}

/// WebSocket ハンドラー: `/ws/lanes`
pub async fn lanes_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_lanes_socket(socket, state))
}

/// Canvas クライアントとの WebSocket 接続を管理
async fn handle_lanes_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // ブリッジチャネル: 各 Process WS → 集約 → Canvas
    let (bridge_tx, mut bridge_rx) = mpsc::channel::<BridgeMsg>(256);

    // 接続済みポートの追跡（重複接続を防止）
    let connected_ports: Arc<Mutex<HashSet<u16>>> = Arc::new(Mutex::new(HashSet::new()));

    // 初期スキャン: 稼働中 Process を発見して接続
    let initial_lanes = discover_and_connect(&state, bridge_tx.clone(), &connected_ports).await;

    // 初期 Lane 一覧を送信
    let lanes_event = LaneEvent::Lanes {
        lanes: initial_lanes.clone(),
    };
    let text = serde_json::to_string(&lanes_event).unwrap_or_default();
    if ws_sender.send(Message::Text(text.into())).await.is_err() {
        return;
    }

    // ローカル Hub 購読: ScreenshotRequest 等の Canvas レベルメッセージを転送
    let hub_bridge_tx = bridge_tx.clone();
    let mut hub_rx = state.hub.subscribe();
    let hub_task = tokio::spawn(async move {
        loop {
            match hub_rx.recv().await {
                Ok(msg) => {
                    // ScreenshotRequest のみ直接転送（Lane ラップ不要）
                    if matches!(msg, ProcessMessage::ScreenshotRequest { .. }) {
                        let json = serde_json::to_value(&msg).unwrap_or_default();
                        let _ = hub_bridge_tx.send(BridgeMsg::DirectMessage(json)).await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Lanes hub subscriber lagged: {} dropped", n);
                }
            }
        }
    });

    // 定期スキャンタスク（5秒ごとに Process 一覧を更新）
    let scan_tx = bridge_tx.clone();
    let state_for_scan = state.clone();
    let ports_for_scan = Arc::clone(&connected_ports);
    let scan_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let lanes =
                discover_and_connect(&state_for_scan, scan_tx.clone(), &ports_for_scan).await;
            if scan_tx.send(BridgeMsg::LanesUpdated(lanes)).await.is_err() {
                break;
            }
        }
    });

    // ブリッジ → Canvas 送信タスク（切断時に connected_ports からも除去）
    let ports_for_send = Arc::clone(&connected_ports);
    let send_task = tokio::spawn(async move {
        while let Some(msg) = bridge_rx.recv().await {
            let event = match msg {
                BridgeMsg::ProcessMessage {
                    lane,
                    port,
                    message,
                } => LaneEvent::LaneMessage {
                    lane,
                    port,
                    message,
                },
                BridgeMsg::StatusChanged {
                    ref lane,
                    port,
                    ref status,
                } => {
                    // 切断時に追跡セットから除去 → 次回スキャンで再接続可能に
                    if matches!(status, LaneStatus::Disconnected) {
                        ports_for_send.lock().await.remove(&port);
                        tracing::debug!("Lane {} (port {}) を追跡セットから除去", lane, port);
                    }
                    LaneEvent::LaneStatusChanged {
                        lane: lane.clone(),
                        port,
                        status: status.clone(),
                    }
                }
                BridgeMsg::DirectMessage(json) => {
                    // Lane ラップせずに直接送信
                    let text = serde_json::to_string(&json).unwrap_or_default();
                    if ws_sender.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                    continue;
                }
                BridgeMsg::LanesUpdated(lanes) => LaneEvent::Lanes { lanes },
            };
            let text = serde_json::to_string(&event).unwrap_or_default();
            if ws_sender.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // Canvas → サーバー受信タスク
    let recv_bridge_tx = bridge_tx.clone();
    let state_for_recv = state.clone();
    let ports_for_recv = Arc::clone(&connected_ports);
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    // ScreenshotResponse を処理
                    if let Ok(browser_msg) =
                        serde_json::from_str::<crate::protocol::BrowserMessage>(&text)
                        && let crate::protocol::BrowserMessage::ScreenshotResponse {
                            request_id,
                            data,
                            width,
                            height,
                        } = browser_msg
                    {
                        let mut waiters = state_for_recv.screenshot_waiters.lock().await;
                        if let Some(tx) = waiters.remove(&request_id) {
                            let _ = tx.send(crate::process::state::ScreenshotData {
                                data,
                                width,
                                height,
                            });
                        }
                        continue;
                    }
                    // LaneCommand を処理
                    if let Ok(cmd) = serde_json::from_str::<LaneCommand>(&text) {
                        match cmd {
                            LaneCommand::Refresh => {
                                let lanes = discover_and_connect(
                                    &state_for_recv,
                                    recv_bridge_tx.clone(),
                                    &ports_for_recv,
                                )
                                .await;
                                let _ = recv_bridge_tx.send(BridgeMsg::LanesUpdated(lanes)).await;
                            }
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // いずれかのタスクが終了したら全て停止
    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
    scan_task.abort();
    hub_task.abort();

    tracing::info!("Canvas Lane WS 接続終了");
}

/// 稼働中 Process を発見し、未接続のものに WS クライアントを接続
async fn discover_and_connect(
    state: &Arc<AppState>,
    bridge_tx: mpsc::Sender<BridgeMsg>,
    connected_ports: &Arc<Mutex<HashSet<u16>>>,
) -> Vec<LaneInfo> {
    // World が有効な場合、World から Process 一覧を取得
    let running_processes = if let Some(world) = &state.world {
        let world = world.read().await;
        world.list_running_processes().await
    } else {
        // World なし — discovery で稼働中 Process を発見
        crate::discovery::list()
            .await
            .into_iter()
            .map(|p| crate::capability::RunningProcess {
                project_name: p
                    .project_dir
                    .rsplit('/')
                    .next()
                    .unwrap_or("unknown")
                    .to_string(),
                port: p.port,
                pid: p.pid,
                project_path: p.project_dir.into(),
                discovered_via_bonjour: false,
            })
            .collect()
    };

    let mut lanes = Vec::new();
    let mut ports = connected_ports.lock().await;

    for proc in &running_processes {
        // 自分自身はローカル Hub 経由で接続（WS クライアント不要）
        if proc.port == state.port {
            if !ports.contains(&proc.port) {
                ports.insert(proc.port);
                // ローカル Hub を購読してメッセージを Lane ラップで転送
                let tx = bridge_tx.clone();
                let lane_name = proc.project_name.clone();
                let port = proc.port;
                let mut hub_rx = state.hub.subscribe();
                tokio::spawn(async move {
                    loop {
                        match hub_rx.recv().await {
                            Ok(msg) => {
                                let value = serde_json::to_value(&msg).unwrap_or_default();
                                let _ = tx
                                    .send(BridgeMsg::ProcessMessage {
                                        lane: lane_name.clone(),
                                        port,
                                        message: value,
                                    })
                                    .await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Lane self hub lagged: {} dropped", n);
                            }
                        }
                    }
                });
            }
            lanes.push(LaneInfo {
                name: proc.project_name.clone(),
                port: proc.port,
                status: LaneStatus::Connected,
            });
            continue;
        }

        // 既に接続済み or 接続中のポートはスキップ
        if ports.contains(&proc.port) {
            lanes.push(LaneInfo {
                name: proc.project_name.clone(),
                port: proc.port,
                status: LaneStatus::Connected,
            });
            continue;
        }

        // 接続中としてマーク（重複 spawn 防止）
        ports.insert(proc.port);

        let lane_name = proc.project_name.clone();
        let port = proc.port;

        // WS クライアント接続を spawn
        let tx = bridge_tx.clone();
        tokio::spawn(async move {
            spawn_lane_bridge(lane_name, port, tx).await;
        });

        lanes.push(LaneInfo {
            name: proc.project_name.clone(),
            port: proc.port,
            status: LaneStatus::Connecting,
        });
    }

    lanes
}

/// 1つの Process に対する WS クライアント接続を確立し、メッセージを中継
async fn spawn_lane_bridge(lane: String, port: u16, bridge_tx: mpsc::Sender<BridgeMsg>) {
    let url = format!("ws://[::1]:{}/ws", port);

    // WS クライアント接続
    let ws_stream = match tokio_tungstenite::connect_async(&url).await {
        Ok((stream, _)) => stream,
        Err(e) => {
            tracing::warn!("Lane {} (port {}) 接続失敗: {}", lane, port, e);
            let _ = bridge_tx
                .send(BridgeMsg::StatusChanged {
                    lane,
                    port,
                    status: LaneStatus::Disconnected,
                })
                .await;
            return;
        }
    };

    tracing::info!("Lane {} (port {}) 接続成功", lane, port);

    // 接続成功を通知
    let _ = bridge_tx
        .send(BridgeMsg::StatusChanged {
            lane: lane.clone(),
            port,
            status: LaneStatus::Connected,
        })
        .await;

    let (_, mut read) = ws_stream.split();

    // Process からのメッセージを読み取り、Lane ラップして転送
    while let Some(Ok(msg)) = read.next().await {
        match msg {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    let _ = bridge_tx
                        .send(BridgeMsg::ProcessMessage {
                            lane: lane.clone(),
                            port,
                            message: value,
                        })
                        .await;
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }

    // 切断を通知
    tracing::info!("Lane {} (port {}) 切断", lane, port);
    let _ = bridge_tx
        .send(BridgeMsg::StatusChanged {
            lane,
            port,
            status: LaneStatus::Disconnected,
        })
        .await;
}
