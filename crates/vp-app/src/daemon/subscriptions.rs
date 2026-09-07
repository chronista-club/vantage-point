//! daemon の Unison channel 購読 pump — lanes（retained `repo/runtime/state/#`）/ canvas（`repo/board/#`）/
//! device（`daemon-device`）。受け取った event は `AppEvent` に変換して event loop へ渡す。
//!
//! 再接続は持たない（`daemon::conn` の manager が唯一の所有者、`wait_client` で追従するだけ）。
//! 5 本の loop は骨格が似ているが障害時の方針が違う（lanes は 12 秒で `LanesError` を UI へ / canvas は
//! 500 ms retry / device は backoff + `MAX_FAILURES` で終了）。契約は doc 60 §4 の表、共通化は出荷条件にしない。
//!
//! 旧 `app/mod.rs` から移設（棚卸し 項目 6 / 6-1 #6、2026-09-08。本文は順序付き diff で一致、差分は
//! `spawn_*` の `pub(crate)` のみ）。canvas 購読が `webview::editor_bridge::editor_bridge_js` を呼ぶのは
//! doc 60 §2 の既知の例外（解消は op を `AppEvent` で渡す形に変える 6-2 で）。

use tao::event_loop::EventLoopProxy;

use crate::daemon::conn::{SharedDaemonConn, SubscriptionOutcome};
use crate::events::AppEvent;
// doc 60 §2 の既知の例外: daemon 側の購読が webview の JS builder を呼ぶ（解消は 6-2）
use crate::webview::editor_bridge::editor_bridge_js;

/// wiremsg Stage 1 consumer: repo の "lanes" Unison channel を購読し、retained Lane
/// snapshot を受信して `AppEvent::LanesLoaded` を emit する。旧 `spawn_lanes_fetch`
/// (one-shot HTTP poll) を置換する long-lived 購読。F1b: 共有 connection 上の stream で、
/// reconnect は `SharedDaemonConn` の manager が所有するので give-up せず追従する。
/// 設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
///
/// L0 SP-portless (lanes slice): 接続先は repo 直結ではなく **Daemon :32000 の集約 "lanes" channel**。
/// daemon は registry channel 経由で各 repo の lane snapshot/diff を受けて lane_registry に集約済で、
/// 本購読は repo_path で scope して当該 repo の snapshot を受ける (繋ぎ先が変わっただけで
/// consumer ロジックは不変)。
pub(crate) fn spawn_lanes_subscription(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    repo_path: String,
    conn: SharedDaemonConn,
) {
    rt_handle.spawn(lanes_subscription_loop(proxy, repo_path, conn));
}

/// lanes 購読の各フェーズ (wait_client / open / subscribe / 初回 snapshot) の stall 判定 timeout。
/// これを超えたら Daemon lanes channel 無応答 (half-alive) or QUIC 未接続とみなし Err 化 →
/// `LanesError` surface (UI が stalled 表示) + retry (self-heal)。 retained topic は本来即応するので
/// 余裕を見て 12s。 doc 30 §5-3 (loading lanes の状態区別)。
const LANES_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// "lanes" channel の購読 → 再購読を司る long-lived ループ (F1b: 共有 connection 上の stream)。
///
/// reconnect は `SharedDaemonConn` の manager が一手に所有するので、 本ループは
/// `wait_client` で接続を待ち、 得た client で session を回すだけ。 repo unreachable でも諦めず
/// 共有 connection に追従する (旧 10 連続失敗 give-up + `LanesSubscriptionEnded` は廃止)。
async fn lanes_subscription_loop(
    proxy: EventLoopProxy<AppEvent>,
    repo_path: String,
    mut conn: SharedDaemonConn,
) {
    loop {
        // 共有 connection が確立するまで待つ。 self-heal: Daemon QUIC が長時間 未接続 (dead Daemon)
        // だと wait_client が永久ブロックし「loading lanes」が silent 滞留する。 timeout を張って
        // 未接続を LanesError として surface し (UI が stalled 表示 → user が daemon restart できる)、
        // 待ち直す。 App 終了 (sender drop) は None で即抜ける。
        let client = match tokio::time::timeout(LANES_STALL_TIMEOUT, conn.wait_client()).await {
            Ok(Some(c)) => c,
            Ok(None) => return, // app 終了
            Err(_) => {
                let _ = proxy.send_event(AppEvent::LanesError {
                    repo_path: repo_path.clone(),
                    message: "daemon QUIC 未接続 (wait_client timeout)".to_string(),
                });
                continue;
            }
        };
        match run_lanes_session(&proxy, &repo_path, &client).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            // 切断は共有 manager が面倒を見るので、 次の client を待つだけ (per-session error 扱い無し)。
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                // open_channel / handshake 失敗。 surface に通知しつつ give-up せず次の接続機会を待つ。
                tracing::warn!("lanes subscription error: repo={}: {}", repo_path, e);
                let _ = proxy.send_event(AppEvent::LanesError {
                    repo_path: repo_path.clone(),
                    message: e,
                });
                // connected だが open_channel が連続失敗するケースの busy loop を避ける小休止。
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の接続セッション: QUIC connect → `open_channel("lanes")` → recv ループ。
///
/// retained topic なので接続直後に現スナップショットが届き、以降 LanePool 変化のたび
/// push される。`Ok` = セッション確立後の終了 (再接続 or app 終了)、`Err` = 接続 or
/// channel open に失敗 (失敗カウンタの対象)。
async fn run_lanes_session(
    proxy: &EventLoopProxy<AppEvent>,
    repo_path: &str,
    client: &unison::ProtocolClient,
) -> Result<SubscriptionOutcome, String> {
    // F1b: 共有 connection 上に "lanes" stream を開く (旧: session ごと別 connect)。
    // self-heal: open を LANES_STALL_TIMEOUT で括る。 timeout 時は channel 未確立 (recv_task も
    // 未起動) なので、 raw stream の drop = implicit reset で片付く (close 不要)。
    let channel = tokio::time::timeout(LANES_STALL_TIMEOUT, client.open_channel("lanes"))
        .await
        .map_err(|_| "open lanes channel: timeout".to_string())?
        .map_err(|e| format!("open lanes channel: {}", e))?;

    // ここから先の全 early-return は **確立済み channel** を残すため、 内側で結果を作ってから
    // 抜けに 1 度だけ `channel.close()` する (recv_task abort + stream close)。 close せず drop すると
    // recv_task と QUIC stream がリークし、 half-alive 障害の 12.5s retry ごとに積み上がって
    // MAX_STREAMS 枯渇 → この fix が直そうとした症状が再発する (Moody Blues #1)。
    let outcome = lanes_session_after_open(proxy, repo_path, &channel).await;
    let _ = channel.close().await;
    outcome
}

/// `run_lanes_session` の channel 確立後のロジック (subscribe → recv loop)。 呼び出し元が
/// 戻り後に必ず `channel.close()` するため、 本体は close を気にせず早期 return してよい。
async fn lanes_session_after_open(
    proxy: &EventLoopProxy<AppEvent>,
    repo_path: &str,
    channel: &unison::network::UnisonChannel,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // L0 SP-portless: Daemon "lanes" channel は repo 単位なので、 接続後に subscribe
    // handshake で repo_path を渡す (daemon 側で path_key に正規化されて lane_registry と突合)。
    // ack 後に当該 repo の snapshot が `send_event("snapshot", ...)` で初期配信される。
    // self-heal: subscribe を LANES_STALL_TIMEOUT で括る (half-alive で永久ブロックしない)。
    tokio::time::timeout(
        LANES_STALL_TIMEOUT,
        channel.request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "repo_path": repo_path }),
        ),
    )
    .await
    .map_err(|_| "lanes subscribe handshake: timeout".to_string())?
    .map_err(|e| format!("lanes subscribe handshake: {}", e))?;
    tracing::info!(
        "lanes subscription connected (via Daemon): repo={}",
        repo_path
    );

    // 初回 snapshot deadline: retained topic なので即届くはず。 来なければ stall とみなし Err (retry)。
    // 初回受信後は deadline を外し、 steady-state の変化 push を無期限に待つ。
    let mut first_snapshot_deadline = Some(tokio::time::Instant::now() + LANES_STALL_TIMEOUT);
    loop {
        let msg = match first_snapshot_deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, channel.recv()).await {
                Ok(Ok(m)) => m,
                Ok(Err(_)) => return Ok(SubscriptionOutcome::Disconnected),
                Err(_) => return Err("lanes first snapshot timeout".to_string()),
            },
            // セッション確立後の切断 (repo 停止 / channel close)。再接続対象。
            None => match channel.recv().await {
                Ok(m) => m,
                Err(_) => return Ok(SubscriptionOutcome::Disconnected),
            },
        };
        // repo 側 "lanes" channel は `send_event("snapshot", ...)` で push する。
        if msg.msg_type != MessageType::Event || msg.method != "snapshot" {
            continue;
        }
        let payload = match msg.payload_as_value() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("lanes snapshot payload parse failed: {}", e);
                continue;
            }
        };
        // payload = RepoMessage::LanesSnapshot = {"type":"lanes_snapshot","lanes":[...]}。
        // topic は `repo/runtime/state/#` の wildcard 購読なので、将来別 message
        // 種別が同 subtree に publish されても無視する。
        if payload.get("type").and_then(|t| t.as_str()) != Some("lanes_snapshot") {
            continue;
        }
        let lanes: Vec<crate::daemon_wire::LaneInfo> =
            match serde_json::from_value(payload.get("lanes").cloned().unwrap_or_default()) {
                Ok(lanes) => lanes,
                Err(e) => {
                    tracing::warn!("lanes snapshot decode failed: {}", e);
                    continue;
                }
            };
        // doc 44 D4: 開発起点 lane 名（publisher が帳簿から解決して添える）。
        // 欠落 = 旧 server / 解決不能 → None のまま送り、受け手が前回値を保つ。
        let origin = payload
            .get("origin")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // 初回 snapshot を受けたら deadline 解除 (以降は変化 push を無期限に待つ = steady-state)。
        first_snapshot_deadline = None;
        // LanesLoaded push (= retained snapshot + delta) は repo × frequency で
        // ループする systematic event なので log omit (= info / debug どちらでも noise)。
        if proxy
            .send_event(AppEvent::LanesLoaded {
                repo_path: repo_path.to_string(),
                lanes,
                origin,
            })
            .is_err()
        {
            // event loop が閉じた = app 終了。購読スレッドを畳む。
            return Ok(SubscriptionOutcome::AppClosing);
        }
    }
}

/// wiremsg Stage 2 consumer: repo の "canvas" Unison channel を購読し、Canvas (Board)
/// RepoMessage を受信して `AppEvent::CanvasMessage` を emit する。`spawn_lanes_subscription`
/// と同型（QUIC 購読 + 指数バックオフ再接続）。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
///
/// L0 SP-portless (canvas slice): 接続先は repo 直結ではなく **Daemon :32000 の集約 "canvas" channel**。
/// 各 repo が board topic を daemon に push し、 daemon が repo の TopicRouter に集約済なので、
/// 本購読は repo_path で scope して当該 repo の canvas (retained + live) を受ける。
pub(crate) fn spawn_canvas_subscription(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    repo_path: String,
    conn: SharedDaemonConn,
) {
    rt_handle.spawn(canvas_subscription_loop(proxy, repo_path, conn));
}

/// "canvas" channel の購読 → 再購読を司る long-lived ループ (F1b: 共有 connection 上の stream)。
///
/// reconnect は `SharedDaemonConn` の manager が所有。 本ループは `wait_client` で接続を待ち
/// session を回すだけで、 give-up + `CanvasSubscriptionEnded` は廃止 (共有 conn に追従)。
async fn canvas_subscription_loop(
    proxy: EventLoopProxy<AppEvent>,
    repo_path: String,
    mut conn: SharedDaemonConn,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_canvas_session(&proxy, &repo_path, &client).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("canvas subscription error: repo={}: {}", repo_path, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の "canvas" channel 接続セッション: connect → `open_channel("canvas")` → recv ループ。
///
/// "canvas" channel は `repo/board/#` retained topic を購読しており、接続直後に
/// 現スナップショット（最新 Show 等）が届く。各メッセージは `send_event("pane", <JSON>)` で
/// 来る（payload = RepoMessage の生 JSON）。
async fn run_canvas_session(
    proxy: &EventLoopProxy<AppEvent>,
    repo_path: &str,
    client: &unison::ProtocolClient,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // F1b: 共有 connection 上に "gui" stream を開く (旧: session ごと別 connect)。
    // doc 52 §6: channel 名は "canvas" → "gui"（board / terminal / conversation / editor の配信バス）。
    let channel = client
        .open_channel("gui")
        .await
        .map_err(|e| format!("open gui channel: {}", e))?;
    // L0 SP-portless: Daemon "gui" channel は repo 単位なので、 接続後に subscribe handshake で
    // repo_path を渡す (daemon 側で path_key に正規化され TopicRouter と突合)。 ack 後に当該 repo の
    // retained board (最新 Show 等) が `send_event("pane", ...)` で初期配信される。
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "repo_path": repo_path }),
        )
        .await
        .map_err(|e| format!("canvas subscribe handshake: {}", e))?;
    tracing::info!(
        "canvas subscription connected (via Daemon): repo={}",
        repo_path
    );

    loop {
        let msg = match channel.recv().await {
            Ok(m) => m,
            Err(_) => return Ok(SubscriptionOutcome::Disconnected),
        };
        // repo 側 "canvas" channel は `send_event("pane", <RepoMessage JSON>)` で push する。
        if msg.msg_type != MessageType::Event || msg.method != "pane" {
            continue;
        }
        let payload = match msg.payload_as_value() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("canvas payload parse failed: {}", e);
                continue;
            }
        };
        // doc 48 Phase 2: editor bridge command は board-handler (webview) に流さず、
        // ここで JS 評価を event loop へ依頼し、結果を同一 channel の `editor_result` で
        // 返す (request-response。channel は subscribe 済なので repo 束縛も正しい)。
        // この await 中は当該 repo の canvas event が最大 ~2.5s 待たされるが、editor
        // 操作は人間スケールの頻度なので許容 (別 task 化は順序/相関の複雑さに見合わない)。
        if payload.get("type").and_then(|v| v.as_str()) == Some("editor_command") {
            let request_id = payload
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if request_id.is_empty() {
                continue;
            }
            let op = payload.get("op").and_then(|v| v.as_str()).unwrap_or("");
            let body = match editor_bridge_js(
                op,
                payload.get("field_id").and_then(|v| v.as_str()),
                payload.get("value"),
            ) {
                Some(js) => {
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    if proxy
                        .send_event(AppEvent::EditorEval { js, resp: tx })
                        .is_err()
                    {
                        return Ok(SubscriptionOutcome::AppClosing);
                    }
                    // daemon 側の待ち (3s) より短く切る (VP-163 と同じ向き: 内側が先に諦める)
                    match tokio::time::timeout(std::time::Duration::from_millis(2500), rx.recv())
                        .await
                    {
                        Ok(Some(raw)) => serde_json::from_str::<serde_json::Value>(&raw)
                            .unwrap_or(serde_json::Value::String(raw)),
                        _ => serde_json::json!({"error": "webview 評価 timeout"}),
                    }
                }
                None => serde_json::json!({"error": format!("未知の editor op: {op}")}),
            };
            if let Err(e) = channel
                .request::<serde_json::Value, serde_json::Value>(
                    "editor_result",
                    &serde_json::json!({ "request_id": request_id, "payload": body }),
                )
                .await
            {
                tracing::warn!("editor bridge: editor_result 送信失敗: {}", e);
            }
            continue;
        }
        if proxy
            .send_event(AppEvent::CanvasMessage {
                repo_path: repo_path.to_string(),
                message: payload,
            })
            .is_err()
        {
            // event loop が閉じた = app 終了。
            return Ok(SubscriptionOutcome::AppClosing);
        }
    }
}

/// DeviceRegistry 🧲 device event 購読: daemon (32000) の "daemon-device" channel を購読して
/// `AppEvent::DeviceEvent` を emit する。 daemon に 1 本のみ (canvas/lanes は per-repo だが
/// device は machine scope = singleton)。 F1b で共有 connection 上の stream に集約。
pub(crate) fn spawn_device_subscription(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedDaemonConn,
    fleet_rx: tokio::sync::watch::Receiver<serde_json::Value>,
) {
    rt_handle.spawn(device_subscription_loop(proxy, conn, fleet_rx));
}

/// "daemon-device" channel の購読 → 再購読を司る long-lived ループ (F1b: 共有 connection 上の stream)。
///
/// device channel は **optional** (daemon が feature midi 無効 / DeviceRegistry 不在なら未登録)。 connection
/// 自体は共有 manager が維持するので、 「接続済なのに open_channel が連続失敗」= channel 未提供と
/// 判断して graceful give-up する (= device 機能なしで app は動く)。 connection-down (Disconnected)
/// は失敗カウントに含めない (channel は在った)。
async fn device_subscription_loop(
    proxy: EventLoopProxy<AppEvent>,
    mut conn: SharedDaemonConn,
    fleet_rx: tokio::sync::watch::Receiver<serde_json::Value>,
) {
    const MAX_FAILURES: u32 = 10;
    let mut failures: u32 = 0;

    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_device_session(&proxy, &client, fleet_rx.clone()).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {
                // channel は在った (= 接続できた)。 失敗カウントを reset し次 client を待つ。
                failures = 0;
            }
            Err(e) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    // 接続済なのに open_channel が連続失敗 = daemon が daemon-device を出さない
                    // (feature midi 無効 / DeviceRegistry 不在) → graceful degrade。
                    tracing::warn!(
                        "daemon-device subscription giving up (no midi / device registry absent): {}",
                        e
                    );
                    return;
                }
                let delay_ms = std::cmp::min(500u64 << (failures - 1), 16_000);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// 1 回の "daemon-device" channel 接続セッション: connect → `open_channel("daemon-device")` →
/// recv ループ。 daemon-device は接続即購読 (canvas 方式)。 各 device event は daemon が
/// `send_event("event", <DeviceEvent JSON>)` で push する。
async fn run_device_session(
    proxy: &EventLoopProxy<AppEvent>,
    client: &unison::ProtocolClient,
    mut fleet_rx: tokio::sync::watch::Receiver<serde_json::Value>,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // F1b: 共有 connection 上に "daemon-device" stream を開く (旧: 専用 connect)。
    let channel = std::sync::Arc::new(
        client
            .open_channel("daemon-device")
            .await
            .map_err(|e| format!("open daemon-device channel: {}", e))?,
    );
    tracing::info!("daemon-device subscription connected");

    // フィードバック方向 (doc 49 LE-19): webview の場の状態を daemon へ上り event で送る。
    // watch = latest-wins (連続更新は自然に coalesce)。session 終了時に abort。
    // 本関数は rt_handle.spawn 済み task 内で走るため runtime context がある —
    // 素の tokio::spawn は disallowed (tao main thread 規約) なので Handle::current 経由。
    let feedback_channel = channel.clone();
    let feedback_task = tokio::runtime::Handle::current().spawn(async move {
        while fleet_rx.changed().await.is_ok() {
            let value = fleet_rx.borrow_and_update().clone();
            if value.is_null() {
                continue;
            }
            if feedback_channel
                .send_event("feedback", &value)
                .await
                .is_err()
            {
                return; // 切断 — session ごと作り直される
            }
        }
    });

    let outcome = loop {
        let msg = match channel.recv().await {
            Ok(m) => m,
            Err(_) => break Ok(SubscriptionOutcome::Disconnected),
        };
        if msg.msg_type != MessageType::Event || msg.method != "event" {
            continue;
        }
        let payload = match msg.payload_as_value() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("daemon-device payload parse failed: {}", e);
                continue;
            }
        };
        if proxy.send_event(AppEvent::DeviceEvent { payload }).is_err() {
            // event loop が閉じた = app 終了。
            break Ok(SubscriptionOutcome::AppClosing);
        }
    };
    feedback_task.abort();
    outcome
}
