//! `vp auth` subcommand — Creo ID 認証 (= Phase A2、 vp-cli client integration)
//!
//! ## 概要
//!
//! vp-cli が Creo ID OIDC で取得した access token を保持し、 nexus
//! (= protected resource) に Bearer header で talk するための CLI 入口。
//!
//! ## A2a 段階 (= dogfood 9、 本 module 初版)
//!
//! `vp auth me` のみ実装。 既存の `~/.vp/credentials.json` を読んで nexus の
//! `/v1/auth/me` を叩く。 token 取得 (= `vp auth login`) は A2b で実装するため、
//! A2a smoke は「手で作った credential file」 もしくは「credential なし → usage 表示」
//! で動作確認する。
//!
//! ## flow
//!
//! ```text
//! vp auth me
//!   ↓ read ~/.vp/credentials.json (or ${VP_CREDENTIALS_PATH})
//!   ↓ GET ${VP_NEXUS_URL}/v1/auth/me with Authorization: Bearer <token>
//!   ↓ 200 → print {sub, scope}
//!     401 → "token invalid or expired" + exit 1
//!     その他 → anyhow error
//! ```

use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `vp auth` subcommand 一覧。 A2a 段階では `me` のみ、 A2b/c/d で login / logout 追加予定。
#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    /// 現在 login しているユーザー情報を表示 (= nexus /v1/auth/me を叩く)
    Me,
}

/// `~/.vp/credentials.json` で保存される credentials の serde shape。
///
/// access_token 以外は optional (= 最小 token のみでも parse OK)。
/// A2b で OAuth token endpoint レスポンス全体を保存予定。
#[derive(Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// credentials の保存先 — 順序: env `VP_CREDENTIALS_PATH` > `~/.vp/credentials.json`。
pub fn credentials_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("VP_CREDENTIALS_PATH") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var("HOME").context("HOME env not set")?;
    Ok(PathBuf::from(home).join(".vp").join("credentials.json"))
}

/// credentials を file から読む。 file 不在なら `Ok(None)` を返し、 上位で usage 表示。
pub fn read_credentials() -> Result<Option<Credentials>> {
    let path = credentials_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let creds: Credentials = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(creds))
}

/// nexus base URL — env `VP_NEXUS_URL` > default `http://127.0.0.1:9200`。
fn nexus_url() -> String {
    std::env::var("VP_NEXUS_URL").unwrap_or_else(|_| "http://127.0.0.1:9200".to_string())
}

/// `vp auth <subcommand>` のエントリ — main.rs から呼ばれる dispatch。
pub async fn execute(cmd: AuthCommands) -> Result<()> {
    match cmd {
        AuthCommands::Me => me().await,
    }
}

/// `vp auth me` — credential を読んで nexus `/v1/auth/me` を叩き user info 表示。
async fn me() -> Result<()> {
    let creds = match read_credentials()? {
        Some(c) => c,
        None => {
            eprintln!("error: not logged in (= ~/.vp/credentials.json なし)");
            eprintln!("       run `vp auth login` first (= TBD、 A2b で実装予定)");
            std::process::exit(1);
        }
    };

    let url = format!("{}/v1/auth/me", nexus_url());
    let resp = reqwest::Client::new()
        .get(&url)
        .header("authorization", format!("Bearer {}", creds.access_token))
        .send()
        .await
        .with_context(|| format!("failed to call {url}"))?;

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        eprintln!("error: token invalid or expired");
        eprintln!("       run `vp auth login` again (= TBD、 A2b)");
        std::process::exit(1);
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("nexus returned {status}: {body}");
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse /v1/auth/me JSON response")?;
    println!("sub: {}", body["sub"].as_str().unwrap_or("<unknown>"));
    if let Some(scope) = body["scope"].as_str() {
        println!("scope: {scope}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_deserialize_minimal() {
        let json = r#"{"access_token": "test-token"}"#;
        let creds: Credentials = serde_json::from_str(json).expect("should parse");
        assert_eq!(creds.access_token, "test-token");
        assert!(creds.token_type.is_none());
        assert!(creds.expires_at.is_none());
        assert!(creds.refresh_token.is_none());
        assert!(creds.scope.is_none());
    }

    #[test]
    fn credentials_deserialize_full() {
        let json = r#"{
            "access_token": "test-token",
            "token_type": "Bearer",
            "expires_at": 1764412800,
            "refresh_token": "refresh-xyz",
            "scope": "openid profile email"
        }"#;
        let creds: Credentials = serde_json::from_str(json).expect("should parse");
        assert_eq!(creds.access_token, "test-token");
        assert_eq!(creds.token_type.as_deref(), Some("Bearer"));
        assert_eq!(creds.expires_at, Some(1764412800));
        assert_eq!(creds.refresh_token.as_deref(), Some("refresh-xyz"));
        assert_eq!(creds.scope.as_deref(), Some("openid profile email"));
    }

    #[test]
    fn credentials_serialize_skips_none() {
        let creds = Credentials {
            access_token: "tok".to_string(),
            token_type: None,
            expires_at: None,
            refresh_token: None,
            scope: None,
        };
        let json = serde_json::to_string(&creds).expect("should serialize");
        // None field は skip されて access_token のみ
        assert_eq!(json, r#"{"access_token":"tok"}"#);
    }

    #[test]
    fn credentials_path_uses_env_override() {
        // SAFETY: test 並列下で他 test が同 env を触ると flake、 ただし default path
        // よりも明示性を優先。 後で remove して影響を限定。
        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", "/tmp/test-vp-creds.json");
        }
        let path = credentials_path().expect("path resolved");
        assert_eq!(path, PathBuf::from("/tmp/test-vp-creds.json"));
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
    }

    /// `VP_NEXUS_URL` の default と override を **1 test 内で順次** 検証。
    /// 別 test 関数に分けると Rust の parallel test runner で env race が起き、
    /// 一方の remove_var が他方の assertion 直前に実行されて flaky 化する
    /// (= dogfood 4 で観察した trap の再発見)。
    #[test]
    fn nexus_url_env_resolution() {
        unsafe {
            std::env::remove_var("VP_NEXUS_URL");
        }
        assert_eq!(nexus_url(), "http://127.0.0.1:9200");

        unsafe {
            std::env::set_var("VP_NEXUS_URL", "https://nexus.example.test");
        }
        assert_eq!(nexus_url(), "https://nexus.example.test");

        unsafe {
            std::env::remove_var("VP_NEXUS_URL");
        }
        assert_eq!(nexus_url(), "http://127.0.0.1:9200");
    }
}
