//! Lane 階層 Stand container (LSCM、 doc 12 §3 / §9 / doc 13 §3 参照)
//!
//! Lane (Lead/Worker) あたり 1 instance、 SP per Project で host。 LSCM (Layer-Stand
//! Composition Model) の **Lane Layer 実体** = "Lane が必要な Stand を抱える" の物理表現。
//!
//! ## host する Stand (target、 doc 12 §9 catalog)
//!
//! - **Echoes 💬** = mise task 経由の PtySlot で立つ (LaneCapabilities では host しない、
//!   `LanePool` の各 Lane entry の PtySlot で扱う)
//! - **Paisley Park 🧭** (`PaisleyParkState`、 Option): PR-β-2 (planned) で `ProjectStands` から
//!   物理移管、 Lane あたり 1 instance に
//! - **Gold Experience 🌿** (Option): PR-γ (planned) で同様に Lane instance 化
//! - **The Hand 🤚** = mise task 経由の PtySlot で立つ (LaneCapabilities では host しない)
//!
//! ## 実装状態
//!
//! - PR-β-1 (VP-119、 本 PR): **受け皿 struct のみ**、 既存挙動への影響ゼロ。
//!   全 Stand field を Option で予約、 物理 host は後続 PR で。
//! - PR-β-2 (planned): PP を `ProjectStandsPool.paisley_park` から本 struct の
//!   `paisley_park` field に物理移管、 2026-04-27 rule comment update。
//! - PR-β-3 (planned): caller migration (`mcp__show` を Lane PP 経由に rewire)。
//! - PR-β-4 (cleanup planned): catalog row 更新、 doc 12 §10 既存実装表 update。
//! - PR-γ (planned): GE を Lane instance 化 (PR-β series と同 pattern)。
//!
//! 関連: doc 12 (`docs/design/12-stand-architecture.md` §9 Catalog)、
//! doc 13 (`docs/design/13-paisley-park-revival.md` §3 / §9)、
//! Linear VP-109 (parent epic)、 VP-119 (本 PR)。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::lanes_state::LaneAddress;
use super::project_stands_state::PaisleyParkState;

/// Lane 階層 Stand container (Lane あたり 1 instance)。
///
/// PR-β-1 (VP-119) 受け皿: 全 Stand field を Option で予約、 物理 host は PR-β-2 (PP) /
/// PR-γ (GE) で。 Echoes / The Hand は mise task PtySlot 経由なので本 struct には host しない
/// (`LanePool` の各 Lane entry の PtySlot で扱う、 doc 13 §10 Q-7 暫定確定)。
pub struct LaneCapabilities {
    /// Lane の identity (Lead / Worker、 project 名 + name)
    pub address: LaneAddress,

    /// mise task 名 (例: `"echoes"` / `"shell"` / `"tmux"`、 PR-pre2 で `"hd"` → `"echoes"` rename)。
    /// `LanePool` の Lane entry と同期、 spawn 経路で参照される。
    pub stand: String,

    /// Paisley Park 🧭 — Information Router (target = Lane instance、 doc 13 §3 P4)。
    /// PR-β-1 では None placeholder、 PR-β-2 で `ProjectStandsPool.paisley_park` から
    /// 物理移管して `Some(...)` に。 doc 13 §10 Q-1 で Worker Lane の常時 spawn vs on-demand を
    /// 暫定: 常時 spawn (= None の Lane が無くなる) だが、 本 PR では受け皿のみ。
    pub paisley_park: Option<Arc<RwLock<PaisleyParkState>>>,
    // PR-γ で追加予定:
    //   pub gold_experience: Option<Arc<RwLock<GoldExperienceState>>>,
}

impl LaneCapabilities {
    /// 新規構築 (全 Stand None placeholder、 PR-β-1 受け皿)。
    ///
    /// PR-α-1 の `WorldCapabilities::new` と同 pattern (= midi None で構築、 `with_midi` で host)。
    /// 本 struct も将来 `with_paisley_park` factory が PR-β-2 で追加される予定。
    pub fn new(address: LaneAddress, stand: impl Into<String>) -> Self {
        Self {
            address,
            stand: stand.into(),
            paisley_park: None,
        }
    }
}

/// Lane scope Stand pool — SP per Project で 1 instance、 各 Lane の `LaneCapabilities` を集約。
///
/// PR-β-1 受け皿: **空 HashMap で構築**、 spawn 時の entry 追加 logic は PR-β-2 (PP 物理移管時)
/// で実装。 既存 `LanePool` (`lanes_state.rs`) とは並立する独立 pool で、 移管完了まで
/// data の duplicate 共存を許容 (doc 13 §1 の supersede 明示と同 pattern)。
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_capabilities_new_smoke() {
        // PR-β-1 受け皿: 全 Stand field None で構築
        let addr = LaneAddress::lead("vp");
        let lc = LaneCapabilities::new(addr.clone(), "echoes");

        assert_eq!(lc.address, addr);
        assert_eq!(lc.stand, "echoes");
        assert!(
            lc.paisley_park.is_none(),
            "PR-β-1 受け皿は paisley_park None (PR-β-2 で物理移管時に Some 化)"
        );
    }

    #[test]
    fn lane_capabilities_with_worker_address() {
        // Worker Lane でも同 pattern で構築可能
        let addr = LaneAddress::worker("vp", "feat-test");
        let lc = LaneCapabilities::new(addr.clone(), "shell");

        assert_eq!(lc.address, addr);
        assert_eq!(lc.stand, "shell");
        assert!(lc.paisley_park.is_none());
    }

    #[test]
    fn lane_capabilities_pool_empty_default() {
        // PR-β-1 受け皿の Pool は空 HashMap で初期化
        let pool = LaneCapabilitiesPool::new();
        assert_eq!(pool.count(), 0);
        assert!(pool.entries.is_empty(), "PR-β-1 では空 map");
    }

    #[test]
    fn lane_capabilities_pool_can_insert() {
        // entry 追加可能性の確認 (PR-β-2 で実 logic で使う想定)
        let mut pool = LaneCapabilitiesPool::new();
        let addr = LaneAddress::lead("vp");
        let lc = Arc::new(LaneCapabilities::new(addr.clone(), "echoes"));

        pool.entries.insert(addr.clone(), lc);
        assert_eq!(pool.count(), 1);
        assert!(pool.entries.contains_key(&addr));
    }
}
