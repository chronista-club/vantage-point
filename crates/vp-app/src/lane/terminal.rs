//! lane の **terminal session**（PTY 出力の購読 + keystroke / resize の上り request）。
//!
//! `run()` が `HashMap<String, LaneTerminal>` で lane ごとに 1 本持ち、`cmd_tx` 経由で命令を送る。
//! 接続は `daemon::conn::SharedDaemonConn` を握り、再接続は conn manager の所有（doc 60 §3）。
//! `cmd_rx` は再接続を跨いで生きる（切断中の write / resize は次接続で送る、doc 60 §4）。
//! app/mod.rs から移設（棚卸し 項目 6 / 6-1 #10、2026-09-08）。

use tao::event_loop::EventLoopProxy;

use crate::daemon::conn::{SharedDaemonConn, SubscriptionOutcome};
use crate::events::AppEvent;

/// terminal S4 (doc 27 §4.1): per-lane terminal session への command (WebView → repo)。
#[derive(Debug)]
pub(crate) enum TermCmd {
    /// keystroke (session, base64)。 canvas channel 上り request `terminal_write` で repo に送る。
    /// doc 50 §4.6 A6: `session` は宛先 slot（0 = 未指定 → repo が root に解決）。
    Write(u32, String),
    /// resize (session, cols, rows)。 `terminal_resize` で送る（0 = 未指定 → root）。
    Resize(u32, u16, u16),
}

/// terminal S4: 1 lane の terminal session handle (event loop が保持)。
///
/// map から remove すると `cmd_tx` が drop され、 session loop の `cmd_rx.recv()` が None を返して
/// 停止 → canvas channel drop → daemon 側 demand stop → repo pump stop
/// (= 購読者が消えたら pump を畳む、 S2 demand-driven production の出口)。
pub(crate) struct LaneTerminal {
    pub(crate) cmd_tx: tokio::sync::mpsc::UnboundedSender<TermCmd>,
}

/// terminal S4: lane の terminal を Daemon "canvas" channel に乗せる per-lane session を spawn。
///
/// `lane_key` = `<repo>/root` 等 (`LaneAddressWire::key()`)。 Daemon :32000 の "canvas"
/// channel に `pattern: process/terminal/data/{lane_key}/out` で subscribe → Daemon demand 発火 →
/// repo pump start。 受信した PTY 出力は `AppEvent::TerminalOutput` で event loop に流し、 cmd_rx
/// 経由の write/resize は同 channel の上り request で repo に forward する (S3 bidirectional)。
pub(crate) fn spawn_terminal_session(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
) -> LaneTerminal {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    rt_handle.spawn(terminal_session_loop(
        proxy, conn, repo_path, lane_key, cmd_rx,
    ));
    LaneTerminal { cmd_tx }
}

/// "canvas" channel (terminal pattern) の購読 → 再購読を司る long-lived ループ
/// (F1b: 共有 connection 上の per-lane stream)。 `cmd_rx` は再接続を跨いで保持する
/// (= 切断中に積まれた write/resize は次接続で送れる)。 reconnect は共有 manager が所有するので
/// `wait_client` で接続を待ち、 give-up はしない (lane 消滅 = cmd_tx drop で AppClosing 終了)。
async fn terminal_session_loop(
    proxy: EventLoopProxy<AppEvent>,
    mut conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<TermCmd>,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_terminal_session(&proxy, &repo_path, &lane_key, &client, &mut cmd_rx).await {
            // AppClosing = event loop 終了 or lane removed (cmd_tx drop) → session 終了。
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("terminal session error: lane={}: {}", lane_key, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の terminal session: connect → `open_channel("canvas")` → subscribe(terminal pattern) →
/// recv (出力) / cmd (入力・resize) の select ループ。
///
/// 出力 `channel.recv()` と 上り `channel.request()` を同一 select! で扱う。 unison の `request` の
/// response は pending map で解決され `recv()` には来ず、 また `recv()` は内部 buffer 由来で
/// cancel-safe (= concurrent recv+request は control/repo-proxy で実証済) なので、 cmd 分岐で
/// recv future を drop しても出力欠落しない。
async fn run_terminal_session(
    proxy: &EventLoopProxy<AppEvent>,
    repo_path: &str,
    lane_key: &str,
    client: &unison::ProtocolClient,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TermCmd>,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // F1b: 共有 connection 上に per-lane terminal 用 "gui" stream を開く (旧: lane ごと別 connect)。
    let channel = client
        .open_channel("gui")
        .await
        .map_err(|e| format!("open gui channel: {}", e))?;
    // 当該 lane の terminal topic を pattern 指定で subscribe (= demand を立てて repo pump を起こす)。
    let topic = format!("repo/terminal/data/{}/out", lane_key.replace('/', "~"));
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "repo_path": repo_path, "pattern": topic }),
        )
        .await
        .map_err(|e| format!("terminal subscribe handshake: {}", e))?;
    tracing::info!(
        "terminal session connected: lane={} topic={}",
        lane_key,
        topic
    );

    loop {
        tokio::select! {
            recvd = channel.recv() => {
                let msg = match recvd {
                    Ok(m) => m,
                    Err(_) => return Ok(SubscriptionOutcome::Disconnected),
                };
                if msg.msg_type != MessageType::Event || msg.method != "pane" {
                    continue;
                }
                let payload = match msg.payload_as_value() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // LaneTerminalOutput { lane, session, data(base64) }。 lane は subscription で
                // 確定済なので、session（doc 50 §4.6 A6 — 同 topic に複数 session が流れる）と
                // data を抜いて lane_key 付きで JS に渡す。session 欠落は 1（旧 sender 互換 =
                // RepoMessage 側の serde default と同値）。
                if let Some(data) = payload.get("data").and_then(|v| v.as_str())
                    && proxy
                        .send_event(AppEvent::TerminalOutput {
                            lane: lane_key.to_string(),
                            session: payload
                                .get("session")
                                .and_then(serde_json::Value::as_u64)
                                .and_then(|n| u32::try_from(n).ok())
                                .unwrap_or(1),
                            data: data.to_string(),
                        })
                        .is_err()
                {
                    return Ok(SubscriptionOutcome::AppClosing);
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(TermCmd::Write(session, data)) => {
                        // session=0 は「未指定」= repo が root に解決する（slot 系の規律）。
                        let mut payload = serde_json::json!({ "lane": lane_key, "data": data });
                        if session > 0 {
                            payload["session"] = serde_json::Value::from(session);
                        }
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "terminal_write",
                                &payload,
                            )
                            .await;
                    }
                    Some(TermCmd::Resize(session, cols, rows)) => {
                        let mut payload =
                            serde_json::json!({ "lane": lane_key, "cols": cols, "rows": rows });
                        if session > 0 {
                            payload["session"] = serde_json::Value::from(session);
                        }
                        // ⚠️ 応答は捨てる。repo 側は slot 未登録でも **intent を預かって登録時に
                        // 適用する**（`LanePool::desired_size`）ので、ここで retry する必要が無い。
                        // 2026-07-26 以前はそれが無く、この `let _` が「resize が落ちた」ことを
                        // 3 層にわたって不可視にしていた。
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "terminal_resize",
                                &payload,
                            )
                            .await;
                    }
                    // cmd_tx drop = lane removed → session 終了 (channel drop で demand stop)。
                    None => return Ok(SubscriptionOutcome::AppClosing),
                }
            }
        }
    }
}
