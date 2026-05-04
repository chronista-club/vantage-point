//! Lane 階層 Stand container (LSCM、 doc 12 §3 / §9 / doc 13 §3 参照)
//!
//! Lane (Lead/Worker) あたり 1 instance、 SP per Project で host。 LSCM (Layer-Stand
//! Composition Model) の **Lane Layer 実体** = "Lane が必要な Stand を抱える" の物理表現。
//!
//! ## host する Stand (target、 doc 12 §9 catalog)
//!
//! - **Echoes 💬** = mise task 経由の PtySlot で立つ (LaneCapabilities では host しない、
//!   `LanePool` の各 Lane entry の PtySlot で扱う、 doc 13 §10 Q-7 暫定確定)
//! - **Paisley Park 🧭** (`PaisleyParkState`): PR-β-2 (VP-120) で物理移管完了、 Lane あたり
//!   独立 instance を host (= 1 → N cardinality migration)
//! - **Gold Experience 🌿** (Option): PR-γ (planned) で同様に Lane instance 化
//! - **The Hand 🤚** = mise task 経由の PtySlot で立つ (LaneCapabilities では host しない)
//!
//! ## 実装状態
//!
//! - PR-β-1 (VP-119 ✅、 #274): 受け皿 struct 新設、 既存挙動への影響ゼロ。
//!   `paisley_park: None` placeholder。
//! - PR-β-2 (VP-120 ✅、 本 PR): **PP 物理移管完了**。 `ProjectStandsPool.paisley_park` field 削除、
//!   本 struct の `new()` で `paisley_park: Some(Arc::new(RwLock::new(default)))` 自動 populate
//!   (Lane あたり独立 instance、 doc 13 §6 自動 spawn rule = default + opt-out)。
//!   2026-04-27 rule comment supersede 明示。
//! - PR-β-3 ⏭️ skip: caller migration は実 caller (PaisleyParkState field access) ゼロと
//!   PR-β-2 着手前 grep で判明、 doc 13 §9 roadmap を 5 sub → 4 sub に縮小。
//! - PR-β-4 (planned): cleanup、 catalog row 更新、 doc 12 §10 既存実装表 update、 legacy alias 削除。
//! - PR-γ (planned): GE を Lane instance 化 (PR-β-2 と同 pattern、 cardinality 1 → N)。
//!
//! 関連: doc 12 (`docs/design/12-stand-architecture.md` §9 Catalog)、
//! doc 13 (`docs/design/13-paisley-park-revival.md` §3 / §9)、
//! Linear VP-109 (parent epic)、 VP-119 (受け皿) / VP-120 (本 PR、 物理移管)。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::lanes_state::LaneAddress;
use super::project_stands_state::PaisleyParkState;

/// Lane 階層 Stand container (Lane あたり 1 instance)。
///
/// PR-β-1 (VP-119) で受け皿 struct 新設、 PR-β-2 (VP-120) で PP を物理 host 化。
/// Echoes / The Hand は mise task PtySlot 経由なので本 struct には host しない
/// (`LanePool` の各 Lane entry の PtySlot で扱う、 doc 13 §10 Q-7 暫定確定)。
pub struct LaneCapabilities {
    /// Lane の identity (Lead / Worker、 project 名 + name)
    pub address: LaneAddress,

    /// mise task 名 (例: `"echoes"` / `"shell"` / `"tmux"`、 PR-pre2 で `"hd"` → `"echoes"` rename)。
    /// `LanePool` の Lane entry と同期、 spawn 経路で参照される。
    pub stand: String,

    /// Paisley Park 🧭 — Information Router (target = Lane instance、 doc 13 §3 P4)。
    /// PR-β-2 (VP-120) で物理移管完了、 `new()` で常に `Some(...)` (Lane あたり独立 instance)。
    /// doc 13 §6 自動 spawn rule = default で Lane 起動時に PP 同時 spawn。
    /// `Option<>` 型は将来の opt-out path (= PP なし Lane を許す) のために残置。
    pub paisley_park: Option<Arc<RwLock<PaisleyParkState>>>,
    // PR-γ で追加予定:
    //   pub gold_experience: Option<Arc<RwLock<GoldExperienceState>>>,
}

impl LaneCapabilities {
    /// 新規構築 (PR-β-2 / VP-120 で `paisley_park` を Some 化)。
    ///
    /// PR-α-1 の `WorldCapabilities::new` (= midi None で構築) とは異なり、 本 struct は
    /// **PR-β-2 で常時 PP host 化**。 doc 13 §6 自動 spawn rule (Lane 起動時に PP 同時 spawn が
    /// default) との整合のため、 factory を分けず `new()` で populate する設計。
    /// 将来 PP なし Lane (opt-out) が必要になったら `without_paisley_park()` factory 追加で対応。
    pub fn new(address: LaneAddress, stand: impl Into<String>) -> Self {
        Self {
            address,
            stand: stand.into(),
            // PR-β-2 (VP-120): PP を Lane あたり独立 instance で populate (1 → N migration)
            paisley_park: Some(Arc::new(RwLock::new(PaisleyParkState::default()))),
        }
    }
}

/// Lane scope Stand pool — SP per Project で 1 instance、 各 Lane の `LaneCapabilities` を集約。
///
/// PR-β-1 (VP-119) で空 HashMap 受け皿として新設、 PR-β-2 (VP-120) で `LanePool::with_lead`
/// と `lane_spawn_actor` (Worker spawn 経路) から populate される lifecycle と sync。
/// 既存 `LanePool` (`lanes_state.rs`) とは並立、 PR-β-4 cleanup PR で整合性 review 予定。
#[derive(Default)]
pub struct LaneCapabilitiesPool {
    pub entries: HashMap<LaneAddress, Arc<LaneCapabilities>>,
}

impl LaneCapabilitiesPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// entry 数を返す (debug / metrics / test 用)。
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// PR-β-2 (VP-120): Lane spawn 時に LaneCapabilities entry を populate。
    ///
    /// `LanePool::with_lead` / `lane_spawn_actor` (Worker spawn) から呼ばれて、 Lane あたり
    /// 独立 PaisleyParkState を持つ entry を HashMap に挿入する。 同じ address で重複 insert
    /// した場合は overwrite (= idempotent、 restart / respawn 経路で安全)。
    pub fn populate_lane(&mut self, address: LaneAddress, stand: impl Into<String>) {
        let lc = Arc::new(LaneCapabilities::new(address.clone(), stand));
        self.entries.insert(address, lc);
    }

    /// PR-β-2 (VP-120): Lane destroy 時に entry を削除 (cascade lifecycle)。
    pub fn remove_lane(&mut self, address: &LaneAddress) -> Option<Arc<LaneCapabilities>> {
        self.entries.remove(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_capabilities_new_populates_paisley_park() {
        // PR-β-2 (VP-120): PP 物理移管後、 new() は paisley_park を常に Some で populate
        let addr = LaneAddress::lead("vp");
        let lc = LaneCapabilities::new(addr.clone(), "echoes");

        assert_eq!(lc.address, addr);
        assert_eq!(lc.stand, "echoes");
        assert!(
            lc.paisley_park.is_some(),
            "PR-β-2 物理移管後は paisley_park = Some(...) (Lane あたり独立 instance)"
        );
    }

    #[test]
    fn lane_capabilities_with_worker_address() {
        // Worker Lane でも独立 PaisleyParkState で構築
        let addr = LaneAddress::worker("vp", "feat-test");
        let lc = LaneCapabilities::new(addr.clone(), "shell");

        assert_eq!(lc.address, addr);
        assert_eq!(lc.stand, "shell");
        assert!(
            lc.paisley_park.is_some(),
            "Worker Lane も独立 PaisleyParkState を持つ"
        );
    }

    #[tokio::test]
    async fn lane_capabilities_pp_instances_are_independent() {
        // PR-β-2 (VP-120) cardinality 1 → N 検証: 異なる Lane の PP は別 instance
        let lead = LaneCapabilities::new(LaneAddress::lead("vp"), "echoes");
        let worker = LaneCapabilities::new(LaneAddress::worker("vp", "sub"), "echoes");

        let lead_pp = lead.paisley_park.expect("Lead PP");
        let worker_pp = worker.paisley_park.expect("Worker PP");

        // Lead に content set しても Worker に伝播しないこと
        lead_pp.write().await.content = Some("lead-canvas".to_string());

        assert_eq!(lead_pp.read().await.content.as_deref(), Some("lead-canvas"));
        assert!(
            worker_pp.read().await.content.is_none(),
            "Worker PP は Lead PP と独立 (cross-Lane state share なし、 doc 12 A6)"
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
        // PR-β-2 (VP-120): populate_lane で entry 挿入
        let mut pool = LaneCapabilitiesPool::new();
        let addr = LaneAddress::lead("vp");
        pool.populate_lane(addr.clone(), "echoes");

        assert_eq!(pool.count(), 1);
        let lc = pool.entries.get(&addr).expect("Lead entry 不在");
        assert_eq!(lc.stand, "echoes");
        assert!(lc.paisley_park.is_some(), "populate 後は PP host 済");
    }

    #[test]
    fn lane_capabilities_pool_populate_is_idempotent() {
        // PR-β-2 (VP-120): 同 address で 2 回 populate しても overwrite で 1 entry のまま
        let mut pool = LaneCapabilitiesPool::new();
        let addr = LaneAddress::lead("vp");
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
        let addr = LaneAddress::lead("vp");
        pool.populate_lane(addr.clone(), "echoes");

        let removed = pool.remove_lane(&addr);
        assert!(removed.is_some(), "remove で entry 削除");
        assert_eq!(pool.count(), 0);

        // 二度目の remove は None
        assert!(pool.remove_lane(&addr).is_none());
    }
}
