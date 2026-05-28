//! vp-nexus `/v1/auth/me` endpoint の integration test (= Phase A1b)。
//!
//! tower::ServiceExt::oneshot pattern (= 既存 tests/api.rs と同じ慣行) で
//! port bind せず Router に直接 request を流す。 mock RSA keypair を生成して
//! 自前 JWKS を `vp_nexus::auth::install_test_jwks` で cache に注入することで、
//! `Bearer <token>` flow を end-to-end (= verify middleware → handler) で検証する。
//!
//! ## test 設計
//!
//! - keypair 生成 (RSA-2048) は重いので `OnceLock` で 1 回だけ、 全 test で共有
//! - JWKS install も冪等 (= 同一 JwkSet を毎回 install しても結果同じ)、 test 並列で安全
//! - sign 用の RSA private PEM + verify 用の JWKS は同 keypair から派生

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde_json::json;
use std::sync::OnceLock;
use tower::ServiceExt;
use vp_nexus::auth::{AUDIENCE, ISSUER, install_test_jwks};

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

        let kid = "test-auth-me-kid".to_string();
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

fn sign_test_token(sub: &str, scope: Option<&str>) -> String {
    let kp = keypair();
    let now = now_secs();
    let mut claims = json!({
        "sub": sub,
        "iss": ISSUER,
        "aud": AUDIENCE,
        "exp": now + 3600,
        "iat": now,
    });
    if let Some(s) = scope {
        claims["scope"] = json!(s);
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kp.kid.clone());
    let key = EncodingKey::from_rsa_pem(kp.private_pem.as_bytes()).expect("encoding key");
    encode(&header, &claims, &key).expect("token encode")
}

async fn install_keypair_jwks() {
    install_test_jwks(keypair().jwks.clone()).await;
}

#[tokio::test]
async fn auth_me_returns_user_info_with_valid_token() {
    install_keypair_jwks().await;
    let token = sign_test_token("user_abc", Some("vp:read vp:write"));

    let app = vp_nexus::app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/me")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("body collect")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid JSON");

    assert_eq!(body["sub"], "user_abc");
    assert_eq!(body["scope"], "vp:read vp:write");
}

#[tokio::test]
async fn auth_me_without_authorization_header_returns_401() {
    install_keypair_jwks().await;

    let app = vp_nexus::app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_me_with_garbage_token_returns_401() {
    install_keypair_jwks().await;

    let app = vp_nexus::app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/me")
                .header("authorization", "Bearer not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
