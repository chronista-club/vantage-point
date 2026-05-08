//! Registry — TheWorld 中央 actor 登録簿（Msgbox Phase 3）
//!
//! Cross-Process msgbox messaging のための **actor → port マッピング**。
//! Process が起動時に自身の actor 一覧を register、停止時に unregister。
//! 他 Process からの lookup で送信先 port を解決する。
//!
//! ## アーキテクチャ位置
//!
//! TheWorld (port 32000) のみが保持する。VP Process は HTTP API 経由で操作:
//! - `POST /api/world/msgbox/register` — Process 起動時
//! - `POST /api/world/msgbox/unregister` — Process 停止時
//! - `GET /api/world/msgbox/lookup` — メッセージ送信時の宛先解決
//!
//! ## Address Syntax (v3.1 federated、 VP-146)
//!
//! BNF (= [docs/spec/mailbox-address-v3.md](../../../../docs/spec/mailbox-address-v3.md)):
//!
//! ```text
//! address  = (actor "@")? location
//! actor    = [a-zA-Z0-9_-]+ | "*"   // 省略時 default = "agent"、 `*` は wildcard
//! location = (world "/")? project ("/" lane)* | port
//! world    = world-segment ("." world-segment)+   // dot 必須 (Phase 1 disambiguation rule A)
//! project  = [a-zA-Z0-9_-]+                       // dot 不可 (v1 dotted-project は廃止)
//! lane     = [a-zA-Z0-9_-]+
//! port     = [0-9]+ (= u16)
//! ```
//!
//! ### sample address
//!
//! - `agent` — Local form (self process inbox)
//! - `agent@33003` — Port form (internal optimization)
//! - `agent@vantage-point` — v1 forward-compat (lane なし、 caller 側で default lane "lead" injection)
//! - `vantage-point/lead` — actor 省略 (default = `agent`)、 lane = ["lead"]
//! - `notify@vantage-point/lead` — reserved actor "notify"
//! - `*@vantage-point/lead` — wildcard actor (project + lane broadcast)
//! - `vantage-point/worker/objrec` — multi-level lane = ["worker", "objrec"]
//! - `mako.chronista.club/vantage-point/lead` — world = "mako.chronista.club" (dot 含む)
//! - `macbook.local/vantage-point/lead` — world = "macbook.local" (mDNS、 Phase 3)
//! - `hermit_purple@world` — reserved project "world" (TheWorld daemon)
//!
//! ### disambiguation rule A (Phase 1)
//!
//! 3+ segment location で **先頭 segment が dot 含む** ならば world、
//! さもなくば 全 segment を project + lane 扱い。
//! single-word alias world (例: `mako`) は Phase 3 (mDNS) で `mako.local` に正規化、
//! Phase 4 (hub) で alias 直接 resolve。 Phase 1 では project 解釈 (= alias は使えない)。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Actor 登録エントリ
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActorEntry {
    /// Actor 名（例: "agent", "protocol", "notify", "mcp"）
    pub actor: String,
    /// Process が紐づく project 名（TheWorld の project registry と一致）
    pub project_name: String,
    /// Process の HTTP/Unison ポート
    pub port: u16,
    /// 登録時刻（Unix epoch ms）
    pub registered_at: u64,
}

impl ActorEntry {
    fn new(actor: impl Into<String>, project_name: impl Into<String>, port: u16) -> Self {
        Self {
            actor: actor.into(),
            project_name: project_name.into(),
            port,
            registered_at: now_ms(),
        }
    }
}

/// 登録簿のキー: (project_name, actor)
type RegistryKey = (String, String);

/// Registry — TheWorld の actor 登録簿
///
/// 構造: `(project_name, actor)` → `ActorEntry`
/// project + actor の組で一意。同じ actor 名でも異なる project に居られる。
#[derive(Debug, Default)]
pub struct Registry {
    entries: Arc<RwLock<HashMap<RegistryKey, ActorEntry>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Actor を register（同一 key の既存 entry は上書き）
    ///
    /// validate_actor / validate_project で検証する。不正な名前は AddressError。
    pub async fn register(
        &self,
        actor: impl Into<String>,
        project_name: impl Into<String>,
        port: u16,
    ) -> Result<(), AddressError> {
        let actor = actor.into();
        let project_name = project_name.into();
        validate_actor(&actor)?;
        validate_project(&project_name)?;

        let entry = ActorEntry::new(&actor, &project_name, port);
        self.entries
            .write()
            .await
            .insert((project_name, actor), entry);
        Ok(())
    }

    /// Actor を unregister（戻り値: 削除されたエントリ数 = 0 or 1）
    pub async fn unregister(&self, project_name: &str, actor: &str) -> usize {
        self.entries
            .write()
            .await
            .remove(&(project_name.to_string(), actor.to_string()))
            .map(|_| 1)
            .unwrap_or(0)
    }

    /// project_name から actor を lookup
    pub async fn lookup_by_project(&self, actor: &str, project_name: &str) -> Option<ActorEntry> {
        self.entries
            .read()
            .await
            .get(&(project_name.to_string(), actor.to_string()))
            .cloned()
    }

    /// port から actor を lookup（actor 名 + port 一致のエントリ）
    pub async fn lookup_by_port(&self, actor: &str, port: u16) -> Option<ActorEntry> {
        self.entries
            .read()
            .await
            .values()
            .find(|e| e.actor == actor && e.port == port)
            .cloned()
    }

    /// Process（port）配下の全 actor を一括 unregister（戻り値: 削除数）
    ///
    /// Process 停止時に TheWorld 側の reconciliation で呼ぶ。
    pub async fn unregister_process(&self, port: u16) -> usize {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|_, e| e.port != port);
        before - entries.len()
    }

    /// 全エントリを返す（debug / list API 用）
    pub async fn list(&self) -> Vec<ActorEntry> {
        self.entries.read().await.values().cloned().collect()
    }

    /// project_name 配下のエントリだけ返す
    pub async fn list_by_project(&self, project_name: &str) -> Vec<ActorEntry> {
        self.entries
            .read()
            .await
            .values()
            .filter(|e| e.project_name == project_name)
            .cloned()
            .collect()
    }

    /// 登録数
    pub async fn count(&self) -> usize {
        self.entries.read().await.len()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// Address 検証 (v3.1、 VP-146)
// =============================================================================

/// Address 検証エラー
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AddressError {
    #[error("address must not be empty")]
    EmptyAddress,
    #[error("actor name must not be empty")]
    EmptyActor,
    #[error("project name must not be empty")]
    EmptyProject,
    #[error("lane segment must not be empty")]
    EmptyLane,
    #[error("world segment must not be empty")]
    EmptyWorld,
    #[error("actor name contains invalid character (allowed: a-zA-Z0-9_- or `*`)")]
    InvalidActorChar,
    #[error(
        "project name contains invalid character (allowed: a-zA-Z0-9_-, dot reserved for world)"
    )]
    InvalidProjectChar,
    #[error("project name must not be all-numeric (conflicts with port format)")]
    NumericProject,
    #[error("lane segment contains invalid character (allowed: a-zA-Z0-9_-)")]
    InvalidLaneChar,
    #[error("world segment contains invalid character (allowed: a-zA-Z0-9_-)")]
    InvalidWorldChar,
    #[error("address must not contain multiple `@` separators")]
    MultipleAtSeparators,
}

/// Actor 名を検証
///
/// ルール: 非空、 `[a-zA-Z0-9_-]+` または wildcard `*`
pub fn validate_actor(actor: &str) -> Result<(), AddressError> {
    if actor.is_empty() {
        return Err(AddressError::EmptyActor);
    }
    // wildcard `*` は actor 専用 (v1 仕様継承、 spec doc Wildcard 拡張)
    if actor == "*" {
        return Ok(());
    }
    if !actor
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AddressError::InvalidActorChar);
    }
    Ok(())
}

/// Lane segment を検証
///
/// ルール: 非空、 `[a-zA-Z0-9_-]+`
pub fn validate_lane(lane: &str) -> Result<(), AddressError> {
    if lane.is_empty() {
        return Err(AddressError::EmptyLane);
    }
    if !lane
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AddressError::InvalidLaneChar);
    }
    Ok(())
}

/// World segment を検証 (= dot 区切り 1 単位)
///
/// ルール: 非空、 `[a-zA-Z0-9_-]+`
pub fn validate_world_segment(seg: &str) -> Result<(), AddressError> {
    if seg.is_empty() {
        return Err(AddressError::EmptyWorld);
    }
    if !seg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AddressError::InvalidWorldChar);
    }
    Ok(())
}

/// World 文字列全体を検証 (= dot 区切り 1+ segment)
pub fn validate_world(world: &str) -> Result<(), AddressError> {
    if world.is_empty() {
        return Err(AddressError::EmptyWorld);
    }
    for seg in world.split('.') {
        validate_world_segment(seg)?;
    }
    Ok(())
}

/// Project 名を検証
///
/// ルール: 非空、 `[a-zA-Z0-9_-]+`、 全数字禁止 (port との曖昧さ排除)。
/// v3.1 で dot は world segment 専用となり、 v1 dotted-project (例 `anycreative.tech`) は廃止。
pub fn validate_project(project: &str) -> Result<(), AddressError> {
    if project.is_empty() {
        return Err(AddressError::EmptyProject);
    }
    if !project
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AddressError::InvalidProjectChar);
    }
    if project.chars().all(|c| c.is_ascii_digit()) {
        return Err(AddressError::NumericProject);
    }
    Ok(())
}

/// 解決済み address — parser の出力 (v3.1)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// Local form: `agent` (= self process inbox、 lookup 不要)
    Local { actor: String },
    /// Port form: `agent@33003` (= internal optimization)
    Port { actor: String, port: u16 },
    /// Federated form: `[actor@](world/)?project(/lane)*`
    ///
    /// `world: None` = self world (= local TheWorld registry)、
    /// `lane` 空 vec は v1 forward-compat (caller 側で default "lead" injection)。
    Project {
        actor: String,
        world: Option<String>,
        project: String,
        lane: Vec<String>,
    },
}

impl Address {
    /// 含まれる actor 名を返す (log / debug 用)
    pub fn actor_or_unknown(&self) -> &str {
        match self {
            Address::Local { actor }
            | Address::Port { actor, .. }
            | Address::Project { actor, .. } => actor,
        }
    }
}

/// Address 文字列を parse して `Address` に解決 (v3.1 syntax)
///
/// 形式:
/// - `agent` → Local
/// - `agent@33003` → Port
/// - `agent@vantage-point` → Project (lane 空、 v1 forward-compat)
/// - `vantage-point/lead` → Project (actor default "agent"、 lane=["lead"])
/// - `vantage-point/worker/objrec` → Project (multi-level lane)
/// - `mako.chronista.club/vantage-point/lead` → Project (world dot 含む)
/// - `*@vantage-point/lead` → Project (wildcard actor)
///
/// disambiguation rule A: 3+ segment location の先頭 segment が dot 含む → world、
/// さもなくば全部 project + lane 扱い (= alias world は Phase 3+ で別途 resolve)。
pub fn parse_address(address: &str) -> Result<Address, AddressError> {
    if address.is_empty() {
        return Err(AddressError::EmptyAddress);
    }

    // `@` separator で actor / location 分離
    let at_count = address.chars().filter(|c| *c == '@').count();
    let (actor_explicit, actor, location) = match at_count {
        0 => {
            // `@` 不在 → actor 省略、 default = "agent"
            (false, "agent", address)
        }
        1 => {
            let (actor_part, loc) = address.split_once('@').unwrap();
            if actor_part.is_empty() {
                return Err(AddressError::EmptyActor);
            }
            (true, actor_part, loc)
        }
        _ => return Err(AddressError::MultipleAtSeparators),
    };

    validate_actor(actor)?;

    // location 解析
    if location.is_empty() {
        return Err(AddressError::EmptyProject);
    }

    // location が単独 u16 数値 → Port form
    if let Ok(port) = location.parse::<u16>() {
        return Ok(Address::Port {
            actor: actor.to_string(),
            port,
        });
    }

    // `@` 不在 + location が単一 segment (`/` 不在) → Local form
    // (= v1 既存 `agent` 単独入力の forward-compat、 self process inbox 解釈)
    // location 全体を actor として再検証 (= `pp.lead` 等の sub-suffix は reject)
    if !actor_explicit && !location.contains('/') {
        validate_actor(location)?;
        return Ok(Address::Local {
            actor: location.to_string(),
        });
    }

    // location を `/` で分解
    let segments: Vec<&str> = location.split('/').collect();

    // disambiguation rule A: 3+ segment、 先頭 dot 含む → world として認識
    let (world, project_idx) = if segments.len() >= 3 && segments[0].contains('.') {
        validate_world(segments[0])?;
        (Some(segments[0].to_string()), 1)
    } else {
        (None, 0)
    };

    let project = segments
        .get(project_idx)
        .copied()
        .ok_or(AddressError::EmptyProject)?;
    validate_project(project)?;

    let lane: Vec<String> = segments[project_idx + 1..]
        .iter()
        .map(|s| {
            validate_lane(s)?;
            Ok::<String, AddressError>((*s).to_string())
        })
        .collect::<Result<_, _>>()?;

    Ok(Address::Project {
        actor: actor.to_string(),
        world,
        project: project.to_string(),
        lane,
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_lookup_by_project() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();

        let entry = registry
            .lookup_by_project("agent", "vantage-point")
            .await
            .unwrap();
        assert_eq!(entry.actor, "agent");
        assert_eq!(entry.project_name, "vantage-point");
        assert_eq!(entry.port, 33003);
    }

    #[tokio::test]
    async fn test_register_and_lookup_by_port() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("protocol", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("agent", "creo-memories", 33000)
            .await
            .unwrap();

        let entry = registry.lookup_by_port("agent", 33003).await.unwrap();
        assert_eq!(entry.project_name, "vantage-point");

        let entry = registry.lookup_by_port("agent", 33000).await.unwrap();
        assert_eq!(entry.project_name, "creo-memories");

        // 存在しない port
        assert!(registry.lookup_by_port("agent", 65000).await.is_none());
    }

    #[tokio::test]
    async fn test_unregister_single() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();
        assert_eq!(registry.count().await, 1);

        let removed = registry.unregister("vantage-point", "agent").await;
        assert_eq!(removed, 1);
        assert_eq!(registry.count().await, 0);

        // 重複 unregister はゼロを返す（panic しない）
        let removed = registry.unregister("vantage-point", "agent").await;
        assert_eq!(removed, 0);
    }

    #[tokio::test]
    async fn test_unregister_process_removes_all_actors_at_port() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("protocol", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("notify", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("agent", "creo-memories", 33000)
            .await
            .unwrap();

        let removed = registry.unregister_process(33003).await;
        assert_eq!(removed, 3);
        assert_eq!(registry.count().await, 1);

        // creo-memories 側は無傷
        let entry = registry
            .lookup_by_project("agent", "creo-memories")
            .await
            .unwrap();
        assert_eq!(entry.port, 33000);
    }

    #[tokio::test]
    async fn test_register_overwrites_existing() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("agent", "vantage-point", 33004)
            .await
            .unwrap(); // port 変更

        assert_eq!(registry.count().await, 1);
        let entry = registry
            .lookup_by_project("agent", "vantage-point")
            .await
            .unwrap();
        assert_eq!(entry.port, 33004);
    }

    #[tokio::test]
    async fn test_list_all() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("protocol", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("agent", "creo-memories", 33000)
            .await
            .unwrap();

        let all = registry.list().await;
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_list_by_project() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("protocol", "vantage-point", 33003)
            .await
            .unwrap();
        registry
            .register("agent", "creo-memories", 33000)
            .await
            .unwrap();

        let vp_actors = registry.list_by_project("vantage-point").await;
        assert_eq!(vp_actors.len(), 2);
        assert!(vp_actors.iter().any(|e| e.actor == "agent"));
        assert!(vp_actors.iter().any(|e| e.actor == "protocol"));

        let creo_actors = registry.list_by_project("creo-memories").await;
        assert_eq!(creo_actors.len(), 1);
    }

    #[tokio::test]
    async fn test_lookup_nonexistent_returns_none() {
        let registry = Registry::new();
        assert!(
            registry
                .lookup_by_project("agent", "vantage-point")
                .await
                .is_none()
        );
        assert!(registry.lookup_by_port("agent", 33003).await.is_none());
    }

    #[tokio::test]
    async fn test_actor_entry_serialization() {
        let registry = Registry::new();
        registry
            .register("agent", "vantage-point", 33003)
            .await
            .unwrap();
        let entry = registry
            .lookup_by_project("agent", "vantage-point")
            .await
            .unwrap();

        let json = serde_json::to_string(&entry).unwrap();
        let restored: ActorEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, entry);
    }

    #[tokio::test]
    async fn test_concurrent_register() {
        let registry = Arc::new(Registry::new());

        let mut handles = Vec::new();
        for i in 0..10 {
            let r = registry.clone();
            handles.push(tokio::spawn(async move {
                r.register(format!("actor-{}", i), "vantage-point", 33003 + i)
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(registry.count().await, 10);
    }

    // =========================================================================
    // v3.1 Address validation / parser tests (VP-146)
    // =========================================================================

    // -------------------------------------------------------------------------
    // validate_actor / validate_project / validate_lane / validate_world_segment
    // -------------------------------------------------------------------------

    #[test]
    fn test_validate_actor_ok() {
        assert!(validate_actor("agent").is_ok());
        assert!(validate_actor("agent-1").is_ok());
        assert!(validate_actor("my_actor").is_ok());
        assert!(validate_actor("ABC123_xyz").is_ok());
        // wildcard `*` は actor 専用 (v3.1 spec)
        assert!(validate_actor("*").is_ok());
    }

    #[test]
    fn test_validate_actor_errors() {
        assert_eq!(validate_actor(""), Err(AddressError::EmptyActor));
        assert_eq!(
            validate_actor("agent@1"),
            Err(AddressError::InvalidActorChar)
        );
        assert_eq!(validate_actor("a.b"), Err(AddressError::InvalidActorChar));
        assert_eq!(validate_actor("a/b"), Err(AddressError::InvalidActorChar));
        assert_eq!(
            validate_actor("日本語"),
            Err(AddressError::InvalidActorChar)
        );
        // wildcard 以外の `*` 含む文字列は invalid
        assert_eq!(validate_actor("a*"), Err(AddressError::InvalidActorChar));
    }

    #[test]
    fn test_validate_project_ok() {
        assert!(validate_project("vantage-point").is_ok());
        assert!(validate_project("creo-memories").is_ok());
        assert!(validate_project("my_project_123").is_ok());
        // reserved name `world` も charset 上は valid (= TheWorld system project)
        assert!(validate_project("world").is_ok());
    }

    #[test]
    fn test_validate_project_errors() {
        assert_eq!(validate_project(""), Err(AddressError::EmptyProject));
        assert_eq!(validate_project("33003"), Err(AddressError::NumericProject));
        assert_eq!(validate_project("0"), Err(AddressError::NumericProject));
        // dot は v3.1 で world segment 専用 (= dotted-project 廃止)
        assert_eq!(
            validate_project("anycreative.tech"),
            Err(AddressError::InvalidProjectChar)
        );
        assert_eq!(
            validate_project("project@name"),
            Err(AddressError::InvalidProjectChar)
        );
        assert_eq!(
            validate_project("project name"),
            Err(AddressError::InvalidProjectChar)
        );
    }

    #[test]
    fn test_validate_lane_directly() {
        assert_eq!(validate_lane("lead"), Ok(()));
        assert_eq!(validate_lane("sub1"), Ok(()));
        assert_eq!(validate_lane("worker_2"), Ok(()));
        assert_eq!(validate_lane("a-b"), Ok(()));

        assert_eq!(validate_lane(""), Err(AddressError::EmptyLane));
        assert_eq!(validate_lane("a.b"), Err(AddressError::InvalidLaneChar));
        assert_eq!(validate_lane("a@b"), Err(AddressError::InvalidLaneChar));
        assert_eq!(validate_lane("a/b"), Err(AddressError::InvalidLaneChar));
    }

    #[test]
    fn test_validate_world_segment_directly() {
        assert_eq!(validate_world_segment("mako"), Ok(()));
        assert_eq!(validate_world_segment("chronista-club"), Ok(()));
        assert_eq!(validate_world_segment("local"), Ok(()));

        assert_eq!(validate_world_segment(""), Err(AddressError::EmptyWorld));
        assert_eq!(
            validate_world_segment("a.b"),
            Err(AddressError::InvalidWorldChar)
        );
        assert_eq!(
            validate_world_segment("a/b"),
            Err(AddressError::InvalidWorldChar)
        );
    }

    #[test]
    fn test_validate_world_full_string() {
        // dot 区切り 1+ segment
        assert_eq!(validate_world("mako.chronista.club"), Ok(()));
        assert_eq!(validate_world("macbook.local"), Ok(()));
        assert_eq!(validate_world("single"), Ok(()));

        // 空文字列 / 連続 dot / 末尾 dot は invalid
        assert_eq!(validate_world(""), Err(AddressError::EmptyWorld));
        assert_eq!(validate_world("a..b"), Err(AddressError::EmptyWorld));
        assert_eq!(validate_world("a."), Err(AddressError::EmptyWorld));
    }

    // -------------------------------------------------------------------------
    // parse_address: v1 forward-compat (Local / Port / Project no lane)
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_local_form() {
        assert_eq!(
            parse_address("agent").unwrap(),
            Address::Local {
                actor: "agent".to_string(),
            }
        );
        // `agent` 以外でも actor として valid なら Local 解釈 (v1 forward-compat)
        assert_eq!(
            parse_address("notify").unwrap(),
            Address::Local {
                actor: "notify".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_port_form() {
        assert_eq!(
            parse_address("agent@33003").unwrap(),
            Address::Port {
                actor: "agent".to_string(),
                port: 33003,
            }
        );
        assert_eq!(
            parse_address("mcp@1").unwrap(),
            Address::Port {
                actor: "mcp".to_string(),
                port: 1,
            }
        );
    }

    #[test]
    fn test_parse_v1_project_no_lane() {
        // v1 forward-compat: `agent@vantage-point` (lane なし)
        assert_eq!(
            parse_address("agent@vantage-point").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: None,
                project: "vantage-point".to_string(),
                lane: vec![],
            }
        );
        assert_eq!(
            parse_address("mcp@creo-memories").unwrap(),
            Address::Project {
                actor: "mcp".to_string(),
                world: None,
                project: "creo-memories".to_string(),
                lane: vec![],
            }
        );
    }

    #[test]
    fn test_parse_v1_wildcard_no_lane() {
        // v1 forward-compat: `*@vantage-point` (project broadcast)
        assert_eq!(
            parse_address("*@vantage-point").unwrap(),
            Address::Project {
                actor: "*".to_string(),
                world: None,
                project: "vantage-point".to_string(),
                lane: vec![],
            }
        );
    }

    #[test]
    fn test_parse_v1_reserved_project_world() {
        // PR-α-3 で確立した World scope HACK `hp@world` 等は v3.1 でも valid
        assert_eq!(
            parse_address("hermit_purple@world").unwrap(),
            Address::Project {
                actor: "hermit_purple".to_string(),
                world: None,
                project: "world".to_string(),
                lane: vec![],
            }
        );
    }

    // -------------------------------------------------------------------------
    // parse_address: v3.1 新文法 (lane / wildcard / world segment)
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_v31_actor_omitted_default_agent() {
        // `vantage-point/lead` → actor 省略で default = "agent"、 lane = ["lead"]
        assert_eq!(
            parse_address("vantage-point/lead").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: None,
                project: "vantage-point".to_string(),
                lane: vec!["lead".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_v31_explicit_actor_with_lane() {
        // `agent@vantage-point/lead` → actor 明示、 lane = ["lead"]
        assert_eq!(
            parse_address("agent@vantage-point/lead").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: None,
                project: "vantage-point".to_string(),
                lane: vec!["lead".to_string()],
            }
        );
        // `notify@vantage-point/lead` → reserved actor "notify"
        assert_eq!(
            parse_address("notify@vantage-point/lead").unwrap(),
            Address::Project {
                actor: "notify".to_string(),
                world: None,
                project: "vantage-point".to_string(),
                lane: vec!["lead".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_v31_wildcard_with_lane() {
        // `*@vantage-point/lead` → wildcard actor + lane broadcast
        assert_eq!(
            parse_address("*@vantage-point/lead").unwrap(),
            Address::Project {
                actor: "*".to_string(),
                world: None,
                project: "vantage-point".to_string(),
                lane: vec!["lead".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_v31_multilevel_lane() {
        // `vantage-point/worker/objrec` → multi-level lane (world: None、 dot 不在)
        assert_eq!(
            parse_address("vantage-point/worker/objrec").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: None,
                project: "vantage-point".to_string(),
                lane: vec!["worker".to_string(), "objrec".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_v31_world_with_dots() {
        // `mako.chronista.club/vantage-point/lead` → world dot 含む = host 認識
        assert_eq!(
            parse_address("mako.chronista.club/vantage-point/lead").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: Some("mako.chronista.club".to_string()),
                project: "vantage-point".to_string(),
                lane: vec!["lead".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_v31_world_dot_local() {
        // `macbook.local/vantage-point/lead` → world = "macbook.local" (mDNS)
        assert_eq!(
            parse_address("macbook.local/vantage-point/lead").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: Some("macbook.local".to_string()),
                project: "vantage-point".to_string(),
                lane: vec!["lead".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_v31_explicit_actor_with_world() {
        // `agent@mako.chronista.club/vantage-point/lead` → 全 field 明示
        assert_eq!(
            parse_address("agent@mako.chronista.club/vantage-point/lead").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: Some("mako.chronista.club".to_string()),
                project: "vantage-point".to_string(),
                lane: vec!["lead".to_string()],
            }
        );
    }

    #[test]
    fn test_parse_v31_alias_world_treated_as_project_phase1() {
        // disambiguation rule A: `mako/vantage-point/lead` (3 seg、 dot 不在) → project=mako, lane=[vantage-point, lead]
        // alias-only world (dot 不在) は Phase 1 では project 解釈、 Phase 3+ で `mako.local` 等に正規化
        assert_eq!(
            parse_address("mako/vantage-point/lead").unwrap(),
            Address::Project {
                actor: "agent".to_string(),
                world: None,
                project: "mako".to_string(),
                lane: vec!["vantage-point".to_string(), "lead".to_string()],
            }
        );
    }

    // -------------------------------------------------------------------------
    // parse_address: PR-β-0 sub-suffix grammar の廃止 (VP-146 で reject)
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_v31_subsuffix_rejected_with_at() {
        // `pp.lead@vp` → actor 部に `.` = invalid char (PR-β-0 廃止、 v3.1 では `pp@vp/lead` を使う)
        assert_eq!(
            parse_address("pp.lead@vp"),
            Err(AddressError::InvalidActorChar)
        );
        assert_eq!(
            parse_address("echoes.sub1@vp"),
            Err(AddressError::InvalidActorChar)
        );
    }

    #[test]
    fn test_parse_v31_subsuffix_rejected_local_form() {
        // `pp.lead` (no `@`) → Local form 解釈 + actor 名に `.` 含む = invalid
        assert_eq!(
            parse_address("pp.lead"),
            Err(AddressError::InvalidActorChar)
        );
    }

    #[test]
    fn test_parse_v31_subsuffix_rejected_port_form() {
        // `pp.lead@33003` → actor 部に `.` = invalid (PR-β-0 廃止)
        assert_eq!(
            parse_address("pp.lead@33003"),
            Err(AddressError::InvalidActorChar)
        );
    }

    // -------------------------------------------------------------------------
    // parse_address: invalid input (negative cases)
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_empty_address() {
        assert_eq!(parse_address(""), Err(AddressError::EmptyAddress));
    }

    #[test]
    fn test_parse_empty_actor_after_at() {
        assert_eq!(
            parse_address("@vantage-point"),
            Err(AddressError::EmptyActor)
        );
        assert_eq!(parse_address("@33003"), Err(AddressError::EmptyActor));
    }

    #[test]
    fn test_parse_empty_project_after_at() {
        // `agent@` → location 不在
        assert_eq!(parse_address("agent@"), Err(AddressError::EmptyProject));
    }

    #[test]
    fn test_parse_trailing_slash_empty_lane() {
        // `agent@vantage-point/` → lane 部分が空
        assert_eq!(
            parse_address("agent@vantage-point/"),
            Err(AddressError::EmptyLane)
        );
    }

    #[test]
    fn test_parse_double_slash_empty_lane() {
        // `agent@vantage-point//objrec` → 中間 lane 空
        assert_eq!(
            parse_address("agent@vantage-point//objrec"),
            Err(AddressError::EmptyLane)
        );
    }

    #[test]
    fn test_parse_overflow_port_falls_to_numeric_project() {
        // 65536 は u16 範囲外 → project 解釈 + 全数字 → NumericProject
        assert_eq!(
            parse_address("agent@65536"),
            Err(AddressError::NumericProject)
        );
    }

    #[test]
    fn test_parse_multiple_at_separators_rejected() {
        // `agent@@vantage-point` → `@` が 2 個 = invalid
        assert_eq!(
            parse_address("agent@@vantage-point"),
            Err(AddressError::MultipleAtSeparators)
        );
        assert_eq!(
            parse_address("a@b@c"),
            Err(AddressError::MultipleAtSeparators)
        );
    }

    #[test]
    fn test_parse_invalid_world_segment() {
        // world segment に invalid char (`.` 連続 = empty segment)
        assert_eq!(
            parse_address("a..b/vantage-point/lead"),
            Err(AddressError::EmptyWorld)
        );
    }

    // -------------------------------------------------------------------------
    // Registry register input validation (v3.1)
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_register_validates_inputs() {
        let registry = Registry::new();

        // 不正 actor
        assert_eq!(
            registry.register("", "vantage-point", 33003).await,
            Err(AddressError::EmptyActor)
        );
        // 不正 project (全数字)
        assert_eq!(
            registry.register("agent", "33003", 33003).await,
            Err(AddressError::NumericProject)
        );
        // dot 含む project は v3.1 で invalid
        assert_eq!(
            registry.register("agent", "anycreative.tech", 33003).await,
            Err(AddressError::InvalidProjectChar)
        );
        // 全て invalid な場合 actor が先に hit
        assert_eq!(
            registry.register("a.b", "vantage-point", 33003).await,
            Err(AddressError::InvalidActorChar)
        );
        assert_eq!(registry.count().await, 0);
    }
}
