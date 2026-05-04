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
//! ## Actor Address 形式
//!
//! **正規形は `{actor}@{project-name}`**。port 形式は実装詳細・最適化キャッシュ用途。
//!
//! - `{actor}@{project-name}` — **推奨**。Process 再起動 / cross-machine に強い
//! - `{actor}@{port}` — 内部最適化用（registry skip で直接 Unison）。**外向き API では非推奨**
//! - `{actor}` — 送信元と同一 Process（後方互換）
//!
//! `from` フィールドの正規化は常に project-name 形式を使う（port 形式は受信側で
//! リプライ時に port 変動で死ぬため）。

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
// Address 検証（Phase 3 仕様）
// =============================================================================

/// Address 検証エラー
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AddressError {
    #[error("actor name must not be empty")]
    EmptyActor,
    #[error("project name must not be empty")]
    EmptyProject,
    #[error("actor name contains invalid character (allowed: a-zA-Z0-9_-)")]
    InvalidActorChar,
    #[error("project name contains invalid character (allowed: a-zA-Z0-9_.-)")]
    InvalidProjectChar,
    #[error("project name must not be all-numeric (conflicts with port format)")]
    NumericProject,
    /// PR-β-0 (VP-117): Lane sub-suffix grammar 拡張で導入。
    /// `pp.lead@vp` 形式で lane 部分が空 (例: `pp.@vp`)。
    #[error("lane sub-suffix must not be empty")]
    EmptyLane,
    /// `pp..lead@vp` 等の lane 部分に invalid char。
    #[error("lane sub-suffix contains invalid character (allowed: a-zA-Z0-9_-)")]
    InvalidLaneChar,
    /// `pp.lead` (no `@`) のように Local form で lane を指定。
    /// Local form は同一 Process 内 mailbox routing で lane 概念がない。
    #[error("lane sub-suffix not allowed in local address (use `actor.lane@project`)")]
    LaneNotAllowedInLocal,
    /// `pp.lead@33003` のように Port form で lane を指定。
    /// Port form は port 直指定で lane 概念がない (lane 付きは project 形式で表現)。
    #[error("lane sub-suffix not allowed in port address (use `actor.lane@project`)")]
    LaneNotAllowedInPort,
}

/// Actor 名を検証
///
/// ルール: 非空、`[a-zA-Z0-9_-]+`
pub fn validate_actor(actor: &str) -> Result<(), AddressError> {
    if actor.is_empty() {
        return Err(AddressError::EmptyActor);
    }
    if !actor
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AddressError::InvalidActorChar);
    }
    Ok(())
}

/// Lane sub-suffix を検証 (PR-β-0 / VP-117)
///
/// ルール: 非空、`[a-zA-Z0-9_-]+` (actor と同 charset)。
/// `actor.lane@project` 形式で `lane` 部分の検証に使う。
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

/// Project 名を検証
///
/// ルール: 非空、`[a-zA-Z0-9_.-]+`、全数字禁止（port との曖昧さ排除）
pub fn validate_project(project: &str) -> Result<(), AddressError> {
    if project.is_empty() {
        return Err(AddressError::EmptyProject);
    }
    if !project
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(AddressError::InvalidProjectChar);
    }
    // 全数字は port と区別不能になるため禁止
    if project.chars().all(|c| c.is_ascii_digit()) {
        return Err(AddressError::NumericProject);
    }
    Ok(())
}

/// 解決済み address — parser の出力
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAddress {
    /// ローカル box（送信元と同 Process）
    Local { actor: String },
    /// Port 直指定
    Port { actor: String, port: u16 },
    /// Project name lookup 必要
    ///
    /// PR-β-0 (VP-117): `lane` field を追加。 `pp.lead@vp` のような
    /// sub-suffix 付き wire format を parse すると `lane: Some("lead")`、
    /// 旧来の `pp@vp` 形式は `lane: None` で後方互換。
    /// doc 13 §3 (LSCM application doc) 参照。
    Project {
        actor: String,
        lane: Option<String>,
        project: String,
    },
}

impl ResolvedAddress {
    /// 含まれる actor 名を返す（log 等で使う、debug 用）
    pub fn actor_or_unknown(&self) -> &str {
        match self {
            ResolvedAddress::Local { actor }
            | ResolvedAddress::Port { actor, .. }
            | ResolvedAddress::Project { actor, .. } => actor,
        }
    }
}

/// Address 文字列を parse して `ResolvedAddress` に解決
///
/// 形式:
/// - `agent` → Local
/// - `agent@33003` → Port (suffix が u16 数値)
/// - `agent@vantage-point` → Project (suffix が文字列、 lane = None)
/// - `agent.lane@vantage-point` → Project (sub-suffix 付き、 lane = Some(...))
///
/// PR-β-0 (VP-117): `actor.lane@project` sub-suffix grammar を追加。
/// `actor.lane` 形式は **Project 形式専用**、 Local (`agent.lane`) や Port
/// (`agent.lane@33003`) では reject (lane 概念がない address kind のため)。
/// doc 12 §5 + doc 13 §3 (LSCM application doc) 参照。
pub fn parse_address(address: &str) -> Result<ResolvedAddress, AddressError> {
    match address.split_once('@') {
        None => {
            // Local form。 sub-suffix 付き (`pp.lead`) は意味がないため reject。
            if address.contains('.') {
                // actor 名に `.` が含まれる = sub-suffix 形式の意図 → Local では不可
                return Err(AddressError::LaneNotAllowedInLocal);
            }
            validate_actor(address)?;
            Ok(ResolvedAddress::Local {
                actor: address.to_string(),
            })
        }
        Some((actor_part, locator)) => {
            // sub-suffix grammar: `actor.lane` を split
            let (actor, lane) = match actor_part.split_once('.') {
                Some((a, l)) => (a, Some(l)),
                None => (actor_part, None),
            };

            validate_actor(actor)?;
            if let Some(l) = lane {
                validate_lane(l)?;
            }

            // locator が u16 数値なら port、それ以外は project
            if let Ok(port) = locator.parse::<u16>() {
                if lane.is_some() {
                    return Err(AddressError::LaneNotAllowedInPort);
                }
                Ok(ResolvedAddress::Port {
                    actor: actor.to_string(),
                    port,
                })
            } else {
                validate_project(locator)?;
                Ok(ResolvedAddress::Project {
                    actor: actor.to_string(),
                    lane: lane.map(|s| s.to_string()),
                    project: locator.to_string(),
                })
            }
        }
    }
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
    // Address validation / parser tests
    // =========================================================================

    #[test]
    fn test_validate_actor_ok() {
        assert!(validate_actor("agent").is_ok());
        assert!(validate_actor("agent-1").is_ok());
        assert!(validate_actor("my_actor").is_ok());
        assert!(validate_actor("ABC123_xyz").is_ok());
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
    }

    #[test]
    fn test_validate_project_ok() {
        assert!(validate_project("vantage-point").is_ok());
        assert!(validate_project("creo-memories").is_ok());
        assert!(validate_project("anycreative.tech").is_ok());
        assert!(validate_project("my_project_123").is_ok());
    }

    #[test]
    fn test_validate_project_errors() {
        assert_eq!(validate_project(""), Err(AddressError::EmptyProject));
        assert_eq!(validate_project("33003"), Err(AddressError::NumericProject));
        assert_eq!(validate_project("0"), Err(AddressError::NumericProject));
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
    fn test_parse_address_local() {
        assert_eq!(
            parse_address("agent").unwrap(),
            ResolvedAddress::Local {
                actor: "agent".to_string()
            }
        );
    }

    #[test]
    fn test_parse_address_port() {
        assert_eq!(
            parse_address("agent@33003").unwrap(),
            ResolvedAddress::Port {
                actor: "agent".to_string(),
                port: 33003,
            }
        );
        assert_eq!(
            parse_address("mcp@1").unwrap(),
            ResolvedAddress::Port {
                actor: "mcp".to_string(),
                port: 1,
            }
        );
    }

    #[test]
    fn test_parse_address_project() {
        assert_eq!(
            parse_address("agent@vantage-point").unwrap(),
            ResolvedAddress::Project {
                actor: "agent".to_string(),
                lane: None,
                project: "vantage-point".to_string(),
            }
        );
        assert_eq!(
            parse_address("mcp@creo-memories").unwrap(),
            ResolvedAddress::Project {
                actor: "mcp".to_string(),
                lane: None,
                project: "creo-memories".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_address_overflow_port_falls_to_project() {
        // 65536 は u16 範囲外 → project 名として扱う...が "65536" は全数字なので NumericProject
        assert_eq!(
            parse_address("agent@65536"),
            Err(AddressError::NumericProject)
        );
    }

    #[test]
    fn test_parse_address_invalid_actor() {
        assert_eq!(parse_address("@33003"), Err(AddressError::EmptyActor));
        assert_eq!(parse_address(""), Err(AddressError::EmptyActor));
    }

    #[test]
    fn test_parse_address_with_dot_in_project() {
        // anycreative.tech のような TLD 風 project 名
        assert_eq!(
            parse_address("agent@anycreative.tech").unwrap(),
            ResolvedAddress::Project {
                actor: "agent".to_string(),
                lane: None,
                project: "anycreative.tech".to_string(),
            }
        );
    }

    // =============================================================================
    // PR-β-0 (VP-117): sub-suffix grammar `actor.lane@project` の test 群
    // =============================================================================

    #[test]
    fn test_parse_address_with_lane_sub_suffix() {
        // 正常系: pp.lead@vp / hd.sub1@vp
        assert_eq!(
            parse_address("pp.lead@vp").unwrap(),
            ResolvedAddress::Project {
                actor: "pp".to_string(),
                lane: Some("lead".to_string()),
                project: "vp".to_string(),
            }
        );
        assert_eq!(
            parse_address("hd.sub1@vp").unwrap(),
            ResolvedAddress::Project {
                actor: "hd".to_string(),
                lane: Some("sub1".to_string()),
                project: "vp".to_string(),
            }
        );
        // lane 名に `_` `-` 数字許容
        assert_eq!(
            parse_address("pp.worker_2@creo-memories").unwrap(),
            ResolvedAddress::Project {
                actor: "pp".to_string(),
                lane: Some("worker_2".to_string()),
                project: "creo-memories".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_address_lane_in_local_form_rejected() {
        // Local form (no `@`) で lane 指定は不可 (lane 概念なし)
        assert_eq!(
            parse_address("pp.lead"),
            Err(AddressError::LaneNotAllowedInLocal)
        );
    }

    #[test]
    fn test_parse_address_lane_in_port_form_rejected() {
        // Port form で lane 指定は不可 (lane 概念なし、 lane 付きは project 形式で表現)
        assert_eq!(
            parse_address("pp.lead@33003"),
            Err(AddressError::LaneNotAllowedInPort)
        );
    }

    #[test]
    fn test_parse_address_empty_lane_rejected() {
        // `pp.@vp` lane 部分が空
        assert_eq!(parse_address("pp.@vp"), Err(AddressError::EmptyLane));
    }

    #[test]
    fn test_parse_address_invalid_lane_char_rejected() {
        // `pp..lead@vp` 連続 dot → split_once で先頭 dot が空 lane を生成 → EmptyLane
        // (split_once は最初の `.` で分割するので、 `pp..lead` → ("pp", ".lead") となり
        //  lane = ".lead" が validate_lane で reject される)
        assert_eq!(
            parse_address("pp..lead@vp"),
            Err(AddressError::InvalidLaneChar)
        );
        // lane に invalid char (`.` `/` `@`)
        assert_eq!(
            parse_address("pp.lead/x@vp"),
            Err(AddressError::InvalidLaneChar)
        );
    }

    #[test]
    fn test_parse_address_back_compat_no_lane() {
        // 後方互換: `pp@vp` → lane = None で既存挙動維持
        assert_eq!(
            parse_address("pp@vp").unwrap(),
            ResolvedAddress::Project {
                actor: "pp".to_string(),
                lane: None,
                project: "vp".to_string(),
            }
        );
        // PR-α-3 で確立した World scope HACK `hp@world`
        assert_eq!(
            parse_address("hp@world").unwrap(),
            ResolvedAddress::Project {
                actor: "hp".to_string(),
                lane: None,
                project: "world".to_string(),
            }
        );
        // hermit_purple@world (PR-α-3 mailbox register と同形)
        assert_eq!(
            parse_address("hermit_purple@world").unwrap(),
            ResolvedAddress::Project {
                actor: "hermit_purple".to_string(),
                lane: None,
                project: "world".to_string(),
            }
        );
    }

    #[test]
    fn test_validate_lane_directly() {
        // lane validation の直接 test
        assert_eq!(super::validate_lane("lead"), Ok(()));
        assert_eq!(super::validate_lane("sub1"), Ok(()));
        assert_eq!(super::validate_lane("worker_2"), Ok(()));
        assert_eq!(super::validate_lane("a-b"), Ok(()));

        assert_eq!(super::validate_lane(""), Err(AddressError::EmptyLane));
        assert_eq!(
            super::validate_lane("a.b"),
            Err(AddressError::InvalidLaneChar)
        );
        assert_eq!(
            super::validate_lane("a@b"),
            Err(AddressError::InvalidLaneChar)
        );
        assert_eq!(
            super::validate_lane("a/b"),
            Err(AddressError::InvalidLaneChar)
        );
    }

    #[tokio::test]
    async fn test_register_validates_inputs() {
        let registry = Registry::new();

        // 不正 actor
        assert_eq!(
            registry.register("", "vantage-point", 33003).await,
            Err(AddressError::EmptyActor)
        );
        // 不正 project
        assert_eq!(
            registry.register("agent", "33003", 33003).await,
            Err(AddressError::NumericProject)
        );
        // 全て invalid な場合 actor が先に hit
        assert_eq!(
            registry.register("a.b", "vantage-point", 33003).await,
            Err(AddressError::InvalidActorChar)
        );
        assert_eq!(registry.count().await, 0);
    }
}
