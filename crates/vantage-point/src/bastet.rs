//! Bastet 🧲 — World scope の物理 device 集約 registry (doc 23 §5)
//!
//! 現 `MidiCapability` (single-device monitor) を multi-device registry に発展させる。
//! E2-1: struct 定義 + `Service` impl のみ（「受け皿」）。lifecycle は E2-2 以降。
//!
//! 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`
//!
//! 責務 (doc 23 §5.3):
//! - **registry**: 接続中 device を `HashMap<port_displayName, ConnectedDevice>` で hold
//! - **hot-plug discovery**: midir enumeration polling (2〜3s) で接続/切断検出
//! - **input parse**: device byte → `DeviceInput::parse` → `ControlEvent` 化
//! - **routing policy**: `ControlEvent` を active Lane の Justice へ dispatch
//! - **active Lane track**: SP の「lanes」QUIC channel を購読し cache 更新

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::capability::eventbus::EventBus;
use crate::capability::stand_service::{LayerScope, Service};
use crate::process::lanes_state::LaneAddress;

// ─── data ──────────────────────────────────────────────────

/// Bastet registry に登録される接続中の物理 device (doc 23 §5.2)。
///
/// HashMap の value 型。E2-2 で in/out port handle・`DeviceInput` / `DeviceProfile` を保持する。
#[derive(Debug)]
pub struct ConnectedDevice {
    /// CoreMIDI port の displayName（HashMap key と一致）
    pub port_name: String,
}

// ─── Bastet struct ─────────────────────────────────────────

/// World scope の物理 device 集約 registry（Bastet 🧲）。
///
/// key = CoreMIDI port の displayName（背骨 mem 準拠、doc 23 §5.2）。
/// 現 `MidiCapability` の single-device monitor を multi-device registry に発展させる。
pub struct Bastet {
    /// 接続中 device を port displayName で引く
    devices: HashMap<String, ConnectedDevice>,
    /// active Lane の購読 cache（SSOT は SP の lanes_state、Bastet は購読側。doc 23 Q-1）
    active_lane: Arc<RwLock<Option<LaneAddress>>>,
    /// Capability event bus（接続/切断イベント配信用）
    event_bus: Arc<EventBus>,
}

impl Bastet {
    /// 空の registry で構築（E2-2 で hot-plug discovery が device を populate する）
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            devices: HashMap::new(),
            active_lane: Arc::new(RwLock::new(None)),
            event_bus,
        }
    }

    /// 接続中 device 数
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// active Lane cache の read handle（Justice dispatch / 外部クエリ用）
    pub fn active_lane(&self) -> &Arc<RwLock<Option<LaneAddress>>> {
        &self.active_lane
    }

    /// event bus の read handle
    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }
}

// ─── Service impl ──────────────────────────────────────────

impl Service for Bastet {
    fn actor_name(&self) -> &str {
        "bastet"
    }

    fn layer_scope(&self) -> LayerScope {
        LayerScope::World
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
    fn new_creates_empty_registry() {
        let bus = Arc::new(EventBus::new());
        let bastet = Bastet::new(bus);
        assert_eq!(bastet.device_count(), 0);
    }

    #[test]
    fn service_impl_correct() {
        let bus = Arc::new(EventBus::new());
        let bastet = Bastet::new(bus);
        assert_eq!(bastet.actor_name(), "bastet");
        assert_eq!(bastet.layer_scope(), LayerScope::World);
    }

    #[tokio::test]
    async fn active_lane_initially_none() {
        let bus = Arc::new(EventBus::new());
        let bastet = Bastet::new(bus);
        let lane = bastet.active_lane().read().await;
        assert!(lane.is_none());
    }

    #[test]
    fn service_supports_downcast() {
        let bus = Arc::new(EventBus::new());
        let bastet = Bastet::new(bus);
        let service: &dyn Service = &bastet;
        let downcast = service.as_any().downcast_ref::<Bastet>();
        assert!(downcast.is_some());
    }
}
