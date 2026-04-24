//! TheWorld daemon の `/ws/terminal` endpoint
//!
//! WebSocket で PTY 単発 (PtySlot) を remote 化する。
//! vp-app など native client が local portable-pty を持たずに、
//! daemon 経由で shell session を張るためのエントリポイント。
//!
//! ## プロトコル
//!
//! - URL: `ws://host:32000/ws/terminal?shell=bash&cols=80&rows=24&cwd=/path`
//! - Server → Client: `Message::Binary(pty_output_bytes)` — PTY からの生バイト列
//! - Client → Server:
//!   - `Message::Binary(bytes)` → PTY write (user input)
//!   - `Message::Text(json)` → 制御メッセージ (`{"type":"resize","cols":N,"rows":M}`)
//!   - `Message::Close(_)` → 切断
//!
//! ## Step 2a MVP の範囲
//!
//! - 認証なし (localhost/LAN の信頼前提)
//! - 接続ごとに独立した PtySlot (shared state なし)
//! - WS 切断 = PtySlot drop = child process kill
//! - Step 2b で tmux session 共有 / project 紐付けを追加予定

use std::sync::Arc;

use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;

use crate::daemon::pty_slot::PtySlot;
use crate::process::state::AppState;

/// クエリパラメータ
#[derive(Debug, Deserialize)]
pub struct TerminalQuery {
    /// シェルコマンド (default: "bash")
    #[serde(default = "default_shell")]
    pub shell: String,
    /// 初期幅 (default: 80)
    #[serde(default = "default_cols")]
    pub cols: u16,
    /// 初期高さ (default: 24)
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// 作業ディレクトリ (default: $HOME)
    #[serde(default)]
    pub cwd: Option<String>,
}

fn default_shell() -> String {
    "bash".into()
}
fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

/// Client → Server の制御メッセージ (Message::Text で JSON)
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMsg {
    /// PTY リサイズ
    Resize { cols: u16, rows: u16 },
}

/// axum ハンドラ (GET /ws/terminal)
pub async fn ws_terminal_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<TerminalQuery>,
    State(_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_terminal_socket(socket, params))
}

async fn handle_terminal_socket(socket: WebSocket, params: TerminalQuery) {
    let (mut sender, mut receiver) = socket.split();

    let cwd = params
        .cwd
        .clone()
        .or_else(|| dirs::home_dir().map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "/tmp".into());

    let (mut slot, mut rx) = match PtySlot::spawn(&cwd, &params.shell, params.cols, params.rows) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!("/ws/terminal: PtySlot::spawn failed: {}", e);
            let err = format!(r#"{{"type":"error","message":"{}"}}"#, e);
            let _ = sender.send(Message::Text(err.into())).await;
            return;
        }
    };

    let pid = slot.pid();
    tracing::info!(
        "/ws/terminal connected: shell={}, cwd={}, pid={}",
        params.shell,
        cwd,
        pid
    );

    // PTY 出力 → WS binary
    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(bytes) => {
                    if sender.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("/ws/terminal output lagged: {} messages dropped", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // WS → PTY / 制御
    while let Some(msg_res) = receiver.next().await {
        let msg = match msg_res {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("/ws/terminal recv error: {}", e);
                break;
            }
        };
        match msg {
            Message::Binary(bytes) => {
                if let Err(e) = slot.write(&bytes) {
                    tracing::warn!("/ws/terminal pty write failed: {}", e);
                    break;
                }
            }
            Message::Text(text) => match serde_json::from_str::<ControlMsg>(&text) {
                Ok(ControlMsg::Resize { cols, rows }) => {
                    if let Err(e) = slot.resize(cols, rows) {
                        tracing::warn!("/ws/terminal resize failed: {}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!("/ws/terminal bad control msg: {} text={}", e, text);
                }
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
    tracing::info!("/ws/terminal disconnected: pid={}", pid);
    // slot は drop 時に child を kill する
}
