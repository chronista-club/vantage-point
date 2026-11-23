// mem_1ChK5jmvVJTbLbZqvQawgk: 実 QUIC の direct と relay の到着を数える。
use super::*;
use std::sync::atomic::AtomicUsize;
use unison::ProtocolServer;

async fn delivery_case(mode: &'static str) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let direct = Arc::new(AtomicUsize::new(0));
    let relay = Arc::new(AtomicUsize::new(0));
    let target = Arc::new(ProtocolServer::with_identity("target", "1", "test"));
    if mode != "open-failure" {
        let direct = direct.clone();
        target
            .register_channel("wire", move |_ctx, stream| {
                let direct = direct.clone();
                async move {
                    let ch: UnisonChannel = UnisonChannel::new(stream);
                    let msg = ch.recv().await?;
                    direct.fetch_add(1, Ordering::SeqCst);
                    match mode {
                        "reject" => {
                            ch.send_error(msg.id, &msg.method, &"direct refusal")
                                .await?
                        }
                        "legacy" => {
                            ch.send_response(
                                msg.id,
                                &msg.method,
                                &json!({"error":"direct refusal"}),
                            )
                            .await?
                        }
                        "eof" => ch.close().await?,
                        "timeout" => {
                            let _ = ch.recv().await;
                        }
                        _ => {
                            ch.send_response(msg.id, &msg.method, &json!({"ok":true}))
                                .await?
                        }
                    }
                    Ok(())
                }
            })
            .await;
    }
    let target_handle = target.listener("[::1]:0").spawn().await.unwrap();
    let endpoint = target_handle.local_addr().to_string();
    let hub = Arc::new(ProtocolServer::with_identity("hub", "1", "test"));
    hub.register_channel("nodes", move |_ctx, stream| {
        let endpoint = endpoint.clone();
        async move {
            let ch: UnisonChannel = UnisonChannel::new(stream);
            while let Ok(msg) = ch.recv().await {
                ch.send_response(
                    msg.id,
                    &msg.method,
                    &json!({"nodes":[{
                        "node_id":"target", "handle":"target", "name":"target", "registered_at":"",
                        "endpoints":[endpoint], "connected":true
                    }]}),
                )
                .await?;
            }
            Ok(())
        }
    })
    .await;
    let relay_count = relay.clone();
    hub.register_channel("relay", move |_ctx, stream| {
        let relay = relay_count.clone();
        async move {
            let ch: UnisonChannel = UnisonChannel::new(stream);
            ch.recv().await?;
            relay.fetch_add(1, Ordering::SeqCst);
            ch.send_event("status", &json!({"status":"established"}))
                .await?;
            let _ = ch.recv().await;
            Ok(())
        }
    })
    .await;
    let handle = hub.listener("[::1]:0").spawn().await.unwrap();
    // 認証 refresh や稼働 hub に触れず、送信処理を loopback 接続に渡す。
    let addr = handle.local_addr().to_string();
    let client = build_hub_client(&addr).unwrap();
    client.connect(&addr).await.unwrap();
    let ch = client.open_channel("nodes").await.unwrap();
    let client = HubClient {
        client,
        ch,
        credentialed: false,
    };
    let result =
        federate_wire_send_on(&client, "target", "source", &json!({"id":"once"}), true).await;
    let relay_calls = relay.load(Ordering::SeqCst);
    let direct_calls = direct.load(Ordering::SeqCst);
    client.client.disconnect().await.unwrap();
    handle.shutdown().await.unwrap();
    target_handle.shutdown().await.unwrap();
    if mode == "open-failure" {
        assert!(result.is_ok(), "{result:?}");
        assert_eq!((direct_calls, relay_calls), (0, 1));
    } else {
        assert_eq!(relay_calls, 0, "送信後の失敗を relay しない: {mode}");
        assert_eq!(direct_calls, 1);
        let error = result.unwrap_err();
        if matches!(mode, "reject" | "legacy") {
            assert!(format!("{error:#}").contains("direct refusal"));
        }
    }
}

#[tokio::test]
async fn rejection_does_not_fall_back_to_relay() {
    delivery_case("reject").await;
}
#[tokio::test]
async fn legacy_rejection_does_not_fall_back_to_relay() {
    delivery_case("legacy").await;
}
#[tokio::test]
async fn eof_does_not_fall_back_to_relay() {
    delivery_case("eof").await;
}
#[tokio::test]
async fn timeout_does_not_fall_back_to_relay() {
    delivery_case("timeout").await;
}
#[tokio::test]
async fn channel_open_failure_can_use_relay() {
    delivery_case("open-failure").await;
}
