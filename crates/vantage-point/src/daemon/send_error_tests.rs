//! mem_1ChM9ambsYeZTn6gHmuMEt: daemon の handler の失敗は Error frame（`send_error`）で返る。
//!
//! 本物の channel 群（[`register_daemon_channels`]）を loopback の server に載せ、client の
//! `request()` が成功の値ではなく `NetworkError::Protocol` で理由を受けることを確かめる。
//! 断った後も同じ channel で次の request が通ること、断られた `wire/send` が store に何も
//! 残さないこと、新旧の client（v0.88 の federation direct を写したもの / v0.89 の client）が
//! 理由を読めることも、ここで固定する。
//!
//! daemon の常駐プロセス・実 repo・実機 MIDI は使わない（`build_test_daemon_state` の前提）。

use super::*;
use serde_json::{Value, json};
use unison::{ProtocolClient, ServerHandle};

struct Fixture {
    handle: ServerHandle,
    client: ProtocolClient,
    port: u16,
}

impl Fixture {
    async fn start(state: Arc<DaemonState>) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = ProtocolServer::with_identity("vp-daemon-test", "1.0.0", "test");
        register_daemon_channels(&server, &state).await;
        let handle = Arc::new(server).listener("[::1]:0").spawn().await.unwrap();
        let port = handle.local_addr().port();
        let client = ProtocolClient::insecure_localhost().unwrap();
        client
            .connect(&format!("quic://[::1]:{port}"))
            .await
            .unwrap();
        Self {
            handle,
            client,
            port,
        }
    }

    async fn channel(&self, name: &str) -> UnisonChannel {
        self.client.open_channel(name).await.unwrap()
    }

    async fn stop(self) {
        self.client.disconnect().await.unwrap();
        self.handle.shutdown().await.unwrap();
    }
}

/// request を 1 回送り、返事を待つ（黙って捨てられたら落とす）。
async fn call(
    channel: &UnisonChannel,
    method: &str,
    payload: Value,
) -> Result<Value, NetworkError> {
    tokio::time::timeout(
        Duration::from_secs(5),
        channel.request::<Value, Value>(method, &payload),
    )
    .await
    .unwrap_or_else(|_| panic!("{method}: 応答が来ない（黙って捨てられた）"))
}

/// 断られた request の Error frame の文面を返す。成功の frame で返ったら落とす。
async fn refused(channel: &UnisonChannel, method: &str, payload: Value) -> String {
    match call(channel, method, payload).await {
        Ok(v) => panic!("{method}: 失敗が成功の frame で返った: {v}"),
        Err(NetworkError::Protocol(message)) => {
            assert!(
                message.starts_with("Request error: "),
                "{method}: Error frame でない Protocol error: {message}"
            );
            message
        }
        Err(e) => panic!("{method}: Error frame でない失敗: {e}"),
    }
}

/// Error frame の payload は `{"error": 理由}`（旧形式の成功の frame と同じ JSON、unison 自身の
/// `channel-not-found` / `connection closed` と同じ形）。
fn error_frame_text(reason: &str) -> String {
    format!("Request error: {}", json!({ "error": reason }))
}

/// 断られた後に同じ channel で通るはずの request（method, payload）。
type FollowUp = Option<(&'static str, Value)>;

#[tokio::test]
async fn unknown_methods_are_refused_with_reason_and_the_channel_keeps_serving() {
    let fx = Fixture::start(build_test_daemon_state().await).await;
    // (channel, 断られる理由, 断られた後に同じ channel で通る request)
    let cases: Vec<(&str, String, FollowUp)> = vec![
        (
            "registry",
            "不明なメソッド: registry.no_such".into(),
            Some(("list", json!({}))),
        ),
        (
            "events",
            "不明なメソッド: events.no_such".into(),
            Some(("query", json!({}))),
        ),
        (
            "daemon-control",
            "不明なメソッド: daemon-control.no_such".into(),
            Some(("ping", json!({}))),
        ),
        (
            "link",
            "unknown link method: no_such".into(),
            Some(("surfaces", json!({}))),
        ),
        (
            "daemon-repo",
            "不明なメソッド: daemon-repo.no_such".into(),
            Some(("list", json!({}))),
        ),
        ("wire", "不明な wire channel method: no_such".into(), None),
        #[cfg(feature = "midi")]
        ("device", "不明なメソッド: device.no_such".into(), None),
    ];
    for (name, reason, follow_up) in cases {
        let channel = fx.channel(name).await;
        assert_eq!(
            refused(&channel, "no_such", json!({})).await,
            error_frame_text(&reason),
            "{name}"
        );
        if let Some((method, payload)) = follow_up {
            call(&channel, method, payload)
                .await
                .unwrap_or_else(|e| panic!("{name}.{method}: 断った後の request が通らない: {e}"));
        }
    }
    fx.stop().await;
}

#[tokio::test]
async fn handler_errors_are_refused_with_reason() {
    let fx = Fixture::start(build_test_daemon_state().await).await;
    let cases: Vec<(&str, &str, Value, &str)> = vec![
        ("events", "emit", json!({}), "kind は必須"),
        (
            "daemon-control",
            "repos/add",
            json!({ "path": "/tmp" }),
            "name is required",
        ),
        (
            "wire",
            "wire/send",
            json!({ "from": "agent@vp", "to": ["agent@nexus"], "body": {} }),
            "wire store not initialized",
        ),
        (
            "wire",
            "lane/session-changed",
            json!({}),
            "lane/session-changed: 'repo' (repo 名) required",
        ),
        (
            "link",
            "editor_set",
            json!({ "value": 1 }),
            "editor_set: id 必須",
        ),
        #[cfg(feature = "midi")]
        (
            "device",
            "report_device",
            json!({ "port_name": "x", "state": "weird" }),
            "不明な device state: weird",
        ),
    ];
    for (name, method, payload, reason) in cases {
        let channel = fx.channel(name).await;
        let message = refused(&channel, method, payload).await;
        assert!(
            message.contains(reason),
            "{name}.{method}: 理由が届かない: {message}"
        );
    }
    fx.stop().await;
}

#[tokio::test]
async fn repo_proxy_and_gui_relay_the_dispatch_failure_as_an_error_frame() {
    let fx = Fixture::start(build_test_daemon_state().await).await;
    let repo = tempfile::tempdir().unwrap();
    let key = crate::capability::normalize_path_key(repo.path());
    let not_started = error_frame_text(&format!("repo 未起動 (key={key})"));

    // repo-proxy: 最初の request に repo_path が無ければ断って handler を終える。
    let channel = fx.channel("repo-proxy").await;
    assert_eq!(
        refused(&channel, "lanes_list", json!({})).await,
        error_frame_text("repo-proxy: non-empty repo_path required on first request")
    );

    // repo-proxy: 未起動の repo への forward は理由付きで断り、channel は次も応える。
    let channel = fx.channel("repo-proxy").await;
    for _ in 0..2 {
        assert_eq!(
            refused(&channel, "lanes_list", json!({ "repo_path": repo.path() })).await,
            not_started
        );
    }

    // gui の上り（surface → repo）も同じ forward を通る。
    let channel = fx.channel("gui").await;
    call(&channel, "subscribe", json!({ "repo_path": repo.path() }))
        .await
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            refused(
                &channel,
                "terminal_write",
                json!({ "lane": "x/lead", "data": "a" })
            )
            .await,
            not_started
        );
    }
    fx.stop().await;
}

// =============================================================================
// wire/send — federation の受け口（他の Mac の daemon が direct で叩く）
// =============================================================================

async fn mem_wire_store() -> crate::capability::WiremsgStore {
    let db = crate::db::VpDb::connect_mem().await.expect("connect_mem");
    db.define_schema().await.expect("define_schema");
    crate::capability::WiremsgStore::new(Arc::new(db.inner().clone()))
        .await
        .expect("WiremsgStore::new")
}

async fn stored_messages(store: &crate::capability::WiremsgStore) -> usize {
    let mut res = store
        .db()
        .query("SELECT id FROM wire_messages")
        .await
        .expect("query");
    let rows: Vec<Value> = res.take(0).expect("take");
    rows.len()
}

/// 受信側の `wire/send` が断るのは「保存していない」時だけ。
///
/// federation の古い相手（v0.88 以前）は、断られると同じ envelope を relay でもう一度送る。
/// それが二重配送にならないのは、断った時点で store に何も残っていないから。保存の後に
/// 失敗し得る段を足すと、ここが落ちる。
#[tokio::test]
async fn refused_wire_send_leaves_nothing_in_the_store() {
    let store = mem_wire_store().await;
    let fx =
        Fixture::start(build_test_daemon_state_with_wire_store(Some(store.clone())).await).await;
    let channel = fx.channel("wire").await;
    let rejected = [
        (
            json!({ "from": "agent", "to": ["agent@nexus"], "body": { "k": 1 } }),
            "bare",
        ),
        (
            json!({ "from": "agent@vp", "to": ["agent"], "body": { "k": 1 } }),
            "bare",
        ),
        (
            json!({ "to": ["agent@nexus"], "body": { "k": 1 } }),
            "'from' required",
        ),
        (
            json!({
                "from": "agent@vp", "to": ["agent@nexus"], "body": { "k": 1 },
                "reply_to": "no-such-message"
            }),
            "not found",
        ),
    ];
    for (envelope, reason) in rejected {
        let message = refused(&channel, "wire/send", envelope.clone()).await;
        assert!(message.contains(reason), "{envelope}: {message}");
    }
    assert_eq!(
        stored_messages(&store).await,
        0,
        "断った wire/send が残っている"
    );

    // 対照: 受け付けた送信は store に残る（数え方が正しいことの確認）。
    let sent = call(
        &channel,
        "wire/send",
        json!({ "from": "agent@vp", "to": ["agent@nexus"], "body": { "k": 1 } }),
    )
    .await
    .expect("正しい envelope は受け付ける");
    assert_eq!(sent["status"], "ok");
    assert_eq!(stored_messages(&store).await, 1);
    fx.stop().await;
}

/// v0.88.0 の `dialer::wire_send_direct` の送信部分の写し（federation の古い相手）。
///
/// 旧形式（成功の frame の `{"error"}`）を期待するが、`request()` の Err も同じ Err にする。
/// 古い相手はこの Err を受けると relay に回す（旧形式で断られた時も同じ）。
async fn v088_direct_send(channel: &UnisonChannel, envelope: &Value) -> anyhow::Result<()> {
    use anyhow::Context;
    let resp: Value = channel
        .request("wire/send", envelope)
        .await
        .context("direct: wire/send request 失敗")?;
    if let Some(err) = resp.get("error") {
        anyhow::bail!("direct: 受信側 wire/send がエラー: {err}");
    }
    Ok(())
}

#[tokio::test]
async fn old_federation_peer_reads_the_refusal_as_err_with_reason() {
    let store = mem_wire_store().await;
    let fx =
        Fixture::start(build_test_daemon_state_with_wire_store(Some(store.clone())).await).await;
    let channel = fx.channel("wire").await;
    let error = v088_direct_send(
        &channel,
        &json!({ "from": "agent", "to": ["agent@nexus"], "body": { "k": 1 } }),
    )
    .await
    .expect_err("断られた送信は Err");
    assert!(format!("{error:#}").contains("bare"), "{error:#}");
    // 受け付けられる送信は、古い相手でも成功として読める。
    v088_direct_send(
        &channel,
        &json!({ "from": "agent@vp", "to": ["agent@nexus"], "body": { "k": 1 } }),
    )
    .await
    .expect("受け付けた送信は Ok");
    assert_eq!(stored_messages(&store).await, 1);
    fx.stop().await;
}

#[tokio::test]
async fn current_federation_direct_send_gets_after_send_with_reason() {
    let store = mem_wire_store().await;
    let fx =
        Fixture::start(build_test_daemon_state_with_wire_store(Some(store.clone())).await).await;
    let entry = crate::daemon::hub_client::NodeEntry {
        node_id: "target".into(),
        endpoints: vec![format!("[::1]:{}", fx.port)],
        handle: "target".into(),
        name: "target".into(),
        registered_at: String::new(),
        connected: true,
    };
    let error = crate::daemon::dialer::wire_send_direct(
        &entry,
        &json!({ "from": "agent", "to": ["agent@nexus"], "body": { "k": 1 } }),
    )
    .await
    .expect_err("断られた送信は Err");
    // 送った後の拒否 = relay に回さない側（#1279）。理由も届く。
    assert!(
        matches!(error, crate::daemon::dialer::DirectSendError::AfterSend(_)),
        "{error:?}"
    );
    assert!(error.to_string().contains("bare"), "{error}");
    assert_eq!(stored_messages(&store).await, 0);
    fx.stop().await;
}

// =============================================================================
// v0.89 の client × 新しい server — 理由がそのまま届く
// =============================================================================

#[tokio::test]
async fn current_clients_receive_the_reason() {
    let fx = Fixture::start(build_test_daemon_state().await).await;
    let repo = tempfile::tempdir().unwrap();

    // CLI の repo-proxy ask（`vp lane …` 系）。
    let error = crate::commands::process_client::daemon_repo_request(
        fx.port,
        repo.path().to_str().unwrap(),
        "lanes_list",
        json!({}),
    )
    .await
    .expect_err("未起動 repo は Err");
    assert!(format!("{error:#}").contains("repo 未起動"), "{error:#}");

    // CLI の daemon-control。
    let control = crate::daemon::client::DaemonControlClient::connect(fx.port, 1)
        .await
        .unwrap();
    let error = control
        .repos_stop("no-such-repo-send-error-test")
        .await
        .expect_err("未登録 repo の停止は Err");
    assert!(
        format!("{error:#}").contains("Repo not found: no-such-repo-send-error-test"),
        "{error:#}"
    );

    // CLI の events。旧形式では成功の frame に seq が無く、理由（kind は必須）を落としていた。
    let events = crate::daemon::client::DaemonClient::connect(fx.port, 1)
        .await
        .unwrap();
    let error = events
        .events_emit("", None, Value::Null)
        .await
        .expect_err("kind 無しの emit は Err");
    assert!(format!("{error:#}").contains("kind は必須"), "{error:#}");
    fx.stop().await;
}
