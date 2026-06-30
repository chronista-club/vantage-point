//! Bastet 🧲 — World scope の物理 device 集約 registry (doc 23 §5)
//!
//! 現 `MidiCapability` (single-device monitor) を multi-device registry に発展させる。
//!
//! 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`
//!
//! 責務 (doc 23 §5.3):
//! - **registry**: 接続中 device を `HashMap<port_displayName, ConnectedDevice>` で hold
//! - **hot-plug discovery**: midir enumeration polling (2〜3s) で接続/切断検出
//! - **input parse**: device byte → `DeviceInput::parse` → `ControlEvent` 化
//! - **routing policy**: `ControlEvent` を active Lane の Justice へ dispatch (E3)
//! - **active Lane track**: SP の「lanes」QUIC channel を購読し cache 更新 (E3)

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

use tokio_util::sync::CancellationToken;

use nostos::{AsyncDriver, Outcome};

use crate::capability::ProcessManagerCapability;
use crate::capability::core::CapabilityEvent;
use crate::capability::eventbus::EventBus;
use crate::capability::stand_service::{LayerScope, Service};
use crate::commands::roto_control::{
    InProcessLaneSource, QuicSwitchSink, RotoDescriptor, RotoHealDriver, RotoSessionBracket,
    RotoView,
};
use crate::device_input::DeviceInput;
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

/// port name から対応する DeviceInput parser を生成する factory。
/// 未対応の機材は None（parser なし = input 監視対象外）。
fn create_device_input(port_name: &str) -> Option<Box<dyn DeviceInput + Send>> {
    use crate::device_input::roto::RotoInput;

    if port_name.contains("Roto") {
        Some(Box::new(RotoInput::default()))
    } else {
        // X-Touch, LPD8 等の入力 parser は Converge で追加
        None
    }
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

/// 指定 port の MIDI input を listen し、DeviceInput::parse で ControlEvent 化して EventBus に emit する。
/// parser が未対応 or 接続失敗の場合は None。
fn spawn_input_listener(port_name: &str, event_bus: Arc<EventBus>) -> Option<JoinHandle<()>> {
    let mut parser = create_device_input(port_name)?;

    let midi_in = midir::MidiInput::new("vp-bastet-input").ok()?;
    let ports = midi_in.ports();
    let port_idx = ports.iter().position(|p| {
        midi_in
            .port_name(p)
            .map(|n| n == port_name)
            .unwrap_or(false)
    })?;
    let port = &ports[port_idx];

    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(256);
    let port_name_owned = port_name.to_string();

    // midir callback は別スレッドで走る → blocking_send で async 側へ bridge
    let connection = midi_in
        .connect(
            port,
            "vp-bastet-input",
            move |_timestamp, message, _| {
                let _ = tx.blocking_send(message.to_vec());
            },
            (),
        )
        .ok()?;

    let handle = tokio::spawn(async move {
        // connection を move して keep alive（drop = 切断）
        let _conn = connection;
        let port = port_name_owned;

        tracing::info!("🧲 input listener started: {}", port);

        while let Some(msg) = rx.recv().await {
            if let Some(event) = parser.parse(&msg) {
                tracing::debug!("🧲 control event from {}: {:?}", port, event);
                let cap_event = CapabilityEvent::new("bastet.control_event", "bastet")
                    .with_payload(&serde_json::json!({
                        "port_name": port,
                        "event": event,
                    }));
                event_bus.emit(cap_event).await;
            }
        }

        tracing::info!("🧲 input listener stopped: {}", port);
    });

    Some(handle)
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
    /// ROTO 持続セッション task（nostos self-heal driver、World lifecycle に enclose）
    roto_task: Option<JoinHandle<()>>,
    /// ROTO セッションの shutdown 子 token
    roto_cancel: Option<CancellationToken>,
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
            roto_task: None,
            roto_cancel: None,
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

    /// hot-plug discovery を開始（2s 周期で port enumeration → diff → devices 更新 + input listener 管理）
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

            // device ごとの input listener task を追跡（discovery task ローカル）
            let mut input_listeners: HashMap<String, JoinHandle<()>> = HashMap::new();

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

                    // input port + parser がある device は input listener を spawn。
                    // ただし ROTO は Bastet の持続 control loop（start_roto_control）が
                    // input + keepalive + output を所有するため、discovery の input-only
                    // listener からは除外する（同一 port への二重接続を回避）。
                    if *has_in
                        && !name.contains("Roto")
                        && let Some(handle) = spawn_input_listener(name, Arc::clone(&event_bus))
                    {
                        input_listeners.insert(name.clone(), handle);
                    }
                }

                for name in &diff.removed {
                    tracing::info!("🧲 device disconnected: {}", name);
                    devs.remove(name);
                    let event = CapabilityEvent::new("bastet.device_disconnected", "bastet")
                        .with_payload(&serde_json::json!({ "port_name": name }));
                    event_bus.emit(event).await;

                    // input listener も停止
                    if let Some(handle) = input_listeners.remove(name) {
                        handle.abort();
                    }
                }

                // RwLock を release してから sleep
                drop(devs);

                tokio::select! {
                    _ = cancel_rx.recv() => {
                        // cleanup: 全 input listener を abort
                        for (_, handle) in input_listeners.drain() {
                            handle.abort();
                        }
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

    /// ROTO 持続セッションを開始する。
    ///
    /// 前景 `vp midi roto control` が一発でやっていた〔open(in+out) + DAW_START handshake +
    /// keepalive(autorespond) + LCD projection + track button → switch_lane〕を、daemon 常駐 +
    /// 自動再接続（抜き差し heal）の持続サービスに昇格させる。nostos `AsyncBracket`/`AsyncDriver`
    /// で「接続 1 サイクル = enter→control loop→exit」を表し、disconnect は `Reborn` で再接続する。
    ///
    /// lane data は `ProcessManagerCapability` の Arc を in-process 直読み（QUIC self-loop なし、
    /// `build_world_lanes` 共有で CLI と並び一致）。switch_lane は L0 portless で SP が listen
    /// しなくなったため World :32000 の process-proxy ask 経由で forward する（daemon = World への
    /// self-loop QUIC だが、ボタン押下時のみの低頻度なので lane poll と違い cache 不要）。
    /// `shutdown` の子 token で World/daemon の shutdown chain に enclose する。
    ///
    /// ⚠️ CoreMIDI 物理 port は単一 owner。daemon 常駐中は CLI `vp midi roto control` が
    /// 同 port を取得できない（想定挙動）。
    pub async fn start_roto_control(
        &mut self,
        world_cap: Arc<RwLock<ProcessManagerCapability>>,
        shutdown: CancellationToken,
    ) {
        // 二重起動防止（既に走っていれば no-op）
        if self.roto_task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let child = shutdown.child_token();
        self.roto_cancel = Some(child.clone());

        // ProcessManagerCapability から lane data の Arc を取り出す（in-process 直読み）。
        let (running_processes, lane_registry) = {
            let pmc = world_cap.read().await;
            (pmc.running_processes_ref(), pmc.lane_registry_ref())
        };
        let lane_source = InProcessLaneSource {
            running_processes,
            lane_registry: Some(lane_registry),
            world_cap: Some(world_cap),
        };
        let bracket = RotoSessionBracket::new(lane_source, QuicSwitchSink::new(), child.clone());
        let mut driver = RotoHealDriver {
            shutdown: child,
            backoff: Duration::from_millis(800),
        };

        let task = tokio::spawn(async move {
            let initial = RotoDescriptor {
                port_pattern: "Roto".to_string(),
                view: RotoView::default(),
            };
            tracing::info!("🧲 Bastet ROTO 持続セッション開始 (self-heal)");
            match driver.run(&bracket, initial).await {
                Outcome::Done(()) => {
                    tracing::info!("🧲 Bastet ROTO セッション終了 (graceful)")
                }
                Outcome::Reborn(_) => {
                    tracing::info!("🧲 Bastet ROTO セッション離脱 (shutdown)")
                }
                Outcome::Failed(msg) => {
                    tracing::warn!("🧲 Bastet ROTO セッション fatal: {}", msg)
                }
            }
        });
        self.roto_task = Some(task);
    }

    /// ROTO 持続セッションを停止する（shutdown chain から呼ぶ）。
    pub async fn stop_roto_control(&mut self) {
        if let Some(cancel) = self.roto_cancel.take() {
            cancel.cancel();
        }
        if let Some(task) = self.roto_task.take() {
            task.abort();
        }
    }

    // ─── agent device report 受け口（M2: doc 26 §2 `device` channel）──────
    //
    // Model D（doc 25）: hot-plug 検知の authority を daemon の midir polling から
    // macOS menu bar agent（Swift `CoreMIDIWatcher`、AppKit run loop で CoreMIDI 通知が
    // 自然に効く）へ移した。agent が `device` stream channel で `ReportDevice` を送り、
    // 下記メソッドが registry + EventBus に反映する（discovery loop の added/removed 分岐と
    // 同一効果）。EventBus の `bastet.device_*` は既存 world-device bridge が拾って vp-app に push。

    /// agent からの device 接続報告を registry に反映し、新規なら EventBus に emit する。
    ///
    /// 冪等: agent が reconnect 時に現在の全 device を再報告しても、既知 device は
    /// HashMap 上書きのみで重複 event を出さない（`is_new` gate）。
    pub async fn report_device_connected(
        &self,
        port_name: &str,
        has_input: bool,
        has_output: bool,
    ) {
        let is_new = {
            let mut devs = self.devices.write().await;
            let is_new = !devs.contains_key(port_name);
            devs.insert(
                port_name.to_string(),
                ConnectedDevice {
                    port_name: port_name.to_string(),
                    has_input,
                    has_output,
                    connected_at: Instant::now(),
                },
            );
            is_new
        };
        if is_new {
            tracing::info!("🧲 device connected (agent report): {}", port_name);
            let event = CapabilityEvent::new("bastet.device_connected", "bastet").with_payload(
                &serde_json::json!({
                    "port_name": port_name,
                    "has_input": has_input,
                    "has_output": has_output,
                }),
            );
            self.event_bus.emit(event).await;
        }
    }

    /// agent からの device 切断報告を registry に反映し、存在した場合のみ EventBus に emit する。
    pub async fn report_device_disconnected(&self, port_name: &str) {
        let existed = self.devices.write().await.remove(port_name).is_some();
        if existed {
            tracing::info!("🧲 device disconnected (agent report): {}", port_name);
            let event = CapabilityEvent::new("bastet.device_disconnected", "bastet")
                .with_payload(&serde_json::json!({ "port_name": port_name }));
            self.event_bus.emit(event).await;
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

    // ─── agent device report（M2）──────────────────────

    #[tokio::test]
    async fn report_device_connected_updates_registry_idempotently() {
        let bus = Arc::new(EventBus::new());
        let bastet = Bastet::new(bus);

        bastet
            .report_device_connected("X-Touch Compact", true, true)
            .await;
        assert_eq!(bastet.device_count().await, 1);

        // 同一 device の再報告（agent reconnect 時の initial 再送）は重複しない
        bastet
            .report_device_connected("X-Touch Compact", true, true)
            .await;
        assert_eq!(bastet.device_count().await, 1);

        // 別 device を足すと増える
        bastet
            .report_device_connected("LPD8 mk2", true, false)
            .await;
        assert_eq!(bastet.device_count().await, 2);
    }

    #[tokio::test]
    async fn report_device_disconnected_removes_from_registry() {
        let bus = Arc::new(EventBus::new());
        let bastet = Bastet::new(bus);

        bastet.report_device_connected("ROTO", true, true).await;
        assert_eq!(bastet.device_count().await, 1);

        bastet.report_device_disconnected("ROTO").await;
        assert_eq!(bastet.device_count().await, 0);

        // 未知 device の切断報告は no-op（panic しない）
        bastet.report_device_disconnected("Unknown").await;
        assert_eq!(bastet.device_count().await, 0);
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

    // ─── create_device_input factory ──────────────────

    #[test]
    fn factory_creates_roto_parser() {
        assert!(create_device_input("MIDI9 Roto Control").is_some());
        assert!(create_device_input("Roto").is_some());
    }

    #[test]
    fn factory_returns_none_for_unknown() {
        assert!(create_device_input("X-Touch Compact").is_none());
        assert!(create_device_input("LPD8 mk2").is_none());
        assert!(create_device_input("Unknown Device").is_none());
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
