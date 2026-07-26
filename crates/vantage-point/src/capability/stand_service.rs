//! ECS entity bound actor 用 `Agent` trait と singleton infra 用 `Service` trait の
//! 受け皿 (VP-159 / VP-156 epic H1 段、 PR-1)。
//!
//! VP-24 (Mailbox core) で「Agent に component bolt-on で msgbox 使える」 が original 設計
//! 意図だったが、 後付けで infra actor (`mcp` / `notify` / `lane-spawn` / `repo-bootstrap`) が
//! 混入し ECS 純度が崩れた。 VP-157 (PR #325) で `mcp` box 廃止 + agent observer 化が
//! 第一歩、 本 PR で残 actor を **Agent** (= ECS entity bound) と **Service** (= singleton
//! infra) の 2 trait に分離する受け皿を新設する。
//!
//! design-decision: creo-memories `mem_1CatjVq5NUsjn1EHjRcaPG` §E
//! roadmap memory: creo-memories `mem_1CatuBTFNcEHgC3nfJJMke`
//!
//! ## VP-159 PR roadmap (sub-PR 5 段)
//!
//! | sub-PR | scope | status |
//! |--------|-------|--------|
//! | PR-1 | `Agent` / `Service` trait + `LayerScope` enum 受け皿 + `LaneStand` → `LaneStandHost` rename | 本 PR |
//! | PR-2 | Agent migrate (`agent` + `protocol`、 observer/consumer pattern 形式化) | 未着手 |
//! | PR-3 | Service migrate (`notify` + `lane-spawn` + `repo-bootstrap` + `devices`) | 未着手 |
//! | PR-4 | supervisor 統一 (`ActorRegistry` / `SupervisorFactory` 集約) | 未着手 |
//! | PR-5 | cleanup + guideline docs/spec + board / runner は将来 PR-γ | 未着手 |
//!
//! ## 設計分岐 (= i 路線採用、 PR-δ-1 同型)
//!
//! - **B 路線** (lifecycle 込み): trait に `mailbox()` / `handle()` async method を持たせ、
//!   PR-1 段階で各 actor に impl 強制 (= 既存挙動への影響が出る)
//! - **i 路線** (minimal marker first): passive marker trait のみ定義、 lifecycle method は
//!   PR-2/3 で各 actor の現実 (= consumer / observer の非対称) に合わせて trait 拡張
//!
//! 本 PR は **i 路線** を採用 (PR-δ-1 と同 pattern)。 PR-2 で agent / protocol を Agent impl
//! する際に observer / consumer pattern を trait 形式化、 PR-4 で supervisor 統一する際に
//! lifecycle method を追加する段階的 path。
//!
//! ## 関連
//!
//! - parent: VP-156 (Mailbox routing 統一 + 永続化 first-class、 epic)
//! - design-decision: `mem_1CatjVq5NUsjn1EHjRcaPG` §E (中期 framework 整理)
//! - VP-24 (Mailbox core 2026-03-18) — original ECS 意図
//! - VP-157 (PR #325) — `mcp` 廃止 + agent observer 化 (= ECS 純度回復の第一歩)
//! - PR-α-1 (#265、 VP-111) — 受け皿 pattern 先例 (`MachineCapabilities`)
//! - PR-β-1 (#274、 VP-119) — 受け皿 pattern 直近先例 (`LaneCapabilities`)
//! - PR-δ-1 (#288、 VP-135) — minimal marker pattern 先例 (`LaneStandHost`)

use std::any::Any;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// actor の lifecycle / address 範囲を表現する layer enum (LSCM 公理: Daemon / Repo / Lane)。
///
/// VP の 3 層 architecture (`docs/design/12-stand-architecture.md` LSCM):
/// - **Daemon**: machine-wide singleton (daemon scope、 例: `devices@machine`)
/// - **Repo**: repo 起動単位 (= 1 Process per repo、 例: `agent` / `protocol` / `notify`)
/// - **Lane**: Repo 内 Lane 単位 (= 1 Lane per Agent instance、 例: `board`)
///
/// 既存 code は dev discipline 任せで scope を表現していたが、 本 enum で trait 内に
/// 明示的に持たせる事で supervisor (PR-4) が scope 検証可能になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerScope {
    /// machine-wide singleton (daemon scope)。
    Machine,
    /// repo 起動単位 (= 1 Process per repo)。
    Repo,
    /// Repo 内 Lane 単位 (= 1 Lane per Agent instance)。
    Lane,
}

/// ECS entity bound actor の **minimal marker trait** (PR-1 受け皿)。
///
/// `Agent` は VP-24 original 設計意図 「Agent に component bolt-on で msgbox 使える」 の
/// 形式化。 Echoes (Claude CLI) / Protocol / 将来 board / runner 等の **能力
/// (= ECS entity)** を表現する。
///
/// `Any` super-trait で downcast を支援 (caller が specific Agent state を取り出すため)。
/// PR-1 は **passive marker** のみで、 actor lifecycle method は PR-2 で各 actor の
/// observer / consumer pattern に合わせて追加する。
///
/// ## impl 例 (PR-2 で導入予定)
///
/// ```rust,ignore
/// use vantage_point::capability::{Agent, LayerScope};
///
/// pub struct AgentCapability { /* ... */ }
///
/// impl Stand for AgentCapability {
///     fn name(&self) -> &str { "agent" }
///     fn layer_scope(&self) -> LayerScope { LayerScope::Repo }
///     fn as_any(&self) -> &dyn std::any::Any { self }
/// }
/// ```
///
/// ## `LaneStandHost` との違い
///
/// `process::lane_stand::LaneStandHost` は **Lane に host される受動的 marker** (= passive、
/// `board` 等)。 `Agent` は **ECS entity bound actor** (= active、 `agent` /
/// `protocol`)。 同じ「Agent」 という言葉で 2 概念を区別する規約:
///
/// - `Agent` (本 trait): ECS entity bound、 自律的に lifecycle / message を処理
/// - `LaneStandHost` (PR-δ-1): Lane に hosted、 LaneStandRegistry で N 個 host する marker
///
/// 将来的に `board` が `LaneStandHost` impl から `Agent` impl に進化する path も
/// 想定 (= PR-γ で Lane に migrate されたら entity bound 化)、 その際は両 trait impl も可能。
pub trait Stand: Any + Send + Sync + 'static {
    /// actor 名 (例: `"agent"` / `"protocol"`)。 mailbox address の actor 部分と一致する。
    ///
    /// `name` ではなく `actor_name` という命名は、 既存 `Capability::name()` (default method、
    /// `info().name` を clone する shortcut) との衝突回避のため (PR-2 着手時に発覚、 PR-1 で
    /// `name()` で landed したが caller 皆無で rename safe)。
    fn actor_name(&self) -> &str;

    /// actor の lifecycle / address scope (= LSCM 公理)。
    fn layer_scope(&self) -> LayerScope;

    /// `&dyn Any` への型強制 (downcast 用)。
    ///
    /// trait 自体に `Any` 制約があるが、 `&dyn Stand` から `&dyn Any` への自動 cast は
    /// Rust の trait object 制約により不可、 explicit な変換 method が必要 (`Any` 慣用句)。
    fn as_any(&self) -> &dyn Any;
}

/// singleton infra actor の **minimal marker trait** (PR-1 受け皿)。
///
/// `Service` は ECS entity に紐づかない **infra-side actor** (= 通信 / lifecycle / external
/// integration の専属担当)。 VP-24 original 設計後に後付けで混入した actor 群を、 ECS Agent
/// と区別する形式化。
///
/// 分類対象 (PR-3 で migrate 予定):
/// - `notify` (= DistributedNotification bridge、 Repo scope)
/// - `lane-spawn` (= Lane spawn lifecycle infra、 Repo scope)
/// - `repo-bootstrap` (= repo startup bootstrap、 Repo scope)
/// - `devices@machine` (= MIDI / external control、 machine scope)
///
/// `Agent` 同様 `Any` super-trait で downcast 支援、 lifecycle method は PR-3 で各 Service の
/// 現実に合わせて追加する。
pub trait Service: Any + Send + Sync + 'static {
    /// service 名 (例: `"notify"` / `"devices"`)。 mailbox address の actor 部分と一致する。
    ///
    /// `Agent::actor_name()` と同じ命名規約 (= 既存 `Capability::name()` 衝突回避、 PR-2 で
    /// rename された後)。
    fn actor_name(&self) -> &str;

    /// service の lifecycle / address scope。 多くは `Repo`、 `devices` のみ `Daemon`。
    fn layer_scope(&self) -> LayerScope;

    /// `&dyn Any` への型強制 (downcast 用)。
    fn as_any(&self) -> &dyn Any;
}

/// 持続的 recv loop を起動できる `Service` の super-trait (VP-159 PR-4b)。
///
/// `Service` の中で **`tokio::spawn` で background recv loop を起動できる** actor が impl する:
/// - `NotificationActor` (= `notify` mailbox の Notification msg 処理)
/// - `LaneSpawnActor` (= `lane-spawn` mailbox の `LaneCmd::SpawnLane` 処理)
///
/// 一方、 `DeviceRegistry` 🧲 のような「**instance hold + 内部 `monitor_task`
/// field**」 pattern の Service は本 trait を impl **しない** (= consume pattern に fit しない、
/// `MachineCapabilities.devices` で instance を保持する必要があるため)。 MIDI の正しい abstraction
/// は dynamic routing vision 確定後に再設計 (= design-spark `mem_1CavFi5D1aMSpEkas89SvQ` 参照)。
///
/// `ActorRegistry::spawn_service<S: SpawnableService>` が spawn 統合 + JoinHandle 保持する際の
/// trait bound。 PR-5 supervisor 統一で JoinHandle 経由の abort / await を activate する foundation。
pub trait SpawnableService: Service {
    /// recv loop を `tokio::spawn` で起動し、 `JoinHandle<()>` を返す。 `self` は consume される。
    ///
    /// `shutdown_token.cancelled()` で loop 終了、 channel close (= recv が None) でも終了。
    /// 返り値の `JoinHandle` を `ActorRegistry` が保持し、 supervisor 統一 (PR-5) で
    /// abort / await できる設計。
    fn spawn_loop(self, shutdown: CancellationToken) -> JoinHandle<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test fixture: minimal `Agent` impl (= name / scope / Any のみ)
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
    fn stand_trait_can_be_implemented() {
        let s = FixtureStand {
            name: "agent",
            scope: LayerScope::Repo,
        };
        assert_eq!(s.actor_name(), "agent");
        assert_eq!(s.layer_scope(), LayerScope::Repo);
    }

    #[test]
    fn service_trait_can_be_implemented() {
        let s = FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        };
        assert_eq!(s.actor_name(), "notify");
        assert_eq!(s.layer_scope(), LayerScope::Repo);
    }

    #[test]
    fn stand_supports_downcast_via_any() {
        // Box<dyn Stand> から `as_any()` 経由で specific 型に downcast できる事を確認
        let boxed: Box<dyn Stand> = Box::new(FixtureStand {
            name: "protocol",
            scope: LayerScope::Repo,
        });
        let downcast = boxed.as_any().downcast_ref::<FixtureStand>();
        assert!(downcast.is_some(), "FixtureStand への downcast は成立");
        assert_eq!(downcast.unwrap().name, "protocol");
    }

    #[test]
    fn service_supports_downcast_via_any() {
        let boxed: Box<dyn Service> = Box::new(FixtureService {
            name: "devices",
            scope: LayerScope::Machine,
        });
        let downcast = boxed.as_any().downcast_ref::<FixtureService>();
        assert!(downcast.is_some());
        assert_eq!(downcast.unwrap().scope, LayerScope::Machine);
    }

    #[test]
    fn layer_scope_variants_are_distinct() {
        // 3 variant が PartialEq で区別される事 (supervisor PR-4 で scope 検証に使う)
        assert_ne!(LayerScope::Machine, LayerScope::Repo);
        assert_ne!(LayerScope::Repo, LayerScope::Lane);
        assert_ne!(LayerScope::Machine, LayerScope::Lane);
    }

    #[test]
    fn layer_scope_is_copy() {
        // Copy trait で値渡し可能な事 (= trait method の戻り値として軽量に扱える)
        let scope = LayerScope::Repo;
        let copy = scope;
        assert_eq!(scope, copy);
    }

    #[test]
    fn stand_and_service_can_coexist_for_same_layer() {
        // 同じ LayerScope::Repo に Agent (agent) と Service (notify) が共存できる事
        let agent: Box<dyn Stand> = Box::new(FixtureStand {
            name: "agent",
            scope: LayerScope::Repo,
        });
        let service: Box<dyn Service> = Box::new(FixtureService {
            name: "notify",
            scope: LayerScope::Repo,
        });
        assert_eq!(agent.layer_scope(), service.layer_scope());
        assert_ne!(agent.actor_name(), service.actor_name());
    }

    #[test]
    fn n_distinct_stands_coexist_in_collection() {
        // PR-2 invariant (PR-δ-3 同型): 異なる name / scope の N 個 Agent impl が同じ
        // Vec<Box<dyn Stand>> で共存できる事 (= PR-4 supervisor 統一の foundation)。
        // 実 impl (AgentCapability / ProtocolCapability) を fixture で代理、 actor_name と
        // layer_scope の組合わせで supervisor が dispatch / filter できる pattern を検証。
        let agents: Vec<Box<dyn Stand>> = vec![
            Box::new(FixtureStand {
                name: "agent",
                scope: LayerScope::Repo,
            }),
            Box::new(FixtureStand {
                name: "protocol",
                scope: LayerScope::Repo,
            }),
            Box::new(FixtureStand {
                name: "devices",
                scope: LayerScope::Machine,
            }),
        ];
        assert_eq!(agents.len(), 3);

        // name が distinct で取り出せる
        let names: Vec<&str> = agents.iter().map(|s| s.actor_name()).collect();
        assert_eq!(names, vec!["agent", "protocol", "devices"]);

        // layer_scope で filter できる (= PR-4 supervisor が scope 別に dispatch する pattern)
        let repo_count = agents
            .iter()
            .filter(|s| s.layer_scope() == LayerScope::Repo)
            .count();
        let machine_count = agents
            .iter()
            .filter(|s| s.layer_scope() == LayerScope::Machine)
            .count();
        assert_eq!(repo_count, 2, "Repo scope の Agent は 2 個");
        assert_eq!(machine_count, 1, "machine scope の Agent は 1 個");
    }

    #[test]
    fn n_distinct_services_coexist_in_collection() {
        // PR-3 invariant (PR-2 同型): 異なる name / scope の N 個 Service impl が同じ
        // Vec<Box<dyn Service>> で共存できる事 (= PR-4 supervisor 統一の foundation)。
        // 実 impl (DeviceRegistry / NotificationActor / LaneSpawnActor) を fixture で代理、
        // 3 actor の actor_name + layer_scope を 1 collection で host できる pattern を検証。
        let services: Vec<Box<dyn Service>> = vec![
            Box::new(FixtureService {
                name: "notify",
                scope: LayerScope::Repo,
            }),
            Box::new(FixtureService {
                name: "lane-spawn",
                scope: LayerScope::Repo,
            }),
            Box::new(FixtureService {
                name: "devices",
                scope: LayerScope::Machine,
            }),
        ];
        assert_eq!(services.len(), 3);

        // actor_name が distinct で取り出せる (= repo-bootstrap は actor じゃないので含めない)
        let names: Vec<&str> = services.iter().map(|s| s.actor_name()).collect();
        assert_eq!(names, vec!["notify", "lane-spawn", "devices"]);

        // layer_scope で filter できる (= PR-4 supervisor が scope 別に dispatch する pattern)
        let repo_count = services
            .iter()
            .filter(|s| s.layer_scope() == LayerScope::Repo)
            .count();
        let machine_count = services
            .iter()
            .filter(|s| s.layer_scope() == LayerScope::Machine)
            .count();
        assert_eq!(
            repo_count, 2,
            "Repo scope の Service は 2 個 (= notify + lane-spawn)"
        );
        assert_eq!(
            machine_count, 1,
            "machine scope の Service は 1 個 (= devices)"
        );
    }
}
