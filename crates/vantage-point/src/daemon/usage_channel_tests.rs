//! "daemon-usage" channel（engine usage meter）の振る舞い。
//!
//! 本物の channel 登録（[`register_usage_channel`] / [`register_daemon_channels`]）を loopback の
//! server に載せ、client 側から「繋いだら snapshot が来る」「`refresh` event で probe が走り、
//! 結果が下りの snapshot で届く」を確かめる。probe は偽物（本物の CLI は起動しない）。

use super::*;
use crate::usage::probe::UsageProbe;
use crate::usage::{EngineUsage, UsageEngine, UsageService, UsageSnapshot, UsageWindow};
use serde_json::Value;
use unison::{ProtocolClient, ServerHandle};

struct CountingProbe {
    calls: std::sync::Mutex<Vec<UsageEngine>>,
}

#[async_trait::async_trait]
impl UsageProbe for CountingProbe {
    async fn probe(&self, engine: UsageEngine) -> EngineUsage {
        self.calls.lock().unwrap().push(engine);
        EngineUsage {
            windows: vec![UsageWindow::new("weekly", "weekly", 42.0, None)],
            ..EngineUsage::empty(engine)
        }
    }
}

async fn start(register: impl AsyncFnOnce(&ProtocolServer)) -> (ServerHandle, ProtocolClient) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = ProtocolServer::with_identity("vp-daemon-test", "1.0.0", "test");
    register(&server).await;
    let handle = Arc::new(server).listener("[::1]:0").spawn().await.unwrap();
    let port = handle.local_addr().port();
    let client = ProtocolClient::insecure_localhost().unwrap();
    client
        .connect(&format!("quic://[::1]:{port}"))
        .await
        .unwrap();
    (handle, client)
}

/// 次の `snapshot` event を待つ（来なければ落とす）。
async fn next_snapshot(channel: &UnisonChannel) -> UsageSnapshot {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), channel.recv())
            .await
            .expect("snapshot が届かない")
            .expect("recv");
        if msg.msg_type == MessageType::Event && msg.method == "snapshot" {
            let value: Value = msg.payload_as_value().expect("payload");
            return serde_json::from_value(value).expect("UsageSnapshot の形");
        }
    }
}

#[tokio::test]
async fn subscribe_gets_snapshot_and_refresh_event_runs_probe() {
    let probe = Arc::new(CountingProbe {
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let usage = Arc::new(UsageService::new(probe.clone(), None));
    let service = usage.clone();
    let (handle, client) =
        start(async move |server| register_usage_channel(server, service).await).await;

    let channel = client.open_channel("daemon-usage").await.unwrap();
    let first = next_snapshot(&channel).await;
    assert_eq!(first.engines.len(), 5, "繋いだ直後に現在の全量が来る");
    assert!(first.engines.iter().all(|e| e.windows.is_empty()));
    assert!(
        usage.watched(),
        "購読中は watched（定期 poll が spawn を伴う engine も起こす）"
    );

    channel
        .send_event("refresh", &serde_json::json!({}))
        .await
        .unwrap();
    // 5 engine 分が順に反映される — 最後の 1 通まで待つ
    let mut latest = next_snapshot(&channel).await;
    while latest.engines.iter().any(|e| e.windows.is_empty()) {
        latest = next_snapshot(&channel).await;
    }
    for engine in UsageEngine::ALL {
        assert_eq!(
            latest.engine(engine).unwrap().windows[0].used_pct,
            42.0,
            "{engine:?}"
        );
    }
    assert_eq!(probe.calls.lock().unwrap().len(), 5);

    channel.close().await.ok();
    client.disconnect().await.unwrap();
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn daemon_registers_usage_channel() {
    let state = build_test_daemon_state().await;
    let (handle, client) =
        start(async move |server| register_daemon_channels(server, &state).await).await;
    let channel = client.open_channel("daemon-usage").await.unwrap();
    let first = next_snapshot(&channel).await;
    let names: Vec<&str> = first.engines.iter().map(|e| e.engine.as_str()).collect();
    assert_eq!(
        names,
        vec!["claude", "codex", "agy", "grok", "muse"],
        "表示順の固定"
    );
    channel.close().await.ok();
    client.disconnect().await.unwrap();
    handle.shutdown().await.unwrap();
}
