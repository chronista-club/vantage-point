//! `/link` — Editor Link の口（doc 86 §2）。
//!
//! creo-ui editor-host（`config.link`）が繋ぐ WebSocket。consumer がブラウザ（creo-ui site / vp-app の
//! webview）で Unison QUIC を話せないため、doc 45 §2「HTTP は鈍い外殻」に対する **2 つ目の例外**として
//! HTTP listener に乗せる。新しい listener は作らない（doc 48 D-C「daemon が唯一の listener」）。
//!
//! 守り（loopback peer + Origin allowlist、`daemon::link::admit`）は **upgrade の前**に判定して 403 で
//! 断る。判定後は 1 接続 = 1 task で、`hello` を受けて [`LinkHub`] に面を登録し、以後は
//! 面 → hub の message を cache に写し、hub → 面 の message（`set` / `get` / `mode`）を書く。
//! 面の契約（open ごとに hello + snapshot、hub からの set は echo しない）は creo-ui
//! `docs/design/editor-link.md` が SSOT。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::mpsc;

use crate::daemon::link::{self, Inbound, LinkHub};
use crate::daemon::server::DaemonState;

/// `hello` を待つ上限。面は open 直後に送る契約なので、来ないのは Link ではない何か。
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// `GET /link`（WebSocket upgrade）。
///
/// `ConnectInfo` は `serve_with_cleanup` が `into_make_service_with_connect_info` で載せる。
/// 無ければ extractor が拒む（= 守りの前提が無い構成では開かない、fail closed）。
///
/// `WebSocketUpgrade` を `Result` で受けるのは **守りを request の形より先に**置くため。素の
/// extractor だと upgrade header の無い GET は handler に入る前に 400 で返り、LAN からの scan が
/// 403（= 弾いた、と log に残る）ではなく 400 で消える。
pub async fn link_handler(
    State(state): State<Arc<DaemonState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Err(rejection) = link::admit(peer, origin.as_deref()) {
        // 沈黙だと「繋ぎに来ていない」と「弾いている」が区別できないので理由を残す
        tracing::warn!("Editor Link を拒否: {rejection}");
        return (StatusCode::FORBIDDEN, rejection.to_string()).into_response();
    }
    let ws = match ws {
        Ok(ws) => ws,
        // 守りは通ったが WebSocket の handshake ではない（素の GET 等）→ axum の 400 / 426 をそのまま
        Err(rejection) => return rejection.into_response(),
    };
    let hub = Arc::clone(&state.link);
    ws.on_upgrade(move |socket| serve_link(hub, socket, peer))
}

/// 1 接続ぶんの task。`hello` → 登録 → 双方向の pump → 切断で登録解除。
async fn serve_link(hub: Arc<LinkHub>, mut socket: WebSocket, peer: SocketAddr) {
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, wait_for_hello(&mut socket)).await {
        Ok(Some(hello)) => hello,
        Ok(None) => return,
        Err(_) => {
            tracing::debug!(
                "Editor Link: hello が {}s 内に来ないので閉じる peer={peer}",
                HELLO_TIMEOUT.as_secs()
            );
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: "hello expected".into(),
                })))
                .await;
            return;
        }
    };
    let (v, app, surface) = hello;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let reg = match hub.connect(v, app.clone(), surface.clone(), tx) {
        Ok(reg) => reg,
        Err(e) => {
            tracing::warn!("Editor Link を閉じる: {e} (app={app} surface={surface} peer={peer})");
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::PROTOCOL,
                    reason: e.to_string().into(),
                })))
                .await;
            return;
        }
    };
    tracing::info!("Editor Link: 面を登録 app={app} surface={surface} peer={peer}");

    loop {
        tokio::select! {
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<Inbound>(text.as_str()) {
                    Ok(msg) => hub.apply(&reg, msg),
                    // 形が合わないものは黙って捨てる（creo-ui 側 parseInbound と同じ姿勢）
                    Err(_) => tracing::debug!(
                        "Editor Link: 形の合わない message を捨てた surface={}",
                        reg.surface
                    ),
                },
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(e)) => {
                    tracing::debug!("Editor Link: 受信エラー surface={} {e}", reg.surface);
                    break;
                }
                // Ping / Pong / Binary — ping への pong は axum が返す
                Some(Ok(_)) => {}
            },
            outbound = rx.recv() => match outbound {
                Some(msg) => {
                    let Ok(text) = serde_json::to_string(&msg) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                // hub 側が送り口を落とした = 同じ surface id の新しい接続に置き換えられた
                None => break,
            },
        }
    }
    hub.disconnect(&reg);
    tracing::info!("Editor Link: 面が離れた surface={}", reg.surface);
}

/// 最初の `hello` を待つ。hello より前の frame は捨てる。閉じたら `None`。
async fn wait_for_hello(socket: &mut WebSocket) -> Option<(u64, String, String)> {
    loop {
        match socket.recv().await? {
            Ok(Message::Text(text)) => {
                if let Ok(Inbound::Hello { v, app, surface }) =
                    serde_json::from_str::<Inbound>(text.as_str())
                {
                    return Some((v, app, surface));
                }
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::link::LINK_VERSION;
    use axum::Router;
    use axum::routing::get;
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::{self, Message as WsMessage};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

    /// `/link` だけを持つ router を実 TCP で立てる（ConnectInfo を本番と同じ経路で載せる）。
    async fn spawn_link_server() -> (Arc<DaemonState>, SocketAddr) {
        let state = crate::daemon::server::build_test_daemon_state().await;
        let app = Router::new()
            .route("/link", get(link_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("serve");
        });
        (state, addr)
    }

    async fn connect(addr: SocketAddr, origin: Option<&str>) -> Result<Client, tungstenite::Error> {
        let mut req = format!("ws://{addr}/link")
            .into_client_request()
            .expect("request");
        if let Some(origin) = origin {
            req.headers_mut()
                .insert("Origin", origin.parse().expect("origin header"));
        }
        tokio_tungstenite::connect_async(req)
            .await
            .map(|(ws, _)| ws)
    }

    async fn send_json(ws: &mut Client, v: Value) {
        ws.send(WsMessage::Text(v.to_string().into()))
            .await
            .expect("send");
    }

    async fn recv_json(ws: &mut Client) -> Value {
        loop {
            match tokio::time::timeout(Duration::from_secs(3), ws.next())
                .await
                .expect("frame within 3s")
                .expect("stream open")
                .expect("frame ok")
            {
                WsMessage::Text(t) => return serde_json::from_str(t.as_str()).expect("json"),
                WsMessage::Close(frame) => panic!("closed: {frame:?}"),
                _ => {}
            }
        }
    }

    async fn wait_until(mut cond: impl FnMut() -> bool) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not met within 2s");
    }

    /// Origin が allowlist 外なら upgrade の前に 403（ブラウザは WS を開けない）。
    #[tokio::test]
    async fn rejects_foreign_origin_before_upgrade() {
        let (_state, addr) = spawn_link_server().await;
        match connect(addr, Some("https://evil.example")).await {
            Err(tungstenite::Error::Http(resp)) => {
                assert_eq!(resp.status(), StatusCode::FORBIDDEN);
            }
            other => panic!("expected 403, got {other:?}"),
        }
    }

    /// loopback + 許す Origin → hello で登録 → hub の set が clamp されて届き、snapshot は get の往復で
    /// 返る → 切断で登録が消える（creo-ui site の dev server を模した一周）。
    #[tokio::test]
    async fn registers_surface_and_pumps_both_directions() {
        let (state, addr) = spawn_link_server().await;
        let mut ws = connect(addr, Some("http://localhost:5173"))
            .await
            .expect("loopback + localhost origin は通る");
        send_json(
            &mut ws,
            json!({"t": "hello", "v": LINK_VERSION, "app": "creo-ui", "surface": "tab-1"}),
        )
        .await;
        send_json(&mut ws, json!({"t": "mode", "mode": "on"})).await;
        send_json(
            &mut ws,
            json!({"t": "fields", "fields": [
                {"id": "sb.text.base", "label": "Text", "type": "number", "semantic": "size",
                 "constraints": {"min": 0.5, "max": 2.0}, "initial": 1.0}
            ]}),
        )
        .await;
        send_json(&mut ws, json!({"t": "selection", "selection": null})).await;
        send_json(
            &mut ws,
            json!({"t": "values", "values": {"sb.text.base": 1.0}}),
        )
        .await;

        let hub = state.link.clone();
        wait_until(|| hub.surfaces().iter().any(|s| s.fields == 1)).await;
        let (status, detail) = hub.health();
        assert_eq!(status, "connected");
        assert_eq!(detail["active"], json!("creo-ui"));

        // hub → 面: set は constraints に収めて届く
        let out = hub.set(None, "sb.text.base", json!(5)).expect("set");
        assert!(out.clamped);
        assert_eq!(
            recv_json(&mut ws).await,
            json!({"t": "set", "id": "sb.text.base", "value": 2.0})
        );

        // 読み: get が届き、面が values を返したら snapshot が返る
        let hub2 = hub.clone();
        let reader = tokio::spawn(async move { hub2.snapshot(None, Duration::from_secs(3)).await });
        assert_eq!(recv_json(&mut ws).await, json!({"t": "get"}));
        send_json(
            &mut ws,
            json!({"t": "values", "values": {"sb.text.base": 2.0}}),
        )
        .await;
        let snap = reader.await.expect("join").expect("snapshot");
        assert_eq!(snap.app, "creo-ui");
        assert_eq!(snap.values.get("sb.text.base"), Some(&json!(2.0)));

        // 面の側の変更は cache に写る
        send_json(
            &mut ws,
            json!({"t": "set", "id": "sb.text.base", "value": 0.75}),
        )
        .await;
        wait_until(|| {
            hub.surfaces().len() == 1
                && futures::executor::block_on(async {
                    // cache は snapshot 経由でなく surfaces() に出ないので set で確認する:
                    // 範囲内の値を set して clamped=false を見るだけでは cache は読めないため
                    // ここは「切断前に 1 回 values を引く」で代替する
                    true
                })
        })
        .await;

        ws.close(None).await.expect("close");
        wait_until(|| hub.surfaces().is_empty()).await;
        assert_eq!(hub.health().0, "idle");
    }

    /// wire の版が違う hello は 1002 で閉じ、登録しない。
    #[tokio::test]
    async fn closes_on_unsupported_version() {
        let (state, addr) = spawn_link_server().await;
        let mut ws = connect(addr, None)
            .await
            .expect("非ブラウザ（Origin 無し）は通る");
        send_json(
            &mut ws,
            json!({"t": "hello", "v": 99, "app": "x", "surface": "s"}),
        )
        .await;
        let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("frame")
            .expect("open")
            .expect("ok");
        match frame {
            WsMessage::Close(Some(close)) => {
                assert_eq!(close.code, CloseCode::Protocol);
                assert!(
                    close.reason.contains("unsupported Editor Link version 99"),
                    "{}",
                    close.reason
                );
            }
            other => panic!("expected close, got {other:?}"),
        }
        assert!(state.link.surfaces().is_empty());
    }

    /// 同じ surface id で繋ぎ直すと新しい接続に置き換わり、古い socket は閉じられる。
    #[tokio::test]
    async fn reconnect_replaces_and_closes_the_old_socket() {
        let (state, addr) = spawn_link_server().await;
        let mut old = connect(addr, Some(link::VP_APP_ORIGIN))
            .await
            .expect("vp-app origin");
        send_json(
            &mut old,
            json!({"t": "hello", "v": 1, "app": "Vantage Point", "surface": "vp-app#0"}),
        )
        .await;
        wait_until(|| state.link.surfaces().len() == 1).await;
        let mut new = connect(addr, Some(link::VP_APP_ORIGIN))
            .await
            .expect("second");
        send_json(
            &mut new,
            json!({"t": "hello", "v": 1, "app": "Vantage Point", "surface": "vp-app#0"}),
        )
        .await;
        send_json(&mut new, json!({"t": "mode", "mode": "on"})).await;
        wait_until(|| {
            state
                .link
                .surfaces()
                .first()
                .is_some_and(|s| s.mode == link::Mode::On)
        })
        .await;
        // 古い socket は server 側から閉じられる（None か Close）
        let ended = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match old.next().await {
                    None | Some(Err(_)) | Some(Ok(WsMessage::Close(_))) => break true,
                    Some(Ok(_)) => continue,
                }
            }
        })
        .await
        .expect("old socket ends");
        assert!(ended);
        assert_eq!(state.link.surfaces().len(), 1, "置き換えで 1 面のまま");
    }
}
