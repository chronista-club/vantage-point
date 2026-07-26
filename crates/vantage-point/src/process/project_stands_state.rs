//! Paisley Park 🧭 の Lane Stand 実体（`PaisleyParkState` + `LaneStandHost` wrapper）。
//!
//! ## Project scope pool は消滅した（doc 44 P1 露払い、2026-07-20）
//!
//! 本 module は元々 Project scope の Stand pool (`ProjectStandsPool`) を提供していたが、
//! Stand は順次 Lane scope へ移管され、最後まで残っていた GE 🌿 の skeleton
//! (`GoldExperienceState`) も **一度も read されないまま**残置されていた。
//! doc 44 P1 で `AppState.project_stands` を除去した結果 pool は到達不能になったため、
//! pool・GE skeleton とも削除した（PR-γ の「GE を Lane 移管したら pool が空になる」
//! という見通しに、pool 側から先に到達した形）。
//!
//! 旧 External Control stand は PR-α で World 移管 + epic v3.1 で
//! Bastet 🧲 (World device registry) / Justice 🌫️ (Lane device I/O) に再編済。
//!
//! ## 現在ここが提供するもの
//!
//! - `PaisleyParkState` — Canvas content の data model (content + content_type)
//! - `PaisleyParkStand` — それを `LaneStandHost` trait に適合させる wrapper。
//!   `LaneCapabilities.registry` が Lane あたり 1 instance を host する (PR-δ-2 / VP-136)。
//!
//! 関連: doc 12 LSCM (VP-109) — Layer container は World/Project/Lane の 3 kind、
//! 各 Stand の居住可能 Layer は doc 12 §9 catalog の「保持 layer pattern」列が定める。

use std::any::Any;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::lane_stand::LaneStandHost;

/// PP (Paisley Park) — Canvas content store (PR-β-2 (VP-120) で Lane あたり 1 instance に物理移管)
///
/// PR-β-2 で Lane 移管、 PR-δ-2 (VP-136) で `PaisleyParkStand` wrapper 経由 host に進化。
/// data model 自体は変わらず content + content_type の serde 直列化可能 struct。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaisleyParkState {
    /// Canvas 表示中の content (HTML/MD/markdown body)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// content の MIME (例: "text/html", "text/markdown")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// PaisleyParkStand — `PaisleyParkState` を `LaneStandHost` trait に適合させる wrapper (PR-δ-2、 VP-136)。
///
/// PR-δ-1 (VP-135) で新設の `LaneStandHost` trait の **最初の impl**。 internal mutability 用に
/// `RwLock<PaisleyParkState>` を持ち、 caller は `state()` accessor 経由で Read/Write する。
///
/// `stand_kind() = "paisley_park"` を ID として Registry の HashMap key に使われる。
///
/// ## 関連
///
/// - PR-δ-1 (#288 / VP-135) — `LaneStandHost` trait + `LaneStandRegistry` 受け皿
/// - PR-δ-2 (本 PR / VP-136) — PP impl + LaneCapabilities 統合
/// - doc 13 §9 boundary invariant 「N Stand を host できる generic interface」 への path
pub struct PaisleyParkStand {
    // 要確認（audit 2026-07-18、先行実装の可能性）: Phase A4-2b skeleton（module doc 参照）。
    #[allow(dead_code)]
    state: RwLock<PaisleyParkState>,
}

impl PaisleyParkStand {
    /// 新規構築 (state は default = content/content_type 共に None)。
    pub fn new() -> Self {
        Self {
            state: RwLock::new(PaisleyParkState::default()),
        }
    }

    /// internal `RwLock<PaisleyParkState>` への参照 (caller が Read/Write する)。
    // 要確認（audit 2026-07-18、先行実装の可能性）: Phase A4-2b skeleton（module doc 参照）。
    #[allow(dead_code)]
    pub fn state(&self) -> &RwLock<PaisleyParkState> {
        &self.state
    }
}

impl Default for PaisleyParkStand {
    fn default() -> Self {
        Self::new()
    }
}

impl LaneStandHost for PaisleyParkStand {
    fn stand_kind(&self) -> &'static str {
        "paisley_park"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paisley_park_stand_reports_its_kind() {
        let stand = PaisleyParkStand::new();
        assert_eq!(stand.stand_kind(), "paisley_park");
    }

    #[test]
    fn paisley_park_stand_starts_empty() {
        let stand = PaisleyParkStand::new();
        let state = stand.state().blocking_read();
        assert!(state.content.is_none());
        assert!(state.content_type.is_none());
    }
}
