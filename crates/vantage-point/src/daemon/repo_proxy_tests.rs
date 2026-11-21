//! mem_1ChFcDcS19UU6vAiWpq6x8: daemon や repo/agent を起動せず、実 channel の
//! フレームと repo routing を検証する。空の runtime registry が返す path 付き error で、
//! 選択した path が runtime dispatcher に届いたことを確認する。
//! 実際の session mode 切替は別の実機確認で扱う。

use super::*;
use serde_json::{Value, json};
use unison::{ProtocolClient, ServerHandle};

async fn fixture() -> (ServerHandle, ProtocolClient) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = ProtocolServer::with_identity("repo-proxy-test", "1.0.0", "test");
    server
        .register_channel("test", |_ctx, stream| async move {
            let channel = UnisonChannel::new(stream);
            handle_repo_proxy(
                &channel,
                &Arc::new(crate::repo::repo_registry::RepoRuntimes::new()),
            )
            .await;
            Ok(())
        })
        .await;
    let handle = Arc::new(server).listener("[::1]:0").spawn().await.unwrap();
    let client = ProtocolClient::insecure_localhost().unwrap();
    client
        .connect(&format!("quic://[::1]:{}", handle.local_addr().port()))
        .await
        .unwrap();
    (handle, client)
}

async fn request(channel: &UnisonChannel, method: &str, payload: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(2), channel.request(method, &payload))
        .await
        .expect("request was silently discarded instead of answered")
        .expect("request must receive a decodable response")
}

fn expected_dispatch_error(path: &std::path::Path) -> Value {
    let key = crate::capability::normalize_path_key(path);
    json!({"error": format!("repo 未起動 (key={key})")})
}

#[tokio::test]
async fn repo_proxy_first_request_is_dispatched_and_binding_is_retained() {
    let (handle, client) = fixture().await;
    let repo = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let channel = client.open_channel("test").await.unwrap();
    // Event では宛先を決めず、最初の Request だけが宛先を固定する。
    channel
        .send_event("subscribe", &json!({"repo_path": other.path()}))
        .await
        .unwrap();
    let expected = expected_dispatch_error(repo.path());
    assert_eq!(
        request(
            &channel,
            "session_set_mode",
            json!({
                "repo_path": repo.path(), "lane": "test/lead", "session": 1, "mode": "gui"
            })
        )
        .await,
        expected
    );
    // 後続 request に別の path があっても、最初に固定した宛先を使う。
    assert_eq!(
        request(&channel, "lanes_list", json!({"repo_path": other.path()})).await,
        expected
    );
    assert_eq!(request(&channel, "lanes_list", json!({})).await, expected);
    // 新しい channel での単発 read も、subscribe なしで宛先を固定できる。
    let read_channel = client.open_channel("test").await.unwrap();
    assert_eq!(
        request(
            &read_channel,
            "lanes_list",
            json!({"repo_path": other.path()})
        )
        .await,
        expected_dispatch_error(other.path())
    );
    client.disconnect().await.unwrap();
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn repo_proxy_missing_or_invalid_initial_path_gets_error() {
    let (handle, client) = fixture().await;
    for payload in [
        json!({}),
        json!({"repo_path": ""}),
        json!({"repo_path": null}),
        json!({"repo_path": 42}),
    ] {
        let channel = client.open_channel("test").await.unwrap();
        let response = request(&channel, "lanes_list", payload).await;
        assert_eq!(
            response,
            json!({"error": "repo-proxy: non-empty repo_path required on first request"})
        );
    }
    client.disconnect().await.unwrap();
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn repo_proxy_explicit_subscribe_still_acks_then_dispatches() {
    let (handle, client) = fixture().await;
    let repo = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let channel = client.open_channel("test").await.unwrap();
    assert_eq!(
        request(&channel, "subscribe", json!({"repo_path": repo.path()})).await,
        json!({"status": "ok"})
    );
    let expected = expected_dispatch_error(repo.path());
    assert_eq!(request(&channel, "lanes_list", json!({})).await, expected);
    assert_eq!(
        request(&channel, "lanes_list", json!({"repo_path": other.path()})).await,
        expected
    );
    client.disconnect().await.unwrap();
    handle.shutdown().await.unwrap();
}
