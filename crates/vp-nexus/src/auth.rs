//! Creo ID 認証 — JWT verify middleware (= Phase A1a)。
//!
//! ## 設計
//!
//! - Creo ID (= `https://id.creo-memories.in/`) を OIDC issuer として使用
//! - Authorization header に `Bearer <JWT>` を期待
//! - JWKS を起動時 lazy fetch、 in-memory cache (= refresh / rotation は将来 task)
//! - axum 0.8 の Extractor pattern: handler signature に `claims: Claims` を書くと
//!   middleware が走る (= `.route_layer(...)` より型安全)
//!
//! ## canonical 設定値 (= 2026-05-28 Phase A spec で確定)
//!
//! - `iss`: `https://id.creo-memories.in/`
//! - `aud`: `https://api.vantage-point.app`
//! - signing alg: RS256
//! - JWKS URL: `https://id.creo-memories.in/.well-known/jwks.json`
//!
//! ## 適用 scope
//!
//! - 本 A1a では JWT verify middleware の単体実装のみ
//! - `/v1/auth/me` endpoint 統合は A1b
//! - OAuth `/login` + `/callback` flow は A1c (= Auth0 console 登録必要)

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use std::sync::OnceLock;
use tokio::sync::RwLock;

/// canonical issuer (= Creo ID OIDC issuer URL)
pub const ISSUER: &str = "https://id.creo-memories.in/";

/// canonical audience (= VP cloud API identifier、 2026-05-28 Q1 確定値)
pub const AUDIENCE: &str = "https://api.vantage-point.app";

/// JWKS URL (= 起動時 1 回 fetch、 in-memory cache)
pub const JWKS_URL: &str = "https://id.creo-memories.in/.well-known/jwks.json";

/// JWT Claims (= Creo ID から発行される token の payload)
#[derive(Debug, Deserialize, Clone)]
pub struct Claims {
    /// user ID (= OIDC `sub` claim、 record の owner field 等に使う)
    pub sub: String,
    /// issuer URL (= [`ISSUER`] に一致するはず、 [`Validation`] で check 済)
    pub iss: String,
    /// audience (= string or array、 OIDC 仕様)
    pub aud: AudienceClaim,
    /// expiry (= Unix seconds)
    pub exp: u64,
    /// issued at (= Unix seconds)
    pub iat: u64,
    /// scope (= `"vp:read vp:write"` 等 space-separated)
    #[serde(default)]
    pub scope: Option<String>,
}

/// `aud` claim — string か array (= OIDC 仕様で両方許容)
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum AudienceClaim {
    Single(String),
    Multiple(Vec<String>),
}

impl AudienceClaim {
    /// 指定 audience が含まれているか
    pub fn contains(&self, aud: &str) -> bool {
        match self {
            Self::Single(s) => s == aud,
            Self::Multiple(v) => v.iter().any(|s| s == aud),
        }
    }
}

/// JWKS cache — 起動時 1 回 fetch、 in-memory keep。
///
/// refresh / rotation 対応は将来 task (= Phase A の later subtask)。
static JWKS_CACHE: OnceLock<RwLock<Option<JwkSet>>> = OnceLock::new();

fn cache() -> &'static RwLock<Option<JwkSet>> {
    JWKS_CACHE.get_or_init(|| RwLock::new(None))
}

/// JWKS を canonical URL から fetch して cache に保存する。
/// production では起動時 1 回呼ぶ想定 (= `main.rs` から)。
pub async fn refresh_jwks() -> Result<(), reqwest::Error> {
    let jwks: JwkSet = reqwest::get(JWKS_URL).await?.json().await?;
    *cache().write().await = Some(jwks);
    Ok(())
}

/// test 用 — JWKS cache に直接 JwkSet を注入する。
#[cfg(test)]
pub async fn install_test_jwks(jwks: JwkSet) {
    *cache().write().await = Some(jwks);
}

/// JWT verify エラー種別 (= 全て 401 にマップ、 OIDC canonical rule)
#[derive(Debug, Clone, Copy)]
pub enum AuthError {
    /// Authorization header 不在 / 不正形式 (= `Bearer ` prefix なし等)
    MissingHeader,
    /// token 自体の verify 失敗 (= 改竄 / 期限切れ / iss/aud 不一致 / kid 未知 / ...)
    InvalidToken,
    /// JWKS が cache に未 load (= [`refresh_jwks`] 未実行)
    JwksNotLoaded,
}

impl AuthError {
    fn status(self) -> StatusCode {
        StatusCode::UNAUTHORIZED
    }

    fn message(self) -> &'static str {
        match self {
            Self::MissingHeader => "missing Authorization header",
            Self::InvalidToken => "invalid token",
            Self::JwksNotLoaded => "JWKS not loaded",
        }
    }
}

/// token (= Bearer 部分の string) を verify して Claims を返す。
async fn verify_token(token: &str) -> Result<Claims, AuthError> {
    let header = decode_header(token).map_err(|_| AuthError::InvalidToken)?;
    let kid = header.kid.ok_or(AuthError::InvalidToken)?;

    let jwks_guard = cache().read().await;
    let jwks = jwks_guard.as_ref().ok_or(AuthError::JwksNotLoaded)?;
    let jwk = jwks.find(&kid).ok_or(AuthError::InvalidToken)?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| AuthError::InvalidToken)?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[AUDIENCE]);

    let data = decode::<Claims>(token, &key, &validation).map_err(|_| AuthError::InvalidToken)?;
    Ok(data.claims)
}

/// axum Extractor 実装 — handler signature に `claims: Claims` を書くと
/// middleware が自動発火する。 失敗時は 401 Response を返す。
///
/// # Example
///
/// ```ignore
/// async fn me(claims: Claims) -> Json<UserInfo> {
///     Json(UserInfo { sub: claims.sub })
/// }
/// ```
impl<S: Send + Sync> FromRequestParts<S> for Claims {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let auth = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or((
                AuthError::MissingHeader.status(),
                AuthError::MissingHeader.message(),
            ))?;

        verify_token(auth)
            .await
            .map_err(|e| (e.status(), e.message()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rsa::{
        RsaPrivateKey, RsaPublicKey,
        pkcs8::{EncodePrivateKey, LineEnding},
        traits::PublicKeyParts,
    };
    use serde_json::json;
    use std::sync::OnceLock;

    /// test 用 RSA keypair + JWKS form。 RSA-2048 生成は重いので 1 回だけ生成して全 test で共有。
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

            let kid = "test-kid".to_string();
            // RSA modulus / exponent を base64url で encode (= JWK 仕様)
            let n =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.n().to_bytes_be());
            let e =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.e().to_bytes_be());

            // JWKS JSON を組み立てて parse (= 型 detail を knowsudo に書かず serde に任せる)
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

    fn sign_token(claims: serde_json::Value, kid: Option<&str>) -> String {
        let kp = keypair();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(String::from);
        let key = EncodingKey::from_rsa_pem(kp.private_pem.as_bytes()).expect("encoding key");
        encode(&header, &claims, &key).expect("token encode")
    }

    async fn install_test_keypair() {
        install_test_jwks(keypair().jwks.clone()).await;
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[tokio::test]
    async fn valid_token_verifies() {
        install_test_keypair().await;
        let now = now_secs();
        let claims = json!({
            "sub": "user_abc",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now + 3600,
            "iat": now,
            "scope": "vp:read"
        });
        let token = sign_token(claims, Some(&keypair().kid));

        let result = verify_token(&token)
            .await
            .expect("valid token should verify");

        assert_eq!(result.sub, "user_abc");
        assert!(result.aud.contains(AUDIENCE));
        assert_eq!(result.scope.as_deref(), Some("vp:read"));
    }

    #[tokio::test]
    async fn expired_token_rejected() {
        install_test_keypair().await;
        let now = now_secs();
        let claims = json!({
            "sub": "user_abc",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now - 100,
            "iat": now - 200,
        });
        let token = sign_token(claims, Some(&keypair().kid));

        let err = verify_token(&token).await.expect_err("expired should fail");
        assert!(matches!(err, AuthError::InvalidToken));
    }

    #[tokio::test]
    async fn wrong_issuer_rejected() {
        install_test_keypair().await;
        let now = now_secs();
        let claims = json!({
            "sub": "user_abc",
            "iss": "https://attacker.example.com/",
            "aud": AUDIENCE,
            "exp": now + 3600,
            "iat": now,
        });
        let token = sign_token(claims, Some(&keypair().kid));

        let err = verify_token(&token)
            .await
            .expect_err("wrong issuer should fail");
        assert!(matches!(err, AuthError::InvalidToken));
    }

    #[tokio::test]
    async fn wrong_audience_rejected() {
        install_test_keypair().await;
        let now = now_secs();
        let claims = json!({
            "sub": "user_abc",
            "iss": ISSUER,
            "aud": "https://different.example.com/",
            "exp": now + 3600,
            "iat": now,
        });
        let token = sign_token(claims, Some(&keypair().kid));

        let err = verify_token(&token)
            .await
            .expect_err("wrong audience should fail");
        assert!(matches!(err, AuthError::InvalidToken));
    }

    #[tokio::test]
    async fn missing_kid_rejected() {
        install_test_keypair().await;
        let now = now_secs();
        let claims = json!({
            "sub": "user_abc",
            "iss": ISSUER,
            "aud": AUDIENCE,
            "exp": now + 3600,
            "iat": now,
        });
        let token = sign_token(claims, None);

        let err = verify_token(&token)
            .await
            .expect_err("missing kid should fail");
        assert!(matches!(err, AuthError::InvalidToken));
    }
}
