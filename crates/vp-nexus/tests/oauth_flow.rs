//! vp-nexus OAuth flow (= /v1/auth/login + /v1/auth/callback) の integration test (= Phase A1c)。
//!
//! token exchange (= 実 IdP との往復 POST) は test では skip — mock OAuth server を
//! 立てるのは scope 外、 後で Auth0 console 登録後の手動 smoke で検証する。
//! 本 file では login の 302 redirect と callback の state 検査までを verify する。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;
use vp_nexus::oauth::{OAuthConfig, OAuthStateStore};
use vp_nexus::{AppState, app, app_with_state};

fn test_oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: "test-client-id".to_string(),
        redirect_uri: "http://127.0.0.1:32100/v1/auth/callback".to_string(),
        scope: "openid profile email vp:read".to_string(),
        authorize_endpoint: "https://id.example.test/authorize".to_string(),
        token_endpoint: "https://id.example.test/oauth/token".to_string(),
    }
}

fn state_with_oauth(config: OAuthConfig) -> AppState {
    AppState {
        oauth_config: Some(Arc::new(config)),
        oauth_state_store: Arc::new(OAuthStateStore::new()),
    }
}

#[tokio::test]
async fn login_without_oauth_config_returns_503() {
    // default app() = OAuth config なし → 503
    let app = app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn login_with_oauth_config_returns_302_with_authorize_url() {
    let config = test_oauth_config();
    let state = state_with_oauth(config.clone());
    let store = Arc::clone(&state.oauth_state_store);
    let app = app_with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let location = response
        .headers()
        .get(axum::http::header::LOCATION)
        .expect("Location header should be present")
        .to_str()
        .expect("Location header should be ASCII");

    // authorize endpoint を base にしている
    assert!(
        location.starts_with(&config.authorize_endpoint),
        "Location should start with authorize endpoint, got: {location}"
    );
    // 必須 OAuth params すべて含む
    assert!(location.contains("response_type=code"));
    assert!(location.contains("client_id=test-client-id"));
    assert!(location.contains("scope=openid"));
    assert!(location.contains("state="));
    assert!(location.contains("code_challenge="));
    assert!(location.contains("code_challenge_method=S256"));

    // state store に 1 entry 追加されている (= state ↔ verifier、 callback で消費される)
    assert_eq!(store.len(), 1);
}

#[tokio::test]
async fn callback_with_unknown_state_returns_400() {
    let state = state_with_oauth(test_oauth_config());
    let app = app_with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/callback?code=fake-code&state=never-inserted")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn callback_without_oauth_config_returns_503() {
    // default app() = OAuth config なし → 503 (= state 関係なく config 優先)
    let app = app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/callback?code=x&state=y")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn callback_missing_query_params_returns_400() {
    let state = state_with_oauth(test_oauth_config());
    let app = app_with_state(state);

    // code only (= state なし)、 Query Extractor が 400 を返す
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/callback?code=only-code")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
