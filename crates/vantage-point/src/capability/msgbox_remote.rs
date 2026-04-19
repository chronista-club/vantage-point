//! Msgbox Remote Routing — Cross-Process メッセージ転送（Msgbox Phase 3 Step 2）
//!
//! `Router` から remote address 宛のメッセージを受け取り、TheWorld registry で
//! target Process の port を解決、HTTP（暫定）で `msgbox_remote_deliver` を呼ぶ。
//!
//! ## 改善ポイント（Step 2 設計レビュー対応）
//!
//! 1. **Auth**: `RegistryToken` を Bearer header で送信、receive 側で検証
//! 2. **Backpressure**: routing_loop 側で bounded channel + persistent 強制
//! 3. **`from` 正規化**: port 形式 → project 形式に書き換え（reply 安定化）
//! 4. **Retry**: exponential backoff（1s/2s/4s/8s/16s）最大 5 回
//! 5. **LRU cache**: 30s TTL で TheWorld lookup を抑制

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::capability::msgbox::Message;
use crate::capability::msgbox_registry::{ActorEntry, ResolvedAddress};

/// Lookup cache の TTL（30 秒）
const LOOKUP_CACHE_TTL: Duration = Duration::from_secs(30);

/// Forward 失敗時のリトライ最大回数
const FORWARD_MAX_RETRIES: u32 = 5;

/// 認証トークン形式
///
/// TheWorld registry が発行 / 受信側 Process が検証する Bearer token。
/// Phase 3 Step 2 簡易版: 環境変数 `VP_REGISTRY_TOKEN` から取得。
/// 未設定の場合は空 token = auth 無効（development デフォルト）。
pub fn registry_token() -> Option<String> {
    std::env::var("VP_REGISTRY_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
}

// =============================================================================
// TheWorld registry への register/unregister（Step 2b: Process startup/shutdown）
// =============================================================================

/// 単一 actor を TheWorld registry に register
pub async fn register_actor_to_world(
    world_port: u16,
    project_name: &str,
    self_port: u16,
    actor: &str,
) -> anyhow::Result<()> {
    let url = format!("http://[::1]:{}/api/world/msgbox/register", world_port);
    let body = serde_json::json!({
        "actor": actor,
        "project_name": project_name,
        "port": self_port,
    });

    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?
        .post(&url)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("register failed: HTTP {} - {}", status, body);
    }
    Ok(())
}

/// 一括 register（Process 起動時）
///
/// 各 actor の register に失敗しても他は試す。失敗 actor 名のリストを返す。
pub async fn register_actors_to_world(
    world_port: u16,
    project_name: &str,
    self_port: u16,
    actors: &[String],
) -> Vec<String> {
    let mut failed = Vec::new();
    for actor in actors {
        if let Err(e) = register_actor_to_world(world_port, project_name, self_port, actor).await {
            tracing::warn!(
                "Router: register '{}' to TheWorld failed: {}",
                actor,
                e
            );
            failed.push(actor.clone());
        }
    }
    failed
}

/// Process（port）配下の全 actor を TheWorld registry から一括 unregister
///
/// Process 停止時に呼ぶ。失敗してもログ出すだけ（shutdown を止めない）。
pub async fn unregister_process_from_world(world_port: u16, self_port: u16) -> anyhow::Result<()> {
    let url = format!(
        "http://[::1]:{}/api/world/msgbox/unregister-process",
        world_port
    );
    let body = serde_json::json!({ "port": self_port });

    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?
        .post(&url)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("unregister failed: HTTP {} - {}", status, body);
    }
    Ok(())
}

/// Remote routing 用のクライアント
///
/// TheWorld への HTTP lookup（cache 付き）と target Process への forward を担う。
#[derive(Debug, Clone)]
pub struct RemoteRoutingClient {
    /// TheWorld の HTTP base URL（例: `http://[::1]:32000`）
    world_base_url: String,
    /// 自 Process の project_name（local 判定 + from 正規化用）
    local_project: String,
    /// 自 Process の port（local 判定用）
    local_port: u16,
    /// Lookup cache（30s TTL）— `(actor, port_or_project)` → entry
    lookup_cache: Arc<Mutex<HashMap<String, (ActorEntry, Instant)>>>,
}

/// Remote routing エラー
#[derive(Debug, thiserror::Error)]
pub enum RemoteRoutingError {
    #[error("TheWorld lookup failed: {0}")]
    LookupFailed(String),
    #[error("actor not found in registry: {actor}")]
    ActorNotFound { actor: String },
    #[error("forward to {port} failed: {reason}")]
    ForwardFailed { port: u16, reason: String },
    #[error("invalid address format: {0}")]
    InvalidAddress(String),
    #[error("forward retries exhausted ({retries} times)")]
    RetriesExhausted { retries: u32 },
}

impl RemoteRoutingClient {
    /// 新しい RemoteRoutingClient を作成
    pub fn new(world_port: u16, local_project: impl Into<String>, local_port: u16) -> Self {
        Self {
            world_base_url: format!("http://[::1]:{}", world_port),
            local_project: local_project.into(),
            local_port,
            lookup_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// アドレスが local（自 Process）を指しているか判定
    pub fn is_local(&self, resolved: &ResolvedAddress) -> bool {
        match resolved {
            ResolvedAddress::Local { .. } => true,
            ResolvedAddress::Port { port, .. } => *port == self.local_port,
            ResolvedAddress::Project { project, .. } => project == &self.local_project,
        }
    }

    /// 自 Process の project_name
    pub fn local_project(&self) -> &str {
        &self.local_project
    }

    /// Cache key を生成
    fn cache_key(resolved: &ResolvedAddress) -> Option<String> {
        match resolved {
            ResolvedAddress::Local { .. } => None,
            ResolvedAddress::Port { actor, port } => Some(format!("{}@p{}", actor, port)),
            ResolvedAddress::Project { actor, project } => Some(format!("{}@n{}", actor, project)),
        }
    }

    /// Cache から有効 entry を引く（期限切れは削除）
    async fn cache_get(&self, key: &str) -> Option<ActorEntry> {
        let mut cache = self.lookup_cache.lock().await;
        if let Some((entry, inserted_at)) = cache.get(key) {
            if inserted_at.elapsed() < LOOKUP_CACHE_TTL {
                return Some(entry.clone());
            }
            // 期限切れ: 削除
            cache.remove(key);
        }
        None
    }

    /// Cache に insert
    async fn cache_put(&self, key: String, entry: ActorEntry) {
        self.lookup_cache
            .lock()
            .await
            .insert(key, (entry, Instant::now()));
    }

    /// TheWorld registry で actor を lookup（cache 経由、必要時 HTTP）
    pub async fn lookup(
        &self,
        resolved: &ResolvedAddress,
    ) -> Result<ActorEntry, RemoteRoutingError> {
        // 1. Cache lookup
        if let Some(key) = Self::cache_key(resolved)
            && let Some(entry) = self.cache_get(&key).await
        {
            return Ok(entry);
        }

        // 2. HTTP lookup
        let url = match resolved {
            ResolvedAddress::Local { .. } => {
                return Err(RemoteRoutingError::InvalidAddress(
                    "local address cannot be looked up remotely".to_string(),
                ));
            }
            // actor / project は validate 済み（[a-zA-Z0-9_.-]）で URL 安全文字のみ
            ResolvedAddress::Port { actor, port } => {
                format!(
                    "{}/api/world/msgbox/lookup?actor={}&port={}",
                    self.world_base_url, actor, port
                )
            }
            ResolvedAddress::Project { actor, project } => {
                format!(
                    "{}/api/world/msgbox/lookup?actor={}&project_name={}",
                    self.world_base_url, actor, project
                )
            }
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| RemoteRoutingError::LookupFailed(e.to_string()))?;

        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| RemoteRoutingError::LookupFailed(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let actor_name = resolved.actor_or_unknown().to_string();
            return Err(RemoteRoutingError::ActorNotFound { actor: actor_name });
        }

        if !resp.status().is_success() {
            return Err(RemoteRoutingError::LookupFailed(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        #[derive(serde::Deserialize)]
        struct LookupResponse {
            entry: ActorEntry,
        }

        let body: LookupResponse = resp
            .json()
            .await
            .map_err(|e| RemoteRoutingError::LookupFailed(format!("decode: {}", e)))?;

        // 3. Cache に保存
        if let Some(key) = Self::cache_key(resolved) {
            self.cache_put(key, body.entry.clone()).await;
        }

        Ok(body.entry)
    }

    /// 解決済みアドレスにメッセージを forward（retry 付き）
    ///
    /// 1. TheWorld で lookup（cache）→ ActorEntry
    /// 2. msg.to を actor 名のみ、msg.from を `actor@local_project` に正規化
    /// 3. exponential backoff で最大 5 回リトライ
    pub async fn forward(
        &self,
        resolved: &ResolvedAddress,
        msg: Message,
    ) -> Result<(), RemoteRoutingError> {
        let entry = self.lookup(resolved).await?;
        let target_port = entry.port;
        let target_project = entry.project_name.clone();

        // 正規化:
        // - to: actor 名のみ（@... は剥がす）
        // - from: actor@local_project 形式（port 形式の場合は project 形式に変換）
        let actor_only = resolved.actor_or_unknown().to_string();
        let mut normalized = msg.clone();
        normalized.to = actor_only;
        normalized.from = self.normalize_from(&normalized.from).await;

        // exponential backoff: 1s/2s/4s/8s/16s
        let mut delay = Duration::from_secs(1);
        let mut last_err: Option<String> = None;
        for attempt in 0..FORWARD_MAX_RETRIES {
            match http_forward(target_port, &normalized).await {
                Ok(()) => {
                    if attempt > 0 {
                        tracing::debug!(
                            "Router: forward 成功（{} 回目のリトライ） to={}@{}",
                            attempt + 1,
                            normalized.to,
                            target_project
                        );
                    }
                    return Ok(());
                }
                Err(e) => {
                    let reason = e.to_string();
                    tracing::warn!(
                        "Router: forward 試行 {}/{} 失敗 to={}@{} reason={}",
                        attempt + 1,
                        FORWARD_MAX_RETRIES,
                        normalized.to,
                        target_project,
                        reason
                    );
                    last_err = Some(reason);
                    if attempt < FORWARD_MAX_RETRIES - 1 {
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(16));
                    }
                }
            }
        }

        Err(RemoteRoutingError::ForwardFailed {
            port: target_port,
            reason: last_err.unwrap_or_else(|| "unknown".to_string()),
        })
    }

    /// `from` を project 形式に正規化
    ///
    /// - 既に `actor@project` → そのまま
    /// - `actor` → `actor@local_project` を付与
    /// - `actor@port`（数字 suffix）→ port から project に逆引き（cache 経由）
    async fn normalize_from(&self, from: &str) -> String {
        let Some((actor, locator)) = from.split_once('@') else {
            // suffix なし → 自 project を付与
            return format!("{}@{}", from, self.local_project);
        };

        if locator.parse::<u16>().is_ok() {
            // port 形式 → project に逆引き
            // 自 project と同じ port なら簡単、違うなら lookup（cache）
            let port: u16 = locator.parse().unwrap();
            if port == self.local_port {
                return format!("{}@{}", actor, self.local_project);
            }
            // cache から探す
            let cache = self.lookup_cache.lock().await;
            for (_, (entry, _)) in cache.iter() {
                if entry.port == port {
                    return format!("{}@{}", actor, entry.project_name);
                }
            }
            // 見つからない: そのまま port 形式で送る（reply は port-based になるが致命的ではない）
            from.to_string()
        } else {
            // 既に project 形式
            from.to_string()
        }
    }
}

/// HTTP fallback で remote_deliver を呼ぶ（Step 2 暫定 — Step 2b で Unison QUIC へ）
async fn http_forward(target_port: u16, msg: &Message) -> anyhow::Result<()> {
    let url = format!("http://[::1]:{}/api/msgbox/remote_deliver", target_port);
    let mut req = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?
        .post(&url)
        .json(msg);

    // Auth: VP_REGISTRY_TOKEN 設定時のみ Bearer 付与
    if let Some(token) = registry_token() {
        req = req.bearer_auth(token);
    }

    let resp = req.send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("HTTP {}: {}", status, body);
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_client() -> RemoteRoutingClient {
        RemoteRoutingClient::new(32000, "vantage-point", 33003)
    }

    #[test]
    fn test_is_local_with_local_address() {
        let client = make_client();
        let addr = ResolvedAddress::Local {
            actor: "agent".to_string(),
        };
        assert!(client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_matching_port() {
        let client = make_client();
        let addr = ResolvedAddress::Port {
            actor: "agent".to_string(),
            port: 33003,
        };
        assert!(client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_different_port() {
        let client = make_client();
        let addr = ResolvedAddress::Port {
            actor: "agent".to_string(),
            port: 33000,
        };
        assert!(!client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_matching_project() {
        let client = make_client();
        let addr = ResolvedAddress::Project {
            actor: "agent".to_string(),
            project: "vantage-point".to_string(),
        };
        assert!(client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_different_project() {
        let client = make_client();
        let addr = ResolvedAddress::Project {
            actor: "agent".to_string(),
            project: "creo-memories".to_string(),
        };
        assert!(!client.is_local(&addr));
    }

    #[tokio::test]
    async fn test_lookup_local_address_returns_invalid() {
        let client = make_client();
        let addr = ResolvedAddress::Local {
            actor: "agent".to_string(),
        };
        let result = client.lookup(&addr).await;
        assert!(matches!(result, Err(RemoteRoutingError::InvalidAddress(_))));
    }

    #[tokio::test]
    async fn test_normalize_from_no_suffix_adds_local_project() {
        let client = make_client();
        let result = client.normalize_from("agent").await;
        assert_eq!(result, "agent@vantage-point");
    }

    #[tokio::test]
    async fn test_normalize_from_with_project_unchanged() {
        let client = make_client();
        let result = client.normalize_from("agent@creo-memories").await;
        assert_eq!(result, "agent@creo-memories");
    }

    #[tokio::test]
    async fn test_normalize_from_with_local_port_uses_local_project() {
        let client = make_client();
        let result = client.normalize_from("agent@33003").await;
        assert_eq!(result, "agent@vantage-point");
    }

    #[tokio::test]
    async fn test_cache_key_local_returns_none() {
        let addr = ResolvedAddress::Local {
            actor: "agent".to_string(),
        };
        assert!(RemoteRoutingClient::cache_key(&addr).is_none());
    }

    #[tokio::test]
    async fn test_cache_key_port_format() {
        let addr = ResolvedAddress::Port {
            actor: "agent".to_string(),
            port: 33003,
        };
        assert_eq!(
            RemoteRoutingClient::cache_key(&addr),
            Some("agent@p33003".to_string())
        );
    }

    #[tokio::test]
    async fn test_cache_key_project_format() {
        let addr = ResolvedAddress::Project {
            actor: "agent".to_string(),
            project: "vantage-point".to_string(),
        };
        assert_eq!(
            RemoteRoutingClient::cache_key(&addr),
            Some("agent@nvantage-point".to_string())
        );
    }

    #[tokio::test]
    async fn test_cache_put_and_get_within_ttl() {
        let client = make_client();
        let entry = ActorEntry {
            actor: "agent".to_string(),
            project_name: "vantage-point".to_string(),
            port: 33003,
            registered_at: 0,
        };

        client
            .cache_put("test-key".to_string(), entry.clone())
            .await;
        let got = client.cache_get("test-key").await;
        assert_eq!(got, Some(entry));
    }

    #[test]
    fn test_registry_token_from_env() {
        // env var 未設定時は None
        unsafe {
            std::env::remove_var("VP_REGISTRY_TOKEN");
        }
        assert!(registry_token().is_none());

        // 空文字列も None 扱い
        unsafe {
            std::env::set_var("VP_REGISTRY_TOKEN", "");
        }
        assert!(registry_token().is_none());

        // セット時は Some
        unsafe {
            std::env::set_var("VP_REGISTRY_TOKEN", "test-token-123");
        }
        assert_eq!(registry_token(), Some("test-token-123".to_string()));

        // クリーンアップ
        unsafe {
            std::env::remove_var("VP_REGISTRY_TOKEN");
        }
    }
}
