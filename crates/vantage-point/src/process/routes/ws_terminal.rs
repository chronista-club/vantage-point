//! SP / TheWorld の `/ws/terminal` endpoint
//!
//! WebSocket で PTY を remote 化する。 2 つのモードを併存:
//!
//! 1. **Lane attach mode (Phase 2)**: `?lane=<address>` 指定 →
//!    SP の `LanePool` から既存 PtySlot を `subscribe_output()` で attach。
//!    複数 client が同じ Lane の PTY (Conductor Lane の Claude CLI など) を共有できる。
//!    WS 切断しても PtySlot は生き続ける (Lane lifecycle は LanePool が支配)。
//!
//! 2. **Spawn mode (legacy)**: `lane` 未指定 → 接続ごとに新 PtySlot を spawn。
//!    WS 切断 = PtySlot drop = child kill。 旧来の挙動、 互換のため残置。
//!
//! ## プロトコル
//!
//! - URL: `ws://host:33xxx/ws/terminal?lane=<project>/conductor`  (Lane attach)
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
    /// Lane address (Phase 2 attach mode、 例: `"vantage-point/conductor"` / `"vp/performer/foo"`)。
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
///
/// Phase 2.x-d (Architecture v4 cleanup): `?lane=<address>` を **必須**化。
/// 旧 Spawn mode (`lane` 未指定で新 PtySlot を毎回起動する path) は削除済。
/// 「TheWorld direct shell」 を将来復活させたい場合は新 endpoint (例: `/ws/shell`) で実装する。
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
                // Phase 2.x-d: lane 未指定は protocol error として close
                let (mut sender, _receiver) = socket.split();
                let err = r#"{"type":"error","message":"lane query parameter is required (?lane=<address>)"}"#;
                let _ = sender.send(Message::Text(err.into())).await;
                let _ = sender.send(Message::Close(None)).await;
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
    //
    // VP-108 (PR #260 後継): tmux 経由 lane は PtySlot scrollback ring buffer の dump を
    // skip する。 判定は task ファイル冒頭の `#VP layer=N` メタデータ駆動 (`layer >= 1` =
    // tmux 経由)、 内蔵 stand 名 hardcode は廃止。 user 独自 stand (例: `zellij`) も
    // task ファイルに `#VP layer=1` を書けば自動的に skip 経路に入る。
    //
    // 経緯:
    // - tmux pane は alt-screen mode で claude TUI redraw frame が大量に発生、
    //   scrollback ring に redraw 履歴が蓄積される。 reattach 時にこれを replay すると、
    //   古い viewport size 前提の cursor positioning が新 viewport で wrap して
    //   「同 line を 多重複製で render」 する文字化け bug が発生 (user 報告 2026-05-03)。
    // - tmux 自身が pane content を保持しており、 client 接続 + resize trigger で
    //   フル redraw を発信する。 PtySlot scrollback と二重持ちで害悪。
    // - bare login shell (= layer=0) は tmux 経由しないので scrollback 価値あり、
    //   こちらは従来通り replay する。
    let (mut rx, initial_bytes, is_tmux_hosted) = {
        let pool = state.lane_pool.read().await;
        let stand_name = pool.get(&addr).map(|info| info.stand.clone());
        match pool.subscribe_with_scrollback(&addr) {
            Some((rx, bytes)) => {
                // stand metadata を install root から resolve (file 不在時は default = layer 0)。
                // install root は OnceLock cache、 file read は ~50 行のシェルスクリプト 1 つ
                // なので attach 毎の cost は無視できる。
                let metadata = stand_name
                    .as_deref()
                    .and_then(|name| {
                        crate::process::install_root::locate_install_root().map(|root| {
                            crate::process::stand_metadata::StandMetadata::from_install_root(
                                root, name,
                            )
                        })
                    })
                    .unwrap_or_default();
                let is_tmux_hosted = metadata.is_tmux_hosted();
                if is_tmux_hosted {
                    tracing::debug!(
                        "/ws/terminal lane attach: tmux-hosted (stand={:?} tier={})、 scrollback {} bytes を skip (tmux redraw に委譲)",
                        stand_name,
                        metadata.tier,
                        bytes.len()
                    );
                    (rx, Vec::new(), true)
                } else {
                    (rx, bytes, false)
                }
            }
            None => {
                tracing::warn!(
                    "/ws/terminal lane attach: lane not found or no PtySlot: {}",
                    addr
                );
                let err = format!(r#"{{"type":"error","message":"lane not found: {}"}}"#, addr);
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
    if !initial_bytes.is_empty()
        && sender
            .send(Message::Binary(initial_bytes.into()))
            .await
            .is_err()
    {
        tracing::warn!(
            "/ws/terminal lane={} initial flush 送出失敗、 disconnect",
            addr
        );
        return;
    }

    // 初期 resize: query param の cols/rows で PtySlot を同期する。
    //
    // ただし **tmux-hosted lane (layer >= 1) は attach 時 resize を skip** する。
    // tmux は `window-size latest` で「最後に attach した client」 に session size を
    // 追従させるため、 headless lane に vp-app の hidden xterm.js (sidebar は全 project
    // 分の lane を ensureLane する。 非表示 pane は `display:none` で fit → container
    // clientWidth=0 → 極小 dims) が attach すると、 query param 経由でも tmux session が
    // 9×3 に潰れる。 tmux-hosted lane の size は spawn 時の `tmux new-session -x120 -y48`
    // を初期値に、 以降は **可視 client の Resize 制御メッセージのみ**で駆動する
    // (attach だけでは変えない)。 bare shell (layer 0) は従来どおり query param で同期。
    if !is_tmux_hosted {
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
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    // VP-132: PtySlot drop で broadcast 終了 → WS にも明示 Close frame 送信。
                    // client (vp-app) の auto-reconnect (PR #218) が Close 受信で発火、
                    // 新 PtySlot に attach し直す。 旧実装の silent break では WS が TCP
                    // 維持で client は dead と認識せず、 Lane Restart 後 Pane refresh 不全
                    // の bug を生んでいた (= VP-131 server-side fix と組で UI 復活経路完成)。
                    //
                    // architectural invariant: PtySlot lifecycle = WS lifecycle、 PtySlot drop
                    // 検出時は WS にも Close 通知義務がある。 send 失敗は ignore (= もう client が
                    // disconnect してる場合等、 best-effort)。
                    tracing::info!(
                        "/ws/terminal lane={} broadcast closed (PtySlot dropped)、 WS Close 送信",
                        send_addr
                    );
                    let _ = sender.send(Message::Close(None)).await;
                    break;
                }
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
