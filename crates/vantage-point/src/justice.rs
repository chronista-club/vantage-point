//! Justice 🌫️ — Lane scope の双方向 device I/O endpoint (doc 23 §6)
//!
//! Bastet 🧲 (World scope の device registry) が集約した物理 device に対し、
//! Lane 単位の双方向 I/O を担う。
//!
//! 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`
//!
//! 2 片方向 flow (doc 23 §6.2):
//! - **input**: Bastet が parse した ControlEvent を「active Lane = 自分」のときだけ受け取り、
//!   Lane command context に着地させる
//! - **output**: Lane state 変化を subscribe → DeviceProfile で byte 化 → Bastet の out port 経由送出
//!
//! ## 実装状態 (sub-PR 追跡)
//!
//! | sub-PR | scope | status |
//! |--------|-------|--------|
//! | E3-1 | JusticeStand (LaneStandHost impl) 型 + LaneStandRegistry insert | ← 本 PR |
//! | E3-2 | output projection: lane state subscribe → DeviceProfile → send_batch 委譲 | planned |

use std::any::Any;

use tokio::sync::RwLock;

use crate::process::lane_stand::LaneStandHost;

// ─── data ──────────────────────────────────────────────────

/// Justice 🌫️ の Lane-local state。
///
/// E3-1 は受け皿のみ（i 路線）。E3-2 で `profiles: Vec<Box<dyn DeviceProfile>>` 等を追加し、
/// Lane state → device projection の経路を構築する。
#[derive(Debug, Default)]
pub struct JusticeState {
    /// placeholder — E3-2 で device profile binding を追加
    _private: (),
}

// ─── JusticeStand ──────────────────────────────────────────

/// Lane に host される device I/O endpoint（Justice 🌫️）。
///
/// `PaisleyParkStand` と同型の **passive marker**（`LaneStandHost` impl）。
/// `LaneCapabilities::new()` で各 Lane に自動登録される（midi feature 有効時）。
///
/// `stand_kind() = "justice"` を内部 ID として `LaneStandRegistry` の HashMap key に使う。
pub struct JusticeStand {
    state: RwLock<JusticeState>,
}

impl JusticeStand {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(JusticeState::default()),
        }
    }

    /// internal `RwLock<JusticeState>` への参照。
    pub fn state(&self) -> &RwLock<JusticeState> {
        &self.state
    }
}

impl Default for JusticeStand {
    fn default() -> Self {
        Self::new()
    }
}

impl LaneStandHost for JusticeStand {
    fn stand_kind(&self) -> &'static str {
        "justice"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ─── tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stand_kind_is_justice() {
        let stand = JusticeStand::new();
        assert_eq!(stand.stand_kind(), "justice");
    }

    #[test]
    fn default_state_is_empty() {
        let stand = JusticeStand::default();
        assert_eq!(stand.stand_kind(), "justice");
    }

    #[test]
    fn supports_downcast() {
        let stand = JusticeStand::new();
        let host: &dyn LaneStandHost = &stand;
        let downcast = host.as_any().downcast_ref::<JusticeStand>();
        assert!(downcast.is_some());
    }
}
