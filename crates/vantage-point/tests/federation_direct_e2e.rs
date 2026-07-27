//! federation direct dialer（ADR-020 §S6、HEv2 staggered race）の e2e。
//!
//! relay_e2e と違い **hub 不要** — target daemon 相当の QUIC server（`wire` channel）を
//! in-process に立て、dead decoy を混ぜた endpoints への direct race が生きている
//! endpoint を掴んで wire envelope を配送することを実証する。
//!
//! 通常 CI では `#[ignore]`（実 QUIC/UDP loopback を張る medium tier）。手動実行:
//!
//! ```bash
//! cargo test -p vantage-point --test federation_direct_e2e -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::timeout;
use unison::network::MessageType;
use unison::network::channel::UnisonChannel;
use unison::{ProtocolServer, ServerHandle};
use vantage_point::daemon::dialer;
use vantage_point::daemon::hub_client::NodeEntry;

/// target daemon 相当の QUIC server を `[::1]:0` に立てる。daemon の wire channel 慣習を
/// ミラー: `wire/send` request の payload を `tx` へ流し、`{"ok": true}` を返す
/// （エラー時は success frame に `{"error": ...}` — VP-163 慣習）。
async fn start_wire_target(tx: mpsc::Sender<Value>) -> anyhow::Result<(ServerHandle, u16)> {
    let server = ProtocolServer::with_identity("direct-e2e-target", "1.0.0", "test");
    server
        .register_channel("wire", move |_ctx, stream| {
            let tx = tx.clone();
            async move {
                let channel: UnisonChannel = UnisonChannel::new(stream);
                loop {
                    let msg = match channel.recv().await {
                        Ok(msg) => msg,
                        Err(_) => break,
                    };
                    if msg.msg_type != MessageType::Request {
                        continue;
                    }
                    let payload = msg.payload_as_value().unwrap_or_default();
                    let response = if msg.method == "wire/send" {
                        let _ = tx.send(payload).await;
                        json!({ "ok": true })
                    } else {
                        json!({ "error": format!("unknown method: {}", msg.method) })
                    };
                    if channel
                        .send_response(msg.id, &msg.method, &response)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(())
            }
        })
        .await;
    let handle = server.spawn_listen("[::1]:0").await?;
    let port = handle.local_addr().port();
    Ok((handle, port))
}

/// dead decoy（`[::1]:1` — 誰も listen していない）を先頭に置いても、direct race が生きている
/// target を掴んで wire envelope を配送する（§S6 の VP 消費側 happy path）。
#[tokio::test]
#[ignore = "medium: 実 QUIC/UDP loopback を張る（hub は不要）"]
async fn direct_race_delivers_wire_envelope_despite_dead_decoy() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let (rx_tx, mut rx) = mpsc::channel::<Value>(4);
    let (handle, port) = start_wire_target(rx_tx).await?;

    let entry = NodeEntry {
        wld_id: "wld_direct-e2e".to_string(),
        // dead decoy 先頭 + live: race が decoy の失敗を待たずに live を掴むこと。
        endpoints: vec!["[::1]:1".to_string(), format!("[::1]:{port}")],
        handle: "direct-e2e".to_string(),
        name: "direct e2e target".to_string(),
        registered_at: String::new(),
        connected: false,
    };
    let envelope = json!({ "from": "lane@src", "to": "lane@dst", "body": "hello via direct" });

    let t0 = Instant::now();
    dialer::wire_send_direct(&entry, &envelope).await?;
    let elapsed = t0.elapsed();

    // 受信側が同じ payload を受け取った = direct 経路の配送成立。
    let received = timeout(Duration::from_secs(5), rx.recv())
        .await?
        .expect("target が wire/send payload を受信するはず");
    assert_eq!(received, envelope, "envelope が direct 経路で無変形に届く");
    // dead decoy の存在は stagger 1 tick + α に収まる（deadline 1.5s の内側で決着）。
    assert!(
        elapsed < Duration::from_millis(1500),
        "race は deadline 内に live を掴む（elapsed={elapsed:?}）"
    );

    handle.shutdown().await?;
    Ok(())
}

/// 全 endpoint が dead → 有界時間内に Err（federate_wire_send はこれを受けて relay floor へ
/// 落とす。§S6 の degrade ladder が「数秒の hang」にならないことの担保）。
#[tokio::test]
#[ignore = "medium: 実 QUIC/UDP loopback を張る（hub は不要）"]
async fn direct_all_dead_fails_bounded() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let entry = NodeEntry {
        wld_id: "wld_direct-dead".to_string(),
        endpoints: vec!["[::1]:1".to_string()],
        handle: "direct-dead".to_string(),
        name: "dead target".to_string(),
        registered_at: String::new(),
        connected: false,
    };
    let envelope = json!({ "body": "never delivered" });

    let t0 = Instant::now();
    let res = dialer::wire_send_direct(&entry, &envelope).await;
    let elapsed = t0.elapsed();

    assert!(res.is_err(), "全滅は Err（caller が relay floor に落とす）");
    // connection refused は fast-fail（deadline を待たない）。緩めに 3s で bound を検証。
    assert!(
        elapsed < Duration::from_secs(3),
        "全滅判定は有界（elapsed={elapsed:?}）"
    );
    Ok(())
}
