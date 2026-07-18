//! Project scope の Stand pool — GE の registry (PP は Lane 移管済、 HP→Bastet 🧲 は World 移管で本 pool から除去)
//!
//! 関連 memory:
//! - 「多 scope architecture + protocol/msg 連携」rule (2026-04-27 確定) は
//!   doc 12 LSCM (VP-109 / 2026-05-04) で **明示的に supersede** された。
//!   現 LSCM の Layer container は World/Project/Lane の 3 kind で、 各 Stand 種は
//!   doc 12 §9 catalog の「保持 layer pattern」 列で居住可能 Layer が定められる。
//! - PR-β-2 (VP-120 / 2026-05-04): **PP を Project → Lane に物理移管**、 本 struct から
//!   `paisley_park` field を削除。 PR-γ (planned) で GE も Lane 移管予定、 完了後に
//!   ProjectStandsPool 自体が空 (= 削除可能) になる予想。
//! - PR-δ-2 (VP-136 / 2026-05-06): PP を **`LaneStandHost` trait impl 化** (PaisleyParkStand)、
//!   `LaneCapabilities` の hardcoded field から `LaneStandRegistry` 経由 host に置換。
//!   doc 13 §9 boundary invariant 「N Stand を host できる generic interface」 を実現。
//!
//! ## 現状 (PR-δ-2 後)
//!
//! Project scope に残る Stand:
//! - GE 🌿 Gold Experience — Code Runner (1 / project、 PR-γ で Lane 移管予定)
//!
//! 旧 HP 🍇 Hermit Purple (External Control) は PR-α で World 移管 + epic v3.1 で
//! Bastet 🧲 (World device registry) / Justice 🌫️ (Lane device I/O) に再編。
//! 死蔵していた HermitPurpleState skeleton は E2-0 で削除済。
//!
//! Lane 移管完了済:
//! - PP 🧭 Paisley Park — `LaneCapabilities.registry` で host (PR-δ-2)、
//!   wrapper struct = `PaisleyParkStand` (LaneStandHost trait impl)
//!
//! Phase A4-2b の skeleton という位置付けは継続、 各 state は最小実装。
//! 実 Stand 操作 (Ruby eval / MIDI 制御) は既存 routes/handler 経由で動いており、
//! ここはそれらを Project scope の概念として位置付けるための data model。

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

/// GE (Gold Experience) — Code Runner state (1 / project)
///
/// 既存の Ruby eval / process_runner 関連はここの state を読み書きする想定 (gradual migration)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoldExperienceState {
    /// 直近の eval 結果 (簡素化、A4-2b では skeleton)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_eval: Option<String>,
}

/// Project scope の Stand pool (GE を集約、 PP は Lane 移管済、 HP→Bastet は World 移管)
///
/// PR-β-2 (VP-120): `paisley_park` field を **削除**、 PP は `LaneCapabilities.registry`
/// (PR-δ-2 / VP-136 で `LaneStandRegistry` 経由 host に進化、 wrapper struct =
/// `PaisleyParkStand`) で Lane あたり独立 instance に物理移管。 epic v3.1 E2-0 で旧
/// `hermit_purple` field (HermitPurpleState skeleton) も削除 (External Control は
/// World/Bastet 🧲 + Lane/Justice 🌫️ へ再編)。 残る GE は Project あたり 1 instance、
/// PR-γ で Lane 移管予定 (完了後に本 pool 自体が削除可能になる)。
#[derive(Debug, Default)]
pub struct ProjectStandsPool {
    // 要確認（audit 2026-07-18、先行実装の可能性）: Phase A4-2b skeleton（module doc 参照）。
    #[allow(dead_code)]
    pub gold_experience: GoldExperienceState,
}

impl ProjectStandsPool {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_stands_pool_default_empty() {
        // PR-β-2 (VP-120): paisley_park は LaneCapabilities に物理移管、 本 pool では確認不要。
        // epic v3.1 E2-0: hermit_purple skeleton 削除済 (External Control は World/Bastet + Lane/Justice へ)
        let pool = ProjectStandsPool::new();
        assert!(pool.gold_experience.last_eval.is_none());
    }
}
