//! ECS entity bound actor 用 `Stand` trait と singleton infra 用 `Service` trait の
//! 受け皿 (VP-159 / VP-156 epic H1 段、 PR-1)。
//!
//! VP-24 (Mailbox core) で「Stand に component bolt-on で msgbox 使える」 が original 設計
//! 意図だったが、 後付けで infra actor (`mcp` / `notify` / `lane-spawn` / `sp-bootstrap`) が
//! 混入し ECS 純度が崩れた。 VP-157 (PR #325) で `mcp` box 廃止 + agent observer 化が
//! 第一歩、 本 PR で残 actor を **Stand** (= ECS entity bound) と **Service** (= singleton
//! infra) の 2 trait に分離する受け皿を新設する。
//!
//! design-decision: creo-memories `mem_1CatjVq5NUsjn1EHjRcaPG` §E
//! roadmap memory: creo-memories `mem_1CatuBTFNcEHgC3nfJJMke`
//!
//! ## VP-159 PR roadmap (sub-PR 5 段)
//!
//! | sub-PR | scope | status |
//! |--------|-------|--------|
//! | PR-1 | `Stand` / `Service` trait + `LayerScope` enum 受け皿 + `LaneStand` → `LaneStandHost` rename | 本 PR |
//! | PR-2 | Stand migrate (`agent` + `protocol`、 observer/consumer pattern 形式化) | 未着手 |
//! | PR-3 | Service migrate (`notify` + `lane-spawn` + `sp-bootstrap` + `hermit_purple`) | 未着手 |
//! | PR-4 | supervisor 統一 (`ActorRegistry` / `SupervisorFactory` 集約) | 未着手 |
//! | PR-5 | cleanup + guideline docs/spec + paisley_park / gold_experience は将来 PR-γ | 未着手 |
//!
//! ## 設計分岐 (= i 路線採用、 PR-δ-1 同型)
//!
//! - **B 路線** (lifecycle 込み): trait に `mailbox()` / `handle()` async method を持たせ、
//!   PR-1 段階で各 actor に impl 強制 (= 既存挙動への影響が出る)
//! - **i 路線** (minimal marker first): passive marker trait のみ定義、 lifecycle method は
//!   PR-2/3 で各 actor の現実 (= consumer / observer の非対称) に合わせて trait 拡張
//!
//! 本 PR は **i 路線** を採用 (PR-δ-1 と同 pattern)。 PR-2 で agent / protocol を Stand impl
//! する際に observer / consumer pattern を trait 形式化、 PR-4 で supervisor 統一する際に
//! lifecycle method を追加する段階的 path。
//!
//! ## 関連
//!
//! - parent: VP-156 (Mailbox routing 統一 + 永続化 first-class、 epic)
//! - design-decision: `mem_1CatjVq5NUsjn1EHjRcaPG` §E (中期 framework 整理)
//! - VP-24 (Mailbox core 2026-03-18) — original ECS 意図
//! - VP-157 (PR #325) — `mcp` 廃止 + agent observer 化 (= ECS 純度回復の第一歩)
//! - PR-α-1 (#265、 VP-111) — 受け皿 pattern 先例 (`WorldCapabilities`)
//! - PR-β-1 (#274、 VP-119) — 受け皿 pattern 直近先例 (`LaneCapabilities`)
//! - PR-δ-1 (#288、 VP-135) — minimal marker pattern 先例 (`LaneStandHost`)

use std::any::Any;

/// actor の lifecycle / address 範囲を表現する layer enum (LSCM 公理: World / Project / Lane)。
///
/// VP の 3 層 architecture (`docs/design/12-stand-architecture.md` LSCM):
/// - **World**: machine-wide singleton (TheWorld daemon scope、 例: `hermit_purple@world`)
/// - **Project**: SP 起動単位 (= 1 Process per project、 例: `agent` / `protocol` / `notify`)
/// - **Lane**: Project 内 Lane 単位 (= 1 Lane per Stand instance、 例: `paisley_park`)
///
/// 既存 code は dev discipline 任せで scope を表現していたが、 本 enum で trait 内に
/// 明示的に持たせる事で supervisor (PR-4) が scope 検証可能になる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerScope {
    /// machine-wide singleton (TheWorld daemon scope)。
    World,
    /// SP 起動単位 (= 1 Process per project)。
    Project,
    /// Project 内 Lane 単位 (= 1 Lane per Stand instance)。
    Lane,
}

/// ECS entity bound actor の **minimal marker trait** (PR-1 受け皿)。
///
/// `Stand` は VP-24 original 設計意図 「Stand に component bolt-on で msgbox 使える」 の
/// 形式化。 Echoes (Claude CLI) / Protocol / 将来 PaisleyPark / GoldExperience 等の **能力
/// (= ECS entity)** を表現する。
///
/// `Any` super-trait で downcast を支援 (caller が specific Stand state を取り出すため)。
/// PR-1 は **passive marker** のみで、 actor lifecycle method は PR-2 で各 actor の
/// observer / consumer pattern に合わせて追加する。
///
/// ## impl 例 (PR-2 で導入予定)
///
/// ```rust,ignore
/// use vantage_point::capability::{Stand, LayerScope};
///
/// pub struct AgentCapability { /* ... */ }
///
/// impl Stand for AgentCapability {
///     fn name(&self) -> &str { "agent" }
///     fn layer_scope(&self) -> LayerScope { LayerScope::Project }
///     fn as_any(&self) -> &dyn std::any::Any { self }
/// }
/// ```
///
/// ## `LaneStandHost` との違い
///
/// `process::lane_stand::LaneStandHost` は **Lane に host される受動的 marker** (= passive、
/// `paisley_park` 等)。 `Stand` は **ECS entity bound actor** (= active、 `agent` /
/// `protocol`)。 同じ「Stand」 という言葉で 2 概念を区別する規約:
///
/// - `Stand` (本 trait): ECS entity bound、 自律的に lifecycle / message を処理
/// - `LaneStandHost` (PR-δ-1): Lane に hosted、 LaneStandRegistry で N 個 host する marker
///
/// 将来的に `paisley_park` が `LaneStandHost` impl から `Stand` impl に進化する path も
/// 想定 (= PR-γ で Lane に migrate されたら entity bound 化)、 その際は両 trait impl も可能。
pub trait Stand: Any + Send + Sync + 'static {
    /// actor 名 (例: `"agent"` / `"protocol"`)。 mailbox address の actor 部分と一致する。
    fn name(&self) -> &str;

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
/// integration の専属担当)。 VP-24 original 設計後に後付けで混入した actor 群を、 ECS Stand
/// と区別する形式化。
///
/// 分類対象 (PR-3 で migrate 予定):
/// - `notify` (= DistributedNotification bridge、 Project scope)
/// - `lane-spawn` (= Lane spawn lifecycle infra、 Project scope)
/// - `sp-bootstrap` (= SP startup bootstrap、 Project scope)
/// - `hermit_purple@world` (= MIDI / external control、 World scope)
///
/// `Stand` 同様 `Any` super-trait で downcast 支援、 lifecycle method は PR-3 で各 Service の
/// 現実に合わせて追加する。
pub trait Service: Any + Send + Sync + 'static {
    /// service 名 (例: `"notify"` / `"hermit_purple"`)。 mailbox address の actor 部分と一致する。
    fn name(&self) -> &str;

    /// service の lifecycle / address scope。 多くは `Project`、 `hermit_purple` のみ `World`。
    fn layer_scope(&self) -> LayerScope;

    /// `&dyn Any` への型強制 (downcast 用)。
    fn as_any(&self) -> &dyn Any;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// test fixture: minimal `Stand` impl (= name / scope / Any のみ)
    struct FixtureStand {
        name: &'static str,
        scope: LayerScope,
    }

    impl Stand for FixtureStand {
        fn name(&self) -> &str {
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
        fn name(&self) -> &str {
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
            scope: LayerScope::Project,
        };
        assert_eq!(s.name(), "agent");
        assert_eq!(s.layer_scope(), LayerScope::Project);
    }

    #[test]
    fn service_trait_can_be_implemented() {
        let s = FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        };
        assert_eq!(s.name(), "notify");
        assert_eq!(s.layer_scope(), LayerScope::Project);
    }

    #[test]
    fn stand_supports_downcast_via_any() {
        // Box<dyn Stand> から `as_any()` 経由で specific 型に downcast できる事を確認
        let boxed: Box<dyn Stand> = Box::new(FixtureStand {
            name: "protocol",
            scope: LayerScope::Project,
        });
        let downcast = boxed.as_any().downcast_ref::<FixtureStand>();
        assert!(downcast.is_some(), "FixtureStand への downcast は成立");
        assert_eq!(downcast.unwrap().name, "protocol");
    }

    #[test]
    fn service_supports_downcast_via_any() {
        let boxed: Box<dyn Service> = Box::new(FixtureService {
            name: "hermit_purple",
            scope: LayerScope::World,
        });
        let downcast = boxed.as_any().downcast_ref::<FixtureService>();
        assert!(downcast.is_some());
        assert_eq!(downcast.unwrap().scope, LayerScope::World);
    }

    #[test]
    fn layer_scope_variants_are_distinct() {
        // 3 variant が PartialEq で区別される事 (supervisor PR-4 で scope 検証に使う)
        assert_ne!(LayerScope::World, LayerScope::Project);
        assert_ne!(LayerScope::Project, LayerScope::Lane);
        assert_ne!(LayerScope::World, LayerScope::Lane);
    }

    #[test]
    fn layer_scope_is_copy() {
        // Copy trait で値渡し可能な事 (= trait method の戻り値として軽量に扱える)
        let scope = LayerScope::Project;
        let copy = scope;
        assert_eq!(scope, copy);
    }

    #[test]
    fn stand_and_service_can_coexist_for_same_layer() {
        // 同じ LayerScope::Project に Stand (agent) と Service (notify) が共存できる事
        let stand: Box<dyn Stand> = Box::new(FixtureStand {
            name: "agent",
            scope: LayerScope::Project,
        });
        let service: Box<dyn Service> = Box::new(FixtureService {
            name: "notify",
            scope: LayerScope::Project,
        });
        assert_eq!(stand.layer_scope(), service.layer_scope());
        assert_ne!(stand.name(), service.name());
    }
}
