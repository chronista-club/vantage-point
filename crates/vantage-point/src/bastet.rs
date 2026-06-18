//! Bastet 🧲 — World scope の物理 device 集約 registry (doc 23 §5)
//!
//! 現 `MidiCapability` (single-device monitor) を multi-device registry に発展させる。
//!
//! 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`
//!
//! 責務 (doc 23 §5.3):
//! - **registry**: 接続中 device を `HashMap<port_displayName, ConnectedDevice>` で hold
//! - **hot-plug discovery**: midir enumeration polling (2〜3s) で接続/切断検出
//! - **input parse**: device byte → `DeviceInput::parse` → `ControlEvent` 化 (E2-3)
//! - **routing policy**: `ControlEvent` を active Lane の Justice へ dispatch (E2-3)
//! - **active Lane track**: SP の「lanes」QUIC channel を購読し cache 更新 (E2-3)

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

use crate::capability::core::CapabilityEvent;
use crate::capability::eventbus::EventBus;
use crate::capability::stand_service::{LayerScope, Service};
use crate::process::lanes_state::LaneAddress;

/// hot-plug polling 間隔（doc 23 Q-4: 2〜3s、体感重視）
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);

// ─── data ──────────────────────────────────────────────────

/// Bastet registry に登録される接続中の物理 device (doc 23 §5.2)。
///
/// HashMap の value 型。key = CoreMIDI port の displayName。
#[derive(Debug)]
pub struct ConnectedDevice {
    /// CoreMIDI port の displayName（HashMap key と一致）
    pub port_name: String,
    /// input port（device → VP）が存在するか
    pub has_input: bool,
    /// output port（VP → device）が存在するか
    pub has_output: bool,
    /// registry に登録された時刻
    pub connected_at: Instant,
}

/// `compute_diff` の出力 — 前回 scan との差分
struct DiscoveryDiff {
    added: Vec<(String, bool, bool)>,
    removed: Vec<String>,
}

// ─── calculations（純粋）──────────────────────────────────

/// 既知の device map と最新 scan 結果の diff を計算（純粋関数）
fn compute_diff(
    known: &HashMap<String, ConnectedDevice>,
    current: &HashMap<String, (bool, bool)>,
) -> DiscoveryDiff {
    let added: Vec<_> = current
        .iter()
        .filter(|(name, _)| !known.contains_key(*name))
        .map(|(name, &(has_in, has_out))| (name.clone(), has_in, has_out))
        .collect();

    let removed: Vec<_> = known
        .keys()
        .filter(|name| !current.contains_key(*name))
        .cloned()
        .collect();

    DiscoveryDiff { added, removed }
}

// ─── actions（I/O）────────────────────────────────────────

/// midir で input + output の全 port を enumeration し、displayName → (has_input, has_output) の map を返す。
/// 物理デバイスは同じ displayName で input/output 両方のポートを持つため、名前で merge する。
fn enumerate_ports() -> HashMap<String, (bool, bool)> {
    let mut result: HashMap<String, (bool, bool)> = HashMap::new();

    if let Ok(midi_in) = midir::MidiInput::new("vp-bastet-scan") {
        for port in midi_in.ports() {
            if let Ok(name) = midi_in.port_name(&port) {
                result.entry(name).or_insert((false, false)).0 = true;
            }
        }
    }

    if let Ok(midi_out) = midir::MidiOutput::new("vp-bastet-scan") {
        for port in midi_out.ports() {
            if let Ok(name) = midi_out.port_name(&port) {
                result.entry(name).or_insert((false, false)).1 = true;
            }
        }
    }

    result
}

// ─── Bastet struct ─────────────────────────────────────────

/// World scope の物理 device 集約 registry（Bastet 🧲）。
///
/// key = CoreMIDI port の displayName（背骨 mem 準拠、doc 23 §5.2）。
/// 現 `MidiCapability` の single-device monitor を multi-device registry に発展させる。
pub struct Bastet {
    /// 接続中 device を port displayName で引く（polling task と共有）
    devices: Arc<RwLock<HashMap<String, ConnectedDevice>>>,
    /// active Lane の購読 cache（SSOT は SP の lanes_state、Bastet は購読側。doc 23 Q-1）
    active_lane: Arc<RwLock<Option<LaneAddress>>>,
    /// Capability event bus（接続/切断イベント配信用）
    event_bus: Arc<EventBus>,
    /// hot-plug polling task handle
    discovery_task: Option<JoinHandle<()>>,
    /// discovery cancel signal
    cancel_tx: Option<mpsc::Sender<()>>,
}

impl Bastet {
    /// 空の registry で構築
    pub fn new(event_bus: Arc<EventBus>) -> Self {
        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
            active_lane: Arc::new(RwLock::new(None)),
            event_bus,
            discovery_task: None,
            cancel_tx: None,
        }
    }

    /// 接続中 device 数
    pub async fn device_count(&self) -> usize {
        self.devices.read().await.len()
    }

    /// devices registry の read handle
    pub fn devices(&self) -> &Arc<RwLock<HashMap<String, ConnectedDevice>>> {
        &self.devices
    }

    /// active Lane cache の read handle（Justice dispatch / 外部クエリ用）
    pub fn active_lane(&self) -> &Arc<RwLock<Option<LaneAddress>>> {
        &self.active_lane
    }

    /// event bus の read handle
    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    /// discovery が稼働中か
    pub fn is_discovering(&self) -> bool {
        self.discovery_task
            .as_ref()
            .is_some_and(|t| !t.is_finished())
    }

    /// hot-plug discovery を開始（2s 周期で port enumeration → diff → devices 更新）
    pub async fn start_discovery(&mut self) {
        if self.is_discovering() {
            return;
        }

        let devices = Arc::clone(&self.devices);
        let event_bus = Arc::clone(&self.event_bus);
        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        self.cancel_tx = Some(cancel_tx);

        let task = tokio::spawn(async move {
            tracing::info!(
                "Bastet 🧲 discovery started (interval: {}s)",
                DISCOVERY_INTERVAL.as_secs()
            );

            loop {
                let current = enumerate_ports();

                let mut devs = devices.write().await;
                let diff = compute_diff(&devs, &current);

                for (name, has_in, has_out) in &diff.added {
                    tracing::info!(
                        "🧲 device connected: {} (in={}, out={})",
                        name,
                        has_in,
                        has_out
                    );
                    devs.insert(
                        name.clone(),
                        ConnectedDevice {
                            port_name: name.clone(),
                            has_input: *has_in,
                            has_output: *has_out,
                            connected_at: Instant::now(),
                        },
                    );
                    let event = CapabilityEvent::new("bastet.device_connected", "bastet")
                        .with_payload(&serde_json::json!({
                            "port_name": name,
                            "has_input": has_in,
                            "has_output": has_out,
                        }));
                    event_bus.emit(event).await;
                }

                for name in &diff.removed {
                    tracing::info!("🧲 device disconnected: {}", name);
                    devs.remove(name);
                    let event = CapabilityEvent::new("bastet.device_disconnected", "bastet")
                        .with_payload(&serde_json::json!({ "port_name": name }));
                    event_bus.emit(event).await;
                }

                // RwLock を release してから sleep
                drop(devs);

                tokio::select! {
                    _ = cancel_rx.recv() => {
                        tracing::info!("Bastet 🧲 discovery stopped");
                        break;
                    }
                    _ = tokio::time::sleep(DISCOVERY_INTERVAL) => {}
                }
            }
        });

        self.discovery_task = Some(task);
    }

    /// hot-plug discovery を停止
    pub async fn stop_discovery(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(()).await;
        }
        if let Some(task) = self.discovery_task.take() {
            task.abort();
        }
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

    #[tokio::test]
    async fn new_creates_empty_registry() {
        let bus = Arc::new(EventBus::new());
        let bastet = Bastet::new(bus);
        assert_eq!(bastet.device_count().await, 0);
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

    // ─── compute_diff tests（pure）─────────────────────

    #[test]
    fn diff_detects_new_device() {
        let known = HashMap::new();
        let mut current = HashMap::new();
        current.insert("X-Touch Compact".to_string(), (true, true));

        let diff = compute_diff(&known, &current);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].0, "X-Touch Compact");
        assert!(diff.added[0].1); // has_input
        assert!(diff.added[0].2); // has_output
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_detects_removed_device() {
        let mut known = HashMap::new();
        known.insert(
            "LPD8 mk2".to_string(),
            ConnectedDevice {
                port_name: "LPD8 mk2".to_string(),
                has_input: true,
                has_output: true,
                connected_at: Instant::now(),
            },
        );
        let current = HashMap::new();

        let diff = compute_diff(&known, &current);
        assert!(diff.added.is_empty());
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0], "LPD8 mk2");
    }

    #[test]
    fn diff_no_change() {
        let mut known = HashMap::new();
        known.insert(
            "ROTO".to_string(),
            ConnectedDevice {
                port_name: "ROTO".to_string(),
                has_input: true,
                has_output: true,
                connected_at: Instant::now(),
            },
        );
        let mut current = HashMap::new();
        current.insert("ROTO".to_string(), (true, true));

        let diff = compute_diff(&known, &current);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_simultaneous_add_and_remove() {
        let mut known = HashMap::new();
        known.insert(
            "Old Device".to_string(),
            ConnectedDevice {
                port_name: "Old Device".to_string(),
                has_input: true,
                has_output: false,
                connected_at: Instant::now(),
            },
        );
        let mut current = HashMap::new();
        current.insert("New Device".to_string(), (false, true));

        let diff = compute_diff(&known, &current);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].0, "New Device");
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0], "Old Device");
    }

    // ─── discovery lifecycle ───────────────────────────

    #[tokio::test]
    async fn discovery_lifecycle() {
        let bus = Arc::new(EventBus::new());
        let mut bastet = Bastet::new(bus);

        assert!(!bastet.is_discovering());

        bastet.start_discovery().await;
        assert!(bastet.is_discovering());

        // 二重起動は no-op
        bastet.start_discovery().await;

        bastet.stop_discovery().await;
        // task abort 後は is_discovering = false
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!bastet.is_discovering());
    }
}
