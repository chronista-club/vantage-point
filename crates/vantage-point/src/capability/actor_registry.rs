//! ActorRegistry — VP-159 Stand / Service actor の supervisor 受け皿 (PR-4a)
//!
//! VP-159 spike v0.1 (creo-memories `mem_1CavCepJdf8XyQ82AAiSpv`) で確定した
//! supervisor 統一 path の第一歩 (= 受け皿)。 既存 actor の spawn loop を集約せず、
//! **metadata catalog として actor の存在を track する受け皿 struct** を新設。 actual な
//! spawn 統合 + caller 集約は PR-4b で実施 (= PR-1 / PR-2 / PR-3 同型 i 路線 minimal pattern)。
//!
//! ## i 路線 minimal の意義
//!
//! PR-4a は **caller 経路を開かない passive catalog**:
//! - 既存 Service trait sig は無変更 (= `actor_name` / `layer_scope` / `as_any` のまま)
//! - 既存 actor の spawn 経路 (= `NotificationActor::spawn` 等) は touchしない
//! - PR-4b で `Service` trait に `spawn_loop` method 追加 + caller を ActorRegistry 経由に
//!   migrate する path で activate
//!
//! 「passive marker minimal」 path で既存挙動への影響を厳密にゼロ化、 PR-4b 時の risk を
//! 局所化する設計。
//!
//! ## 役割
//!
//! - actor 名 / scope / kind の **metadata catalog** (= HashMap<String, ActorRegistryEntry>)
//! - layer_scope / kind による filter / lookup (= PR-4b supervisor が dispatch に使う)
//! - PR-4b で `task: Option<JoinHandle<()>>` を `Some(...)` で populate する経路
//!
//! ## 関連
//!
//! - parent epic: VP-156 (Mailbox routing 統一)、 VP-159 (H1 段)
//! - spike v0.1: `mem_1CavCepJdf8XyQ82AAiSpv`
//! - PR-1 受け皿 pattern 先例: `LaneStandHost` (PR-δ-1、 #288/VP-135)、 Stand/Service trait (PR-1、 #326)
//! - PR-2 同型: agent / protocol を Stand impl (= #327)
//! - PR-3 同型: notify / lane-spawn / hermit_purple を Service impl (= #329)
//! - PR-4b 想定: Service trait sig 拡張 (`spawn_loop`) + 既存 3 Service の migration + caller 集約

use std::collections::HashMap;

use tokio::task::JoinHandle;

use super::stand_service::{LayerScope, Service, Stand};

/// actor の種類 (= Stand か Service か)。
///
/// VP-159 PR-1 で定義した 2 trait に対応、 ActorRegistry が同 catalog で host する際の
/// discriminator として機能する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorKind {
    /// ECS entity bound actor (= agent / protocol / paisley_park 等)。 PR-2 で formalized。
    Stand,
    /// singleton infra actor (= notify / lane-spawn / hermit_purple 等)。 PR-3 で formalized。
    Service,
}

/// registry に登録される actor の entry。
///
/// PR-4a 段階では `task: None` (= metadata catalog only)、 PR-4b で各 actor の
/// `spawn_loop()` 経路から `Some(JoinHandle<()>)` で populate される設計。
pub struct ActorRegistryEntry {
    /// actor 名 (= mailbox address の actor 部分と一致、 例: `"notify"` / `"agent"`)
    pub name: String,
    /// actor の lifecycle / address scope (= LSCM World / Project / Lane)
    pub scope: LayerScope,
    /// actor の種類 (= Stand or Service)
    pub kind: ActorKind,
    /// background task の JoinHandle。
    ///
    /// PR-4a: 常に `None` (= passive catalog、 spawn 統合は PR-4b)
    /// PR-4b: `Some(JoinHandle<()>)` で各 Service の spawn_loop が attach される予定
    pub task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ActorRegistryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActorRegistryEntry")
            .field("name", &self.name)
            .field("scope", &self.scope)
            .field("kind", &self.kind)
            .field("task_attached", &self.task.is_some())
            .finish()
    }
}

/// Stand / Service actor の supervisor 受け皿 (PR-4a)。
///
/// PR-4a 段階では metadata catalog として機能、 既存 actor の caller は無変更で `register_*`
/// メソッドを optional に呼ぶ form。 PR-4b で caller (= `server.rs` / `world_capabilities.rs`)
/// を ActorRegistry 経由に集約する path を開く。
///
/// ## scope と kind の 2 軸 filter
///
/// supervisor (PR-4b 想定) は scope (= World / Project / Lane) と kind (= Stand / Service) の
/// 2 軸で actor を dispatch / filter する。 `list_by_scope` / `list_by_kind` で各 axis の
/// subset を取得可能。
#[derive(Default)]
pub struct ActorRegistry {
    entries: HashMap<String, ActorRegistryEntry>,
}

impl ActorRegistry {
    /// 空の registry を構築する。
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// `Service` を registry に register する (= metadata only、 PR-4a 段階では task=None)。
    ///
    /// 引数は `&S` で borrow、 actor instance は caller が保持する。 PR-4b で
    /// `register_service_with_task(&mut self, service: S, shutdown)` 等の owning method を
    /// 追加して spawn 統合する想定。
    ///
    /// 同 name の existing entry があれば上書き (= caller の責務で重複 register を回避)。
    pub fn register_service<S: Service>(&mut self, service: &S) {
        let name = service.actor_name().to_string();
        let entry = ActorRegistryEntry {
            name: name.clone(),
            scope: service.layer_scope(),
            kind: ActorKind::Service,
            task: None,
        };
        self.entries.insert(name, entry);
    }

    /// `Stand` を registry に register する (= metadata only)。
    pub fn register_stand<S: Stand>(&mut self, stand: &S) {
        let name = stand.actor_name().to_string();
        let entry = ActorRegistryEntry {
            name: name.clone(),
            scope: stand.layer_scope(),
            kind: ActorKind::Stand,
            task: None,
        };
        self.entries.insert(name, entry);
    }

    /// 全 entry の iter。
    pub fn entries(&self) -> impl Iterator<Item = &ActorRegistryEntry> {
        self.entries.values()
    }

    /// `LayerScope` で filter (= supervisor が scope 別に dispatch するための pattern)。
    pub fn list_by_scope(&self, scope: LayerScope) -> Vec<&ActorRegistryEntry> {
        self.entries.values().filter(|e| e.scope == scope).collect()
    }

    /// `ActorKind` で filter (= Stand or Service 別の subset 取得)。
    pub fn list_by_kind(&self, kind: ActorKind) -> Vec<&ActorRegistryEntry> {
        self.entries.values().filter(|e| e.kind == kind).collect()
    }

    /// 名前で entry lookup。
    pub fn get(&self, name: &str) -> Option<&ActorRegistryEntry> {
        self.entries.get(name)
    }

    /// entry 数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// empty check。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    /// test fixture: minimal `Stand` impl
    struct FixtureStand {
        name: &'static str,
        scope: LayerScope,
    }

    impl Stand for FixtureStand {
        fn actor_name(&self) -> &str {
            self.name
        }
        fn layer_scope(&self) -> LayerScope {
            self.scope
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// test fixture: minimal `Service` impl
    struct FixtureService {
        name: &'static str,
        scope: LayerScope,
    }

    impl Service for FixtureService {
        fn actor_name(&self) -> &str {
            self.name
        }
        fn layer_scope(&self) -> LayerScope {
            self.scope
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn registry_new_is_empty() {
        let r = ActorRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn registry_default_is_empty() {
        let r = ActorRegistry::default();
        assert!(r.is_empty());
    }

    #[test]
    fn register_service_adds_entry() {
        let mut r = ActorRegistry::new();
        let s = FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        };
        r.register_service(&s);
        assert_eq!(r.len(), 1);
        let entry = r.get("notify").expect("notify entry exists");
        assert_eq!(entry.name, "notify");
        assert_eq!(entry.scope, LayerScope::Project);
        assert_eq!(entry.kind, ActorKind::Service);
        assert!(entry.task.is_none(), "PR-4a では task は常に None");
    }

    #[test]
    fn register_stand_adds_entry() {
        let mut r = ActorRegistry::new();
        let s = FixtureStand {
            name: "agent",
            scope: LayerScope::Project,
        };
        r.register_stand(&s);
        assert_eq!(r.len(), 1);
        let entry = r.get("agent").expect("agent entry exists");
        assert_eq!(entry.kind, ActorKind::Stand);
    }

    #[test]
    fn register_same_name_overwrites() {
        // VP-159 PR-4a invariant: 同 name の re-register は上書き (caller の責務で重複回避)
        let mut r = ActorRegistry::new();
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        });
        // 同 name で異 scope を register
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::World, // 異 scope
        });
        assert_eq!(r.len(), 1, "同 name は 1 entry");
        assert_eq!(
            r.get("notify").unwrap().scope,
            LayerScope::World,
            "上書き済"
        );
    }

    #[test]
    fn list_by_scope_filters_correctly() {
        let mut r = ActorRegistry::new();
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        });
        r.register_service(&FixtureService {
            name: "lane-spawn",
            scope: LayerScope::Project,
        });
        r.register_service(&FixtureService {
            name: "hermit_purple",
            scope: LayerScope::World,
        });

        let project_entries = r.list_by_scope(LayerScope::Project);
        let world_entries = r.list_by_scope(LayerScope::World);
        let lane_entries = r.list_by_scope(LayerScope::Lane);
        assert_eq!(project_entries.len(), 2, "Project scope は 2 個");
        assert_eq!(world_entries.len(), 1, "World scope は 1 個");
        assert_eq!(lane_entries.len(), 0, "Lane scope は 0 個");
    }

    #[test]
    fn list_by_kind_filters_correctly() {
        let mut r = ActorRegistry::new();
        r.register_stand(&FixtureStand {
            name: "agent",
            scope: LayerScope::Project,
        });
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        });

        let stands = r.list_by_kind(ActorKind::Stand);
        let services = r.list_by_kind(ActorKind::Service);
        assert_eq!(stands.len(), 1);
        assert_eq!(services.len(), 1);
        assert_eq!(stands[0].name, "agent");
        assert_eq!(services[0].name, "notify");
    }

    #[test]
    fn coexist_5_actors_in_registry() {
        // VP-159 PR-4a invariant (= PR-2 / PR-3 invariant の延長): 5 actor (2 Stand + 3 Service)
        // が 1 registry に coexist できる事。 PR-4b で AgentCapability (Stand) + ProtocolCapability
        // (Stand) + NotificationActor (Service) + LaneSpawnActor (Service) + MidiCapability
        // (Service) を同 registry で host する path を fixture で代理検証。
        let mut r = ActorRegistry::new();
        r.register_stand(&FixtureStand {
            name: "agent",
            scope: LayerScope::Project,
        });
        r.register_stand(&FixtureStand {
            name: "protocol",
            scope: LayerScope::Project,
        });
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        });
        r.register_service(&FixtureService {
            name: "lane-spawn",
            scope: LayerScope::Project,
        });
        r.register_service(&FixtureService {
            name: "hermit_purple",
            scope: LayerScope::World,
        });

        assert_eq!(r.len(), 5);
        assert_eq!(r.list_by_kind(ActorKind::Stand).len(), 2);
        assert_eq!(r.list_by_kind(ActorKind::Service).len(), 3);
        assert_eq!(r.list_by_scope(LayerScope::Project).len(), 4);
        assert_eq!(r.list_by_scope(LayerScope::World).len(), 1);
        assert_eq!(r.list_by_scope(LayerScope::Lane).len(), 0);
    }

    #[test]
    fn actor_kind_variants_are_distinct() {
        // 2 variant が PartialEq で区別される事 (supervisor が kind で dispatch する基盤)
        assert_ne!(ActorKind::Stand, ActorKind::Service);
    }

    #[test]
    fn entries_iter_returns_all() {
        let mut r = ActorRegistry::new();
        r.register_stand(&FixtureStand {
            name: "agent",
            scope: LayerScope::Project,
        });
        r.register_service(&FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        });

        let all: Vec<&ActorRegistryEntry> = r.entries().collect();
        assert_eq!(all.len(), 2);
    }
}
