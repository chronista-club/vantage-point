//! daemon との接続 — 共有 QUIC connection の manager と、その上の control / repo-proxy ask。
//!
//! **再接続の唯一の所有者**（doc 60 §3）: epoch ごとに fresh `ProtocolClient` を build → connect →
//! `current` に publish → 切断検知で `None` に戻して exp backoff。購読 loop（`app` 側、6-1 で
//! `daemon::subscriptions` へ）は `wait_client` で追従するだけで、自前の reconnect を持たない。
//!
//! 旧 `app/mod.rs` の `SubscriptionOutcome` / `SharedDaemonConn` / `spawn_daemon_conn_manager` /
//! `daemon_repo_request`（棚卸し 項目 6 / 6-1、2026-09-08。本文は順序付き diff で一致、差分は可視性のみ）。
//! ⚠️ `daemon_repo_request` は呼ぶたびに QUIC client を作って捨てる（F6 の暫定）。共有接続への
//! 一本化（1 RPC = 1 stream / 必ず close / 30 秒の別 const / 自動再送しない）は doc 60 §6 B の独立 PR。

use std::time::Duration;

/// 1 回の Unison channel 接続セッションの終わり方 ("lanes" / "canvas" 購読が共用)。
pub(crate) enum SubscriptionOutcome {
    /// セッション確立後に切断 (repo restart / channel close)。即再接続の対象。
    Disconnected,
    /// event loop が閉じた (= app 終了)。購読スレッドを畳む。
    AppClosing,
}

/// F1b (doc 27 §3.4.4): vp-app → Daemon :32000 の全 persistent session (lanes / canvas /
/// terminal / device) を **1 QUIC connection に集約**するための共有ハンドル。
///
/// `current` watch は現 epoch の `ProtocolClient` (= 1 connection) を全 session に配る
/// (None = 未接続 / 再接続中)。 session は `wait_client()` で接続を待ち、 得た client で
/// `open_channel` して自分の stream を張る (= 1 conn × N streams)。 reconnect は manager task が
/// 一手に所有し、 epoch ごとに fresh client を connect → publish する (F1a repo uplink と同パターン)。
///
/// 旧構成は session ごと (lanes / canvas は repo ごと、 terminal は lane ごと) に別 QUIC
/// connection を張り、 QUIC の多重化を使えていなかった (§3.4.4 負債)。 これを 1 connection に畳む。
#[derive(Clone)]
pub(crate) struct SharedDaemonConn {
    current: tokio::sync::watch::Receiver<Option<std::sync::Arc<unison::ProtocolClient>>>,
}

/// control RPC を打つ前に共有 connection の確立を待つ既定の上限 (user 操作 / 定期 poll)。
///
/// 旧 HTTP client は daemon が down なら即 connection refused で返っていたので、
/// 「待たずに諦める」方が近い挙動になる。長くすると offline 時に poll が詰まる。
pub(crate) const CONTROL_WAIT: Duration = Duration::from_secs(5);

/// 起動直後の初回 fetch だけ待ちを伸ばす。
///
/// app 起動 → `spawn_daemon_conn_manager` → `ensure_daemon_ready` (daemon の auto-launch) の順で
/// 走るため、初回は「daemon がまだ listen していない」時間帯に必ずぶつかる。ここで諦めると
/// sidebar が空のまま居座る (activity poller の再 fetch trigger は「値が変化したら」なので、
/// 0 件のまま安定してしまうと二度と発火しない)。
pub(crate) const BOOT_CONTROL_WAIT: Duration = Duration::from_secs(30);

impl SharedDaemonConn {
    /// 共有 connection が確立する (current = Some) まで待ち、 その client を返す。
    /// watch sender が drop された (= app 終了) 場合は None。
    pub(crate) async fn wait_client(&mut self) -> Option<std::sync::Arc<unison::ProtocolClient>> {
        loop {
            if let Some(client) = self.current.borrow().clone() {
                return Some(client);
            }
            // None の間は変化を待つ。 sender drop で Err = app 終了。
            if self.current.changed().await.is_err() {
                return None;
            }
        }
    }

    /// control plane RPC (`daemon-control` / `registry`) 用の client を得る (doc 45 段 3)。
    ///
    /// 共有 connection が未確立なら `wait` まで待つ。 待っても来なければ Err —
    /// caller は旧 HTTP 失敗時と同じく warn して degrade する。
    pub(crate) async fn control_within(
        &self,
        wait: Duration,
    ) -> anyhow::Result<crate::daemon::control::DaemonControl> {
        let mut conn = self.clone();
        match tokio::time::timeout(wait, conn.wait_client()).await {
            Ok(Some(client)) => Ok(crate::daemon::control::DaemonControl::new(client)),
            Ok(None) => anyhow::bail!("app 終了中 (daemon conn manager 停止)"),
            Err(_) => anyhow::bail!("Daemon QUIC 未接続 (daemon 未起動?)"),
        }
    }

    /// [`Self::control_within`] の既定待ち時間版。
    pub(crate) async fn control(&self) -> anyhow::Result<crate::daemon::control::DaemonControl> {
        self.control_within(CONTROL_WAIT).await
    }
}

/// 共有 Daemon connection を connect / reconnect し続ける manager を spawn し、 ハンドルを返す。
///
/// epoch ごとに fresh `ProtocolClient` を build → connect → `current` に publish → 切断検知で
/// None に戻して exp backoff reconnect。 全 session が `wait_client` で追従する。 reconnect 機構を
/// ここに一元化することで、 各 session は channel logic だけを持てば良くなる (関心分離)。
pub(crate) fn spawn_daemon_conn_manager(
    rt_handle: &tokio::runtime::Handle,
    daemon_port: u16,
) -> SharedDaemonConn {
    let (current_tx, current_rx) =
        tokio::sync::watch::channel::<Option<std::sync::Arc<unison::ProtocolClient>>>(None);

    rt_handle.spawn(async move {
        use unison::ProtocolClient;
        use unison::network::ClientConnectionEvent;
        use unison::network::TrustAnchors;
        use unison::network::quic::QuicClient;

        let addr = format!("[::1]:{}", daemon_port);
        const INITIAL_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(16);
        let mut backoff = INITIAL_BACKOFF;
        let mut generation: u64 = 0;

        loop {
            // epoch ごとに fresh client (F1a repo uplink と同じ「再接続 = 新 client」パターン)。
            let transport = match QuicClient::builder()
                .trust_anchors(TrustAnchors::SkipVerification)
                .build()
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("daemon conn: QUIC client build 失敗: {} (リトライ)", e);
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
                    continue;
                }
            };
            let client = std::sync::Arc::new(ProtocolClient::new(transport));

            match client.connect(&addr).await {
                Ok(()) => {
                    backoff = INITIAL_BACKOFF;
                    generation += 1;
                    tracing::info!(
                        "daemon conn: 共有 connection 確立 (gen={}, addr={})",
                        generation,
                        addr
                    );
                    let mut conn_events = client.subscribe_connection_events();
                    // F1b heartbeat: vp-app は passive subscriber (recv 待ち) のみで能動送信が無いため、
                    // connection 死を QUIC idle timeout (60s) でしか検知できない。 15s ごとに
                    // daemon-control へ ping して liveness を能動確認する (client→server 一方向、 server は
                    // 応答のみ = 両端 heartbeat にしない)。 open 失敗時は None で conn_events (60s) に degrade。
                    let heartbeat = client.open_channel("daemon-control").await.ok();
                    // session に新 client を配る。 receiver 全滅 (= app 終了) なら manager も終了。
                    if current_tx.send(Some(client.clone())).is_err() {
                        return;
                    }
                    let mut hb_tick = tokio::time::interval(std::time::Duration::from_secs(15));
                    hb_tick.tick().await; // 最初の tick (即時) をスキップ
                    // 切断を待つ (conn_events か heartbeat 失敗のどちらか早い方で再接続へ抜ける)。
                    loop {
                        tokio::select! {
                            conn_ev = conn_events.recv() => {
                                match conn_ev {
                                    Ok(ClientConnectionEvent::Disconnected { reason }) => {
                                        tracing::warn!("daemon conn: 切断検知 ({}) → 再接続", reason);
                                        break;
                                    }
                                    Ok(_) => {}
                                    Err(_) => break, // event channel closed = client 異常、 再接続へ
                                }
                            }
                            _ = hb_tick.tick() => {
                                if let Some(hb) = &heartbeat {
                                    // 5s 以内に pong が返らなければ connection 死と判断 (idle timeout 60s を待たない)。
                                    let pong = tokio::time::timeout(
                                        std::time::Duration::from_secs(5),
                                        hb.request::<serde_json::Value, serde_json::Value>("ping", &serde_json::json!({})),
                                    )
                                    .await;
                                    if !matches!(pong, Ok(Ok(_))) {
                                        tracing::warn!("daemon conn: heartbeat 応答なし → 再接続");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    // 再接続前に None を配る (session は wait_client で次 client を待つ)。
                    if current_tx.send(None).is_err() {
                        return;
                    }
                    drop(heartbeat);
                    // 旧 connection を明示 close する。 session は同じ `Arc<ProtocolClient>` を握って
                    // recv() でブロックしているため、 manager 側の drop だけでは refcount>0 で
                    // connection が閉じず、 session の recv() は old connection の idle timeout (60s)
                    // まで Err にならない (= heartbeat で manager を 15s 再接続させても session が 60s
                    // migrate しない)。 disconnect() で即 stream reset → session の recv() が即 Err →
                    // wait_client で次 client へ移る。
                    let _ = client.disconnect().await;
                    drop(client); // 次 loop で fresh client
                }
                Err(e) => {
                    tracing::debug!(
                        "daemon conn: 接続失敗 ({}), {}ms 後 retry",
                        e,
                        backoff.as_millis()
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
                }
            }
        }
    });

    SharedDaemonConn {
        current: current_rx,
    }
}

/// F6 (doc 27 §3.4): vp-app → daemon repo-proxy → repo の one-shot ask。
///
/// 旧 SP HTTP 直結 (`reqwest http://127.0.0.1:{repo_port}/api/...`) の置換。 surface は Daemon :32000
/// だけに繋ぐ (§6)。 低頻度 ask 専用 (pp:state debounce save / lane ops) なので 1 回ごとに
/// connect → `open_channel("repo-proxy")` → handshake({repo_path}) → request(method) → drop。
/// (connection 共有は F1 で畳む。) method は repo `dispatch_repo_method` に届き、 戻り値が返る。
pub(crate) async fn daemon_repo_request(
    daemon_port: u16,
    repo_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use unison::ProtocolClient;
    use unison::network::TrustAnchors;
    use unison::network::quic::QuicClient;

    let addr = format!("[::1]:{}", daemon_port);
    let transport = QuicClient::builder()
        .trust_anchors(TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client build: {}", e))?;
    let client = ProtocolClient::new(transport);
    client
        .connect(&addr)
        .await
        .map_err(|e| format!("connect {}: {}", addr, e))?;
    let channel = client
        .open_channel("repo-proxy")
        .await
        .map_err(|e| format!("open repo-proxy: {}", e))?;
    // handshake: repo_path → daemon が path_key 正規化 → 当該 repo control へ routing。
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "repo_path": repo_path }),
        )
        .await
        .map_err(|e| format!("repo-proxy handshake: {}", e))?;
    // ask: method を daemon が repo dispatch_repo_method へ forward し応答を relay。
    let resp = channel
        .request::<serde_json::Value, serde_json::Value>(method, &payload)
        .await
        .map_err(|e| format!("repo-proxy {}: {}", method, e))?;
    // repo は dispatch の Err を `{"error": ...}` の**正常応答**として返す（discovery.rs の
    // Daemon uplink/control）。transport 成功 = 処理成功ではないので、ここで Err に戻す。
    // これが無いと呼び手は全員「ok」と読み、未実装 method を旧 binary の repo に投げた時などに
    // 「成功ログが出るのに何も起きない」silent success になる。
    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        return Err(format!("repo-proxy {}: {}", method, err));
    }
    Ok(resp)
}
