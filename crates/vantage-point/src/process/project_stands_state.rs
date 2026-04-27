//! Project scope の Stand pool — PP/GE/HP の registry
//!
//! 関連 memory:
//! - 「多 scope architecture + protocol/msg 連携」rule (2026-04-27 確定):
//!   Project scope に attach する Stand は PP/GE/HP の 3 つ。
//! - `mem_1CaSrCxysdGaaSsN4Dvxth` (3 段 → 4 scope に拡張)
//!
//! ## 概念
//!
//! Project scope (= SP per Project) に attached する Stand:
//! - PP 🧭 Paisley Park   — Canvas (1 / project)
//! - GE 🌿 Gold Experience — Code Runner (1 / project)
//! - HP 🍇 Hermit Purple   — External Control (1 / project)
//!
//! Phase A4-2b では **skeleton のみ** (具体 state は最小)。
//! 実 Stand 操作 (Canvas render / Ruby eval / MIDI 制御) は既存 routes/handler 経由で動いており、
//! ここはそれらを Project scope の概念として位置付けるための data model。

use serde::{Deserialize, Serialize};

/// PP (Paisley Park) — Canvas content store (1 / project)
///
/// 既存の Canvas 関連 routes (`/api/canvas/...`) はここの state を読み書きする想定 (gradual migration)。
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

/// Project scope の Stand pool (PP/GE/HP を集約)
///
/// memory rule: PP/GE/HP は Project あたり 1 つずつ。SP の AppState に attach。
#[derive(Debug, Default)]
pub struct ProjectStandsPool {
    pub paisley_park: PaisleyParkState,
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
        let pool = ProjectStandsPool::new();
        assert!(pool.paisley_park.content.is_none());
        assert!(pool.gold_experience.last_eval.is_none());
        assert!(!pool.hermit_purple.midi_connected);
    }
}
