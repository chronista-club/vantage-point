//! Daemon 経由 terminal (VP-93 Step 2a) — **Phase 3 では bypass**
//!
//! Phase 3 で per-pane local PTY pool に移行したため、本ファイルの
//! `connect_daemon_terminal` は呼び出されない (dead code 扱い)。
//! 復活させる場合は WS protocol を per-pane に拡張する必要がある (Phase 4+ 候補)。
//!
//! TheWorld の `/ws/terminal` WebSocket endpoint に接続し、
//! local portable-pty (`terminal::PtyHandle`) と同等の write / resize API を提供する。
//!
//! ## パイプライン
//! ```text
//!  xterm.js ─ipc──► WsTerminalHandle::write ─► tokio channel ─► WS Binary ─► daemon ─► PtySlot::write
//!  PtySlot  ─broadcast──► daemon WS Binary ─► read loop ─► AppEvent::Output ─► xterm.js.write
//! ```
//!
//! ## プロトコル
//! - Client → Server:
//!   - `Message::Binary(bytes)` = PTY input
//!   - `Message::Text(json)`    = `{"type":"resize","cols":N,"rows":M}`
//! - Server → Client:
//!   - `Message::Binary(bytes)` = PTY output

use std::thread;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tao::event_loop::EventLoopProxy;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::terminal::AppEvent;

/// WsTerminal への指令 (write / resize)
enum WsCommand {
    Input(Vec<u8>),
    Resize(u16, u16),
}

/// WebSocket terminal ハンドル
///
/// `PtyHandle` と同じ shape (write / resize)。app.rs から enum 経由で
/// 相互交換可能に wrap される。
#[derive(Clone)]
pub struct WsTerminalHandle {
    tx: mpsc::UnboundedSender<WsCommand>,
}

impl WsTerminalHandle {
    pub fn write(&self, data: &[u8]) -> Result<()> {
        self.tx
            .send(WsCommand::Input(data.to_vec()))
            .map_err(|_| anyhow::anyhow!("ws terminal channel closed"))?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.tx
            .send(WsCommand::Resize(cols, rows))
            .map_err(|_| anyhow::anyhow!("ws terminal channel closed"))?;
        Ok(())
    }
}

/// TheWorld の `/ws/terminal` に接続して WsTerminalHandle を返す
///
/// - `world_url`: `http://127.0.0.1:32000` のような base URL (ws:// に変換する)
/// - `shell`: server 側で起動するシェル (default "bash"、env `VP_DAEMON_SHELL` で override)
/// - 接続失敗時は `AppEvent::Output` に赤文字 error を流して handle だけ返す
///   (xterm に表示されて user が気付けるように)
pub fn connect_daemon_terminal(
    world_url: &str,
    cols: u16,
    rows: u16,
    proxy: EventLoopProxy<AppEvent>,
) -> Result<WsTerminalHandle> {
    // http:// → ws://, https:// → wss://
    let ws_url = world_url
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    let shell = std::env::var("VP_DAEMON_SHELL").unwrap_or_else(|_| "bash".into());
    let full_url = format!(
        "{}/ws/terminal?shell={}&cols={}&rows={}",
        ws_url, shell, cols, rows
    );

    let (tx, rx) = mpsc::unbounded_channel::<WsCommand>();

    thread::Builder::new()
        .name("vp-app-ws-terminal".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("ws-terminal runtime build failed: {}", e);
                    return;
                }
            };
            rt.block_on(run_ws_loop(full_url, rx, proxy));
        })
        .context("spawn ws-terminal thread")?;

    Ok(WsTerminalHandle { tx })
}

async fn run_ws_loop(
    url: String,
    mut rx: mpsc::UnboundedReceiver<WsCommand>,
    proxy: EventLoopProxy<AppEvent>,
) {
    tracing::info!("/ws/terminal connecting: {}", url);
    let _ = &proxy; // Phase 3: AppEvent::Output 廃止のため未使用 (dead code path)
    let (ws, _) = match connect_async(&url).await {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("/ws/terminal connect failed: {}", e);
            return;
        }
    };
    tracing::info!("/ws/terminal connected");
    let (mut write, mut read) = ws.split();

    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(WsCommand::Input(bytes)) => {
                    if write.send(Message::Binary(bytes)).await.is_err() {
                        tracing::info!("/ws/terminal write closed");
                        break;
                    }
                }
                Some(WsCommand::Resize(c, r)) => {
                    let json = format!(r#"{{"type":"resize","cols":{},"rows":{}}}"#, c, r);
                    if write.send(Message::Text(json)).await.is_err() {
                        tracing::info!("/ws/terminal resize write closed");
                        break;
                    }
                }
                None => {
                    // sender 全部 drop — terminate
                    break;
                }
            },
            msg = read.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    // Phase 3: AppEvent::Output 廃止 → routing 不能なので drop
                    tracing::trace!("/ws/terminal binary recv: {} bytes (drop in Phase 3)", b.len());
                }
                Some(Ok(Message::Text(t))) => {
                    tracing::warn!("/ws/terminal unexpected text from server: {}", t);
                }
                Some(Ok(Message::Close(_))) | None => {
                    tracing::info!("/ws/terminal closed by server");
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::warn!("/ws/terminal recv error: {}", e);
                    break;
                }
            },
        }
    }
    tracing::info!("/ws/terminal loop 終了 (Phase 3 では本 path は呼ばれない)");
}
