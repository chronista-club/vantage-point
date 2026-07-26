//! Board 🧭 の Lane Stand 実体（`BoardState` + `LaneStandHost` wrapper）。
//!
//! ## Project scope pool は消滅した（doc 44 P1 露払い、2026-07-20）
//!
//! 本 module は元々 Project scope の Stand pool (`ProjectStandsPool`) を提供していたが、
//! Stand は順次 Lane scope へ移管され、最後まで残っていた runner 🌿 の skeleton
//! (`RunnerState` 相当) も **一度も read されないまま**残置されていた。
//! doc 44 P1 で `AppState.project_stands` を除去した結果 pool は到達不能になったため、
//! pool・runner skeleton とも削除した（PR-γ の「runner を Lane 移管したら pool が空になる」
//! という見通しに、pool 側から先に到達した形）。
//!
//! 旧 External Control stand は PR-α で World 移管 + epic v3.1 で
//! DeviceRegistry 🧲 (World device registry) / Device I/O 🌫️ (Lane device I/O) に再編済。
//!
//! ## 現在ここが提供するもの
//!
//! - `BoardState` — Canvas content の data model (content + content_type)
//! - `BoardStand` — それを `LaneStandHost` trait に適合させる wrapper。
//!   `LaneCapabilities.registry` が Lane あたり 1 instance を host する (PR-δ-2 / VP-136)。
//!
//! 関連: doc 12 LSCM (VP-109) — Layer container は World/Project/Lane の 3 kind、
//! 各 Stand の居住可能 Layer は doc 12 §9 catalog の「保持 layer pattern」列が定める。

use std::any::Any;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::lane_stand::LaneStandHost;

/// board (Board) — Canvas content store (PR-β-2 (VP-120) で Lane あたり 1 instance に物理移管)
///
/// PR-β-2 で Lane 移管、 PR-δ-2 (VP-136) で `BoardStand` wrapper 経由 host に進化。
/// data model 自体は変わらず content + content_type の serde 直列化可能 struct。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardState {
    /// Canvas 表示中の content (HTML/MD/markdown body)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// content の MIME (例: "text/html", "text/markdown")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// BoardStand — `BoardState` を `LaneStandHost` trait に適合させる wrapper (PR-δ-2、 VP-136)。
///
/// PR-δ-1 (VP-135) で新設の `LaneStandHost` trait の **最初の impl**。 internal mutability 用に
/// `RwLock<BoardState>` を持ち、 caller は `state()` accessor 経由で Read/Write する。
///
/// `stand_kind() = "board"` を ID として Registry の HashMap key に使われる。
///
/// ## 関連
///
/// - PR-δ-1 (#288 / VP-135) — `LaneStandHost` trait + `LaneStandRegistry` 受け皿
/// - PR-δ-2 (本 PR / VP-136) — board impl + LaneCapabilities 統合
/// - doc 13 §9 boundary invariant 「N Stand を host できる generic interface」 への path
pub struct BoardStand {
    // 要確認（audit 2026-07-18、先行実装の可能性）: Phase A4-2b skeleton（module doc 参照）。
    #[allow(dead_code)]
    state: RwLock<BoardState>,
}

impl BoardStand {
    /// 新規構築 (state は default = content/content_type 共に None)。
    pub fn new() -> Self {
        Self {
            state: RwLock::new(BoardState::default()),
        }
    }

    /// internal `RwLock<BoardState>` への参照 (caller が Read/Write する)。
    // 要確認（audit 2026-07-18、先行実装の可能性）: Phase A4-2b skeleton（module doc 参照）。
    #[allow(dead_code)]
    pub fn state(&self) -> &RwLock<BoardState> {
        &self.state
    }
}

impl Default for BoardStand {
    fn default() -> Self {
        Self::new()
    }
}

impl LaneStandHost for BoardStand {
    fn stand_kind(&self) -> &'static str {
        "board"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_stand_reports_its_kind() {
        let stand = BoardStand::new();
        assert_eq!(stand.stand_kind(), "board");
    }

    #[test]
    fn board_stand_starts_empty() {
        let stand = BoardStand::new();
        let state = stand.state().blocking_read();
        assert!(state.content.is_none());
        assert!(state.content_type.is_none());
    }
}
