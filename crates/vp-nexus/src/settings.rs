//! Phase A3a: VP 個人設定同期 — unison QUIC + session-scoped auth。
//!
//! ## transport 棲み分け
//!
//! auth (= `/v1/auth/*`、 [`crate::auth`]) は HTTP のまま (A1/A2)。 settings sync
//! **だけ** unison QUIC channel で行う。 理由は (i) VP が既に unison を多用、
//! (ii) DynamicProtocol の runtime schema discovery を dogfood、 (iii) QUIC TLS 1.3
//! で wire 暗号化済。
//!
//! ## 認証 = session-scoped verify-once
//!
//! settings channel の最初の `Authenticate` request で JWT を **1 回だけ署名検証**し、
//! 抽出した `sub` を channel handler loop の stack-local 変数に束ねる。 以降の
//! `Get` / `Set` は同 channel session 内でその `sub` を使う (= JWT 再送不要、 RSA
//! 検証は session あたり 1 回)。
//!
//! **重要 (security)**: QUIC の暗号化は wire を守るが client を認証しない。 `sub` を
//! そのまま信じるのは なりすまし可能なので、 最初の署名検証は必須。 検証済 `sub` は
//! per-connection task の stack 上にあり、 QUIC stream は 1 本 = 1 session で splice
//! 不能なため、 一度認証すれば session 終了まで信頼してよい。
//!
//! ## store
//!
//! A3a は in-memory ([`SettingsStore`] = `HashMap<sub, StoredSettings>`)。 file
//! 永続化 + conflict 検出は A3b で [`SettingsStore`] の seam を差し替えて実装する
//! (= `get` / `set` の public API は不変に保つ)。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;
use unison::network::channel::UnisonChannel;
use unison::network::quic::QuicServer;
use unison::network::{CertSource, MessageType, NetworkError, ProtocolServer};

/// settings channel 名 (= KDL schema の `channel "settings"` と一致)。
pub const SETTINGS_CHANNEL: &str = "settings";

/// QUIC port の HTTP port からの offset。 TCP (HTTP) と UDP (QUIC) は OS で port
/// 名前空間が独立するため、 同一 port 番号で共存できる (= 既存 vantage-point 慣行)。
pub const QUIC_PORT_OFFSET: u16 = 0;

/// `enable_discovery` に渡す + DynamicProtocol client が validate する KDL schema。
///
/// grammar は club-unison の `schema_registry` test の "demo" schema 書式に準拠
/// (`required=#true` は KDL boolean、 `type="int"` は JSON integer)。 version を
/// bump したら DynamicProtocol client は次回 fetch 時に renegotiate する。
pub const SETTINGS_PROTOCOL_KDL: &str = r#"
protocol "vp-settings" version="0.1.0" {
    namespace "vp.settings"
    channel "settings" from="client" lifetime="persistent" {
        request "Authenticate" {
            field "token" type="string" required=#true
            returns "AuthResult" {
                field "sub" type="string" required=#true
            }
        }
        request "Get" {
            returns "Settings" {
                field "kdl" type="string"
                field "version" type="int"
            }
        }
        request "Set" {
            field "kdl" type="string" required=#true
            returns "SetResult" {
                field "version" type="int"
            }
        }
    }
}
"#;

/// 1 user 分の保存済設定 — KDL 文字列 + version (= 楽観 concurrency の素地)。
#[derive(Clone, Default, Debug)]
pub struct StoredSettings {
    /// 設定本体 (= 不透明 KDL 文字列、 server は中身を解釈しない)
    pub kdl: String,
    /// 書き込みごとに +1 する単調 counter。 A3a は last-write-wins、 A3b で
    /// base_version conflict 検出に拡張する。
    pub version: u64,
}

/// `sub` → [`StoredSettings`] の in-memory store。
///
/// `Arc<Mutex<..>>` を wrap した `Clone` 型。 `serve_settings` で 1 個作り、
/// channel handler closure に clone して渡す。 **`get` / `set` の public API が
/// A3b (file 永続化) の差し替え seam**。
#[derive(Clone, Default)]
pub struct SettingsStore {
    inner: Arc<Mutex<HashMap<String, StoredSettings>>>,
}

impl SettingsStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定 user の設定を取得。 未保存なら default (= `{kdl: "", version: 0}`)。
    pub async fn get(&self, sub: &str) -> StoredSettings {
        self.inner
            .lock()
            .await
            .get(sub)
            .cloned()
            .unwrap_or_default()
    }

    /// 指定 user の設定を上書きし、 新しい version を返す (= A3a は last-write-wins、
    /// version は previous + 1)。
    pub async fn set(&self, sub: &str, kdl: String) -> u64 {
        let mut map = self.inner.lock().await;
        let entry = map.entry(sub.to_string()).or_default();
        entry.kdl = kdl;
        entry.version += 1;
        entry.version
    }
}

/// error response の共通 builder (= `{"error": "..."}`)。
///
/// `MessageType::Error` ではなく通常 response payload に error を載せることで、
/// client (= 生 `ProtocolClient` / `DynamicProtocol` 両方) が普通に受け取って
/// inspect できる (= 既存 unison_server.rs 慣行)。
fn err_value(msg: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "error": msg.into() })
}

/// `Authenticate` request の処理 — JWT を署名検証して `sub` を返す。
///
/// payload に `token` (= access token) が必須。 [`crate::auth::verify_token`] で
/// 署名 / iss / aud / exp を検証する (= HTTP `/v1/auth/me` と同じ verify path)。
async fn authenticate(payload: &serde_json::Value) -> Result<String, String> {
    let token = payload
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Authenticate: missing 'token' field".to_string())?;
    // 署名検証は必須 (= bare sub を信じると なりすまし可能、 暗号化 != 認証)。
    let claims = crate::auth::verify_token(token)
        .await
        .map_err(|e| format!("authentication failed: {e:?}"))?;
    Ok(claims.sub)
}

/// 認証済 session の `Get` / `Set` dispatch。
async fn dispatch_authed(
    store: &SettingsStore,
    sub: &str,
    method: &str,
    payload: &serde_json::Value,
) -> serde_json::Value {
    match method {
        "Get" => {
            let s = store.get(sub).await;
            serde_json::json!({ "kdl": s.kdl, "version": s.version })
        }
        "Set" => match payload.get("kdl").and_then(|v| v.as_str()) {
            None => err_value("Set: missing 'kdl' field"),
            Some(kdl) => {
                let version = store.set(sub, kdl.to_string()).await;
                serde_json::json!({ "version": version })
            }
        },
        _ => err_value(format!("unknown method: {method}")),
    }
}

/// settings channel の handler loop (= 1 connection / channel ごとに 1 回起動)。
///
/// session-scoped auth: `authenticated_sub` が `None` の間は `Get` / `Set` を拒否、
/// `Authenticate` 成功で `Some(sub)` に束ねる。 失敗した `Authenticate` は sub を
/// 束ねない (= session は locked のまま)。 channel 切断 (= `recv` Err) で loop を
/// 抜けて task 終了、 stack-local の sub は自然に破棄される (= cleanup 不要)。
async fn run_settings_channel(
    store: SettingsStore,
    channel: UnisonChannel,
) -> Result<(), NetworkError> {
    // この変数は per-connection task の stack 上にあり、 QUIC stream 1 本 = 1 session
    // なので splice 不能。 一度束ねれば session 終了まで信頼してよい。
    let mut authenticated_sub: Option<String> = None;

    loop {
        let msg = match channel.recv().await {
            Ok(m) => m,
            Err(_) => break, // peer close / transport error → session 終了
        };
        if msg.msg_type != MessageType::Request {
            continue;
        }

        let request_id = msg.id;
        let method = msg.method.clone();
        let payload = msg.payload_as_value().unwrap_or_default();

        let response: serde_json::Value = match method.as_str() {
            "Authenticate" => match authenticate(&payload).await {
                Ok(sub) => {
                    authenticated_sub = Some(sub.clone());
                    serde_json::json!({ "sub": sub })
                }
                // 失敗時は sub を束ねない (= session locked 維持)
                Err(e) => err_value(e),
            },
            "Get" | "Set" => match authenticated_sub.as_deref() {
                None => err_value("unauthenticated: send Authenticate first"),
                Some(sub) => dispatch_authed(&store, sub, &method, &payload).await,
            },
            other => err_value(format!("unknown method: {other}")),
        };

        if channel
            .send_response(request_id, &method, &response)
            .await
            .is_err()
        {
            break;
        }
    }

    Ok(())
}

/// settings QUIC server を build → bind → run する。 caller (= main.rs) は
/// `tokio::spawn` で axum HTTP server と並走させる。
///
/// - `addr`: bind address (= `"[::]:9200"` 等)。
/// - `shutdown_rx`: 発火で graceful shutdown。
/// - `ready_tx`: bind 完了時に実 `SocketAddr` を、 失敗時に `None` を送る (= test が
///   ephemeral port `[::1]:0` を使って実 port を知るため)。
pub async fn serve_settings(
    store: SettingsStore,
    addr: String,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ready_tx: tokio::sync::oneshot::Sender<Option<SocketAddr>>,
) -> anyhow::Result<()> {
    let server =
        ProtocolServer::with_identity("vp-settings", env!("CARGO_PKG_VERSION"), "vp.settings");

    // DynamicProtocol client が schema を fetch できるよう discovery を有効化。
    server
        .enable_discovery(SETTINGS_PROTOCOL_KDL)
        .await
        .context("failed to enable unison discovery")?;

    server
        .register_channel(SETTINGS_CHANNEL, {
            let store = store.clone();
            move |_ctx, stream| {
                let store = store.clone();
                async move { run_settings_channel(store, UnisonChannel::new(stream)).await }
            }
        })
        .await;

    let server = Arc::new(server);
    // TODO(A3-prod): dev_localhost cert を mesh keypair / 実 CA cert に差し替える
    // (= nexus QUIC を production deploy する時の cross-cutting follow-up)。
    let mut quic = QuicServer::builder(server)
        .cert_source(CertSource::dev_localhost())
        .build();

    if let Err(e) = quic.bind(&addr).await {
        let _ = ready_tx.send(None);
        return Err(e).context("settings QUIC server failed to bind");
    }
    let _ = ready_tx.send(quic.local_addr());

    quic.start_with_shutdown(shutdown_rx)
        .await
        .context("settings QUIC server terminated with error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_get_on_empty_returns_default() {
        let store = SettingsStore::new();
        let s = store.get("nobody").await;
        assert_eq!(s.kdl, "");
        assert_eq!(s.version, 0);
    }

    #[tokio::test]
    async fn store_set_bumps_version_monotonically() {
        let store = SettingsStore::new();
        assert_eq!(store.set("u", "a".to_string()).await, 1);
        assert_eq!(store.set("u", "b".to_string()).await, 2);
        let s = store.get("u").await;
        assert_eq!(s.kdl, "b");
        assert_eq!(s.version, 2);
    }

    #[tokio::test]
    async fn store_isolates_per_sub() {
        let store = SettingsStore::new();
        store.set("alice", "alice-kdl".to_string()).await;
        // bob は影響を受けない
        let bob = store.get("bob").await;
        assert_eq!(bob.kdl, "");
        assert_eq!(bob.version, 0);
        let alice = store.get("alice").await;
        assert_eq!(alice.kdl, "alice-kdl");
    }

    #[tokio::test]
    async fn dispatch_get_then_set_then_get() {
        let store = SettingsStore::new();
        // 初期 Get
        let g0 = dispatch_authed(&store, "u", "Get", &serde_json::json!({})).await;
        assert_eq!(g0["version"], 0);
        assert_eq!(g0["kdl"], "");
        // Set
        let s1 = dispatch_authed(
            &store,
            "u",
            "Set",
            &serde_json::json!({"kdl": "theme \"dark\""}),
        )
        .await;
        assert_eq!(s1["version"], 1);
        // 再 Get
        let g1 = dispatch_authed(&store, "u", "Get", &serde_json::json!({})).await;
        assert_eq!(g1["kdl"], "theme \"dark\"");
        assert_eq!(g1["version"], 1);
    }

    #[tokio::test]
    async fn dispatch_set_missing_kdl_returns_error() {
        let store = SettingsStore::new();
        let r = dispatch_authed(&store, "u", "Set", &serde_json::json!({})).await;
        assert!(r.get("error").is_some());
    }

    #[tokio::test]
    async fn authenticate_missing_token_errors() {
        let r = authenticate(&serde_json::json!({})).await;
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("missing 'token'"));
    }

    #[test]
    fn protocol_kdl_declares_settings_channel_and_methods() {
        // schema 文字列の guard (= channel / method 名が handler dispatch と一致)
        assert!(SETTINGS_PROTOCOL_KDL.contains("channel \"settings\""));
        assert!(SETTINGS_PROTOCOL_KDL.contains("request \"Authenticate\""));
        assert!(SETTINGS_PROTOCOL_KDL.contains("request \"Get\""));
        assert!(SETTINGS_PROTOCOL_KDL.contains("request \"Set\""));
    }
}
