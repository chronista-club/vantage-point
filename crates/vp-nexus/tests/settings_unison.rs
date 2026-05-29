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
use vp_nexus::settings::{NexusStores, OrClient, serve_settings};

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
    spawn_test_server_with_stores(NexusStores::new()).await
}

/// OR client を inject した server (= dogfood 15、 or_ref 検証 test 用)。
/// `or_base` を `Some(mock URL)` にすると NodeCreate で or_ref を mock OR に検証しに行く。
async fn spawn_test_server_with_or(
    or_base: Option<String>,
) -> (u16, tokio::sync::oneshot::Sender<()>) {
    let stores = NexusStores {
        or_client: OrClient::new(or_base),
        ..Default::default()
    };
    spawn_test_server_with_stores(stores).await
}

async fn spawn_test_server_with_stores(
    stores: NexusStores,
) -> (u16, tokio::sync::oneshot::Sender<()>) {
    ensure_crypto_provider();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = serve_settings(stores, "[::1]:0".to_string(), shutdown_rx, ready_tx).await;
    });
    let addr = ready_rx
        .await
        .expect("ready signal")
        .expect("server bound to a port");
    (addr.port(), shutdown_tx)
}

/// mock OR server (= plain HTTP axum stub)。 `GET /records/or-exists` → 200、 他 → 404。
/// 返り値の base URL を `OrClient::new(Some(..))` に渡す。 JoinHandle は test 寿命中 hold。
async fn spawn_mock_or() -> (String, tokio::task::JoinHandle<()>) {
    use axum::extract::Path;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::{Json, Router, routing::get};

    let app = Router::new().route(
        "/records/{id}",
        get(|Path(id): Path<String>| async move {
            // bearer token の中身は mock では検証しない (= 存在のみ模倣)。
            // 存在する record は JSON metadata を返す (= TreeResolve が or_meta に添付)。
            if id == "or-exists" {
                Json(serde_json::json!({
                    "id": "or-exists",
                    "filename": "spec.pdf",
                    "size": 1234,
                    "content_type": "application/pdf"
                }))
                .into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock OR bind");
    let addr = listener.local_addr().expect("mock OR addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{}", addr.port()), handle)
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

// === node tree (= OR file grouping、 dogfood 14) ===

#[tokio::test]
async fn node_tree_create_move_delete_roundtrip() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;
    let token = sign_test_token("tree_user");

    let c = client::RawClient::connect(port).await;
    c.request("Authenticate", json!({ "token": token })).await;

    // folder 2 つ + file 1 つ (= 階層)
    let docs = c.request("NodeCreate", json!({ "name": "docs" })).await;
    let docs_id = docs["node"]["id"].as_str().unwrap().to_string();
    let archive = c.request("NodeCreate", json!({ "name": "archive" })).await;
    let archive_id = archive["node"]["id"].as_str().unwrap().to_string();
    let file = c
        .request(
            "NodeCreate",
            json!({ "parent": docs_id, "name": "spec.pdf", "or_ref": "or-uuid-abc" }),
        )
        .await;
    let file_id = file["node"]["id"].as_str().unwrap().to_string();
    assert_eq!(file["node"]["or_ref"], "or-uuid-abc");
    assert_eq!(file["node"]["parent"], docs_id);

    // TreeGet → 3 node
    let t = c.request("TreeGet", json!({})).await;
    assert_eq!(t["nodes"].as_array().unwrap().len(), 3);

    // file を docs → archive へ移動
    let mv = c
        .request(
            "NodeMove",
            json!({ "id": file_id, "new_parent": archive_id }),
        )
        .await;
    assert_eq!(mv["ok"], true, "move response: {mv}");

    // 移動後の parent を確認
    let t2 = c.request("TreeGet", json!({})).await;
    let moved = t2["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == file_id)
        .unwrap();
    assert_eq!(moved["parent"], archive_id);

    // docs を削除 (= 空 folder、 1 node)
    let del = c.request("NodeDelete", json!({ "id": docs_id })).await;
    assert_eq!(del["deleted"], 1);

    // archive (file 含む) を削除 → cascade で 2 node
    let del2 = c.request("NodeDelete", json!({ "id": archive_id })).await;
    assert_eq!(del2["deleted"], 2);

    // 空に戻る
    let t3 = c.request("TreeGet", json!({})).await;
    assert_eq!(t3["nodes"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn node_move_cycle_rejected_over_channel() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;
    let token = sign_test_token("cycle_user");

    let c = client::RawClient::connect(port).await;
    c.request("Authenticate", json!({ "token": token })).await;

    let a = c.request("NodeCreate", json!({ "name": "a" })).await;
    let a_id = a["node"]["id"].as_str().unwrap().to_string();
    let b = c
        .request("NodeCreate", json!({ "parent": a_id, "name": "b" }))
        .await;
    let b_id = b["node"]["id"].as_str().unwrap().to_string();

    // a を自分の子孫 b の下に移そうとする → error
    let r = c
        .request("NodeMove", json!({ "id": a_id, "new_parent": b_id }))
        .await;
    assert!(r.get("error").is_some(), "cycle move should error: {r}");
}

#[tokio::test]
async fn node_ops_require_authentication() {
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await;

    let c = client::RawClient::connect(port).await;
    // Authenticate せずに TreeGet → unauthenticated
    let t = c.request("TreeGet", json!({})).await;
    assert!(t.get("error").is_some());
    assert!(t["error"].as_str().unwrap().contains("unauthenticated"));
}

// === OR 実連携 (= dogfood 15、 or_ref を mock OR で検証) ===

#[tokio::test]
async fn node_create_validates_or_ref_against_or() {
    install_keypair_jwks().await;
    let (or_base, _or_handle) = spawn_mock_or().await;
    let (port, _shutdown) = spawn_test_server_with_or(Some(or_base)).await;
    let token = sign_test_token("or_user");

    let c = client::RawClient::connect(port).await;
    c.request("Authenticate", json!({ "token": token })).await;

    // 実在する or_ref (= mock OR が 200) → create 成功
    let ok = c
        .request(
            "NodeCreate",
            json!({ "name": "spec.pdf", "or_ref": "or-exists" }),
        )
        .await;
    assert_eq!(
        ok["node"]["or_ref"], "or-exists",
        "valid or_ref should create: {ok}"
    );

    // 不在 or_ref (= mock OR が 404) → reject
    let bad = c
        .request(
            "NodeCreate",
            json!({ "name": "ghost.pdf", "or_ref": "or-missing" }),
        )
        .await;
    assert!(
        bad.get("error").is_some(),
        "missing or_ref should be rejected: {bad}"
    );
    assert!(bad["error"].as_str().unwrap().contains("not found in OR"));

    // or_ref なし folder は OR を叩かず作成 OK
    let folder = c.request("NodeCreate", json!({ "name": "folder" })).await;
    assert!(folder["node"]["id"].is_string());

    // tree には成功した 2 node (spec.pdf + folder) のみ
    let tree = c.request("TreeGet", json!({})).await;
    assert_eq!(tree["nodes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn tree_resolve_attaches_or_metadata() {
    install_keypair_jwks().await;
    let (or_base, _or_handle) = spawn_mock_or().await;
    let (port, _shutdown) = spawn_test_server_with_or(Some(or_base)).await;
    let token = sign_test_token("resolve_user");

    let c = client::RawClient::connect(port).await;
    c.request("Authenticate", json!({ "token": token })).await;

    // folder + 実在 file (or-exists)
    let folder = c.request("NodeCreate", json!({ "name": "docs" })).await;
    let folder_id = folder["node"]["id"].as_str().unwrap().to_string();
    let file = c
        .request(
            "NodeCreate",
            json!({ "parent": folder_id, "name": "spec.pdf", "or_ref": "or-exists" }),
        )
        .await;
    let file_id = file["node"]["id"].as_str().unwrap().to_string();

    // TreeResolve → file node に or_meta (= OR の JSON) が添付される
    let resolved = c.request("TreeResolve", json!({})).await;
    let nodes = resolved["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);

    let file_node = nodes.iter().find(|n| n["id"] == file_id).unwrap();
    assert_eq!(file_node["or_meta"]["filename"], "spec.pdf");
    assert_eq!(file_node["or_meta"]["size"], 1234);
    assert_eq!(file_node["or_meta"]["content_type"], "application/pdf");

    // folder node (= or_ref なし) は or_meta null
    let folder_node = nodes.iter().find(|n| n["id"] == folder_id).unwrap();
    assert!(folder_node["or_meta"].is_null());
}

#[tokio::test]
async fn tree_resolve_soft_when_or_disabled() {
    // OR 無効 server では NodeCreate の検証も skip され、 TreeResolve の fetch_record も
    // None → or_meta null (= resolve が OR 無効でも tree を返す soft 性)。
    install_keypair_jwks().await;
    let (port, _shutdown) = spawn_test_server().await; // OR disabled
    let token = sign_test_token("soft_user");

    let c = client::RawClient::connect(port).await;
    c.request("Authenticate", json!({ "token": token })).await;
    c.request(
        "NodeCreate",
        json!({ "name": "x", "or_ref": "or-whatever" }),
    )
    .await;

    let resolved = c.request("TreeResolve", json!({})).await;
    let n = &resolved["nodes"].as_array().unwrap()[0];
    assert!(n["or_meta"].is_null(), "or_meta should be null: {n}");
    assert_eq!(n["or_ref"], "or-whatever");
}
