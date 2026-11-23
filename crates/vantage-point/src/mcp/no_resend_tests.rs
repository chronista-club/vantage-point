// mem_1ChK5jmvVJTbLbZqvQawgk: 実 QUIC loopback で到着回数と次の call を検証する。
use super::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use unison::{ProtocolServer, ServerHandle, UnisonChannel};

struct Peer {
    handle: ServerHandle,
    calls: Arc<AtomicUsize>,
    channels: Arc<AtomicUsize>,
    connections: Arc<std::sync::Mutex<std::collections::HashSet<uuid::Uuid>>>,
}

async fn peer() -> Peer {
    peer_at("[::1]:0").await
}

async fn peer_at(addr: &str) -> Peer {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = Arc::new(ProtocolServer::with_identity("no-resend", "1", "test"));
    let calls = Arc::new(AtomicUsize::new(0));
    let channels = Arc::new(AtomicUsize::new(0));
    let connections = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    for name in ["repo-proxy", "link"] {
        let calls = calls.clone();
        let channels = channels.clone();
        let connections = connections.clone();
        server
            .register_channel(name, move |ctx, stream| {
                let connections = connections.clone();
                let calls = calls.clone();
                let channels = channels.clone();
                async move {
                    channels.fetch_add(1, Ordering::SeqCst);
                    connections.lock().unwrap().insert(ctx.connection_id);
                    let ch: Arc<UnisonChannel> = Arc::new(UnisonChannel::new(stream));
                    while let Ok(msg) = ch.recv().await {
                        if msg.method != "subscribe" {
                            calls.fetch_add(1, Ordering::SeqCst);
                        }
                        match msg.method.as_str() {
                            "reject" => ch.send_error(msg.id, &msg.method, &"拒否の理由").await?,
                            "legacy" => {
                                ch.send_response(
                                    msg.id,
                                    &msg.method,
                                    &json!({"error":"旧形式の理由"}),
                                )
                                .await?
                            }
                            "timeout" => {}
                            "slow" => {
                                let ch = ch.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_millis(80)).await;
                                    ch.send_response(msg.id, &msg.method, &json!({"ok":true}))
                                        .await
                                        .unwrap();
                                });
                            }
                            "eof" => {
                                ch.close().await?;
                                break;
                            }
                            _ => {
                                ch.send_response(msg.id, &msg.method, &json!({"ok":true}))
                                    .await?
                            }
                        }
                    }
                    Ok(())
                }
            })
            .await;
    }
    Peer {
        handle: server.listener(addr).spawn().await.unwrap(),
        calls,
        channels,
        connections,
    }
}

fn client(peer: &Peer) -> VantageMcp {
    let mut mcp = VantageMcp::new(0, "/test/no-resend".into());
    mcp.test_quic_addr = Some(peer.handle.local_addr().to_string());
    mcp
}

async fn call(mcp: &VantageMcp, link: bool, method: &str) -> Result<Value, McpError> {
    if link {
        mcp.link_call(method, json!({"side_effect":true})).await
    } else {
        mcp.quic_call_with_timeout(
            method,
            json!({"side_effect":true}),
            Duration::from_millis(500),
        )
        .await
    }
}

#[tokio::test]
async fn rejection_is_not_resent_and_channel_remains_usable() {
    for link in [false, true] {
        let peer = peer().await;
        let mcp = client(&peer);
        let error = call(&mcp, link, "reject").await.unwrap_err();
        assert_eq!(
            peer.calls.load(Ordering::SeqCst),
            1,
            "拒否した request を再送しない (link={link})"
        );
        assert!(error.message.contains("拒否の理由"), "{error:?}");
        assert!(call(&mcp, link, "ok").await.is_ok());
        assert_eq!(
            peer.channels.load(Ordering::SeqCst),
            2,
            "拒否の後は stream を開き直す"
        );
        assert_eq!(peer.connections.lock().unwrap().len(), 1, "QUIC 接続は保つ");
        peer.handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn legacy_rejection_is_an_error_without_resend() {
    for link in [false, true] {
        let peer = peer().await;
        let mcp = client(&peer);
        let error = call(&mcp, link, "legacy").await.unwrap_err();
        assert!(error.message.contains("旧形式の理由"));
        assert_eq!(peer.calls.load(Ordering::SeqCst), 1);
        assert!(call(&mcp, link, "ok").await.is_ok());
        assert_eq!(peer.channels.load(Ordering::SeqCst), 2);
        peer.handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn timeout_is_not_resent_and_next_call_recovers() {
    for link in [false, true] {
        let peer = peer().await;
        let mcp = client(&peer);
        assert!(call(&mcp, link, "timeout").await.is_err());
        assert_eq!(
            peer.calls.load(Ordering::SeqCst),
            1,
            "timeout 後の再送は禁止 (link={link})"
        );
        assert!(call(&mcp, link, "ok").await.is_ok());
        assert_eq!(peer.calls.load(Ordering::SeqCst), 2);
        peer.handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn stream_eof_is_not_resent_and_next_call_recovers() {
    for link in [false, true] {
        let peer = peer().await;
        let mcp = client(&peer);
        assert!(call(&mcp, link, "eof").await.is_err());
        assert_eq!(
            peer.calls.load(Ordering::SeqCst),
            1,
            "EOF 後の再送は禁止 (link={link})"
        );
        assert!(call(&mcp, link, "ok").await.is_ok());
        assert_eq!(peer.calls.load(Ordering::SeqCst), 2);
        peer.handle.shutdown().await.unwrap();
    }
}

// 子プロセスを強制終了して CONNECTION_CLOSE の無い再起動を再現する。
// 通常実行では起動せず、下の親テストが一時 port/file を渡して呼ぶ。
#[tokio::test]
#[ignore = "abrupt restart test の隔離された子プロセス専用"]
async fn abrupt_server_fixture() {
    let Ok(port_file) = std::env::var("VP_NO_RESEND_TEST_PORT_FILE") else {
        return;
    };
    let bind = std::env::var("VP_NO_RESEND_TEST_BIND").unwrap();
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = Arc::new(ProtocolServer::with_identity("abrupt", "1", "test"));
    for name in ["repo-proxy", "link"] {
        server
            .register_channel(name, |_ctx, stream| async move {
                let ch: Arc<UnisonChannel> = Arc::new(UnisonChannel::new(stream));
                while let Ok(msg) = ch.recv().await {
                    if msg.method != "subscribe" {
                        use std::io::Write;
                        let path = std::env::var("VP_NO_RESEND_TEST_CALLS_FILE").unwrap();
                        writeln!(
                            std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(path)
                                .unwrap(),
                            "{}",
                            msg.method
                        )
                        .unwrap();
                    }
                    ch.send_response(msg.id, &msg.method, &json!({"ok":true}))
                        .await?;
                }
                Ok(())
            })
            .await;
    }
    let handle = server.listener(&bind).spawn().await.unwrap();
    std::fs::write(port_file, handle.local_addr().to_string()).unwrap();
    std::future::pending::<()>().await;
}

struct AbruptServer {
    child: std::process::Child,
    dir: tempfile::TempDir,
}
impl AbruptServer {
    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
impl Drop for AbruptServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn abrupt_server(bind: &str) -> (AbruptServer, String) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("address");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mcp::no_resend_tests::abrupt_server_fixture",
            "--ignored",
        ])
        .env("VP_NO_RESEND_TEST_PORT_FILE", &file)
        .env("VP_NO_RESEND_TEST_BIND", bind)
        .env("VP_NO_RESEND_TEST_CALLS_FILE", dir.path().join("calls"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child = AbruptServer { child, dir };
    let addr = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(addr) = std::fs::read_to_string(&file)
                && !addr.is_empty()
            {
                break addr;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("子 server の起動");
    (child, addr)
}

// 生の旧 stream の失敗を記録する。MCP の回復契約は別テストで検証する。
#[tokio::test]
#[ignore = "旧 stream の失敗型を調べる測定専用"]
async fn abrupt_restart_observation() {
    for gap in [Duration::ZERO, Duration::from_millis(500)] {
        let (old, addr) = abrupt_server("[::1]:0").await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let transport = unison::network::quic::QuicClient::insecure_localhost().unwrap();
        let connection = transport.connection().clone();
        let client = unison::ProtocolClient::new(transport);
        client.connect(&addr).await.unwrap();
        let channel = client
            .open_channel("link")
            .await
            .unwrap()
            .with_request_timeout(Duration::from_secs(2));
        channel
            .request::<Value, Value>("probe", &json!({}))
            .await
            .unwrap();
        drop(old);
        tokio::time::sleep(gap).await;
        let (_new, _) = abrupt_server(&addr).await;
        let connected_before = client.is_connected().await;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            channel.request::<Value, Value>("probe", &json!({})),
        )
        .await;
        let reason = connection
            .read()
            .await
            .as_ref()
            .and_then(|c| c.close_reason());
        eprintln!(
            "abrupt observation gap={gap:?} connected_before={connected_before} phase=request result={result:?} elapsed={:?} quinn_close_reason={reason:?} connected_after={}",
            started.elapsed(),
            client.is_connected().await
        );
        client.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn known_dead_connection_is_replaced_before_first_request() {
    for link in [false, true] {
        let old = peer().await;
        let mcp = client(&old);
        call(&mcp, link, "ok").await.unwrap();
        let used = if link {
            mcp.get_link_channel().await.unwrap()
        } else {
            mcp.get_quic_channel().await.unwrap()
        };
        let addr = old.handle.local_addr().to_string();
        old.handle.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while used.is_connected().await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // shutdown は endpoint.close を呼ぶが UDP socket の解放完了までは待たない。
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::net::UdpSocket::bind(&addr).is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("旧 server の UDP socket 解放");
        let new = peer_at(&addr).await;
        let started = std::time::Instant::now();
        call(&mcp, link, "ok")
            .await
            .expect("既知の死んだ接続は最初の送信前に回復");
        eprintln!(
            "graceful restart recovery link={link} elapsed={:?} business_calls={}",
            started.elapsed(),
            new.calls.load(Ordering::SeqCst)
        );
        assert_eq!(new.calls.load(Ordering::SeqCst), 1);
        new.handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn internal_timeout_is_not_resent_and_next_stream_recovers() {
    let peer = peer().await;
    let mcp = client(&peer);
    let channel = mcp
        .process_connection
        .lock()
        .await
        .get(
            &peer.handle.local_addr().to_string(),
            "repo-proxy",
            "/test",
            Duration::from_millis(30),
        )
        .await
        .unwrap();
    let error = rpc::request_once(
        channel,
        "process",
        "timeout",
        &json!({}),
        Duration::from_secs(1),
    )
    .await
    .unwrap_err();
    assert!(
        error.message.contains("Timeout error"),
        "内部 timeout: {error:?}"
    );
    assert_eq!(peer.calls.load(Ordering::SeqCst), 1);
    call(&mcp, false, "ok").await.unwrap();
    assert_eq!(peer.channels.load(Ordering::SeqCst), 2);
    assert_eq!(peer.connections.lock().unwrap().len(), 1);
    peer.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejection_does_not_close_another_in_flight_request() {
    let peer = peer().await;
    let mcp = client(&peer);
    let slow_mcp = mcp.clone();
    let slow = tokio::spawn(async move { call(&slow_mcp, false, "slow").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while peer.calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(call(&mcp, false, "reject").await.is_err());
    call(&mcp, false, "ok").await.unwrap();
    slow.await
        .unwrap()
        .expect("別 stream の進行中 request を閉じない");
    assert_eq!(peer.calls.load(Ordering::SeqCst), 3);
    assert_eq!(peer.channels.load(Ordering::SeqCst), 3);
    assert_eq!(peer.connections.lock().unwrap().len(), 1);
    peer.handle.shutdown().await.unwrap();
}

async fn first_call_after_abrupt_restart(link: bool) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for gap in [Duration::ZERO, Duration::from_millis(500)] {
        let (old, addr) = abrupt_server("[::1]:0").await;
        let mut mcp = VantageMcp::new(0, "/test/abrupt".into());
        mcp.test_quic_addr = Some(addr.clone());
        call(&mcp, link, "ok").await.unwrap();
        drop(old);
        tokio::time::sleep(gap).await;
        let (new, _) = abrupt_server(&addr).await;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(3), call(&mcp, link, "ok")).await;
        eprintln!(
            "restart recovery link={link} gap={gap:?} elapsed={:?} result={result:?} business_calls={}",
            started.elapsed(),
            new.calls()
        );
        result
            .expect("最初の call は3秒以内")
            .expect("業務送信前の回復で成功する");
        assert_eq!(new.calls(), 1, "業務 request は1回だけ届く");
    }
}

#[tokio::test]
async fn process_first_call_after_abrupt_restart_is_bounded_and_sent_once() {
    first_call_after_abrupt_restart(false).await;
}
#[tokio::test]
async fn link_first_call_after_abrupt_restart_is_bounded_and_sent_once() {
    first_call_after_abrupt_restart(true).await;
}

#[tokio::test]
#[ignore = "loopback 速度測定。単独実行して p50/p95 を証拠 log に保存する"]
async fn fresh_stream_latency_comparison() {
    for name in ["repo-proxy", "link"] {
        let peer = peer().await;
        let addr = peer.handle.local_addr().to_string();
        let old = connect_quic(&addr).await.unwrap();
        let ch = old.0.open_channel(name).await.unwrap();
        if name == "repo-proxy" {
            ch.request::<Value, Value>("subscribe", &json!({"repo_path":"/test"}))
                .await
                .unwrap();
        }
        let mut cache = rpc::ConnectionCache::default();
        let payload = json!({"side_effect":true});
        for fresh in [false, true] {
            let mut samples = Vec::new();
            for iteration in 0..120 {
                let start = std::time::Instant::now();
                if fresh {
                    let channel = cache
                        .get(&addr, name, "/test", Duration::from_secs(60))
                        .await
                        .unwrap();
                    rpc::request_once(channel, name, "ok", &payload, Duration::from_secs(5))
                        .await
                        .unwrap();
                } else {
                    tokio::time::timeout(
                        Duration::from_secs(5),
                        ch.request::<Value, Value>("ok", &payload),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                }
                if iteration >= 20 {
                    samples.push(start.elapsed().as_secs_f64() * 1_000_000.0);
                }
            }
            samples.sort_by(f64::total_cmp);
            eprintln!(
                "latency channel={name} fresh_stream={fresh} n={} warmup=20 p50_us={:.1} p95_us={:.1}",
                samples.len(),
                samples[49],
                samples[94]
            );
        }
        ch.close().await.unwrap();
        old.0.disconnect().await.unwrap();
        drop(cache);
        peer.handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn unavailable_peer_preparation_is_bounded() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    // UDP port は確保するが QUIC handshake には応じない。
    let blackhole = std::net::UdpSocket::bind("[::1]:0").unwrap();
    let mut mcp = VantageMcp::new(0, "/test".into());
    mcp.test_quic_addr = Some(blackhole.local_addr().unwrap().to_string());
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_secs(3), call(&mcp, false, "ok"))
        .await
        .expect("準備2試行は有界")
        .unwrap_err();
    assert!(error.message.contains("preparation timed out"), "{error:?}");
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "2試行の準備期限を観測"
    );
}
