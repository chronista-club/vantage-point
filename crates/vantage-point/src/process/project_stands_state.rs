//! Project scope の Stand pool — GE/HP の registry (PP は PR-β-2 で Lane 移管済)
//!
//! 関連 memory:
//! - 「多 scope architecture + protocol/msg 連携」rule (2026-04-27 確定) は
//!   doc 12 LSCM (VP-109 / 2026-05-04) で **明示的に supersede** された。
//!   現 LSCM の Layer container は World/Project/Lane の 3 kind で、 各 Stand 種は
//!   doc 12 §9 catalog の「保持 layer pattern」 列で居住可能 Layer が定められる。
//! - PR-β-2 (VP-120 / 2026-05-04): **PP を Project → Lane に物理移管**、 本 struct から
//!   `paisley_park` field を削除。 PR-γ (planned) で GE も Lane 移管予定、 完了後に
//!   ProjectStandsPool 自体が空 (= 削除可能) になる予想。
//!
//! ## 現状 (PR-β-2 後)
//!
//! Project scope に残る Stand:
//! - GE 🌿 Gold Experience — Code Runner (1 / project、 PR-γ で Lane 移管予定)
//! - HP 🍇 Hermit Purple   — External Control (target = World、 PR-α 完了で SP 側 capability から取り外し済、
//!   ただし HermitPurpleState skeleton は本 pool に残存)
//!
//! Lane 移管完了済:
//! - PP 🧭 Paisley Park — `LaneCapabilities.paisley_park` (PR-β-2 / VP-120)
//!
//! Phase A4-2b の skeleton という位置付けは継続、 各 state は最小実装。
//! 実 Stand 操作 (Ruby eval / MIDI 制御) は既存 routes/handler 経由で動いており、
//! ここはそれらを Project scope の概念として位置付けるための data model。

use serde::{Deserialize, Serialize};

/// PP (Paisley Park) — Canvas content store (PR-β-2 (VP-120) で Lane あたり 1 instance に物理移管)
///
/// PR-β-2 以降 `LaneCapabilities.paisley_park` で host (Lane あたり独立 instance、
/// doc 12 §9 catalog `target = Lane instance` を実現)。 struct 定義自体は本 module に
/// 残存 (LaneCapabilities が `use` する型として)。 将来 GE / HP も Lane 移管完了後に
/// ProjectStandsPool 全体が空になる予想で、 そのタイミングで struct も別 module に move 検討。
///
/// 旧 caller (Canvas 関連 routes `/api/canvas/...`) は実装上 access していなかった
/// (PR-β-2 grep 検証で field caller ゼロ確認)、 PR-β-3 の caller migration が trivial
/// (= 空) と判明したため doc 13 §9 PR-β series roadmap も 5 sub → 4 sub に縮小。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaisleyParkState {
    /// Canvas 表示中の content (HTML/MD/markdown body)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// content の MIME (例: "text/html", "text/markdown")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
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

/// HP (Hermit Purple) — External Control state (1 / project)
///
/// 既存の MIDI / MCP / tmux module はここの state を読み書きする想定 (gradual migration)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HermitPurpleState {
    /// MIDI 接続状態 (簡素化、A4-2b では skeleton)
    pub midi_connected: bool,
    pub mcp_connected: bool,
    pub tmux_connected: bool,
}

/// Project scope の Stand pool (GE/HP を集約、 PR-β-2 で PP は Lane 移管済)
///
/// PR-β-2 (VP-120): `paisley_park` field を **削除**、 PP は `LaneCapabilities.paisley_park`
/// で Lane あたり独立 instance に物理移管。 残る GE / HP は Project あたり 1 instance、
/// PR-γ で GE も Lane 移管予定。
#[derive(Debug, Default)]
pub struct ProjectStandsPool {
    pub gold_experience: GoldExperienceState,
    pub hermit_purple: HermitPurpleState,
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
        // PR-β-2 (VP-120): paisley_park は LaneCapabilities に物理移管、 本 pool では確認不要
        let pool = ProjectStandsPool::new();
        assert!(pool.gold_experience.last_eval.is_none());
        assert!(!pool.hermit_purple.midi_connected);
    }
}
