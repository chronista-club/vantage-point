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
use crate::capability::msgbox_registry::{ActorEntry, Address};

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
            tracing::warn!("Router: register '{}' to TheWorld failed: {}", actor, e);
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
    ///
    /// v3.1: `Address::Project { world, project, .. }` で world: Some(_) は federated remote、
    /// world: None かつ project = self project のみ local とみなす。
    pub fn is_local(&self, resolved: &Address) -> bool {
        match resolved {
            Address::Local { .. } => true,
            Address::Port { port, .. } => *port == self.local_port,
            Address::Project { world, project, .. } => {
                world.is_none() && project == &self.local_project
            }
        }
    }

    /// 自 Process の project_name
    pub fn local_project(&self) -> &str {
        &self.local_project
    }

    /// Cache key を生成
    ///
    /// v3.1: world / lane segments を含めて cache 分離。
    /// format: `<actor>@n[<world>/]<project>[/<lane>...]`
    fn cache_key(resolved: &Address) -> Option<String> {
        match resolved {
            Address::Local { .. } => None,
            Address::Port { actor, port } => Some(format!("{}@p{}", actor, port)),
            Address::Project {
                actor,
                world,
                project,
                lane,
            } => {
                let mut location = String::new();
                if let Some(w) = world {
                    location.push_str(w);
                    location.push('/');
                }
                location.push_str(project);
                for seg in lane {
                    location.push('/');
                    location.push_str(seg);
                }
                Some(format!("{}@n{}", actor, location))
            }
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
    pub async fn lookup(&self, resolved: &Address) -> Result<ActorEntry, RemoteRoutingError> {
        // 1. Cache lookup
        if let Some(key) = Self::cache_key(resolved)
            && let Some(entry) = self.cache_get(&key).await
        {
            return Ok(entry);
        }

        // 2. HTTP lookup
        let url = match resolved {
            Address::Local { .. } => {
                return Err(RemoteRoutingError::InvalidAddress(
                    "local address cannot be looked up remotely".to_string(),
                ));
            }
            // actor / project は validate 済み（[a-zA-Z0-9_-]）で URL 安全文字のみ
            Address::Port { actor, port } => {
                format!(
                    "{}/api/world/msgbox/lookup?actor={}&port={}",
                    self.world_base_url, actor, port
                )
            }
            Address::Project {
                actor,
                world: _, // v3.1 federated world は Phase 3+ で resolve、 self world のみ lookup 可
                project,
                lane: _, // lane は registry-side では未対応 (LSCM Q-7)、 lookup は actor/project のみ
            } => {
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
    /// 1. **same-machine** (`Address::Project { world: None, .. }` 等): TheWorld で lookup
    ///    （cache）→ ActorEntry → `http_forward("[::1]", entry.port, ...)` で local HTTP
    /// 2. **cross-machine LAN** (VP-148 PR-P3-3、 `Address::Project { world: Some(host), .. }`):
    ///    AddressBook (= `~/.config/vp/addresses.toml`) で `host` を hostname lookup →
    ///    `http_forward(entry.hostname, entry.port, ...)` で cross-machine HTTP forward
    /// 3. msg.to を actor 名のみ、msg.from を `actor@local_project` に正規化
    /// 4. exponential backoff で最大 5 回リトライ
    pub async fn forward(
        &self,
        resolved: &Address,
        msg: Message,
    ) -> Result<(), RemoteRoutingError> {
        // VP-148 PR-P3-3: world: Some(host) の federated address は AddressBook lookup 経路へ
        if let Address::Project {
            world: Some(host), ..
        } = resolved
        {
            return self.forward_cross_machine(host, resolved, msg).await;
        }

        // same-machine (= 既存 path): TheWorld registry lookup → http_forward localhost
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
            match http_forward("[::1]", target_port, &normalized).await {
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

    /// VP-148 PR-P3-3: cross-machine LAN forward (= AddressBook lookup + cross-machine HTTP)
    ///
    /// `host` は v3.1 syntax の world segment (例 `macbook-a.local`、 dot 必須)。
    /// AddressBook の hostname と equality match で entry 取得、 hostname:port に HTTP POST。
    /// hub query (= dot 含む FQDN で `.local` 以外) は Phase 4+ で別 path、 本 PR では
    /// AddressBook miss 時に `ActorNotFound` エラーで返却。
    ///
    /// VP-150 fix: cross-machine forward は **2 hop** で resolve する。
    /// hop 1: AddressBook で host → `<host>:<world_port>` (= remote TheWorld daemon)、
    /// hop 2: remote TheWorld の `/api/world/msgbox/lookup` で project + actor → SP port、
    /// hop 3: 取得した SP port に `http_forward(host, sp_port, msg)` で actual deliver。
    /// 旧実装 (= PR-P3-3、 #312) は hop 2 を欠落させて TheWorld port (32000) に直接 POST して
    /// いたため、 receiver が `/api/msgbox/remote_deliver` endpoint を持たず 404 で fail。
    async fn forward_cross_machine(
        &self,
        host: &str,
        resolved: &Address,
        msg: Message,
    ) -> Result<(), RemoteRoutingError> {
        // hop 1: AddressBook で host → world_port を取得
        let book = crate::commands::lan::AddressBook::load()
            .map_err(|e| RemoteRoutingError::LookupFailed(format!("address book load: {}", e)))?;
        let entry = book
            .find_by_host(host)
            .ok_or_else(|| RemoteRoutingError::ActorNotFound {
                actor: format!(
                    "{} (cross-machine host '{}' not in address book、 `vp lan add` で登録要)",
                    resolved.actor_or_unknown(),
                    host
                ),
            })?;
        let target_host = entry.hostname.clone();
        let world_port = entry.port; // remote TheWorld daemon port (= 32000)

        // hop 2: remote TheWorld に project + actor を query して SP port を取得
        //
        // Moody Blues fix #2 (Score 77): hop 2 は **1-shot 3s timeout** で retry なし、
        // hop 3 (= http_forward retry loop) と非対称。 remote TheWorld が 起動中 / GC pause
        // 等で一時不応答だと、 sender が即 LookupFailed / ActorNotFound で fail する。
        // dogfood 段階では許容、 dogfood で実害 (= short outage で msg lost) が出たら別 PR で
        // 簡易 retry (max 3 回、 1s/2s backoff) を hop 2 にも追加する path 残し。
        let remote_world_url = format!("http://{}:{}", target_host, world_port);
        let sp_entry = lookup_via_world_url(&remote_world_url, resolved).await?;
        let sp_port = sp_entry.port;

        // hop 3: SP port に actual deliver
        let actor_only = resolved.actor_or_unknown().to_string();
        let mut normalized = msg.clone();
        normalized.to = actor_only;
        normalized.from = self.normalize_from(&normalized.from).await;

        // 既存 same-machine path と同じ exponential backoff
        let mut delay = Duration::from_secs(1);
        let mut last_err: Option<String> = None;
        for attempt in 0..FORWARD_MAX_RETRIES {
            match http_forward(&target_host, sp_port, &normalized).await {
                Ok(()) => {
                    if attempt > 0 {
                        tracing::debug!(
                            "Router: cross-machine forward 成功 (retry {}) to={} via {}:{} (sp_port resolved via {}:{})",
                            attempt + 1,
                            normalized.to,
                            target_host,
                            sp_port,
                            target_host,
                            world_port
                        );
                    } else {
                        tracing::debug!(
                            "Router: cross-machine forward to={} via {}:{} (sp_port resolved via {}:{}) ok",
                            normalized.to,
                            target_host,
                            sp_port,
                            target_host,
                            world_port
                        );
                    }
                    return Ok(());
                }
                Err(e) => {
                    let reason = e.to_string();
                    tracing::warn!(
                        "Router: cross-machine forward 試行 {}/{} 失敗 to={} host={} sp_port={} reason={}",
                        attempt + 1,
                        FORWARD_MAX_RETRIES,
                        normalized.to,
                        target_host,
                        sp_port,
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
            port: sp_port,
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

/// VP-150: 任意 base URL の TheWorld registry に `/api/world/msgbox/lookup` query を投げる
///
/// cross-machine forward の **hop 2** で使う。 self の TheWorld (= `RemoteRoutingClient::lookup`
/// の `self.world_base_url` 固定 path) と異なり、 remote `<host>:<world_port>` を base に
/// 同 endpoint を query する。 戻り値の `ActorEntry.port` が **remote machine 上の SP port**
/// で、 caller (= `forward_cross_machine`) がそれに対して `http_forward` する。
///
/// cache 不在: cross-machine の lookup result は host 別で keyed する必要があり、 既存
/// `lookup_cache` (= self-world 想定) と分離が必要。 PR-P3-3 と同じく毎回 query を
/// 許容する design (= dogfood 段階 perf 後送り)。 将来的には `(host, actor, project)`
/// キーの cache を追加する path 残し。
async fn lookup_via_world_url(
    world_base_url: &str,
    resolved: &Address,
) -> Result<ActorEntry, RemoteRoutingError> {
    let url = match resolved {
        Address::Local { .. } => {
            return Err(RemoteRoutingError::InvalidAddress(
                "local address cannot be looked up via remote world".to_string(),
            ));
        }
        Address::Port { .. } => {
            // Moody Blues fix #1 (Score 78): Port form の `port` field は sender 機視点の
            // ローカル port 番号、 remote machine 上の registry では同 port が別 process / 不在の
            // 可能性が高い。 cross-machine context で port lookup は意味的に成立しないため
            // fail-fast (= silent な semantics 違反より明示的 error)。
            return Err(RemoteRoutingError::InvalidAddress(
                "Port form address cannot be looked up via remote world (port is sender-local)"
                    .to_string(),
            ));
        }
        Address::Project { actor, project, .. } => {
            format!(
                "{}/api/world/msgbox/lookup?actor={}&project_name={}",
                world_base_url, actor, project
            )
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| RemoteRoutingError::LookupFailed(e.to_string()))?;

    let resp =
        client.get(&url).send().await.map_err(|e| {
            RemoteRoutingError::LookupFailed(format!("cross-machine lookup: {}", e))
        })?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        let actor_name = resolved.actor_or_unknown().to_string();
        return Err(RemoteRoutingError::ActorNotFound { actor: actor_name });
    }

    if !resp.status().is_success() {
        return Err(RemoteRoutingError::LookupFailed(format!(
            "cross-machine lookup HTTP {}",
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

    Ok(body.entry)
}

/// HTTP fallback で remote_deliver を呼ぶ（Step 2 暫定 — Step 2b で Unison QUIC へ）
///
/// VP-148 PR-P3-3: `target_host` 引数で cross-machine forward に対応 (= 既存 same-machine
/// caller は `"[::1]"` を渡す、 LAN cross-machine caller は AddressBook の hostname を渡す)。
async fn http_forward(target_host: &str, target_port: u16, msg: &Message) -> anyhow::Result<()> {
    let url = format!(
        "http://{}:{}/api/msgbox/remote_deliver",
        target_host, target_port
    );
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
        let addr = Address::Local {
            actor: "agent".to_string(),
        };
        assert!(client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_matching_port() {
        let client = make_client();
        let addr = Address::Port {
            actor: "agent".to_string(),
            port: 33003,
        };
        assert!(client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_different_port() {
        let client = make_client();
        let addr = Address::Port {
            actor: "agent".to_string(),
            port: 33000,
        };
        assert!(!client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_matching_project() {
        let client = make_client();
        let addr = Address::Project {
            actor: "agent".to_string(),
            world: None,
            project: "vantage-point".to_string(),
            lane: vec![],
        };
        assert!(client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_different_project() {
        let client = make_client();
        let addr = Address::Project {
            actor: "agent".to_string(),
            world: None,
            project: "creo-memories".to_string(),
            lane: vec![],
        };
        assert!(!client.is_local(&addr));
    }

    #[test]
    fn test_is_local_with_federated_world_is_remote() {
        // v3.1: world: Some(_) は federated remote、 self world でも remote 扱い
        let client = make_client();
        let addr = Address::Project {
            actor: "agent".to_string(),
            world: Some("mako.chronista.club".to_string()),
            project: "vantage-point".to_string(),
            lane: vec!["lead".to_string()],
        };
        assert!(!client.is_local(&addr));
    }

    #[tokio::test]
    async fn test_lookup_local_address_returns_invalid() {
        let client = make_client();
        let addr = Address::Local {
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
        let addr = Address::Local {
            actor: "agent".to_string(),
        };
        assert!(RemoteRoutingClient::cache_key(&addr).is_none());
    }

    #[tokio::test]
    async fn test_cache_key_port_format() {
        let addr = Address::Port {
            actor: "agent".to_string(),
            port: 33003,
        };
        assert_eq!(
            RemoteRoutingClient::cache_key(&addr),
            Some("agent@p33003".to_string())
        );
    }

    #[tokio::test]
    async fn test_cache_key_project_format_no_lane() {
        let addr = Address::Project {
            actor: "agent".to_string(),
            world: None,
            project: "vantage-point".to_string(),
            lane: vec![],
        };
        assert_eq!(
            RemoteRoutingClient::cache_key(&addr),
            Some("agent@nvantage-point".to_string())
        );
    }

    #[tokio::test]
    async fn test_cache_key_project_with_single_lane() {
        // v3.1: location path 形式 cache key (= `<actor>@n<project>/<lane>`)
        let addr = Address::Project {
            actor: "pp".to_string(),
            world: None,
            project: "vp".to_string(),
            lane: vec!["lead".to_string()],
        };
        assert_eq!(
            RemoteRoutingClient::cache_key(&addr),
            Some("pp@nvp/lead".to_string())
        );
    }

    #[tokio::test]
    async fn test_cache_key_project_with_multilevel_lane() {
        // v3.1: multi-level lane segments も cache key に展開
        let addr = Address::Project {
            actor: "agent".to_string(),
            world: None,
            project: "vantage-point".to_string(),
            lane: vec!["worker".to_string(), "objrec".to_string()],
        };
        assert_eq!(
            RemoteRoutingClient::cache_key(&addr),
            Some("agent@nvantage-point/worker/objrec".to_string())
        );
    }

    #[tokio::test]
    async fn test_cache_key_project_with_world() {
        // v3.1: world prefix も cache key に展開、 federated remote 区別
        let addr = Address::Project {
            actor: "agent".to_string(),
            world: Some("mako.chronista.club".to_string()),
            project: "vantage-point".to_string(),
            lane: vec!["lead".to_string()],
        };
        assert_eq!(
            RemoteRoutingClient::cache_key(&addr),
            Some("agent@nmako.chronista.club/vantage-point/lead".to_string())
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
