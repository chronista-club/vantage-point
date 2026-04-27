//! Daemon 経由 terminal (VP-93 Step 2a)
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
    // shell + args を決定 (Mac=zsh -l, Win=git-bash -l, etc)。
    // VP_DAEMON_SHELL で daemon 経路だけ override 可能、未指定なら shell_detect に委ねる。
    let shell = std::env::var("VP_DAEMON_SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(crate::shell_detect::detect_shell);
    let args = crate::shell_detect::detect_shell_args(&shell);

    // shell + args を query param に乗せる。`-l` 等 ASCII 安全な flag のみ想定なので
    // URL encode は省略 (login flag に & = 等が入ることはない)。
    // 複数 args は comma-separated (例: "-l" or "-NoLogo,-NoExit")。
    let mut full_url = format!(
        "{}/ws/terminal?shell={}&cols={}&rows={}",
        ws_url, shell, cols, rows
    );
    if !args.is_empty() {
        full_url.push_str("&args=");
        full_url.push_str(&args.join(","));
    }

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
    let (ws, _) = match connect_async(&url).await {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("/ws/terminal connect failed: {}", e);
            let err_line =
                format!("\r\n\x1b[31m[vp-app] daemon 接続失敗: {}\x1b[0m\r\n", e).into_bytes();
            let _ = proxy.send_event(AppEvent::Output(err_line));
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
                    if proxy.send_event(AppEvent::Output(b.to_vec())).is_err() {
                        tracing::info!("EventLoop 終了、ws-terminal read 終了");
                        break;
                    }
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
    // PH#3: WS disconnect の user-facing 通知
    // (loop 抜けた = サーバ close / 通信エラー / sender drop のいずれか)
    let line = "\r\n\x1b[31m[vp-app] daemon disconnected (reconnect not implemented; restart vp-app to recover)\x1b[0m\r\n"
        .as_bytes()
        .to_vec();
    let _ = proxy.send_event(AppEvent::Output(line));
}
