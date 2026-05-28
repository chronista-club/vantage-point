//! VP Nexus library — federation hub の router 構築と handler 群
//!
//! main.rs は parse + serve だけ薄く保ち、 router 構築をここに寄せる
//! ことで test では Router を直接 oneshot して assertion できる
//! (= 別 server 起動 / port bind 不要、 test の決定性が上がる)。

use axum::{Json, Router, routing::get};
use serde::Serialize;

/// crate version (= Cargo.toml `version.workspace = true` の値が compile 時に展開される)
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// service 識別子 (= /health response の `service` field、 観測 / federation discovery 用)
pub const SERVICE_NAME: &str = "nexus";

/// build 時の git short SHA (= build.rs で `git rev-parse --short=12 HEAD` から埋め込み)。
/// git 無し環境 / source tarball build では `"unknown"`。
pub const GIT_SHA: &str = env!("NEXUS_GIT_SHA");

/// build 時刻 (= build.rs で `date -u +%Y-%m-%dT%H:%M:%SZ` から埋め込み、 RFC3339 UTC)。
/// 取得失敗時は `"unknown"`。
pub const BUILT_AT: &str = env!("NEXUS_BUILT_AT");

/// `/health` response body
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub version: &'static str,
}

/// `/v1/hello` response body
#[derive(Debug, Serialize)]
pub struct HelloResponse {
    pub name: &'static str,
    pub tagline: &'static str,
}

/// `/v1/version` response body — build info (= ops / debug / release verification 用)
#[derive(Debug, Serialize)]
pub struct VersionResponse {
    pub name: &'static str,
    pub version: &'static str,
    pub git_sha: &'static str,
    pub built_at: &'static str,
}

/// `/health` handler — liveness check 兼 service identification
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: SERVICE_NAME,
        version: VERSION,
    })
}

/// `/v1/hello` handler — federation hub の名乗り (= 将来 capability advertise を載せる枠)
async fn hello() -> Json<HelloResponse> {
    Json(HelloResponse {
        name: SERVICE_NAME,
        tagline: "VP federation hub at nexus.vantage-point.app",
    })
}

/// `/v1/version` handler — build info を返す (= deploy 後の version 確認 / git_sha pinning に使う)
async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        name: SERVICE_NAME,
        version: VERSION,
        git_sha: GIT_SHA,
        built_at: BUILT_AT,
    })
}

/// Axum Router を構築する。 main / test 共通エントリ。
pub fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/hello", get(hello))
        .route("/v1/version", get(version))
}
