//! SP / TheWorld の `/ws/terminal` endpoint
//!
//! WebSocket で PTY を remote 化する。 2 つのモードを併存:
//!
//! 1. **Lane attach mode (Phase 2)**: `?lane=<address>` 指定 →
//!    SP の `LanePool` から既存 PtySlot を `subscribe_output()` で attach。
//!    複数 client が同じ Lane の PTY (Lead Lane の Claude CLI など) を共有できる。
//!    WS 切断しても PtySlot は生き続ける (Lane lifecycle は LanePool が支配)。
//!
//! 2. **Spawn mode (legacy)**: `lane` 未指定 → 接続ごとに新 PtySlot を spawn。
//!    WS 切断 = PtySlot drop = child kill。 旧来の挙動、 互換のため残置。
//!
//! ## プロトコル
//!
//! - URL: `ws://host:33xxx/ws/terminal?lane=<project>/lead`  (Lane attach)
//! - URL: `ws://host:32000/ws/terminal?shell=bash&cols=80&rows=24` (Spawn)
//! - Server → Client: `Message::Binary(pty_output_bytes)` — PTY からの生バイト列
//! - Client → Server:
//!   - `Message::Binary(bytes)` → PTY write (user input)
//!   - `Message::Text(json)` → 制御メッセージ (`{"type":"resize","cols":N,"rows":M}`)
//!   - `Message::Close(_)` → 切断
//!
//! 関連 memory:
//! - mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane = Session Process)
//! - mem_1CaSugEk1W2vr5TAdfDn5D (多 scope architecture: Lane scope は SP per project)

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
    /// シェルコマンド (Spawn mode のみ、 default: "bash"。 client が `vp-app` なら shell_detect で決定して送る)
    #[serde(default = "default_shell")]
    pub shell: String,
    /// シェル起動引数 (comma-separated、 Spawn mode のみ、 例: "-l" or "-NoLogo,-NoExit")。
    /// 未指定なら何も付けない (caller が決めない場合 shell が default 動作)。
    #[serde(default)]
    pub args: Option<String>,
    /// 初期幅 (default: 80)
    #[serde(default = "default_cols")]
    pub cols: u16,
    /// 初期高さ (default: 24)
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// 作業ディレクトリ (Spawn mode のみ、 default: $HOME)
    #[serde(default)]
    pub cwd: Option<String>,
    /// Lane address (Phase 2 attach mode、 例: `"vantage-point/lead"` / `"vp/worker/foo"`)。
    /// Some なら既存 LanePool の PtySlot に attach、 None なら従来の Spawn mode。
    #[serde(default)]
    pub lane: Option<String>,
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
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        match params.lane.as_deref() {
            Some(addr_str) if !addr_str.is_empty() => {
                handle_terminal_socket_lane(socket, params, state).await;
            }
            _ => {
                handle_terminal_socket_spawn(socket, params).await;
            }
        }
    })
}

/// Phase 2: 既存 LanePool の PtySlot に attach する mode。
///
/// `?lane=<address>` で指定された Lane の PtySlot に対して:
/// - subscribe_output() で broadcast 受信 → WS Binary に流す
/// - WS Binary 受信 → LanePool::write_to_lane() で PtySlot に書込
/// - WS Text {"type":"resize"} 受信 → LanePool::resize_lane()
/// - WS 切断: PtySlot は生き続ける (Lane lifecycle は LanePool 管理、 attach client は来たり去ったり可能)
async fn handle_terminal_socket_lane(
    socket: WebSocket,
    params: TerminalQuery,
    state: Arc<AppState>,
) {
    use crate::process::lanes_state::LanePool;

    let (mut sender, mut receiver) = socket.split();
    let addr_str = params.lane.as_deref().unwrap_or("");

    // address parse
    let Some(addr) = LanePool::parse_address(addr_str) else {
        tracing::warn!("/ws/terminal lane attach: invalid address {:?}", addr_str);
        let err = format!(
            r#"{{"type":"error","message":"invalid lane address: {}"}}"#,
            addr_str
        );
        let _ = sender.send(Message::Text(err.into())).await;
        return;
    };

    // Phase 2.x-c: scrollback 付きで attach。 initial bytes を先送してから
    // broadcast loop に入る (atomicity: subscribe_with_scrollback 内で snapshot+subscribe を
    // 同一 ring lock 下で行う、 重複も取りこぼしも無し)。
    let (mut rx, initial_bytes) = {
        let pool = state.lane_pool.read().await;
        match pool.subscribe_with_scrollback(&addr) {
            Some(pair) => pair,
            None => {
                tracing::warn!(
                    "/ws/terminal lane attach: lane not found or no PtySlot: {}",
                    addr
                );
                let err = format!(
                    r#"{{"type":"error","message":"lane not found: {}"}}"#,
                    addr
                );
                let _ = sender.send(Message::Text(err.into())).await;
                return;
            }
        }
    };

    tracing::info!(
        "/ws/terminal lane attach: addr={} scrollback={} bytes",
        addr,
        initial_bytes.len()
    );

    // initial bytes を先に送出 (Phase 2.x-c: scrollback replay)
    if !initial_bytes.is_empty() {
        if sender
            .send(Message::Binary(initial_bytes.into()))
            .await
            .is_err()
        {
            tracing::warn!("/ws/terminal lane={} initial flush 送出失敗、 disconnect", addr);
            return;
        }
    }

    // 初期 resize は client 側 cols/rows で更新 (xterm.js が ready 時 sendResize するが
    // 念のため query param 値で同期しておく)
    {
        let pool = state.lane_pool.read().await;
        if let Err(e) = pool.resize_lane(&addr, params.cols, params.rows) {
            tracing::debug!("初期 resize 失敗 (continue): {}", e);
        }
    }

    // PTY output → WS Binary
    let send_addr = addr.clone();
    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(bytes) => {
                    if sender.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "/ws/terminal lane={} output lagged: {} dropped",
                        send_addr,
                        n
                    );
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // WS → LanePool.write / resize
    while let Some(msg_res) = receiver.next().await {
        let msg = match msg_res {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("/ws/terminal lane={} recv error: {}", addr, e);
                break;
            }
        };
        match msg {
            Message::Binary(bytes) => {
                let pool = state.lane_pool.read().await;
                if let Err(e) = pool.write_to_lane(&addr, &bytes) {
                    tracing::warn!("/ws/terminal lane={} write failed: {}", addr, e);
                    break;
                }
            }
            Message::Text(text) => match serde_json::from_str::<ControlMsg>(&text) {
                Ok(ControlMsg::Resize { cols, rows }) => {
                    let pool = state.lane_pool.read().await;
                    if let Err(e) = pool.resize_lane(&addr, cols, rows) {
                        tracing::warn!("/ws/terminal lane={} resize failed: {}", addr, e);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "/ws/terminal lane={} bad control msg: {} text={}",
                        addr,
                        e,
                        text
                    );
                }
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
    tracing::info!("/ws/terminal lane attach disconnected: addr={}", addr);
    // PtySlot は LanePool が保持し続ける (= Lane の lifecycle は WS 接続と独立)
}

/// Spawn mode (legacy / TheWorld 用): 接続ごとに新 PtySlot を作る。
/// WS 切断 = PtySlot drop = child kill。
async fn handle_terminal_socket_spawn(socket: WebSocket, params: TerminalQuery) {
    let (mut sender, mut receiver) = socket.split();

    let cwd = params
        .cwd
        .clone()
        .or_else(|| dirs::home_dir().map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "/tmp".into());

    // args は comma-separated (vp-app/src/ws_terminal.rs が乗せる)。空 string や trailing comma は除去。
    let args: Vec<String> = params
        .args
        .as_deref()
        .map(|s| {
            s.split(',')
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let (mut slot, mut rx) =
        match PtySlot::spawn(&cwd, &params.shell, &args, params.cols, params.rows) {
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
