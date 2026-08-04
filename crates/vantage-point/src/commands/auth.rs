//! `vp auth` subcommand — Creo ID 認証 (= Phase A2、 vp-cli client integration)
//!
//! ## A2a (= dogfood 9): `vp auth me` skeleton
//!
//! 既存 credential を読んで nexus `/v1/auth/me` に Bearer 付き talk する path。
//!
//! ## A2b (= dogfood 10): `vp auth login` (= loopback OAuth Native App + PKCE) + refresh
//!
//! - RFC 8252 (= OAuth 2.0 for Native Apps) 準拠の loopback IP pattern
//! - vp-cli が `127.0.0.1:32800`（固定、 env `VP_OIDC_CALLBACK_PORT` で上書き可）に TCP listener を
//!   立て、 その port を redirect_uri に embed。 Auth0 は loopback でも port 完全一致を要求する
//!   （2026-07-04 実測）ため RFC 8252 の random-port は使えない — Auth0 app の Allowed Callback
//!   URLs `http://127.0.0.1:32800/callback` と対で固定する
//! - PKCE S256 (= RFC 7636) で code interception 防止
//! - 32-char CSRF state で session fixation 防止
//! - default browser を `webbrowser` crate で spawn、 authorize URL を開く
//! - IdP redirect を loopback listener で 1 connection 受信、 GET line 自前 parse
//! - token endpoint に form-urlencoded POST、 access/refresh token を保存
//! - `vp auth me` は 401 検出時に refresh_token あれば auto-refresh、 失敗で re-login message

use anyhow::{Context, Result};
use base64::Engine;
use clap::Subcommand;
use rand::RngExt;
use rand::distr::Alphanumeric;
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

/// scope の default (= 最小 openid set + refresh_token 取得用 offline_access)。
///
/// `offline_access` は refresh_token を発行させるための scope（OIDC）。ただし実際に
/// refresh_token が返るのは **token を出す Auth0 API（audience）が `allow_offline_access=true`**
/// の時のみ（AND 条件）。scope だけ要求して API が非対応なら refresh_token は出ない（無害）。
/// hub audience（Chronista Hub API）側の設定は hub と調整中（wire thread 019f2a67）。
pub const DEFAULT_SCOPE: &str = "openid profile email offline_access";

/// OAuth client_id の default (= Auth0「Vantage Point CLI」、Native app)。
///
/// Native app の client_id は public 識別子（秘密ではない、RFC 8252 §8.5）なので焼き込んで
/// よい。これにより一般ユーザーは env 設定なしの `vp auth login` だけでログインできる。
/// 別 tenant / 別 app で試す場合は env `VP_OIDC_CLIENT_ID` で上書き。
pub const DEFAULT_CLIENT_ID: &str = "KF9BRED9ZVWEI7YDqbncNQ0LhX9QoUYm";

/// Auth0 API audience の default (= chronista-hub federation、恒久 identifier)。
///
/// audience 付きで発行された access_token は RS256 JWS（aud claim 付き）になり、hub の
/// federation auth（`CREO_ID_AUDIENCES=https://hub.chronista.club`、fail-closed）を通る。
/// audience 無しだと Auth0 は JWE (alg=dir) を返し hub verify を構造的に通らない（実測）。
///
/// ⚠️ トレードオフ: この token は nexus（`aud=https://api.vantage-point.app` 期待）では
/// 拒否される。1 login = 1 audience が Auth0 の制約。現在の実用途は federation なので
/// hub 側を default とし、nexus 用 token が要る場合は `VP_OIDC_AUDIENCE` で上書きする
/// （空文字を設定すると audience なし = 従来の JWE 発行に倒せる escape hatch）。
pub const DEFAULT_AUDIENCE: &str = "https://hub.chronista.club";

/// creo-memories（creo-app-server）の API audience。
///
/// **実値の SSOT は creo の `GET https://app.creo-memories.in/api/config`（認証不要）**で、
/// `auth0.audience` に出る。2026-08-03 の実測がこの値。
/// ⚠️ creo の設計 doc 08 には `https://app.creo-memories.in` と書かれているが**誤り**。
/// 推測で書き換えず、疑ったら上の endpoint を叩いて確かめること。
///
/// env `VP_CREO_AUDIENCE` で上書き可（別 tenant / staging を試す用）。
pub const DEFAULT_CREO_AUDIENCE: &str = "https://id.anycreative.tech";

/// creo audience を解決する（env 上書き > default）。
pub fn creo_audience() -> String {
    std::env::var("VP_CREO_AUDIENCE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CREO_AUDIENCE.to_string())
}

/// PKCE verifier の長さ (= RFC 7636 で 43-128、 64 を採用)。
const VERIFIER_LEN: usize = 64;

/// CSRF state の長さ (= alphanumeric 32 chars、 約 190 bit エントロピー)。
const STATE_LEN: usize = 32;

/// callback 待ち全体の timeout (= 5 分、 user 操作完了猶予)。
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// token の宛先（= Auth0 の API audience）。
///
/// identity は 1 つ（Creo ID）だが、access_token は `aud` claim で宛先が固定されるので
/// **宛先ごとに 1 本ずつ持つ**。`--for` で選ぶ。
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthTarget {
    /// chronista-hub federation（既定）
    #[default]
    Hub,
    /// creo-memories（ACTIONS の同期先）
    Creo,
}

impl AuthTarget {
    /// 実際の audience 文字列へ。
    pub fn audience(self) -> String {
        match self {
            Self::Hub => DEFAULT_AUDIENCE.to_string(),
            Self::Creo => creo_audience(),
        }
    }
    /// 人に見せる短い名前。
    pub fn label(self) -> &'static str {
        match self {
            Self::Hub => "hub",
            Self::Creo => "creo",
        }
    }
}

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

        /// token の宛先。identity は共通なので、2 本目以降は Auth0 の session で素通りする
        #[arg(long = "for", value_enum, default_value_t = AuthTarget::Hub)]
        target: AuthTarget,
    },

    /// 認証情報を削除して logout (= nexus に best-effort 通知後 local credentials 削除)
    Logout {
        /// 宛先を指定するとその token だけ捨てる。省略時は**全部**捨てる（= 完全 logout）
        #[arg(long = "for", value_enum)]
        target: Option<AuthTarget>,
    },

    /// 宛先ごとの login 状態を一覧（token の有無と期限）。network を叩かない
    Status,
}

/// `~/.vp/credentials.json` で保存される credentials の serde shape。
///
/// access_token 以外は optional (= 最小 token のみでも parse OK)。
/// A2b で OAuth token response 全体を保存する (= refresh_token / expires_at 等)。
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Credentials {
    /// expires_at が `now + skew_secs` 以下なら true (= 期限切れまたは間もなく切れる)。
    ///
    /// - `expires_at` が `None` (= 不明) なら false (= expire 扱いしない、 reactive refresh に
    ///   任せる)。
    /// - `skew_secs` は clock 誤差や network latency を吸収する slack (= 通常 30-60 秒)。
    ///
    /// A2c では method 追加のみ、 呼び出しは future の proactive refresh で利用する素地。
    pub fn is_expired(&self, skew_secs: u64) -> bool {
        let Some(exp) = self.expires_at else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        exp <= now + skew_secs
    }
}

/// IdP / OIDC client config — env から起動時に load。
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub client_id: String,
    pub authorize_endpoint: String,
    pub token_endpoint: String,
    pub scope: String,
    /// Auth0 API audience（default = [`DEFAULT_AUDIENCE`]（hub）、env `VP_OIDC_AUDIENCE` で上書き）。
    ///
    /// `Some` なら access_token が **その API 向けの RS256 JWS**（aud claim 付き）で発行される。
    /// `None`（env に空文字を設定した escape hatch）だと Auth0 は JWE (alg=dir) の opaque token を
    /// 返し、外部 verifier（例: chronista-hub の federation auth、fail-closed で exp/iss/aud 必須）
    /// を**構造的に通らない**（2026-07-04 実測）。3 値の解決規則は [`OidcConfig::from_env`] 参照。
    pub audience: Option<String>,
}

impl OidcConfig {
    /// env から load。 全 field に default があり env は上書き用（zero-config で `vp auth login`
    /// が federation-ready な token を取れる）。
    pub fn from_env() -> Result<Self> {
        let client_id = std::env::var("VP_OIDC_CLIENT_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
        let authorize_endpoint = std::env::var("VP_OIDC_AUTHORIZE_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_AUTHORIZE_ENDPOINT.to_string());
        let token_endpoint = std::env::var("VP_OIDC_TOKEN_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_TOKEN_ENDPOINT.to_string());
        let scope = std::env::var("VP_OIDC_SCOPE").unwrap_or_else(|_| DEFAULT_SCOPE.to_string());
        // audience は 3 値: env 未設定 = default (hub) / env 空文字 = audience なし
        // （従来の JWE 発行に倒す escape hatch）/ env 非空 = その値で上書き。
        let audience = match std::env::var("VP_OIDC_AUDIENCE") {
            Err(_) => Some(DEFAULT_AUDIENCE.to_string()),
            Ok(s) => {
                let s = s.trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            }
        };
        Ok(Self {
            client_id,
            authorize_endpoint,
            token_endpoint,
            scope,
            audience,
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
    // home は `dirs::home_dir()` で解決 (Windows で `HOME` 未設定 = `USERPROFILE` を引く。
    // pty_slot.rs / vp-paths と同じ idiom)。 `HOME` 直読みは Windows で federation login を
    // 全滅させる (HOME 不在で即 error) ため使わない。
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".vp").join("credentials.json"))
}

/// audience ごとの credential を束ねた store（`~/.vp/credentials.json` の中身）。
///
/// ## なぜ audience ごとに持つか
///
/// Auth0 の access_token は **`aud` claim で宛先 API が固定**される。hub 向けに発行した token を
/// creo-app-server に出しても拒否されるのが仕様どおりの動作で、転用はできない
/// （`DEFAULT_AUDIENCE` の注記 = 「1 login = 1 audience が Auth0 の制約」）。
///
/// 一方 **identity（Creo ID）は 1 つ**なので、2 本目の authorize は Auth0 の session cookie で
/// 素通りする。つまり「ログインし直す」のではなく「**同じ人の証明書を宛先ごとに持つ**」形。
///
/// ⚠️ 旧形式（flat な `Credentials` が 1 つ）は **hub の entry として読む**（後方互換 1 段）。
/// これが無いと、更新した瞬間に既存 user の hub federation が無言で落ちる。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CredentialStore {
    /// audience → credential。key は authorize 時に要求した audience 文字列そのもの。
    #[serde(default)]
    pub by_audience: std::collections::BTreeMap<String, Credentials>,
}

impl CredentialStore {
    pub fn get(&self, audience: &str) -> Option<&Credentials> {
        self.by_audience.get(audience)
    }
    pub fn set(&mut self, audience: &str, creds: Credentials) {
        self.by_audience.insert(audience.to_string(), creds);
    }
    /// 1 つでも credential を持っているか（= Creo ID にログイン済みか）。
    pub fn is_empty(&self) -> bool {
        self.by_audience.is_empty()
    }
}

/// store を file から読む。file 不在なら**空の store**（未ログイン）。
///
/// 旧形式（flat）も受け付け、hub の entry として扱う。
pub fn read_store() -> Result<CredentialStore> {
    let path = credentials_path()?;
    if !path.exists() {
        return Ok(CredentialStore::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_store(&content).with_context(|| format!("failed to parse {}", path.display()))
}

/// 文字列 → store の純粋変換（新旧どちらの形式も受ける）。テストしやすいよう切り出す。
fn parse_store(content: &str) -> Result<CredentialStore> {
    // 新形式を先に試す。旧形式には `by_audience` が無いので空 store としてしか読めず、
    // その場合は flat として読み直す（順序を逆にすると新形式が flat parse で落ちて紛らわしい）。
    if let Ok(store) = serde_json::from_str::<CredentialStore>(content)
        && !store.is_empty()
    {
        return Ok(store);
    }
    // 旧形式: flat な Credentials 1 つ = hub 向けだった。
    let legacy: Credentials = serde_json::from_str(content)?;
    let mut store = CredentialStore::default();
    store.set(DEFAULT_AUDIENCE, legacy);
    Ok(store)
}

/// 指定 audience の credential を読む。無ければ `Ok(None)`。
pub fn read_credentials_for(audience: &str) -> Result<Option<Credentials>> {
    Ok(read_store()?.get(audience).cloned())
}

/// credentials を file に保存。
///
/// ## atomic write
///
/// `<path>.tmp` に write → `chmod 0600` (unix) → `rename(tmp, path)` で **atomic 化**。
/// rename は同 fs では POSIX atomic (= ENOSPC や mid-write kill で partial file が残らない)。
///
/// ## permissions
///
/// unix: file は `0o600` (= owner read/write only)、 parent dir も `0o700` (= owner only)。
pub fn save_credentials_for(audience: &str, creds: &Credentials) -> Result<()> {
    // ⚠️ 読んでから足して書く。丸ごと上書きすると**他の audience の token を消す**
    // （hub にログイン済みの状態で creo にログインすると federation が落ちる、が起きる）。
    let mut store = read_store().unwrap_or_default();
    store.set(audience, creds.clone());
    save_store(&store)
}

/// store 全体を書く（atomic + 0600）。
pub fn save_store(store: &CredentialStore) -> Result<()> {
    let path = credentials_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to mkdir {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // parent dir も owner only に (= ls 等で他 user に file 名が見えないように)
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(parent, perms)
                .with_context(|| format!("failed to chmod 700 {}", parent.display()))?;
        }
    }
    let json = serde_json::to_string_pretty(store).context("failed to serialize credentials")?;

    // atomic write: tmp file → rename
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp_path, perms)
            .with_context(|| format!("failed to chmod 600 {}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, &path).with_context(|| {
        format!(
            "failed to rename {} → {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// credentials file を削除する。 file 不在は成功扱い (= idempotent、 `vp auth logout` 用)。
pub fn delete_credentials() -> Result<()> {
    let path = credentials_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to delete {}", path.display())),
    }
}

/// nexus base URL — env `VP_NEXUS_URL` > default `http://127.0.0.1:9200`。
fn nexus_url() -> String {
    std::env::var("VP_NEXUS_URL").unwrap_or_else(|_| "http://127.0.0.1:9200".to_string())
}

/// PKCE pair 生成 — `(verifier, challenge)`。 RFC 7636 S256 method。
fn pkce_pair() -> (String, String) {
    let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    // rand 0.9+ で OsRng は TryRngCore 化されたため、CSPRNG の ThreadRng
    // (ChaCha12、OS エントロピーから定期 reseed) を使う。PKCE 用途には十分。
    let mut rng = rand::rng();
    let verifier: String = (0..VERIFIER_LEN)
        .map(|_| {
            let idx = rng.random_range(0..chars.len());
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
    rand::rng()
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

/// 保存済み credential を返す。期限が近ければ refresh_token で更新してから返す（proactive）。
///
/// daemon の hub 接続経路（[`crate::daemon::hub_client`]）が接続直前に呼ぶ想定。credential が
/// 24h で切れると required hub では federation が停止するため、切れる前に無音で巻き直す。
///
/// 挙動:
/// - 未ログイン（file 不在）→ `Ok(None)`（caller は credential なしで接続 = 従来 degrade）
/// - token が有効（skew 内で切れない）→ そのまま返す（refresh しない）
/// - 切れそう & refresh_token あり → refresh 成功で新 token を保存して返す。**失敗しても
///   既存 token をそのまま返す**（fail-safe: まだ有効かもしれず、hub 側の降格経路が最終防御）
/// - 切れそう & refresh_token なし（offline_access 未付与 / API 非対応）→ 既存 token を返す
///   （refresh 不能。期限切れなら hub 側で拒否 → 降格。UX 改善は再 login 誘導が別途必要）
///
/// `skew_secs` = 期限までこの秒数を切ったら「切れそう」とみなす slack（clock 誤差 + refresh
/// 往復の余裕）。
pub async fn credentials_refreshed_if_needed(
    audience: &str,
    skew_secs: u64,
) -> Result<Option<Credentials>> {
    let Some(creds) = read_credentials_for(audience)? else {
        return Ok(None);
    };
    // まだ十分に有効なら触らない（大多数のケース、HTTP を撃たない）。
    if !creds.is_expired(skew_secs) {
        return Ok(Some(creds));
    }
    // 切れそう。refresh_token が無ければ巻き直せない — 既存を返す（期限切れは hub が判断）。
    let Some(refresh) = creds.refresh_token.as_deref() else {
        return Ok(Some(creds));
    };
    let config = OidcConfig::from_env().context("refresh: OidcConfig 構築に失敗")?;
    match refresh_tokens(&config, refresh).await {
        Ok(new_tokens) => {
            let new_creds = new_tokens.into_credentials();
            // ⚠️ 巻き直した token は **元の audience の席へ**戻す。ここを取り違えると
            // hub の refresh が creo の token を上書きする（identity は同じでも宛先が違う）。
            save_credentials_for(audience, &new_creds)?;
            Ok(Some(new_creds))
        }
        // refresh 失敗（network / IdP 側）でも既存 token を返す。まだ有効な可能性があり、
        // ここで None にすると「有効 token を持っているのに credential なし接続」になって
        // required hub で無用に弾かれる。
        Err(e) => {
            tracing::warn!("hub credential の proactive refresh に失敗（既存 token で続行）: {e}");
            Ok(Some(creds))
        }
    }
}

/// `vp auth <subcommand>` のエントリ — main.rs から呼ばれる dispatch。
pub async fn execute(cmd: AuthCommands) -> Result<()> {
    match cmd {
        AuthCommands::Me => me().await,
        AuthCommands::Login { no_browser, target } => login(no_browser, target).await,
        AuthCommands::Logout { target } => logout(target).await,
        AuthCommands::Status => status(),
    }
}

/// `vp auth status` — 宛先ごとの login 状態。**network を叩かない**（local file だけを見る）。
///
/// 「有効な token を持っているか」は local に持っている事実で言い切れる範囲に留める
/// （実際に通るかは相手が決めるので、ここで断言しない）。
fn status() -> Result<()> {
    let store = read_store()?;
    if store.is_empty() {
        println!("not logged in (= run `vp auth login`)");
        return Ok(());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for target in [AuthTarget::Hub, AuthTarget::Creo] {
        let aud = target.audience();
        match store.get(&aud) {
            None => println!("{:<6} —        (no token)  {aud}", target.label()),
            Some(c) => {
                let state = match c.expires_at {
                    None => "valid   (expiry unknown)".to_string(),
                    Some(exp) if exp <= now => "EXPIRED".to_string(),
                    Some(exp) => format!("valid   ({}m left)", (exp - now) / 60),
                };
                println!("{:<6} {state}  {aud}", target.label());
            }
        }
    }
    // 表に出ない audience（env で別 tenant を試した残骸など）も隠さず出す。
    for aud in store.by_audience.keys() {
        if aud != &AuthTarget::Hub.audience() && aud != &AuthTarget::Creo.audience() {
            println!("{:<6} valid   (other)  {aud}", "-");
        }
    }
    Ok(())
}

/// `vp auth logout` — credentials を削除 + nexus に best-effort 通知。
///
/// ## flow
///
/// 1. credentials 不在なら "already logged out" + 終了
/// 2. nexus `/v1/auth/logout` に POST (= best-effort、 failure は warn のみ)
/// 3. `delete_credentials()` で local 削除 (= 必ず実行、 nexus call 結果に関わらず)
/// 4. "Logged out" message
///
/// ## 設計 — best-effort nexus call
///
/// nexus が落ちている / network 断 でも local credentials は確実に削除。
/// 「logout したつもりが token が残る」 の方が UX 最悪。 nexus side は idempotent
/// stub (= A1d で実装済)、 重複 call も OK。
async fn logout(target: Option<AuthTarget>) -> Result<()> {
    let mut store = read_store()?;
    if store.is_empty() {
        println!("already logged out (= no credentials to delete)");
        return Ok(());
    }
    // nexus への notify には**どれか 1 本**の token があればよい（identity は共通）。
    // 宛先指定があればその席の token を、無ければ hub → 先頭の順で拾う。
    let creds = match target {
        Some(t) => match store.get(&t.audience()) {
            Some(c) => c.clone(),
            None => {
                println!("already logged out for {} (= no token)", t.label());
                return Ok(());
            }
        },
        None => store
            .get(DEFAULT_AUDIENCE)
            .or_else(|| store.by_audience.values().next())
            .cloned()
            .expect("store が空でないので必ず 1 本ある"),
    };

    // nexus に best-effort で notify (= 失敗しても warn のみで続行)
    let url = format!("{}/v1/auth/logout", nexus_url());
    let result = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
        .map(|client| async move {
            client
                .post(&url)
                .header("authorization", format!("Bearer {}", creds.access_token))
                .send()
                .await
        });
    if let Some(fut) = result {
        match fut.await {
            Ok(resp) if resp.status().is_success() => {
                // nexus が ack、 何も print しない (= 静かに成功)
            }
            Ok(resp) => {
                eprintln!(
                    "warning: nexus logout returned {} (= continuing local logout)",
                    resp.status()
                );
            }
            Err(e) => {
                eprintln!("warning: nexus logout call failed: {e} (= continuing local logout)");
            }
        }
    }

    // local 削除は必ず実行
    let path = credentials_path()?;
    match target {
        // 宛先指定 = その席だけ捨てる（hub を残して creo だけ切る等）。
        Some(t) => {
            store.by_audience.remove(&t.audience());
            if store.is_empty() {
                delete_credentials()?;
            } else {
                save_store(&store)?;
            }
            println!("✓ Logged out for {} ({})", t.label(), t.audience());
        }
        // 省略 = 全部捨てる（= 完全 logout）。
        None => {
            delete_credentials()?;
            println!("✓ Logged out. Credentials removed from {}", path.display());
        }
    }
    Ok(())
}

/// `vp auth login` — loopback OAuth Native App + PKCE flow を実行、 token を保存。
async fn login(no_browser: bool, target: AuthTarget) -> Result<()> {
    let mut config = OidcConfig::from_env()?;
    // ⚠️ `--for` は env（`VP_OIDC_AUDIENCE`）より**強い**。宛先を明示して呼んでいるので、
    // env の既定に引きずられて別の席へ保存すると事故になる。
    // env の escape hatch（空文字 = audience なし）は `--for` 未指定のときだけ効く。
    config.audience = Some(target.audience());
    let (verifier, challenge) = pkce_pair();
    let state = random_state();

    // callback listener は**固定 port**（default 32800、env `VP_OIDC_CALLBACK_PORT` で上書き）。
    // 旧実装は `127.0.0.1:0` のランダム port を redirect_uri に埋めていたが、Auth0 は loopback
    // callback でも **port の完全一致**を要求する（2026-07-04 実測: tenant log
    // "http://127.0.0.1:62294/callback is not in the list of allowed callback URLs"、
    // 公称の loopback port 無視は効かない）。gcloud / rclone と同じ固定 port 方式に変更。
    // Auth0 app（Vantage Point CLI）の Allowed Callback URLs に
    // `http://127.0.0.1:32800/callback` を登録して対にする。
    let port = std::env::var("VP_OIDC_CALLBACK_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(32800);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| {
            format!(
                "callback port {port} を bind できません（他プロセスが使用中なら \
             VP_OIDC_CALLBACK_PORT で変更し、Auth0 app の Allowed Callback URLs にも \
             同じ port の URL を追加してください）"
            )
        })?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    // authorize URL 構築
    let mut qs = form_urlencoded::Serializer::new(String::new());
    qs.append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &config.scope)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    // audience が Some（= default の hub、または env 上書き）なら付与 — access_token が当該
    // API 向け RS256 JWS になる。None は env 空文字の escape hatch（JWE opaque 発行、nexus 等
    // audience なし前提の経路で使う）。
    if let Some(aud) = &config.audience {
        qs.append_pair("audience", aud);
    }
    let query = qs.finish();
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
    // 宛先の席へ保存（他の audience の token は触らない = save_credentials_for が read-modify-write）。
    save_credentials_for(&target.audience(), &creds)?;

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
    // nexus の /v1/auth/me は identity を返すだけなので、宛先は hub の席を使う
    // （identity は audience を跨いで同じ。宛先ごとの状態は `vp auth status`）。
    let creds = match read_credentials_for(DEFAULT_AUDIENCE)? {
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
                    save_credentials_for(DEFAULT_AUDIENCE, &new_creds)?;
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

    fn creds(token: &str) -> Credentials {
        Credentials {
            access_token: token.to_string(),
            token_type: Some("Bearer".to_string()),
            expires_at: Some(2_000_000_000),
            refresh_token: Some(format!("{token}-refresh")),
            scope: None,
        }
    }

    /// ⚠️ **旧形式（flat）は hub の席として読む**。この後方互換が無いと、更新した瞬間に
    /// 既存 user の hub federation が無言で落ちる（token を持っているのに「未ログイン」になる）。
    #[test]
    fn legacy_flat_credentials_load_as_hub() {
        let legacy = r#"{"access_token":"old-token","refresh_token":"old-r"}"#;
        let store = parse_store(legacy).expect("legacy parse");
        let hub = store.get(DEFAULT_AUDIENCE).expect("hub entry");
        assert_eq!(hub.access_token, "old-token");
        assert_eq!(hub.refresh_token.as_deref(), Some("old-r"));
        // 旧形式には creo の席は無い
        assert!(store.get(DEFAULT_CREO_AUDIENCE).is_none());
    }

    /// 新形式はそのまま読める（旧形式の fallback に落ちない）。
    #[test]
    fn new_store_round_trips() {
        let mut store = CredentialStore::default();
        store.set(DEFAULT_AUDIENCE, creds("hub-token"));
        store.set(DEFAULT_CREO_AUDIENCE, creds("creo-token"));
        let json = serde_json::to_string(&store).expect("ser");
        let back = parse_store(&json).expect("de");
        assert_eq!(
            back.get(DEFAULT_AUDIENCE).unwrap().access_token,
            "hub-token"
        );
        assert_eq!(
            back.get(DEFAULT_CREO_AUDIENCE).unwrap().access_token,
            "creo-token"
        );
    }

    /// ⚠️ **片方の保存が他方を消さない**。これが崩れると「creo にログインしたら hub の
    /// federation が落ちた」になる（save が read-modify-write である理由）。
    #[test]
    fn saving_one_audience_keeps_the_other() {
        let _g = env_guard();
        let dir = std::env::temp_dir().join("vp-auth-multi-aud-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("credentials.json");
        let _ = std::fs::remove_file(&path);
        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", path.to_string_lossy().to_string());
        }

        save_credentials_for(DEFAULT_AUDIENCE, &creds("hub-token")).expect("save hub");
        save_credentials_for(DEFAULT_CREO_AUDIENCE, &creds("creo-token")).expect("save creo");

        let store = read_store().expect("read");
        assert_eq!(
            store.get(DEFAULT_AUDIENCE).unwrap().access_token,
            "hub-token"
        );
        assert_eq!(
            store.get(DEFAULT_CREO_AUDIENCE).unwrap().access_token,
            "creo-token"
        );

        let _ = std::fs::remove_file(&path);
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
    }

    /// `--for` の宛先が audience 文字列に正しく写る（creo は env で上書きできる）。
    #[test]
    fn auth_target_resolves_audience() {
        let _g = env_guard();
        unsafe {
            std::env::remove_var("VP_CREO_AUDIENCE");
        }
        assert_eq!(AuthTarget::Hub.audience(), DEFAULT_AUDIENCE);
        assert_eq!(AuthTarget::Creo.audience(), DEFAULT_CREO_AUDIENCE);
        unsafe {
            std::env::set_var("VP_CREO_AUDIENCE", "https://staging.example");
        }
        assert_eq!(AuthTarget::Creo.audience(), "https://staging.example");
        unsafe {
            std::env::remove_var("VP_CREO_AUDIENCE");
        }
    }

    /// process-global な env var (`VP_CREDENTIALS_PATH` / `VP_NEXUS_URL` / `VP_OIDC_*`) を
    /// 触る test を直列化する共有ロック。 parallel runner だと別 test の `set_var`/`remove_var`
    /// が割り込み、 save→read 間で値が変わって flake する (= dogfood 4/9/10/11 N4 trap の真因。
    /// 各 test は phase 統合で intra-test race を消していたが inter-test race が残っていた)。
    ///
    /// `tokio::sync::Mutex` を使うのは async test（`#[tokio::test]`、A2e で追加）も同じロックを
    /// 共有する必要があるため。sync test は [`env_guard`]（`blocking_lock`、tokio runtime 外から
    /// 呼ぶので安全）、async test は [`env_guard_async`]（`.await` 越しに保持しても
    /// `clippy::await_holding_lock` を踏まない）を使い分ける。
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn env_guard() -> tokio::sync::MutexGuard<'static, ()> {
        ENV_LOCK.blocking_lock()
    }

    /// async test 用の [`env_guard`]。同じ `ENV_LOCK` を待つので sync/async test 間でも直列化される。
    async fn env_guard_async() -> tokio::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().await
    }

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
        let _g = env_guard();
        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", "/tmp/test-vp-creds.json");
        }
        let path = credentials_path().expect("path resolved");
        assert_eq!(path, PathBuf::from("/tmp/test-vp-creds.json"));
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
    }

    /// `credentials_path()` が `HOME` env に依存しないこと (bug/win の回帰ガード)。
    /// override 無し + `HOME` 未設定 = Windows のバグ条件を unix 上で再現し、旧実装
    /// (`std::env::var("HOME")`) なら Err で落ちること、新実装 (`dirs::home_dir()`) は
    /// unix の getpwuid フォールバックで解決することを実証する。
    #[test]
    fn credentials_path_is_home_env_independent() {
        let _g = env_guard();
        let saved_home = std::env::var_os("HOME");
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
            std::env::remove_var("HOME");
        }
        // HOME 未設定のまま結果を確保し、 assert 前に HOME を復元
        // (panic で他テストへ unset を漏らさない)。
        let resolved = credentials_path();
        let home_fallback = dirs::home_dir();
        unsafe {
            if let Some(home) = saved_home {
                std::env::set_var("HOME", home);
            }
        }
        // 旧実装は HOME 未設定で Err → ここで検知。 fallback も無い稀環境では旧新とも
        // 解決不可で一致するので is_err を許容する。
        match home_fallback {
            Some(home) => assert_eq!(
                resolved.expect("HOME 未設定でも dirs::home_dir() で解決できること"),
                home.join(".vp").join("credentials.json"),
            ),
            None => assert!(
                resolved.is_err(),
                "getpwuid フォールバックも無い環境では解決不可で旧新一致",
            ),
        }
    }

    #[test]
    fn nexus_url_env_resolution() {
        let _g = env_guard();
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
        let _g = env_guard();
        // Phase 1: 全 env 未設定 → 全 field が default（zero-config login）
        unsafe {
            std::env::remove_var("VP_OIDC_CLIENT_ID");
            std::env::remove_var("VP_OIDC_AUTHORIZE_ENDPOINT");
            std::env::remove_var("VP_OIDC_TOKEN_ENDPOINT");
            std::env::remove_var("VP_OIDC_SCOPE");
            std::env::remove_var("VP_OIDC_AUDIENCE");
        }
        let config = OidcConfig::from_env().expect("should load with defaults");
        assert_eq!(config.client_id, DEFAULT_CLIENT_ID);
        assert_eq!(config.authorize_endpoint, DEFAULT_AUTHORIZE_ENDPOINT);
        assert_eq!(config.token_endpoint, DEFAULT_TOKEN_ENDPOINT);
        assert_eq!(config.scope, DEFAULT_SCOPE);
        // audience 未設定 = default（hub、federation-ready な RS256 JWS を発行させる）
        assert_eq!(config.audience.as_deref(), Some(DEFAULT_AUDIENCE));

        // Phase 2: env set → 上書き
        unsafe {
            std::env::set_var("VP_OIDC_CLIENT_ID", "test-cid");
            std::env::set_var("VP_OIDC_AUDIENCE", "https://api.example.test");
        }
        let config = OidcConfig::from_env().expect("should load");
        assert_eq!(config.client_id, "test-cid");
        assert_eq!(config.audience.as_deref(), Some("https://api.example.test"));

        // Phase 3: audience の escape hatch — 空文字 = audience なし（従来の JWE 発行に倒す）
        unsafe {
            std::env::set_var("VP_OIDC_AUDIENCE", "");
        }
        let config = OidcConfig::from_env().expect("should load");
        assert_eq!(config.audience, None);

        // cleanup
        unsafe {
            std::env::remove_var("VP_OIDC_CLIENT_ID");
            std::env::remove_var("VP_OIDC_AUDIENCE");
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

    // === A2c tests (= dogfood 11 で追加) ===

    /// credential store の round-trip (= save → read → delete → re-delete idempotent) を
    /// **1 test 関数で順次** 検証。 別関数に分けると VP_CREDENTIALS_PATH env が parallel runner
    /// で race (= dogfood 4/9/10 N4 trap 4 回連続 + 学習)。 sequential で race ゼロ。
    #[test]
    fn credential_store_round_trip() {
        let _g = env_guard();
        // 一時 dir / file path を env override
        let tmp = tempfile::tempdir().expect("tempdir");
        let creds_dir = tmp.path().join(".vp");
        let creds_path = creds_dir.join("credentials.json");
        let path_str = creds_path.to_string_lossy().to_string();

        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", &path_str);
        }

        // Phase 1: read で None (= 不在)
        assert!(
            read_credentials_for(DEFAULT_AUDIENCE)
                .expect("read")
                .is_none()
        );

        // Phase 2: save → read で同 credentials が返る
        let orig = Credentials {
            access_token: "a-token".to_string(),
            token_type: Some("Bearer".to_string()),
            expires_at: Some(2_000_000_000),
            refresh_token: Some("r-token".to_string()),
            scope: Some("openid".to_string()),
        };
        save_credentials_for(DEFAULT_AUDIENCE, &orig).expect("save");
        let loaded = read_credentials_for(DEFAULT_AUDIENCE)
            .expect("read")
            .expect("some");
        assert_eq!(loaded.access_token, orig.access_token);
        assert_eq!(loaded.refresh_token.as_deref(), Some("r-token"));

        // Phase 3: file の mode 0600 + parent dir 0700 (unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(&creds_path)
                .expect("stat file")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(file_mode, 0o600, "credentials file should be 0600");
            let dir_mode = std::fs::metadata(&creds_dir)
                .expect("stat dir")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700, "credentials dir should be 0700");
        }

        // Phase 4: atomic write の tmp file が残らない
        let tmp_path = creds_path.with_extension("json.tmp");
        assert!(
            !tmp_path.exists(),
            "atomic tmp file should be removed after rename"
        );

        // Phase 5: delete → read で None に戻る
        delete_credentials().expect("delete");
        assert!(
            read_credentials_for(DEFAULT_AUDIENCE)
                .expect("read")
                .is_none()
        );

        // Phase 6: re-delete は idempotent (= 不在でも Ok)
        delete_credentials().expect("delete idempotent");

        // cleanup
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
    }

    #[test]
    fn is_expired_returns_false_when_no_expires_at() {
        let creds = Credentials {
            access_token: "x".to_string(),
            token_type: None,
            expires_at: None,
            refresh_token: None,
            scope: None,
        };
        assert!(!creds.is_expired(60));
    }

    #[test]
    fn is_expired_returns_true_when_past_or_within_skew() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // 過去 → expired
        let past = Credentials {
            access_token: "x".to_string(),
            token_type: None,
            expires_at: Some(now - 100),
            refresh_token: None,
            scope: None,
        };
        assert!(past.is_expired(60));

        // skew 内 (= now + 30s < skew 60s) → expired と扱う
        let soon = Credentials {
            access_token: "x".to_string(),
            token_type: None,
            expires_at: Some(now + 30),
            refresh_token: None,
            scope: None,
        };
        assert!(soon.is_expired(60));
    }

    #[test]
    fn is_expired_returns_false_when_future_beyond_skew() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let creds = Credentials {
            access_token: "x".to_string(),
            token_type: None,
            expires_at: Some(now + 3600), // 1 時間先
            refresh_token: None,
            scope: None,
        };
        assert!(!creds.is_expired(60));
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

    /// テスト用に credentials.json を temp path に書いて VP_CREDENTIALS_PATH で差し替える。
    /// 返り値の PathBuf を drop 時に消す簡易 guard は使わず、各テストで remove する。
    fn write_temp_creds(name: &str, json: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[tokio::test]
    async fn refreshed_returns_none_when_not_logged_in() {
        let _g = env_guard_async().await;
        let missing = std::env::temp_dir().join("vp-refresh-absent/credentials.json");
        // SAFETY: env_guard で直列化済み。
        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", &missing);
        }
        // file 不在 → None（credential なし接続に degrade）。HTTP は撃たない。
        let got = credentials_refreshed_if_needed(DEFAULT_AUDIENCE, 300)
            .await
            .expect("ok");
        assert!(got.is_none());
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
    }

    #[tokio::test]
    async fn refreshed_passes_through_valid_token_without_http() {
        let _g = env_guard_async().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // expires が skew の遥か先 → refresh 判定に入らず、そのまま返る（HTTP を撃たない）。
        let json = format!(
            r#"{{"access_token": "still-valid", "expires_at": {}}}"#,
            now + 3600
        );
        let path = write_temp_creds("vp-refresh-valid", &json);
        // SAFETY: env_guard で直列化済み。
        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", &path);
        }
        let got = credentials_refreshed_if_needed(DEFAULT_AUDIENCE, 300)
            .await
            .expect("ok");
        assert_eq!(got.expect("some").access_token, "still-valid");
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn refreshed_returns_existing_when_expiring_but_no_refresh_token() {
        let _g = env_guard_async().await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 期限が近い（skew 内）が refresh_token が無い → 巻き直せないので既存を返す（HTTP 不発）。
        let json = format!(
            r#"{{"access_token": "expiring-no-refresh", "expires_at": {}}}"#,
            now + 10
        );
        let path = write_temp_creds("vp-refresh-norefresh", &json);
        // SAFETY: env_guard で直列化済み。
        unsafe {
            std::env::set_var("VP_CREDENTIALS_PATH", &path);
        }
        let got = credentials_refreshed_if_needed(DEFAULT_AUDIENCE, 300)
            .await
            .expect("ok");
        assert_eq!(got.expect("some").access_token, "expiring-no-refresh");
        unsafe {
            std::env::remove_var("VP_CREDENTIALS_PATH");
        }
        let _ = std::fs::remove_file(&path);
    }
}
