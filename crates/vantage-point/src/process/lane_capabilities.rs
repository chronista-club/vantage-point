//! Lane 階層 Stand container (LSCM、 doc 12 §3 / §9 / doc 13 §3 参照)
//!
//! Lane (Conductor/Performer) あたり 1 instance、 SP per Project で host。 LSCM (Layer-Stand
//! Composition Model) の **Lane Layer 実体** = "Lane が必要な Stand を抱える" の物理表現。
//!
//! ## host する Stand (target、 doc 12 §9 catalog)
//!
//! - **Echoes 💬** = mise task 経由の PtySlot で立つ (LaneCapabilities では host しない、
//!   `LanePool` の各 Lane entry の PtySlot で扱う、 doc 13 §10 Q-7 暫定確定)
//! - **Paisley Park 🧭** (`PaisleyParkStand`): PR-δ-2 (VP-136) で **`LaneStandRegistry` 経由 host** へ進化、
//!   Lane あたり独立 instance (= 1 → N cardinality)
//! - **Gold Experience 🌿** (planned): PR-γ で同様に Lane instance 化、 LaneStandHost impl 追加予定
//! - **shell** = mise task 経由の PtySlot で立つ (LaneCapabilities では host しない)
//!
//! ## 実装状態
//!
//! - PR-β-1 (VP-119 ✅、 #274): 受け皿 struct 新設、 既存挙動への影響ゼロ。
//!   `paisley_park: None` placeholder。
//! - PR-β-2 (VP-120 ✅、 #275): PP 物理移管、 `paisley_park: Some(Arc::new(RwLock::new(default)))` で
//!   Lane あたり独立 instance host (cardinality 1 → N)。
//! - PR-δ-1 (VP-135 ✅、 #288): `LaneStandHost` trait + `LaneStandRegistry` 受け皿新設、
//!   既存挙動への影響ゼロ。
//! - PR-δ-2 (VP-136 ✅、 #289): **PP を `PaisleyParkStand` (LaneStandHost impl) に rewire** +
//!   `paisley_park` hardcoded field を削除、 `registry: LaneStandRegistry` で N Stand host に統一。
//!   production caller ゼロを着手前 grep で確認 (PR-β-2 と同じ argument)、 b 路線 aggressive replace。
//! - PR-δ-3 (VP-137 ✅、 #290): mock Stand B + 「N Stand host」 invariant test 4 件追加
//!   (`lane_capabilities.rs` tests module 内、 production binary 影響ゼロ)。
//! - PR-δ-4 (VP-138 ✅、 本 PR): cleanup — module-level doc roadmap update +
//!   lane_stand.rs 命名規約 doc fix + doc 13 §9 catalog 更新。
//!
//! 関連: doc 12 (`docs/design/12-stand-architecture.md` §9 Catalog)、
//! doc 13 (`docs/design/13-paisley-park-revival.md` §3 / §9)、
//! Linear VP-109 (parent epic)、 VP-119 / VP-120 (PR-β series)、
//! VP-135 / VP-136 / VP-137 / VP-138 (PR-δ series)。

use std::collections::HashMap;
use std::sync::Arc;

use super::lane_stand::LaneStandRegistry;
use super::lanes_state::LaneAddress;
use super::project_stands_state::PaisleyParkStand;
#[cfg(feature = "midi")]
use crate::justice::JusticeStand;

/// Lane 階層 Stand container (Lane あたり 1 instance)。
///
/// PR-δ-2 (VP-136) で **`registry: LaneStandRegistry` 経由 N Stand host** に統一。
/// Echoes / shell は mise task PtySlot 経由なので本 struct には host しない
/// (`LanePool` の各 Lane entry の PtySlot で扱う、 doc 13 §10 Q-7 暫定確定)。
// 要確認（audit 2026-07-18、先行実装の可能性）: PR-δ/PR-γ Stand-host skeleton。現状 field は未 read。
#[allow(dead_code)]
pub struct LaneCapabilities {
    /// Lane の identity (Conductor / Performer、 project 名 + name)
    pub address: LaneAddress,

    /// mise task 名 (例: `"echoes"` / `"shell"` / `"tmux"`、 PR-pre2 で `"hd"` → `"echoes"` rename)。
    /// `LanePool` の Lane entry と同期、 spawn 経路で参照される。
    pub stand: String,

    /// Lane に host される Stand の **registry** (PR-δ-2 / VP-136)。
    ///
    /// `new()` で `PaisleyParkStand` (= `stand_kind = "paisley_park"`) が default 登録される。
    /// PR-γ で `GoldExperienceStand` 等が同 registry に追加される予定。 caller は
    /// `lc.registry.get_typed::<PaisleyParkStand>("paisley_park")` で typed access する。
    pub registry: LaneStandRegistry,
}

impl LaneCapabilities {
    /// 新規構築 (PR-δ-2 / VP-136 で `registry` に PP を default 登録)。
    ///
    /// PR-α-1 の `WorldCapabilities::new` (= midi None で構築) とは異なり、 本 struct は
    /// **PR-β-2 で PP を常時 host 化**、 PR-δ-2 でその host path を `LaneStandRegistry` 経由に
    /// 統一。 doc 13 §6 自動 spawn rule (Lane 起動時に PP 同時 spawn が default) との整合のため、
    /// factory を分けず `new()` で populate する設計を維持。
    /// 将来 PP なし Lane (opt-out) が必要になったら `without_paisley_park()` factory 追加で対応。
    pub fn new(address: LaneAddress, stand: impl Into<String>) -> Self {
        let mut registry = LaneStandRegistry::new();
        // PR-δ-2 (VP-136): PP を最初の住人として LaneStandRegistry に登録
        registry.insert(Arc::new(PaisleyParkStand::new()));
        // E3-1: Justice 🌫️ を Lane に自動 host（midi feature 有効時）
        #[cfg(feature = "midi")]
        registry.insert(Arc::new(JusticeStand::new()));
        Self {
            address,
            stand: stand.into(),
            registry,
        }
    }
}

/// Lane scope Stand pool — SP per Project で 1 instance、 各 Lane の `LaneCapabilities` を集約。
///
/// PR-β-1 (VP-119) で空 HashMap 受け皿として新設、 PR-β-2 (VP-120) で `LanePool::with_root`
/// と `lane_spawn_actor` (Performer spawn 経路) から populate される lifecycle と sync。
/// 既存 `LanePool` (`lanes_state.rs`) とは並立、 PR-δ-4 cleanup PR で整合性 review 予定。
#[derive(Default)]
pub struct LaneCapabilitiesPool {
    pub entries: HashMap<LaneAddress, Arc<LaneCapabilities>>,
}

impl LaneCapabilitiesPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// entry 数を返す (debug / metrics / test 用)。
    // 要確認（audit 2026-07-18、先行実装の可能性）: PR-δ/PR-γ Stand-host skeleton の registry helper。
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// PR-β-2 (VP-120): Lane spawn 時に LaneCapabilities entry を populate。
    ///
    /// `LanePool::with_root` / `lane_spawn_actor` (Performer spawn) から呼ばれて、 Lane あたり
    /// 独立 PaisleyParkState を持つ entry を HashMap に挿入する。 同じ address で重複 insert
    /// した場合は overwrite (= idempotent、 restart / respawn 経路で安全)。
    pub fn populate_lane(&mut self, address: LaneAddress, stand: impl Into<String>) {
        let lc = Arc::new(LaneCapabilities::new(address.clone(), stand));
        self.entries.insert(address, lc);
    }

    /// PR-β-2 (VP-120): Lane destroy 時に entry を削除 (cascade lifecycle)。
    // 要確認（audit 2026-07-18、先行実装の可能性）: PR-δ/PR-γ Stand-host skeleton の registry helper。
    #[allow(dead_code)]
    pub fn remove_lane(&mut self, address: &LaneAddress) -> Option<Arc<LaneCapabilities>> {
        self.entries.remove(address)
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::super::lane_stand::LaneStandHost;
    use super::*;

    /// PR-δ-3 (VP-137): test-only mock Stand。 「LaneCapabilities が PP 含めて N Stand を host
    /// できる generic interface」 invariant (doc 13 §9 boundary) を真の test に格上げするための
    /// 2 つ目の Stand impl。 production binary には登場しない (`#[cfg(test)]` 限定)。
    ///
    /// minimal counter state (`AtomicU32`) を持つだけの passive Stand、 caller が
    /// `increment()` を呼ぶごとに value が +1 される。 PaisleyParkStand とは state shape
    /// (RwLock<PaisleyParkState> vs AtomicU32) が異なるので「型が違う 2 Stand 共存」 の
    /// invariant test 素材として最適。
    struct MockStandB {
        counter: AtomicU32,
    }

    impl MockStandB {
        fn new() -> Self {
            Self {
                counter: AtomicU32::new(0),
            }
        }

        fn increment(&self) -> u32 {
            self.counter.fetch_add(1, Ordering::SeqCst) + 1
        }

        fn value(&self) -> u32 {
            self.counter.load(Ordering::SeqCst)
        }
    }

    impl LaneStandHost for MockStandB {
        fn stand_kind(&self) -> &'static str {
            "mock_b"
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn lane_capabilities_new_populates_paisley_park() {
        // PR-δ-2 (VP-136): PP は registry 経由で host される (Lane あたり独立 instance)
        let addr = LaneAddress::root("vp");
        let lc = LaneCapabilities::new(addr.clone(), "echoes");

        assert_eq!(lc.address, addr);
        assert_eq!(lc.stand, "echoes");
        assert!(
            lc.registry
                .get_typed::<PaisleyParkStand>("paisley_park")
                .is_some(),
            "PR-δ-2 物理移管後は registry に PaisleyParkStand が host される"
        );
    }

    #[test]
    fn lane_capabilities_with_performer_address() {
        // Performer Lane でも独立 PaisleyParkStand で構築
        let addr = LaneAddress::performer("vp", "feat-test");
        let lc = LaneCapabilities::new(addr.clone(), "shell");

        assert_eq!(lc.address, addr);
        assert_eq!(lc.stand, "shell");
        assert!(
            lc.registry
                .get_typed::<PaisleyParkStand>("paisley_park")
                .is_some(),
            "Performer Lane も独立 PaisleyParkStand を持つ"
        );
    }

    #[tokio::test]
    async fn lane_capabilities_pp_instances_are_independent() {
        // PR-β-2 (VP-120) cardinality 1 → N invariant、 PR-δ-2 (VP-136) registry 経由でも維持
        let conductor = LaneCapabilities::new(LaneAddress::root("vp"), "echoes");
        let performer = LaneCapabilities::new(LaneAddress::performer("vp", "sub"), "echoes");

        let conductor_pp = conductor
            .registry
            .get_typed::<PaisleyParkStand>("paisley_park")
            .expect("Conductor PP");
        let performer_pp = performer
            .registry
            .get_typed::<PaisleyParkStand>("paisley_park")
            .expect("Performer PP");

        // Conductor に content set しても Performer に伝播しないこと
        conductor_pp.state().write().await.content = Some("root-canvas".to_string());

        assert_eq!(
            conductor_pp.state().read().await.content.as_deref(),
            Some("root-canvas")
        );
        assert!(
            performer_pp.state().read().await.content.is_none(),
            "Performer PP は Conductor PP と独立 (cross-Lane state share なし、 doc 12 A6)"
        );
    }

    #[test]
    fn lane_capabilities_registry_count_after_new() {
        let lc = LaneCapabilities::new(LaneAddress::root("vp"), "echoes");
        // PP + Justice（midi feature 有効時）
        let expected = if cfg!(feature = "midi") { 2 } else { 1 };
        assert_eq!(
            lc.registry.count(),
            expected,
            "new() 直後は PP + Justice(midi時) で count = {expected}"
        );
    }

    #[test]
    fn lane_capabilities_pool_empty_default() {
        // 初期化直後の Pool は空 (populate_lane を呼んで初めて entry が入る)
        let pool = LaneCapabilitiesPool::new();
        assert_eq!(pool.count(), 0);
        assert!(pool.entries.is_empty());
    }

    #[test]
    fn lane_capabilities_pool_populate_lane_inserts_entry() {
        // PR-β-2 (VP-120): populate_lane で entry 挿入、 PR-δ-2 (VP-136) で registry 経由 PP 確認
        let mut pool = LaneCapabilitiesPool::new();
        let addr = LaneAddress::root("vp");
        pool.populate_lane(addr.clone(), "echoes");

        assert_eq!(pool.count(), 1);
        let lc = pool.entries.get(&addr).expect("Conductor entry 不在");
        assert_eq!(lc.stand, "echoes");
        assert!(
            lc.registry
                .get_typed::<PaisleyParkStand>("paisley_park")
                .is_some(),
            "populate 後は registry に PP host 済"
        );
    }

    #[test]
    fn lane_capabilities_pool_populate_is_idempotent() {
        // PR-β-2 (VP-120): 同 address で 2 回 populate しても overwrite で 1 entry のまま
        let mut pool = LaneCapabilitiesPool::new();
        let addr = LaneAddress::root("vp");
        pool.populate_lane(addr.clone(), "echoes");
        pool.populate_lane(addr.clone(), "shell"); // restart 経路想定

        assert_eq!(pool.count(), 1, "overwrite で 1 entry のみ");
        let lc = pool.entries.get(&addr).expect("entry");
        assert_eq!(lc.stand, "shell", "後の populate が勝つ");
    }

    #[test]
    fn lane_capabilities_pool_remove_lane() {
        // PR-β-2 (VP-120): cascade lifecycle = Lane destroy で entry も削除
        let mut pool = LaneCapabilitiesPool::new();
        let addr = LaneAddress::root("vp");
        pool.populate_lane(addr.clone(), "echoes");

        let removed = pool.remove_lane(&addr);
        assert!(removed.is_some(), "remove で entry 削除");
        assert_eq!(pool.count(), 0);

        // 二度目の remove は None
        assert!(pool.remove_lane(&addr).is_none());
    }

    // ========================================================================
    // PR-δ-3 (VP-137) — 「N Stand host」 invariant tests
    //
    // doc 13 §9 boundary invariant 「LaneCapabilities が PP 含めて N Stand を
    // host できる generic interface」 を、 LaneCapabilities lifecycle context での
    // 2 Stand 共存 + 状態独立性 + per-Lane 独立性 + remove 独立性で test 化。
    // ========================================================================

    /// PR-δ-3 (VP-137): LaneCapabilities が PP + MockStandB を **同時に host できる**
    /// invariant。 new() 直後の registry は PP 1 件、 MockStandB を insert すると count = 2、
    /// 両 Stand が typed access 可能。
    #[test]
    fn lane_capabilities_hosts_pp_and_mock_b_simultaneously() {
        let mut lc = LaneCapabilities::new(LaneAddress::root("vp"), "echoes");
        let base_count = lc.registry.count();

        // MockStandB を insert
        lc.registry.insert(Arc::new(MockStandB::new()));

        assert_eq!(
            lc.registry.count(),
            base_count + 1,
            "MockStandB insert 後は base + 1 Stand 共存"
        );
        assert!(
            lc.registry
                .get_typed::<PaisleyParkStand>("paisley_park")
                .is_some(),
            "PP は依然として typed access 可能"
        );
        assert!(
            lc.registry.get_typed::<MockStandB>("mock_b").is_some(),
            "MockStandB も typed access 可能"
        );
    }

    /// PR-δ-3 (VP-137): PP と MockStandB の **state が完全に独立** している invariant。
    /// MockStandB.increment() が PP state を変化させない、 PP content set が
    /// MockStandB.value() を変化させない、 cross-Stand state share なし (doc 12 A6 整合)。
    #[tokio::test]
    async fn lane_capabilities_pp_and_mock_b_states_are_independent() {
        let mut lc = LaneCapabilities::new(LaneAddress::root("vp"), "echoes");
        lc.registry.insert(Arc::new(MockStandB::new()));

        let pp = lc
            .registry
            .get_typed::<PaisleyParkStand>("paisley_park")
            .expect("PP host 不在");
        let mock_b = lc
            .registry
            .get_typed::<MockStandB>("mock_b")
            .expect("MockStandB host 不在");

        // MockStandB を 3 回 increment、 PP には触らない
        mock_b.increment();
        mock_b.increment();
        mock_b.increment();

        assert_eq!(mock_b.value(), 3, "MockStandB の counter は increment 反映");
        assert!(
            pp.state().read().await.content.is_none(),
            "MockStandB.increment() は PP content に影響しない (state 独立)"
        );

        // PP に content set、 MockStandB には触らない
        pp.state().write().await.content = Some("pp-canvas".to_string());

        assert_eq!(
            pp.state().read().await.content.as_deref(),
            Some("pp-canvas"),
            "PP content set が反映"
        );
        assert_eq!(
            mock_b.value(),
            3,
            "PP content set は MockStandB.counter に影響しない (state 独立)"
        );
    }

    /// PR-δ-3 (VP-137): 異なる Lane の MockStandB が **独立 instance** である invariant。
    /// Lane A の counter increment が Lane B に影響しない (cardinality 1 → N が
    /// MockStandB でも成立)、 PR-β-2 PP 独立性 invariant の generic 拡張。
    #[test]
    fn lane_capabilities_mock_b_independent_per_lane() {
        let mut lane_a = LaneCapabilities::new(LaneAddress::root("vp"), "echoes");
        let mut lane_b = LaneCapabilities::new(LaneAddress::performer("vp", "sub"), "echoes");

        lane_a.registry.insert(Arc::new(MockStandB::new()));
        lane_b.registry.insert(Arc::new(MockStandB::new()));

        let mock_a = lane_a
            .registry
            .get_typed::<MockStandB>("mock_b")
            .expect("Lane A MockStandB");
        let mock_b = lane_b
            .registry
            .get_typed::<MockStandB>("mock_b")
            .expect("Lane B MockStandB");

        // Lane A だけ 5 回 increment
        for _ in 0..5 {
            mock_a.increment();
        }

        assert_eq!(mock_a.value(), 5, "Lane A counter は 5");
        assert_eq!(
            mock_b.value(),
            0,
            "Lane B counter は 0 のまま (instance 独立、 cross-Lane share なし)"
        );
    }

    /// PR-δ-3 (VP-137): MockStandB を `registry.remove("mock_b")` で削除しても **PP は残存**
    /// する invariant。 cardinality は 2 → 1 に戻り、 PP typed access は依然成立。
    /// PR-δ-1 `registry_remove_does_not_affect_other_stands` の Lane lifecycle 版。
    #[test]
    fn lane_capabilities_remove_mock_b_keeps_pp() {
        let mut lc = LaneCapabilities::new(LaneAddress::root("vp"), "echoes");
        let base_count = lc.registry.count();
        lc.registry.insert(Arc::new(MockStandB::new()));
        assert_eq!(lc.registry.count(), base_count + 1, "insert 後は base + 1");

        let removed = lc.registry.remove("mock_b");
        assert!(
            removed.is_some(),
            "MockStandB は registry に存在したので Some"
        );

        assert_eq!(
            lc.registry.count(),
            base_count,
            "remove 後は base count に戻る"
        );
        assert!(
            lc.registry
                .get_typed::<PaisleyParkStand>("paisley_park")
                .is_some(),
            "PP は MockStandB remove の影響を受けず残存"
        );
        assert!(
            lc.registry.get_typed::<MockStandB>("mock_b").is_none(),
            "MockStandB は削除済"
        );
    }
}
