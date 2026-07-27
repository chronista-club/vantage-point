//! DeviceRegistry 🧲 — machine scope の物理 device 集約 registry (doc 23 §5)
//!
//! 旧 `MidiCapability`（single-device monitor、2026-07-27 撤去済）を multi-device registry に発展させたもの。
//!
//! 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`
//!
//! 責務 (doc 23 §5.3):
//! - **registry**: 接続中 device を `HashMap<port_displayName, ConnectedDevice>` で hold
//! - **hot-plug discovery**: midir enumeration polling (2〜3s) で接続/切断検出
//! - **input parse**: device byte → `DeviceInput::parse` → `ControlEvent` 化
//! - **routing policy**: `ControlEvent` を active Lane の Device I/O へ dispatch (E3)
//! - **active Lane track**: repo の「lanes」QUIC channel を購読し cache 更新 (E3)

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

use tokio_util::sync::CancellationToken;

use nostos::{AsyncDriver, Outcome};

use crate::capability::RepoManagerCapability;
use crate::capability::component_service::{LayerScope, Service};
use crate::capability::core::CapabilityEvent;
use crate::capability::eventbus::EventBus;
use crate::commands::roto_control::{
    InProcessLaneSource, QuicSwitchSink, RotoDescriptor, RotoHealDriver, RotoSessionBracket,
    RotoView,
};
use crate::device_input::DeviceInput;
use crate::repo::lanes_state::LaneAddress;

/// hot-plug polling 間隔（doc 23 Q-4: 2〜3s、体感重視）
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);

// ─── data ──────────────────────────────────────────────────

/// DeviceRegistry registry に登録される接続中の物理 device (doc 23 §5.2)。
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
    use crate::device_input::lpd8::Lpd8Input;
    use crate::device_input::roto::RotoInput;
    use crate::device_input::xtouch::XTouchInput;

    if port_name.contains("Roto") {
        Some(Box::new(RotoInput::default()))
    } else if port_name.contains("X-Touch") {
        Some(Box::new(XTouchInput))
    } else if port_name.contains("LPD8") {
        Some(Box::new(Lpd8Input))
    } else {
        None
    }
}

// ─── actions（I/O）────────────────────────────────────────

/// pattern 部分一致で MIDI output port を開く。複数候補は "INT" を含む名を優先
/// （X-Touch INT = MCU の実 port / EXT = 物理 MIDI passthrough の別）。
fn open_output(pattern: &str) -> Option<midir::MidiOutputConnection> {
    let midi_out = midir::MidiOutput::new("vp-devices-feedback").ok()?;
    let ports = midi_out.ports();
    let named: Vec<(String, midir::MidiOutputPort)> = ports
        .into_iter()
        .filter_map(|p| midi_out.port_name(&p).ok().map(|n| (n, p)))
        .filter(|(n, _)| n.contains(pattern))
        .collect();
    let (name, port) = named
        .iter()
        .find(|(n, _)| n.contains("INT"))
        .or_else(|| named.first())?;
    midi_out.connect(port, &format!("vp-feedback-{name}")).ok()
}

/// midir で input + output の全 port を enumeration し、displayName → (has_input, has_output) の map を返す。
/// 物理デバイスは同じ displayName で input/output 両方のポートを持つため、名前で merge する。
fn enumerate_ports() -> HashMap<String, (bool, bool)> {
    let mut result: HashMap<String, (bool, bool)> = HashMap::new();

    if let Ok(midi_in) = midir::MidiInput::new("vp-devices-scan") {
        for port in midi_in.ports() {
            if let Ok(name) = midi_in.port_name(&port) {
                result.entry(name).or_insert((false, false)).0 = true;
            }
        }
    }

    if let Ok(midi_out) = midir::MidiOutput::new("vp-devices-scan") {
        for port in midi_out.ports() {
            if let Ok(name) = midi_out.port_name(&port) {
                result.entry(name).or_insert((false, false)).1 = true;
            }
        }
    }

    result
}

/// 共有の input listener 管理 map（port displayName → listener task）。
///
/// listener の起点は 3 経路（起動時 attach / agent 報告 / polling discovery）あるが、
/// spawn 判定はこの map を通す 1 本に畳む（[[one-edge-two-jobs]] — polling loop の
/// local map に閉じていたせいで Model D 移行後に listener が誰からも張られなくなった）。
type InputListeners = Arc<RwLock<HashMap<String, JoinHandle<()>>>>;

/// ROTO motor へ注入する byte 列（knob_position の 14bit hi-res CC ペア × N knob）
pub(crate) type MotorFrames = Vec<Vec<u8>>;

/// motor 注入路の共有 slot（`start_roto_control` が設定、`apply_feedback` が使う）。
/// watch = 真の latest-wins（mpsc + try_send は満杯時に**新しい方**を落とす drop-newest で
/// 意味論が逆になる — team-b review。vp-app 側の feedback 経路と同型に揃える）
type RotoFeedbackTx = Arc<RwLock<Option<tokio::sync::watch::Sender<Option<MotorFrames>>>>>;

/// parser 対応 device の input listener を冪等に張る。
///
/// - ROTO は専用 loop（`start_roto_control`）が input を独占所有するため対象外
/// - 生存中の listener が既に居れば no-op（二重接続の防止）
/// - parser 対応 device なのに接続に失敗したら warn（無音の取り残しを作らない）
async fn ensure_input_listener(
    listeners: &InputListeners,
    event_bus: &Arc<EventBus>,
    port_name: &str,
) {
    if port_name.contains("Roto") || create_device_input(port_name).is_none() {
        return;
    }
    let mut map = listeners.write().await;
    if let Some(handle) = map.get(port_name)
        && !handle.is_finished()
    {
        return;
    }
    match spawn_input_listener(port_name, Arc::clone(event_bus)) {
        Some(handle) => {
            map.insert(port_name.to_string(), handle);
        }
        None => {
            tracing::warn!(
                "🧲 input listener 接続失敗（port 不在 or midir error）: {}",
                port_name
            );
        }
    }
}

/// 指定 port の MIDI input を listen し、DeviceInput::parse で ControlEvent 化して EventBus に emit する。
/// parser が未対応 or 接続失敗の場合は None。
fn spawn_input_listener(port_name: &str, event_bus: Arc<EventBus>) -> Option<JoinHandle<()>> {
    let mut parser = create_device_input(port_name)?;

    let midi_in = midir::MidiInput::new("vp-devices-input").ok()?;
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
            "vp-devices-input",
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
                let cap_event = CapabilityEvent::new("devices.control_event", "devices")
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

// ─── DeviceRegistry struct ─────────────────────────────────────────

/// machine scope の物理 device 集約 registry（DeviceRegistry 🧲）。
///
/// key = CoreMIDI port の displayName（背骨 mem 準拠、doc 23 §5.2）。
/// 旧 `MidiCapability`（single-device monitor、撤去済）を multi-device registry に発展させたもの。
pub struct DeviceRegistry {
    /// 接続中 device を port displayName で引く（polling task と共有）
    devices: Arc<RwLock<HashMap<String, ConnectedDevice>>>,
    /// active Lane の購読 cache（SSOT は repo の lanes_state、DeviceRegistry は購読側。doc 23 Q-1）
    active_lane: Arc<RwLock<Option<LaneAddress>>>,
    /// Capability event bus（接続/切断イベント配信用）
    event_bus: Arc<EventBus>,
    /// hot-plug polling task handle
    discovery_task: Option<JoinHandle<()>>,
    /// discovery cancel signal
    cancel_tx: Option<mpsc::Sender<()>>,
    /// ROTO 持続セッション task（nostos self-heal driver、Daemon lifecycle に enclose）
    roto_task: Option<JoinHandle<()>>,
    /// ROTO セッションの shutdown 子 token
    roto_cancel: Option<CancellationToken>,
    /// input listener 管理（起動時 attach / agent 報告 / polling discovery の 3 経路で共有）
    input_listeners: InputListeners,
    /// フィードバック方向（LE-19）の出力接続（port displayName → output conn）。
    /// 必要時に開き、send 失敗で捨てて次回再接続する（X-Touch / LPD8。ROTO は専用 loop 所有）
    outputs: Arc<tokio::sync::Mutex<HashMap<String, midir::MidiOutputConnection>>>,
    /// LPD8 pad LED の shadow（RGB 一括 sysex は全 pad 分を持つため差分計算に要る）
    lpd8_profile: Arc<tokio::sync::Mutex<crate::device_profile::lpd8::Lpd8Profile>>,
    /// ROTO motor への注入路（conn_out は roto loop が独占所有するため、byte 列を loop に渡す）。
    /// `start_roto_control` が設定。buffer 超過は drop（feedback は latest-wins で欠落無害）
    roto_feedback_tx: RotoFeedbackTx,
    /// 前回 apply した feedback（section 単位の dedupe — 変わらない sysex を機材に投げない）
    last_feedback: Arc<tokio::sync::Mutex<crate::daemon::protocol::FleetFeedback>>,
}

impl DeviceRegistry {
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
            input_listeners: Arc::new(RwLock::new(HashMap::new())),
            outputs: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            lpd8_profile: Arc::new(tokio::sync::Mutex::new(Default::default())),
            roto_feedback_tx: Arc::new(RwLock::new(None)),
            last_feedback: Arc::new(tokio::sync::Mutex::new(Default::default())),
        }
    }

    /// フィードバック方向（LE-19）: 場の状態を機材の出力面に写す。
    ///
    /// 送り手（webview）が throttle + diff 済みだが、section 単位でもう一段 dedupe する
    /// （knob だけ動いた frame で pad の RGB 一括 sysex を再送しない）。
    /// 出力接続は必要時に開き、send 失敗で捨てて次回再接続（hot-unplug 耐性）。
    pub async fn apply_feedback(&self, fb: &crate::daemon::protocol::FleetFeedback) {
        let mut last = self.last_feedback.lock().await;

        // ROTO motor: knob byte 列を専用 loop に注入（conn_out は loop が独占所有）。
        // watch send = 最新値の置き換え（未消費の古い frame は自然に消える = latest-wins）
        if fb.knobs != last.knobs
            && !fb.knobs.is_empty()
            && let Some(tx) = self.roto_feedback_tx.read().await.as_ref()
        {
            let msgs: MotorFrames = fb
                .knobs
                .iter()
                .filter(|k| k.index < 8)
                .flat_map(|k| crate::device_profile::roto::knob_position(k.index, k.value))
                .collect();
            let _ = tx.send(Some(msgs));
        }

        // X-Touch fader 1（index 0）= t の表示。transition 無し（None）は動かさない
        if fb.fader != last.fader
            && let Some(t) = fb.fader
        {
            let msg = crate::device_profile::xtouch::fader_position(0, t);
            self.send_output("X-Touch INT", &[msg]).await;
        }

        // LPD8 pad RGB: Scene slot の filled 状態（filled = 琥珀 / empty = 消灯）
        if fb.pads != last.pads && !fb.pads.is_empty() {
            let msgs = {
                use crate::device_profile::DeviceProfile;
                let mut profile = self.lpd8_profile.lock().await;
                let mut msgs: Vec<Vec<u8>> = Vec::new();
                for pad in fb.pads.iter().filter(|p| p.index < 8) {
                    let color = if pad.filled {
                        crate::device_profile::Rgb::new(255, 180, 40)
                    } else {
                        crate::device_profile::Rgb::new(0, 0, 0)
                    };
                    // project_track は shadow 更新 + LED 一括 sysex を返す — 最後の 1 回分で足りる
                    msgs = profile.project_track(pad.index, "", color, false);
                }
                msgs
            };
            self.send_output("LPD8", &msgs).await;
        }

        *last = fb.clone();
    }

    /// 出力接続を必要時に開いて送る。send 失敗は接続を捨てる（次回再接続）。
    /// port は displayName の部分一致（複数候補は "INT" 優先 — X-Touch INT/EXT の別）
    async fn send_output(&self, pattern: &str, msgs: &[Vec<u8>]) {
        if msgs.is_empty() {
            return;
        }
        let mut outputs = self.outputs.lock().await;
        if !outputs.contains_key(pattern) {
            match open_output(pattern) {
                Some(conn) => {
                    outputs.insert(pattern.to_string(), conn);
                    tracing::info!("🧲 feedback output opened: {}", pattern);
                }
                None => {
                    tracing::debug!("🧲 feedback output 不在: {}", pattern);
                    return;
                }
            }
        }
        if let Some(conn) = outputs.get_mut(pattern) {
            for msg in msgs {
                if conn.send(msg).is_err() {
                    tracing::warn!(
                        "🧲 feedback send 失敗 — 接続を捨てて次回再接続: {}",
                        pattern
                    );
                    outputs.remove(pattern);
                    return;
                }
            }
        }
    }

    /// 起動時に既接続の艦隊 device へ input listener を張る（1 回の enumeration、polling なし）。
    ///
    /// Model D では hot-plug 検知が Swift agent の報告に移譲されたが、agent 不在 / 未接続の
    /// 環境では報告が来ず、**daemon 起動前から挿さっている device の input が誰からも
    /// 張られない**（fleet #877 の実機で顕在化した取り残し）。起動時の 1 回 enumeration で
    /// 「机上に既にある艦隊」を確実に拾う。以降の抜き差しは agent 報告（`report_device_*`）が
    /// 同じ `ensure_input_listener` を通して反映する。
    pub async fn attach_fleet_inputs(&self) {
        let ports = enumerate_ports();
        for (name, (has_in, has_out)) in &ports {
            // agent 報告と**同じ 1 本の辺**（registry 挿入 + 新規 emit + listener ensure）に
            // 畳む。旧実装は ensure_input_listener 直呼びで listener だけ張り、registry が
            // 空のまま — polling 停止 + agent 報告 0 件の環境では daemon-device snapshot が
            // 常に空で、DeviceRegistry pane が「No devices connected」に固定されていた
            // （discovery の辺の 2 仕事のうち片方だけ移管された取り残し、#878 の同型）
            self.report_device_connected(name, *has_in, *has_out).await;
        }
        let listeners = self.input_listeners.read().await.len();
        let registered = self.devices.read().await.len();
        tracing::info!(
            "🧲 fleet input attach: {} listener(s) / {} device(s) registered（enumeration {} ports）",
            listeners,
            registered,
            ports.len()
        );
    }

    /// 接続中 device 数
    pub async fn device_count(&self) -> usize {
        self.devices.read().await.len()
    }

    /// devices registry の read handle
    pub fn devices(&self) -> &Arc<RwLock<HashMap<String, ConnectedDevice>>> {
        &self.devices
    }

    /// active Lane cache の read handle（Device I/O dispatch / 外部クエリ用）
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
        let input_listeners = Arc::clone(&self.input_listeners);
        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        self.cancel_tx = Some(cancel_tx);

        let task = tokio::spawn(async move {
            tracing::info!(
                "devices 🧲 discovery started (interval: {}s)",
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
                    let event = CapabilityEvent::new("devices.device_connected", "devices")
                        .with_payload(&serde_json::json!({
                            "port_name": name,
                            "has_input": has_in,
                            "has_output": has_out,
                        }));
                    event_bus.emit(event).await;

                    // input port がある device は共有 map 経由で listener を冪等 ensure
                    // （ROTO 除外 / parser 判定 / 二重接続防止は ensure_input_listener が担う）
                    if *has_in {
                        ensure_input_listener(&input_listeners, &event_bus, name).await;
                    }
                }

                for name in &diff.removed {
                    tracing::info!("🧲 device disconnected: {}", name);
                    devs.remove(name);
                    let event = CapabilityEvent::new("devices.device_disconnected", "devices")
                        .with_payload(&serde_json::json!({ "port_name": name }));
                    event_bus.emit(event).await;

                    // input listener も停止
                    if let Some(handle) = input_listeners.write().await.remove(name) {
                        handle.abort();
                    }
                }

                // RwLock を release してから sleep
                drop(devs);

                tokio::select! {
                    _ = cancel_rx.recv() => {
                        // listener は DeviceRegistry 所有（共有 map）— discovery 停止 ≠ device 消滅
                        // なのでここでは畳まない（切断報告 / process 終了が寿命を決める）
                        tracing::info!("devices 🧲 discovery stopped");
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
    /// lane data は `RepoManagerCapability` の Arc を in-process 直読み（QUIC self-loop なし、
    /// `build_node_lanes` 共有で CLI と並び一致）。switch_lane は L0 portless で repo が listen
    /// しなくなったため Daemon :32000 の repo-proxy ask 経由で forward する（daemon = daemon への
    /// self-loop QUIC だが、ボタン押下時のみの低頻度なので lane poll と違い cache 不要）。
    /// `shutdown` の子 token で Daemon/daemon の shutdown chain に enclose する。
    ///
    /// ⚠️ CoreMIDI 物理 port は単一 owner。daemon 常駐中は CLI `vp midi roto control` が
    /// 同 port を取得できない（想定挙動）。
    pub async fn start_roto_control(
        &mut self,
        daemon_cap: Arc<RwLock<RepoManagerCapability>>,
        shutdown: CancellationToken,
    ) {
        // 二重起動防止（既に走っていれば no-op）
        if self.roto_task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let child = shutdown.child_token();
        self.roto_cancel = Some(child.clone());

        // RepoManagerCapability から lane data の Arc を取り出す（in-process 直読み）。
        let (running_repos, lane_registry) = {
            let pmc = daemon_cap.read().await;
            (pmc.running_processes_ref(), pmc.lane_registry_ref())
        };
        let lane_source = InProcessLaneSource {
            running_repos,
            lane_registry: Some(lane_registry),
            daemon_cap: Some(daemon_cap),
        };
        // フィードバック方向: motor byte 列の注入路（conn_out は loop が独占所有するため）。
        // watch = latest-wins（apply_feedback の連続送信は最新だけが残る）
        let (feedback_tx, feedback_rx) = tokio::sync::watch::channel::<Option<MotorFrames>>(None);
        *self.roto_feedback_tx.write().await = Some(feedback_tx);

        let bracket = RotoSessionBracket::new(
            lane_source,
            QuicSwitchSink::new(),
            child.clone(),
            // knob 系入力を devices.control_event に流す（fleet 配線 — doc 49 LE-19）
            Some(Arc::clone(&self.event_bus)),
            feedback_rx,
        );
        let mut driver = RotoHealDriver {
            shutdown: child,
            backoff: Duration::from_millis(800),
        };

        let task = tokio::spawn(async move {
            let initial = RotoDescriptor {
                port_pattern: "Roto".to_string(),
                view: RotoView::default(),
            };
            tracing::info!("🧲 devices: ROTO 持続セッション開始 (self-heal)");
            match driver.run(&bracket, initial).await {
                Outcome::Done(()) => {
                    tracing::info!("🧲 devices: ROTO セッション終了 (graceful)")
                }
                Outcome::Reborn(_) => {
                    tracing::info!("🧲 devices: ROTO セッション離脱 (shutdown)")
                }
                Outcome::Failed(msg) => {
                    tracing::warn!("🧲 devices: ROTO セッション fatal: {}", msg)
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
    // 同一効果）。EventBus の `devices.device_*` は既存 daemon-device bridge が拾って vp-app に push。

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
            let event = CapabilityEvent::new("devices.device_connected", "devices").with_payload(
                &serde_json::json!({
                    "port_name": port_name,
                    "has_input": has_input,
                    "has_output": has_output,
                }),
            );
            self.event_bus.emit(event).await;
        }
        // input listener は is_new と独立に冪等 ensure（再報告 = 再接続の機会。
        // registry 更新だけで listener を張り忘れる取り残しの再発防止）
        if has_input {
            ensure_input_listener(&self.input_listeners, &self.event_bus, port_name).await;
        }
    }

    /// agent からの device 切断報告を registry に反映し、存在した場合のみ EventBus に emit する。
    pub async fn report_device_disconnected(&self, port_name: &str) {
        // input listener は registry の有無と独立に畳む（経路の欠けに対する防御 — 現行は
        // 起動時 attach も report_device_connected 経由で registry に載る）
        if let Some(handle) = self.input_listeners.write().await.remove(port_name) {
            handle.abort();
            tracing::info!("🧲 input listener aborted (disconnect): {}", port_name);
        }
        let existed = self.devices.write().await.remove(port_name).is_some();
        if existed {
            tracing::info!("🧲 device disconnected (agent report): {}", port_name);
            let event = CapabilityEvent::new("devices.device_disconnected", "devices")
                .with_payload(&serde_json::json!({ "port_name": port_name }));
            self.event_bus.emit(event).await;
        }
    }
}

// ─── Service impl ──────────────────────────────────────────

impl Service for DeviceRegistry {
    fn actor_name(&self) -> &str {
        "devices"
    }

    fn layer_scope(&self) -> LayerScope {
        LayerScope::Machine
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
        let devices = DeviceRegistry::new(bus);
        assert_eq!(devices.device_count().await, 0);
    }

    #[test]
    fn service_impl_correct() {
        let bus = Arc::new(EventBus::new());
        let devices = DeviceRegistry::new(bus);
        assert_eq!(devices.actor_name(), "devices");
        assert_eq!(devices.layer_scope(), LayerScope::Machine);
    }

    #[tokio::test]
    async fn active_lane_initially_none() {
        let bus = Arc::new(EventBus::new());
        let devices = DeviceRegistry::new(bus);
        let lane = devices.active_lane().read().await;
        assert!(lane.is_none());
    }

    // ─── agent device report（M2）──────────────────────

    #[tokio::test]
    async fn report_device_connected_updates_registry_idempotently() {
        let bus = Arc::new(EventBus::new());
        let devices = DeviceRegistry::new(bus);

        devices
            .report_device_connected("X-Touch Compact", true, true)
            .await;
        assert_eq!(devices.device_count().await, 1);

        // 同一 device の再報告（agent reconnect 時の initial 再送）は重複しない
        devices
            .report_device_connected("X-Touch Compact", true, true)
            .await;
        assert_eq!(devices.device_count().await, 1);

        // 別 device を足すと増える
        devices
            .report_device_connected("LPD8 mk2", true, false)
            .await;
        assert_eq!(devices.device_count().await, 2);
    }

    #[tokio::test]
    async fn report_device_disconnected_removes_from_registry() {
        let bus = Arc::new(EventBus::new());
        let devices = DeviceRegistry::new(bus);

        devices.report_device_connected("ROTO", true, true).await;
        assert_eq!(devices.device_count().await, 1);

        devices.report_device_disconnected("ROTO").await;
        assert_eq!(devices.device_count().await, 0);

        // 未知 device の切断報告は no-op（panic しない）
        devices.report_device_disconnected("Unknown").await;
        assert_eq!(devices.device_count().await, 0);
    }

    #[test]
    fn service_supports_downcast() {
        let bus = Arc::new(EventBus::new());
        let devices = DeviceRegistry::new(bus);
        let service: &dyn Service = &devices;
        let downcast = service.as_any().downcast_ref::<DeviceRegistry>();
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
    fn factory_creates_fleet_parsers() {
        // 机上の 3 台（doc 49 LE-19 fleet）: ROTO / X-Touch / LPD8
        assert!(create_device_input("MIDI9 Roto Control").is_some());
        assert!(create_device_input("Roto").is_some());
        assert!(create_device_input("X-Touch Compact").is_some());
        assert!(create_device_input("LPD8 mk2").is_some());
    }

    #[test]
    fn factory_returns_none_for_unknown() {
        assert!(create_device_input("Unknown Device").is_none());
        assert!(create_device_input("KeyStage 61").is_none());
    }

    // ─── ensure_input_listener（3 経路共有の spawn 判定）──────

    #[tokio::test]
    async fn ensure_skips_non_fleet_and_roto() {
        let bus = Arc::new(EventBus::new());
        let listeners: InputListeners = Arc::new(RwLock::new(HashMap::new()));
        // parser 対象外 device と ROTO（専用 loop 所有）は map に入らない
        ensure_input_listener(&listeners, &bus, "Unknown Device").await;
        ensure_input_listener(&listeners, &bus, "Roto-Control").await;
        assert!(listeners.read().await.is_empty());
    }

    #[tokio::test]
    async fn ensure_is_graceful_without_real_port() {
        let bus = Arc::new(EventBus::new());
        let listeners: InputListeners = Arc::new(RwLock::new(HashMap::new()));
        // parser 対応 device でも実 port が無ければ warn して no-op（CI = MIDI 無し環境）
        ensure_input_listener(&listeners, &bus, "LPD8 mk2 (absent)").await;
        assert!(listeners.read().await.is_empty());
    }

    #[tokio::test]
    async fn agent_report_paths_do_not_panic_without_ports() {
        // agent 報告経路が listener ensure / abort を通っても実機不在で安全に流れる
        let bus = Arc::new(EventBus::new());
        let devices = DeviceRegistry::new(bus);
        devices
            .report_device_connected("X-Touch INT", true, true)
            .await;
        assert_eq!(devices.device_count().await, 1);
        devices.report_device_disconnected("X-Touch INT").await;
        assert_eq!(devices.device_count().await, 0);
        // 起動時 attach も同様（CI では対象 port が無い前提で走るだけ）
        devices.attach_fleet_inputs().await;
    }

    // ─── discovery lifecycle ───────────────────────────

    #[tokio::test]
    async fn discovery_lifecycle() {
        let bus = Arc::new(EventBus::new());
        let mut devices = DeviceRegistry::new(bus);

        assert!(!devices.is_discovering());

        devices.start_discovery().await;
        assert!(devices.is_discovering());

        // 二重起動は no-op
        devices.start_discovery().await;

        devices.stop_discovery().await;
        // task abort 後は is_discovering = false
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!devices.is_discovering());
    }
}
