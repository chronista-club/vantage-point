//! vp-nexus integration test。
//!
//! port を bind せず、 `tower::ServiceExt::oneshot` で Router に直接
//! request を流し込んで response を検査する (= 既存 VP の test 慣行に
//! 揃える、 port 衝突 / 待ち時間 0、 parallel test safe)。

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn health_endpoint_returns_ok_status_and_version() {
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
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
    assert_eq!(body["status"], "ok");
    assert_eq!(body["service"], "nexus");
    // version は Cargo.toml workspace.version (= env!() で compile 時固定) と一致するはず
    assert_eq!(body["version"], vp_nexus::VERSION);
}

#[tokio::test]
async fn hello_endpoint_returns_federation_hub_tagline() {
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/hello")
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
    assert_eq!(body["name"], "nexus");
    assert_eq!(
        body["tagline"],
        "VP federation hub at nexus.vantage-point.app"
    );
}

#[tokio::test]
async fn version_endpoint_returns_build_info() {
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/version")
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
    assert_eq!(body["name"], "nexus");
    assert_eq!(body["version"], vp_nexus::VERSION);

    // git_sha / built_at は build.rs で埋め込まれる。 performer 内 build なら "unknown"
    // 以外になるはず (= git CLI 利用可能、 .git も上位 dir に存在)。 ただし
    // source tarball build / CI cache 等で "unknown" になる可能性も許容するため、
    // 空でないこと + string であることだけ assert する (= regulation な assertion)。
    let git_sha = body["git_sha"].as_str().expect("git_sha is string");
    assert!(!git_sha.is_empty(), "git_sha should not be empty");

    let built_at = body["built_at"].as_str().expect("built_at is string");
    assert!(!built_at.is_empty(), "built_at should not be empty");
}

#[tokio::test]
async fn capabilities_endpoint_returns_skeleton_with_empty_arrays() {
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/capabilities")
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
    assert_eq!(body["service"], "nexus");
    assert_eq!(body["version"], vp_nexus::VERSION);

    // skeleton 段階: 両 array は存在するが空 (= 後続 task で具体機能を埋めていく前提)
    let capabilities = body["capabilities"]
        .as_array()
        .expect("capabilities should be array");
    assert_eq!(
        capabilities.len(),
        0,
        "capabilities should be empty at skeleton stage"
    );

    let protocols = body["protocols"]
        .as_array()
        .expect("protocols should be array");
    assert_eq!(
        protocols.len(),
        0,
        "protocols should be empty at skeleton stage"
    );
}

#[tokio::test]
async fn unknown_route_returns_404() {
    // 後続 task で federation API を増やす際の回帰防止 (= 未登録 path は 404)
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/wire/forward")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn version_endpoint_git_sha_has_expected_format() {
    // dogfood 3 で追加: build.rs 改善後の git_sha format を検証。
    // 受理する pattern:
    //   - "unknown"                 (= git command 不在 / .git 不在の fallback)
    //   - 12 桁 hex                  (= clean working tree、 例: "b6ba56f3e7d7")
    //   - 12 桁 hex + "-dirty"       (= dirty working tree、 例: "c86ec867905b-dirty")
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("body collect")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("valid JSON");
    let git_sha = body["git_sha"].as_str().expect("git_sha is string");

    assert!(
        is_valid_git_sha(git_sha),
        "git_sha format mismatch: {git_sha:?} (expected 'unknown', 12-hex, or 12-hex+'-dirty')"
    );
}

/// dogfood 3 helper: build.rs の git_sha 出力 format を検証する。
fn is_valid_git_sha(s: &str) -> bool {
    if s == "unknown" {
        return true;
    }
    let core = s.strip_suffix("-dirty").unwrap_or(s);
    core.len() == 12 && core.chars().all(|c| c.is_ascii_hexdigit())
}

/// A1d: POST /v1/auth/logout は認証不要で 200 + `{logged_out: true}` を返す
/// (= stateless logout の ack、 idempotent、 副作用なし)。
#[tokio::test]
async fn logout_endpoint_returns_logged_out_true() {
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/logout")
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
    assert_eq!(body["logged_out"], true);
}

/// A1d: GET /v1/auth/logout は 405 Method Not Allowed を返す
/// (= POST only、 CSRF 経由の偶発的呼び出し防止)。
#[tokio::test]
async fn logout_endpoint_rejects_get_method() {
    let app = vp_nexus::app();

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/auth/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot should succeed");

    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
