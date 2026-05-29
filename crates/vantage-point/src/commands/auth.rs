//! `vp auth` subcommand — Creo ID 認証 (= Phase A2、 vp-cli client integration)
//!
//! ## A2a (= dogfood 9): `vp auth me` skeleton
//!
//! 既存 credential を読んで nexus `/v1/auth/me` に Bearer 付き talk する path。
//!
//! ## A2b (= dogfood 10): `vp auth login` (= loopback OAuth Native App + PKCE) + refresh
//!
//! - RFC 8252 (= OAuth 2.0 for Native Apps) 準拠の loopback IP pattern
//! - vp-cli が `127.0.0.1:0` で一時 TCP listener を立て、 取得した port を redirect_uri に embed
//! - PKCE S256 (= RFC 7636) で code interception 防止
//! - 32-char CSRF state で session fixation 防止
//! - default browser を `webbrowser` crate で spawn、 authorize URL を開く
//! - IdP redirect を loopback listener で 1 connection 受信、 GET line 自前 parse
//! - token endpoint に form-urlencoded POST、 access/refresh token を保存
//! - `vp auth me` は 401 検出時に refresh_token あれば auto-refresh、 失敗で re-login message

use anyhow::{Context, Result};
use base64::Engine;
use clap::Subcommand;
use rand::Rng;
use rand::distributions::Alphanumeric;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::form_urlencoded;

/// IdP authorize endpoint の default (= Creo ID)。
pub const DEFAULT_AUTHORIZE_ENDPOINT: &str = "https://id.creo-memories.in/authorize";

/// IdP token endpoint の default。
pub const DEFAULT_TOKEN_ENDPOINT: &str = "https://id.creo-memories.in/oauth/token";

/// scope の default (= 最小 openid set)。
pub const DEFAULT_SCOPE: &str = "openid profile email";

/// PKCE verifier の長さ (= RFC 7636 で 43-128、 64 を採用)。
const VERIFIER_LEN: usize = 64;

/// CSRF state の長さ (= alphanumeric 32 chars、 約 190 bit エントロピー)。
const STATE_LEN: usize = 32;

/// callback 待ち全体の timeout (= 5 分、 user 操作完了猶予)。
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// `vp auth` subcommand 一覧。 A2c/d で AuthCommands を更に拡張予定 (= Logout 追加)。
#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    /// 現在 login しているユーザー情報を表示 (= nexus /v1/auth/me を叩く)
    Me,

    /// Creo ID で login (= loopback OAuth Native App + PKCE flow)
    Login {
        /// browser を spawn せず authorize URL を print のみ (= headless / debug 用)
        #[arg(long)]
        no_browser: bool,
    },
}

/// `~/.vp/credentials.json` で保存される credentials の serde shape。
///
/// access_token 以外は optional (= 最小 token のみでも parse OK)。
/// A2b で OAuth token response 全体を保存する (= refresh_token / expires_at 等)。
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

/// IdP / OIDC client config — env から起動時に load。
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub client_id: String,
    pub authorize_endpoint: String,
    pub token_endpoint: String,
    pub scope: String,
}

impl OidcConfig {
    /// env から load。 必須は `VP_OIDC_CLIENT_ID`、 他は default fallback。
    pub fn from_env() -> Result<Self> {
        let client_id = std::env::var("VP_OIDC_CLIENT_ID")
            .context("VP_OIDC_CLIENT_ID not set — run `vp auth login` requires a client_id (= Auth0 console で発行)")?;
        let authorize_endpoint = std::env::var("VP_OIDC_AUTHORIZE_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_AUTHORIZE_ENDPOINT.to_string());
        let token_endpoint = std::env::var("VP_OIDC_TOKEN_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_TOKEN_ENDPOINT.to_string());
        let scope = std::env::var("VP_OIDC_SCOPE").unwrap_or_else(|_| DEFAULT_SCOPE.to_string());
        Ok(Self {
            client_id,
            authorize_endpoint,
            token_endpoint,
            scope,
        })
    }
}

/// IdP の token endpoint response (= RFC 6749 §5.1)。
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

impl TokenResponse {
    /// `expires_in` を「now + expires_in」 の絶対 unix epoch に変換、 Credentials に詰める。
    fn into_credentials(self) -> Credentials {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let expires_at = self.expires_in.map(|e| now + e);
        Credentials {
            access_token: self.access_token,
            token_type: self.token_type,
            expires_at,
            refresh_token: self.refresh_token,
            scope: self.scope,
        }
    }
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

/// credentials を file に保存。 unix では mode 0600 で書く (= owner read/write only)。
fn save_credentials(creds: &Credentials) -> Result<()> {
    let path = credentials_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to mkdir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(creds).context("failed to serialize credentials")?;
    std::fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)
            .with_context(|| format!("failed to chmod 600 {}", path.display()))?;
    }
    Ok(())
}

/// nexus base URL — env `VP_NEXUS_URL` > default `http://127.0.0.1:9200`。
fn nexus_url() -> String {
    std::env::var("VP_NEXUS_URL").unwrap_or_else(|_| "http://127.0.0.1:9200".to_string())
}

/// PKCE pair 生成 — `(verifier, challenge)`。 RFC 7636 S256 method。
fn pkce_pair() -> (String, String) {
    let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = OsRng;
    let verifier: String = (0..VERIFIER_LEN)
        .map(|_| {
            let idx = rng.gen_range(0..chars.len());
            chars[idx] as char
        })
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

/// CSRF state 生成 — alphanumeric `STATE_LEN` chars。
fn random_state() -> String {
    OsRng
        .sample_iter(&Alphanumeric)
        .take(STATE_LEN)
        .map(char::from)
        .collect()
}

/// callback で受信した `?code=...&state=...` query を表す。
#[derive(Debug)]
struct CallbackResult {
    code: String,
}

/// loopback listener で 1 connection を受信し、 GET line を parse して code/state を抽出。
/// state が expected と一致しなければ error、 一致すれば browser に "Logged in" HTML を返して
/// code のみ caller に返す。
async fn wait_for_callback(listener: &TcpListener, expected_state: &str) -> Result<CallbackResult> {
    let (mut socket, _) = tokio::time::timeout(CALLBACK_TIMEOUT, listener.accept())
        .await
        .context("timed out waiting for OAuth callback (5 min)")?
        .context("failed to accept OAuth callback")?;

    let mut buf = vec![0u8; 8192];
    let n = socket
        .read(&mut buf)
        .await
        .context("failed to read OAuth callback request")?;
    let req =
        std::str::from_utf8(&buf[..n]).context("OAuth callback request is not valid UTF-8")?;

    let first_line = req
        .lines()
        .next()
        .context("OAuth callback request is empty")?;
    let (code, state) = parse_callback_request_line(first_line)?;

    if state != expected_state {
        anyhow::bail!("OAuth state mismatch (= CSRF check failed)");
    }

    // browser に friendly response を返してから socket close
    let body = "<!DOCTYPE html><html><body style=\"font-family:sans-serif;text-align:center;padding:3em;\"><h1>✓ Logged in</h1><p>Close this tab and return to your terminal.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;

    Ok(CallbackResult { code })
}

/// `GET /callback?code=...&state=... HTTP/1.1` の 1 行から (code, state) を抽出。
/// query で `code` / `state` 何方が不在なら error。
fn parse_callback_request_line(line: &str) -> Result<(String, String)> {
    let mut parts = line.split_whitespace();
    let _method = parts.next().context("no method in request line")?;
    let path = parts.next().context("no path in request line")?;
    let query = path
        .split_once('?')
        .map(|(_, q)| q)
        .context("no query string in callback path")?;

    let mut code = None;
    let mut state = None;
    for (k, v) in form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            _ => {}
        }
    }
    let code = code.context("OAuth callback missing `code` param")?;
    let state = state.context("OAuth callback missing `state` param")?;
    Ok((code, state))
}

/// token endpoint に authorization_code grant POST、 TokenResponse を取得。
async fn exchange_token(
    config: &OidcConfig,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", &config.client_id)
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_verifier", verifier)
        .finish();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;
    let resp = client
        .post(&config.token_endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .with_context(|| format!("failed to call {}", config.token_endpoint))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("token endpoint returned {status}: {body}");
    }
    resp.json::<TokenResponse>()
        .await
        .context("failed to parse token endpoint JSON response")
}

/// refresh_token grant で新 token 取得 (= access expired 時に呼ぶ)。
async fn refresh_tokens(config: &OidcConfig, refresh_token: &str) -> Result<TokenResponse> {
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", &config.client_id)
        .append_pair("refresh_token", refresh_token)
        .finish();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;
    let resp = client
        .post(&config.token_endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .with_context(|| format!("failed to call {}", config.token_endpoint))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("refresh endpoint returned {status}: {body}");
    }
    resp.json::<TokenResponse>()
        .await
        .context("failed to parse refresh endpoint JSON response")
}

/// `vp auth <subcommand>` のエントリ — main.rs から呼ばれる dispatch。
pub async fn execute(cmd: AuthCommands) -> Result<()> {
    match cmd {
        AuthCommands::Me => me().await,
        AuthCommands::Login { no_browser } => login(no_browser).await,
    }
}

/// `vp auth login` — loopback OAuth Native App + PKCE flow を実行、 token を保存。
async fn login(no_browser: bool) -> Result<()> {
    let config = OidcConfig::from_env()?;
    let (verifier, challenge) = pkce_pair();
    let state = random_state();

    // 127.0.0.1:0 で OS 任せの port を取得、 redirect_uri に embed
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind loopback listener")?;
    let port = listener
        .local_addr()
        .context("failed to read listener addr")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    // authorize URL 構築
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .finish();
    let authorize_url = format!("{}?{}", config.authorize_endpoint, query);

    if no_browser {
        println!("Open this URL in your browser:");
        println!("{authorize_url}");
        println!();
        println!("Waiting for callback on {redirect_uri} ...");
    } else {
        println!("Opening browser for Creo ID login ...");
        println!("(if browser does not open, copy this URL manually:)");
        println!("{authorize_url}");
        println!();
        // browser launch、 fail しても続行 (= URL は print 済、 manual で開ける)
        if let Err(e) = webbrowser::open(&authorize_url) {
            eprintln!("warning: failed to launch browser: {e}");
            eprintln!("         open the URL above manually.");
        }
        println!("Waiting for callback on {redirect_uri} ...");
    }

    let callback = wait_for_callback(&listener, &state).await?;
    let tokens = exchange_token(&config, &callback.code, &verifier, &redirect_uri).await?;
    let creds = tokens.into_credentials();
    save_credentials(&creds)?;

    let path = credentials_path()?;
    println!("✓ Logged in. Credentials saved to {}", path.display());
    Ok(())
}

/// access_token を Bearer で nexus `/v1/auth/me` に投げる内部 helper。
/// 401 は `Ok(None)`、 成功は `Ok(Some(body))`、 他失敗は `Err`。
async fn try_me(access_token: &str) -> Result<Option<serde_json::Value>> {
    let url = format!("{}/v1/auth/me", nexus_url());
    let resp = reqwest::Client::new()
        .get(&url)
        .header("authorization", format!("Bearer {access_token}"))
        .send()
        .await
        .with_context(|| format!("failed to call {url}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("nexus returned {status}: {body}");
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse /v1/auth/me JSON response")?;
    Ok(Some(body))
}

fn print_me(body: &serde_json::Value) {
    println!("sub: {}", body["sub"].as_str().unwrap_or("<unknown>"));
    if let Some(scope) = body["scope"].as_str() {
        println!("scope: {scope}");
    }
}

/// `vp auth me` — refresh-aware。 401 検出 → refresh 試行 → 失敗で re-login message。
async fn me() -> Result<()> {
    let creds = match read_credentials()? {
        Some(c) => c,
        None => {
            eprintln!("error: not logged in (= ~/.vp/credentials.json なし)");
            eprintln!("       run `vp auth login` first");
            std::process::exit(1);
        }
    };

    // 1 回目 try
    if let Some(body) = try_me(&creds.access_token).await? {
        print_me(&body);
        return Ok(());
    }

    // 401: refresh_token あれば auto-refresh 試行
    if let Some(refresh) = &creds.refresh_token {
        // OidcConfig は refresh のために必要 (= unset なら fallthrough)
        if let Ok(config) = OidcConfig::from_env() {
            match refresh_tokens(&config, refresh).await {
                Ok(new_tokens) => {
                    let new_creds = new_tokens.into_credentials();
                    save_credentials(&new_creds)?;
                    if let Some(body) = try_me(&new_creds.access_token).await? {
                        print_me(&body);
                        return Ok(());
                    }
                    // refresh で取った token もダメ (= rare、 IdP 側の問題)
                }
                Err(e) => {
                    eprintln!("warning: refresh failed: {e}");
                }
            }
        }
    }

    eprintln!("error: token invalid or expired");
    eprintln!("       run `vp auth login` again");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    // === A2a tests (= dogfood 9 で導入、 不変) ===

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
        assert_eq!(json, r#"{"access_token":"tok"}"#);
    }

    #[test]
    fn credentials_path_uses_env_override() {
        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", "/tmp/test-vp-creds.json");
        }
        let path = credentials_path().expect("path resolved");
        assert_eq!(path, PathBuf::from("/tmp/test-vp-creds.json"));
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
    }

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

    // === A2b tests (= dogfood 10 で追加) ===

    #[test]
    fn pkce_pair_produces_s256_challenge() {
        let (verifier, challenge) = pkce_pair();
        assert_eq!(verifier.len(), VERIFIER_LEN);
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
        );
        // challenge == base64url-no-pad(SHA-256(verifier))
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected);
        assert!(!challenge.contains('='));
    }

    #[test]
    fn pkce_pair_produces_unique_verifiers() {
        let (v1, _) = pkce_pair();
        let (v2, _) = pkce_pair();
        assert_ne!(v1, v2);
    }

    #[test]
    fn random_state_alphanumeric_length() {
        let s = random_state();
        assert_eq!(s.len(), STATE_LEN);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
        // 連続呼びで unique (= OsRng)
        let s2 = random_state();
        assert_ne!(s, s2);
    }

    /// OidcConfig::from_env の missing / loaded 両 case を **1 test 関数内で順次** 検証。
    /// 別 test 関数に分けると Rust parallel runner で env race → flake。
    /// dogfood 4 retro N4 / dogfood 9 retro 再強調 path の **3 回目** 適用 (= 強い signal)。
    #[test]
    fn oidc_config_from_env_resolution() {
        // Phase 1: VP_OIDC_CLIENT_ID 未設定 → error
        unsafe {
            std::env::remove_var("VP_OIDC_CLIENT_ID");
        }
        assert!(OidcConfig::from_env().is_err());

        // Phase 2: client_id のみ set、 他は default
        unsafe {
            std::env::remove_var("VP_OIDC_AUTHORIZE_ENDPOINT");
            std::env::remove_var("VP_OIDC_TOKEN_ENDPOINT");
            std::env::remove_var("VP_OIDC_SCOPE");
            std::env::set_var("VP_OIDC_CLIENT_ID", "test-cid");
        }
        let config = OidcConfig::from_env().expect("should load");
        assert_eq!(config.client_id, "test-cid");
        assert_eq!(config.authorize_endpoint, DEFAULT_AUTHORIZE_ENDPOINT);
        assert_eq!(config.token_endpoint, DEFAULT_TOKEN_ENDPOINT);
        assert_eq!(config.scope, DEFAULT_SCOPE);

        // cleanup
        unsafe {
            std::env::remove_var("VP_OIDC_CLIENT_ID");
        }
    }

    #[test]
    fn parse_callback_request_line_extracts_code_and_state() {
        let line = "GET /callback?code=abc123&state=xyz789 HTTP/1.1";
        let (code, state) = parse_callback_request_line(line).expect("should parse");
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz789");
    }

    #[test]
    fn parse_callback_request_line_url_decodes_values() {
        let line = "GET /callback?code=ab%20c&state=xyz HTTP/1.1";
        let (code, state) = parse_callback_request_line(line).expect("should parse");
        assert_eq!(code, "ab c");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn parse_callback_request_line_errors_on_missing_query() {
        let line = "GET /callback HTTP/1.1";
        assert!(parse_callback_request_line(line).is_err());
    }

    #[test]
    fn parse_callback_request_line_errors_on_missing_code() {
        let line = "GET /callback?state=xyz HTTP/1.1";
        assert!(parse_callback_request_line(line).is_err());
    }

    #[test]
    fn parse_callback_request_line_errors_on_missing_state() {
        let line = "GET /callback?code=abc HTTP/1.1";
        assert!(parse_callback_request_line(line).is_err());
    }

    #[test]
    fn token_response_into_credentials_computes_expires_at() {
        let resp = TokenResponse {
            access_token: "at".to_string(),
            token_type: Some("Bearer".to_string()),
            expires_in: Some(3600),
            refresh_token: Some("rt".to_string()),
            scope: Some("openid".to_string()),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let creds = resp.into_credentials();
        let expires = creds.expires_at.expect("expires_at should be set");
        // expires は概ね now + 3600 (= test 実行時間幅 ±2 秒)
        assert!(expires >= now + 3598 && expires <= now + 3602);
        assert_eq!(creds.access_token, "at");
        assert_eq!(creds.refresh_token.as_deref(), Some("rt"));
        assert_eq!(creds.scope.as_deref(), Some("openid"));
    }
}
