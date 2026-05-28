//! OAuth Authorization Code + PKCE flow の client side 実装 (= Phase A1c)。
//!
//! vp-nexus は OAuth client (= relying party) として振る舞い、 IdP (= Creo ID、
//! `https://id.creo-memories.in`) と協調して JWT を取得する。 vp-cli の
//! `vp login` から呼ばれる server-side endpoint を提供する。
//!
//! ## Flow
//!
//! ```text
//! 1. client → /v1/auth/login
//! 2. server: PKCE pair (verifier, challenge) 生成、 state (= CSRF token) 生成、
//!            state → verifier を store に保存、 authorize URL を構築
//! 3. server → 302 Redirect to IdP authorize endpoint
//! 4. user: IdP で認証、 IdP → /v1/auth/callback?code=...&state=... に redirect
//! 5. server: state を store から consume して verifier 取得、 IdP token endpoint
//!            に POST (code + verifier + client_id + redirect_uri)
//! 6. server → JSON {access_token, token_type, expires_in, ...} を client に返す
//! ```
//!
//! ## 設計判断
//!
//! - **state store は in-memory**: TTL 5 分で短命、 同時 active は数十個程度のため
//!   `Arc<Mutex<HashMap>>` で十分。 Redis 等 external store は overkill。
//! - **PKCE verifier 長さ 64 chars**: RFC 7636 推奨 43-128 の中庸。
//! - **state 長さ 32 chars**: CSRF token として十分なエントロピー (= 約 190 bit)。
//! - **dep 最小化**: `oauth2` crate (= 高機能 OAuth client) を入れず、 直接実装で
//!   control 明確 + slim。 PKCE 計算は `sha2` + `base64` の薄い組み合わせ。

use crate::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Json, Redirect};
use base64::Engine;
use rand::Rng;
use rand::distributions::Alphanumeric;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use url::form_urlencoded;

/// IdP の authorize endpoint default (= Creo ID)。
pub const DEFAULT_AUTHORIZE_ENDPOINT: &str = "https://id.creo-memories.in/authorize";

/// IdP の token endpoint default (= Creo ID)。
pub const DEFAULT_TOKEN_ENDPOINT: &str = "https://id.creo-memories.in/oauth/token";

/// scope default (= 最小 openid set)。
pub const DEFAULT_SCOPE: &str = "openid profile email";

/// state ↔ verifier の有効期間 (= 5 分)。 authorize → callback の往復はこれより
/// 短いはず、 expire したら無効として扱う (= CSRF + replay 防止)。
pub const STATE_TTL: Duration = Duration::from_secs(5 * 60);

/// PKCE verifier の長さ (= RFC 7636 で 43-128、 64 を採用)。
const VERIFIER_LEN: usize = 64;

/// CSRF state の長さ (= alphanumeric 32 chars、 約 190 bit エントロピー)。
const STATE_LEN: usize = 32;

/// OAuth client config — env var から起動時に load される。
///
/// すべて揃わない場合は [`OAuthConfig::from_env`] が `None` を返し、 OAuth 機能は
/// disable される (= `/v1/auth/login` 等が 503 を返す)。
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: String,
    pub authorize_endpoint: String,
    pub token_endpoint: String,
}

impl OAuthConfig {
    /// env から load。 必須は `NEXUS_OAUTH_CLIENT_ID` + `NEXUS_OAUTH_REDIRECT_URI`、
    /// 他は default fallback。
    pub fn from_env() -> Option<Self> {
        let client_id = std::env::var("NEXUS_OAUTH_CLIENT_ID").ok()?;
        let redirect_uri = std::env::var("NEXUS_OAUTH_REDIRECT_URI").ok()?;
        let scope =
            std::env::var("NEXUS_OAUTH_SCOPE").unwrap_or_else(|_| DEFAULT_SCOPE.to_string());
        let authorize_endpoint = std::env::var("NEXUS_OAUTH_AUTHORIZE_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_AUTHORIZE_ENDPOINT.to_string());
        let token_endpoint = std::env::var("NEXUS_OAUTH_TOKEN_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_TOKEN_ENDPOINT.to_string());
        Some(Self {
            client_id,
            redirect_uri,
            scope,
            authorize_endpoint,
            token_endpoint,
        })
    }
}

/// state ↔ verifier の in-memory mapping。 thread-safe (= `Mutex` for interior mutability)。
#[derive(Debug, Default)]
pub struct OAuthStateStore {
    inner: Mutex<HashMap<String, OAuthFlowState>>,
}

/// 1 flow 分の state — verifier と TTL 検査用の created_at。
#[derive(Debug, Clone)]
pub struct OAuthFlowState {
    pub verifier: String,
    pub created_at: SystemTime,
}

impl OAuthStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// state → verifier の mapping を追加。 既存 state を上書き (= 通常 collision なし)。
    pub fn insert(&self, state: String, verifier: String) {
        let mut map = self.inner.lock().unwrap();
        map.insert(
            state,
            OAuthFlowState {
                verifier,
                created_at: SystemTime::now(),
            },
        );
    }

    /// state を消費して verifier を取り出す。 entry が無い / TTL 超過なら `None`。
    /// 副作用として entry を削除 (= replay 防止)。
    pub fn consume(&self, state: &str) -> Option<String> {
        let mut map = self.inner.lock().unwrap();
        let entry = map.remove(state)?;
        let elapsed = entry.created_at.elapsed().ok()?;
        if elapsed > STATE_TTL {
            return None;
        }
        Some(entry.verifier)
    }

    /// TTL 超過 entry を一掃する (= memory leak 防止、 idle GC で呼ぶ想定)。
    pub fn purge_expired(&self) {
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, v| {
            v.created_at
                .elapsed()
                .map(|d| d <= STATE_TTL)
                .unwrap_or(false)
        });
    }

    /// 現在保持中の entry 数 (= 主に test / debug 用)。
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// PKCE pair 生成 — `(verifier, challenge)` を返す。
///
/// verifier: unreserved char set (= `A-Za-z0-9-._~`) から `VERIFIER_LEN` chars。
/// challenge: `base64url-no-pad(SHA-256(verifier))` (= RFC 7636 §4.2 の S256 method)。
pub fn pkce_pair() -> (String, String) {
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

/// CSRF state 生成 — alphanumeric `STATE_LEN` chars (= 約 190 bit エントロピー)。
pub fn random_state() -> String {
    OsRng
        .sample_iter(&Alphanumeric)
        .take(STATE_LEN)
        .map(char::from)
        .collect()
}

/// `/v1/auth/callback` の query string (= IdP からの redirect で付与される)。
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
}

/// IdP の token endpoint レスポンス (= RFC 6749 §5.1)。
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
}

/// `/v1/auth/login` handler — IdP の authorize endpoint に redirect する。
///
/// 1. PKCE pair (verifier, challenge) 生成
/// 2. CSRF state 生成
/// 3. state → verifier を store に insert
/// 4. authorize URL を構築 (= response_type=code、 PKCE S256)
/// 5. 302 Redirect
///
/// OAuth config が未設定なら 503 Service Unavailable。
pub async fn login(State(app): State<AppState>) -> Result<Redirect, StatusCode> {
    let config = app
        .oauth_config
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let (verifier, challenge) = pkce_pair();
    let state = random_state();

    app.oauth_state_store.insert(state.clone(), verifier);

    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .finish();

    let url = format!("{}?{}", config.authorize_endpoint, query);
    Ok(Redirect::to(&url))
}

/// `/v1/auth/callback` handler — IdP からの redirect を受けて token exchange を行う。
///
/// 1. state を store から consume (= verifier 取得、 entry 削除)
/// 2. consume 失敗 (= unknown state / expired) なら 400 Bad Request
/// 3. token endpoint に POST (= grant_type=authorization_code + code + verifier)
/// 4. JWT を JSON で client に返す
///
/// OAuth config が未設定なら 503、 IdP との通信失敗は 502 Bad Gateway。
pub async fn callback(
    State(app): State<AppState>,
    Query(q): Query<CallbackQuery>,
) -> Result<Json<TokenResponse>, StatusCode> {
    let config = app
        .oauth_config
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let verifier = app
        .oauth_state_store
        .consume(&q.state)
        .ok_or(StatusCode::BAD_REQUEST)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // workspace の reqwest が default-features = false なので RequestBuilder::form()
    // が compile されない。 URL-encoded body を自前で構築して送る (= 同じ wire 形式)。
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", &config.client_id)
        .append_pair("code", &q.code)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("code_verifier", &verifier)
        .finish();

    let resp = client
        .post(&config.token_endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !resp.status().is_success() {
        return Err(StatusCode::BAD_GATEWAY);
    }

    let token: TokenResponse = resp.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok(Json(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pair_produces_s256_challenge() {
        let (verifier, challenge) = pkce_pair();
        assert_eq!(verifier.len(), VERIFIER_LEN);
        assert!(verifier.len() >= 43 && verifier.len() <= 128);

        // verifier は unreserved char set のみで構成されているべき
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
        );

        // challenge == base64url(SHA-256(verifier))
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected);

        // base64url no-pad なので `=` が含まれない
        assert!(!challenge.contains('='));
    }

    #[test]
    fn pkce_pair_produces_unique_pairs() {
        // 連続呼び出しで verifier が必ず異なる (= OsRng エントロピー検証)
        let (v1, _) = pkce_pair();
        let (v2, _) = pkce_pair();
        assert_ne!(v1, v2);
    }

    #[test]
    fn random_state_produces_alphanumeric_string() {
        let state = random_state();
        assert_eq!(state.len(), STATE_LEN);
        assert!(state.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn random_state_produces_unique_values() {
        let s1 = random_state();
        let s2 = random_state();
        assert_ne!(s1, s2);
    }

    #[test]
    fn state_store_insert_then_consume_returns_verifier() {
        let store = OAuthStateStore::new();
        store.insert("state1".to_string(), "verifier1".to_string());
        assert_eq!(store.len(), 1);

        let verifier = store.consume("state1");
        assert_eq!(verifier, Some("verifier1".to_string()));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn state_store_consume_twice_returns_none_second_time() {
        let store = OAuthStateStore::new();
        store.insert("state1".to_string(), "verifier1".to_string());
        let _ = store.consume("state1");
        let second = store.consume("state1");
        assert_eq!(second, None);
    }

    #[test]
    fn state_store_consume_unknown_state_returns_none() {
        let store = OAuthStateStore::new();
        assert_eq!(store.consume("never-inserted"), None);
    }

    #[test]
    fn state_store_consume_expired_entry_returns_none() {
        let store = OAuthStateStore::new();

        // TTL を超過した entry を直接 inject (= test 用、 内部 field に同 module から access)
        {
            let mut map = store.inner.lock().unwrap();
            map.insert(
                "stale".to_string(),
                OAuthFlowState {
                    verifier: "stale-verifier".to_string(),
                    created_at: SystemTime::now() - STATE_TTL - Duration::from_secs(60),
                },
            );
        }

        let result = store.consume("stale");
        assert_eq!(result, None);
    }

    #[test]
    fn state_store_purge_expired_removes_old_entries() {
        let store = OAuthStateStore::new();
        store.insert("fresh".to_string(), "fresh-verifier".to_string());

        {
            let mut map = store.inner.lock().unwrap();
            map.insert(
                "old".to_string(),
                OAuthFlowState {
                    verifier: "old-verifier".to_string(),
                    created_at: SystemTime::now() - STATE_TTL - Duration::from_secs(60),
                },
            );
        }

        assert_eq!(store.len(), 2);
        store.purge_expired();
        assert_eq!(store.len(), 1);
        assert_eq!(store.consume("fresh"), Some("fresh-verifier".to_string()));
    }

    #[test]
    fn oauth_config_from_env_returns_none_when_required_missing() {
        // env 干渉を避けるため必須 var を unset、 結果 None を expect
        // 注: test 並列で他 test が env を set している場合 false positive 可能性、
        // ここでは SAFETY 重視で remove → set 順
        // SAFETY: std::env::remove_var は std 2024 edition で unsafe 扱い
        unsafe {
            std::env::remove_var("NEXUS_OAUTH_CLIENT_ID");
            std::env::remove_var("NEXUS_OAUTH_REDIRECT_URI");
        }
        assert!(OAuthConfig::from_env().is_none());
    }
}
