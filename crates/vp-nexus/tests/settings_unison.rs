//! vp-nexus settings sync (= unison QUIC channel) の integration test (= Phase A3a)。
//!
//! `serve_settings` を ephemeral port (`[::1]:0`) で spawn し、 unison client で
//! 接続して Authenticate → Get → Set → Get の flow を verify する。 JWT は
//! `tests/auth_endpoint.rs` と同じ mock RSA keypair + `install_test_jwks` で発行する
//! (= 実 IdP 不要)。
//!
//! client は 2 path を dogfood する:
//! - 生 `ProtocolClient` + `UnisonChannel` (= KnownProtocol 相当、 schema validation なし)
//! - `DynamicProtocol::fetch` (= runtime schema discovery、 A3 の dogfood 目的)
//!
//! ## test 並列性の注意
//!
//! JWKS cache は process-global (`OnceLock`)。 install_test_jwks は冪等 (= 同 keypair を
//! 毎回 install)。 各 test は ephemeral port で独立 server を立てるので port 衝突なし。

use base64::Engine;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use std::sync::OnceLock;
use vp_nexus::auth::{AUDIENCE, ISSUER, install_test_jwks};
use vp_nexus::settings::{SettingsStore, serve_settings};

// ============================================================================
// mock JWT keypair (= tests/auth_endpoint.rs と同じ pattern)
// ============================================================================

struct TestKeyPair {
    private_pem: String,
    jwks: JwkSet,
    kid: String,
}

static KEYPAIR: OnceLock<TestKeyPair> = OnceLock::new();

fn keypair() -> &'static TestKeyPair {
    KEYPAIR.get_or_init(|| {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("RSA key gen");
        let public = RsaPublicKey::from(&private);
        let private_pem = private
            .to_pkcs8_pem(LineEnding::LF)
            .expect("private PEM")
            .to_string();
        let kid = "test-settings-kid".to_string();
        let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.n().to_bytes_be());
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.e().to_bytes_be());
        let jwks_json = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"{kid}","use":"sig","alg":"RS256","n":"{n}","e":"{e}"}}]}}"#
        );
        let jwks: JwkSet = serde_json::from_str(&jwks_json).expect("JWKS parse");
        TestKeyPair {
            private_pem,
            jwks,
            kid,
        }
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn sign_test_token(sub: &str) -> String {
    let kp = keypair();
    let now = now_secs();
    let claims = json!({
        "sub": sub,
        "iss": ISSUER,
        "aud": AUDIENCE,
        "exp": now + 3600,
        "iat": now,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kp.kid.clone());
    let key = EncodingKey::from_rsa_pem(kp.private_pem.as_bytes()).expect("encoding key");
    encode(&header, &claims, &key).expect("token encode")
}

async fn install_keypair_jwks() {
    install_test_jwks(keypair().jwks.clone()).await;
}

// ============================================================================
// test server harness
// ============================================================================

/// rustls の process-level CryptoProvider を 1 回だけ install (= QUIC server/client 用)。
/// 冪等 (= 既 install は Err 無視)。 各 test 冒頭で呼ぶ。
fn ensure_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// settings QUIC server を ephemeral port で spawn し、 実 port と shutdown handle を返す。
/// 返り値の `shutdown_tx` を test 寿命中 hold すること (= drop で server 停止)。
async fn spawn_test_server() -> (u16, tokio::sync::oneshot::Sender<()>) {
    ensure_crypto_provider();
    let store = SettingsStore::new();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = serve_settings(store, "[::1]:0".to_string(), shutdown_rx, ready_tx).await;
    });
    let addr = ready_rx
        .await
        .expect("ready signal")
        .expect("server bound to a port");
    (addr.port(), shutdown_tx)
}

/// client helper を別 module に閉じ込める (= unison の型 import を局所化)。
mod client {
    use serde_json::Value;
    use std::sync::Arc;
    use unison::network::quic::QuicClient;
    use unison::network::{DynamicChannel, DynamicProtocol, TrustAnchors};
    use unison::{ProtocolClient, UnisonChannel};

    /// 生 `ProtocolClient` + `UnisonChannel` (= KnownProtocol 相当)。
    pub struct RawClient {
        channel: UnisonChannel,
        // client を drop させないため保持 (= connection を生かす)
        _client: Arc<ProtocolClient>,
    }

    impl RawClient {
        pub async fn connect(port: u16) -> Self {
            let transport = QuicClient::builder()
                .trust_anchors(TrustAnchors::SkipVerification)
                .build()
                .expect("quic client build");
            let client = Arc::new(ProtocolClient::new(transport));
            client
                .connect(&format!("[::1]:{port}"))
                .await
                .expect("connect");
            let channel = client
                .open_channel("settings")
                .await
                .expect("open settings channel");
            Self {
                channel,
                _client: client,
            }
        }

        pub async fn request(&self, method: &str, payload: Value) -> Value {
            self.channel
                .request::<Value, Value>(method, &payload)
                .await
                .expect("request")
        }
    }

    /// DynamicProtocol path (= discovery 経由、 schema validation あり)。
    /// proto を返り値に含めて drop させない (= connection 維持)。
    pub async fn connect_dynamic(port: u16) -> (DynamicProtocol, DynamicChannel) {
        let transport = QuicClient::builder()
            .trust_anchors(TrustAnchors::SkipVerification)
            .build()
            .expect("quic client build");
        let client = Arc::new(ProtocolClient::new(transport));
        client
            .connect(&format!("[::1]:{port}"))
            .await
            .expect("connect");
        let proto = DynamicProtocol::fetch(client)
            .await
            .expect("fetch protocol schema");
        let channel = proto
            .open_channel("settings")
            .await
            .expect("open settings channel (dynamic)");
        (proto, channel)
    }
}

// ============================================================================
// tests
// ============================================================================

#[tokio::test]
async fn authenticate_then_get_set_get_roundtrip() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;
    let token = sign_test_token("user_abc");

    let c = client::RawClient::connect(port).await;

    // Authenticate → sub 返却
    let auth = c.request("Authenticate", json!({ "token": token })).await;
    assert_eq!(auth["sub"], "user_abc", "auth response: {auth}");

    // 初期 Get → 空 + version 0
    let g0 = c.request("Get", json!({})).await;
    assert_eq!(g0["version"], 0);
    assert_eq!(g0["kdl"], "");

    // Set → version 1
    let s1 = c.request("Set", json!({ "kdl": "theme \"dark\"" })).await;
    assert_eq!(s1["version"], 1, "set response: {s1}");

    // 再 Get → 保存内容 + version 1
    let g1 = c.request("Get", json!({})).await;
    assert_eq!(g1["kdl"], "theme \"dark\"");
    assert_eq!(g1["version"], 1);
}

#[tokio::test]
async fn dynamic_protocol_discovery_roundtrip() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;
    let token = sign_test_token("dyn_user");

    // DynamicProtocol で schema を discover してから叩く (= dogfood path)
    let (proto, channel) = client::connect_dynamic(port).await;
    assert_eq!(proto.protocol_name(), "vp-settings");

    let auth = channel
        .request("Authenticate", json!({ "token": token }))
        .await
        .expect("authenticate");
    assert_eq!(auth["sub"], "dyn_user");

    let s = channel
        .request("Set", json!({ "kdl": "port 9300" }))
        .await
        .expect("set");
    assert_eq!(s["version"], 1);

    let g = channel.request("Get", json!({})).await.expect("get");
    assert_eq!(g["kdl"], "port 9300");
}

#[tokio::test]
async fn get_before_authenticate_is_rejected() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;

    let c = client::RawClient::connect(port).await;
    // Authenticate を送らずに Get → error
    let g = c.request("Get", json!({})).await;
    assert!(
        g.get("error").is_some(),
        "Get before Authenticate should error: {g}"
    );
    assert!(g["error"].as_str().unwrap().contains("unauthenticated"));
}

#[tokio::test]
async fn invalid_token_keeps_session_locked() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;

    let c = client::RawClient::connect(port).await;
    // 不正 token で Authenticate → error
    let auth = c
        .request("Authenticate", json!({ "token": "garbage-token" }))
        .await;
    assert!(auth.get("error").is_some(), "auth should fail: {auth}");

    // session は locked のまま → Set も拒否
    let s = c.request("Set", json!({ "kdl": "x 1" })).await;
    assert!(
        s.get("error").is_some(),
        "Set after failed auth should be rejected: {s}"
    );
    assert!(s["error"].as_str().unwrap().contains("unauthenticated"));
}

#[tokio::test]
async fn unknown_method_returns_error() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;
    let token = sign_test_token("u");

    let c = client::RawClient::connect(port).await;
    c.request("Authenticate", json!({ "token": token })).await;
    let r = c.request("Frobnicate", json!({})).await;
    assert!(r.get("error").is_some(), "unknown method should error: {r}");
}

#[tokio::test]
async fn two_connections_same_sub_share_store() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;
    let token = sign_test_token("shared_user");

    // conn1 で Set
    let c1 = client::RawClient::connect(port).await;
    c1.request("Authenticate", json!({ "token": token.clone() }))
        .await;
    let s = c1.request("Set", json!({ "kdl": "shared 1" })).await;
    assert_eq!(s["version"], 1);

    // conn2 (= 別 session、 同 sub) で Get → conn1 の書き込みが見える
    let c2 = client::RawClient::connect(port).await;
    c2.request("Authenticate", json!({ "token": token })).await;
    let g = c2.request("Get", json!({})).await;
    assert_eq!(g["kdl"], "shared 1");
    assert_eq!(g["version"], 1);
}

#[tokio::test]
async fn server_survives_client_disconnect() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;
    let token = sign_test_token("u1");

    // conn1 を張って即 drop (= 切断)
    {
        let c1 = client::RawClient::connect(port).await;
        c1.request("Authenticate", json!({ "token": token.clone() }))
            .await;
    }

    // server は生きていて、 次の connection が普通に動く
    let c2 = client::RawClient::connect(port).await;
    let auth = c2.request("Authenticate", json!({ "token": token })).await;
    assert_eq!(auth["sub"], "u1");
}
