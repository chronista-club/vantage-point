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
use std::sync::atomic::{AtomicBool, Ordering};
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

// ============================================================================
// 艦隊スイッチ — VP が MIDI 機材を握るか、他アプリへ譲るか
// ============================================================================
//
// CoreMIDI の物理 port は**単一 owner**なので、daemon が握っている間は他アプリ
// （ladyland 等）が同じ機材を開けない（`start_roto_control` の ⚠️ も同じ事実）。
// user が「今は ladyland で使う」と言えるスイッチを持たせる。
//
// ⚠️ OFF は「**握らない**」であって「**見えなくなる**」ではない。registry（device 一覧）は
// そのまま保ち、hardware を掴む側だけを止める。`attach_fleet_inputs` /
// `report_device_connected` は「registry に載せる」と「listener を張る」の 2 仕事を
// 1 本の辺でやっているので、**gate は hardware を掴む側だけに置く**（辺ごと止めると
// GUI の device 一覧が空になり「機材が消えた」に見える）。

/// 艦隊 ON/OFF の永続先。state zone なので `VP_PROFILE` で dev / brew が自然に分かれる。
fn midi_switch_path() -> std::path::PathBuf {
    vp_paths::vp_state_dir().join("midi-switch.json")
}

/// 保存された艦隊スイッチを読む。**既定は ON**（file 不在 = 従来どおり握る）。
///
/// 壊れた file も ON に倒す — 「読めないから機材を握らない」より「読めないから従来どおり」
/// の方が驚きが小さい（OFF は user が明示した時だけの状態）。
pub fn load_midi_enabled() -> bool {
    let Ok(text) = std::fs::read_to_string(midi_switch_path()) else {
        return true;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("enabled").and_then(|b| b.as_bool()))
        .unwrap_or(true)
}

/// 艦隊スイッチを保存する（daemon 再起動をまたいで保つ）。
///
/// 失敗は warn 止まり — 保存できなくても**今の daemon では効いている**ので、
/// ここで止めると「離せたのに離せなかったことにする」になる。
pub fn save_midi_enabled(enabled: bool) {
    let path = midi_switch_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("midi switch の保存先を作れません: {e}");
        return;
    }
    let body = serde_json::json!({ "enabled": enabled }).to_string();
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!("midi switch の保存に失敗（今の daemon では効いています）: {e}");
    }
}

/// parser 対応 device の input listener を冪等に張る。
///
/// - **艦隊 OFF の間は張らない**（user が機材を他アプリへ譲っている）。ここが gate の要 —
///   agent の hot-plug 報告も discovery もこの 1 本を通るので、抜き差しで握り直さない
/// - ROTO は専用 loop（`start_roto_control`）が input を独占所有するため対象外
/// - 生存中の listener が既に居れば no-op（二重接続の防止）
/// - parser 対応 device なのに接続に失敗したら warn（無音の取り残しを作らない）
async fn ensure_input_listener(
    listeners: &InputListeners,
    event_bus: &Arc<EventBus>,
    port_name: &str,
    midi_enabled: &AtomicBool,
) {
    if !midi_enabled.load(Ordering::Relaxed) {
        return;
    }
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
    /// 艦隊を握るか（`false` = user が `vp midi off` で機材を他アプリへ譲っている）。
    ///
    /// spawn 済みの discovery task からも読むので `Arc<AtomicBool>`。**hardware を掴む 3 つの
    /// 入口**（input listener / output 接続 / ROTO セッション）が全部この 1 つを見る。
    midi_enabled: Arc<AtomicBool>,
    /// ROTO を**再取得**するための持ち物（`start_roto_control` の引数を初回に控える）。
    ///
    /// OFF → ON で ROTO を張り直すのに要る。これが無いと、一度 off にした ROTO は
    /// daemon を再起動するまで戻らない（= スイッチが片道になる）。
    roto_deps: Option<(Arc<RwLock<RepoManagerCapability>>, CancellationToken)>,
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
            // 保存済みスイッチを初期値にする（daemon 再起動をまたいで OFF を保つ）。
            midi_enabled: Arc::new(AtomicBool::new(load_midi_enabled())),
            roto_deps: None,
        }
    }

    /// フィードバック方向（LE-19）: 場の状態を機材の出力面に写す。
    ///
    /// 送り手（webview）が throttle + diff 済みだが、section 単位でもう一段 dedupe する
    /// （knob だけ動いた frame で pad の RGB 一括 sysex を再送しない）。
    /// 出力接続は必要時に開き、send 失敗で捨てて次回再接続（hot-unplug 耐性）。
    pub async fn apply_feedback(&self, fb: &crate::daemon::protocol::FleetFeedback) {
        // 艦隊 OFF の間は出力側も握らない。ここを抜かすと、場の状態が動いた瞬間に
        // `send_output` が output port を開き直して他アプリから機材を奪う。
        if !self.midi_enabled.load(Ordering::Relaxed) {
            return;
        }
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

    // ─── 艦隊スイッチ（VP が機材を握るか、他アプリへ譲るか）──────

    /// VP が艦隊を握っているか。`false` = user が `vp midi off` で譲っている。
    pub fn midi_enabled(&self) -> bool {
        self.midi_enabled.load(Ordering::Relaxed)
    }

    /// この port を VP が今**掴んでいるか**と、その理由（Devices pane の計器表示用）。
    ///
    /// ⚠️ **「掴んでいない」には性質の違う 3 つがある**。1 つの bool に潰すと、pane を見た人が
    /// 「ROTO が反応しない」の原因を取り違える:
    ///
    /// | reason | 意味 | user がすべきこと |
    /// |---|---|---|
    /// | `"listener"` / `"roto"` | 掴んでいる | — |
    /// | `"released"` | user が `vp midi off` で譲っている | `vp midi on` |
    /// | `"unsupported"` | VP に parser が無い機材（最初から掴んでいない） | 何もない（取り合っていない） |
    ///
    /// 特に `unsupported` は「VP が邪魔している」と誤読されやすいので明示的に分ける
    /// （実測: 登録 7 台のうち 4 台がここに落ちる = 元々競合していない）。
    pub async fn hold_state(&self, port_name: &str) -> (bool, &'static str) {
        // 対応 parser が無い機材は、スイッチに関わらず最初から掴んでいない。
        // ⚠️ 判定を先にやる — 譲渡中でも「元々掴んでいない」の方が user にとって正しい説明。
        if create_device_input(port_name).is_none() {
            return (false, "unsupported");
        }
        if !self.midi_enabled.load(Ordering::Relaxed) {
            return (false, "released");
        }
        // ROTO は listener ではなく専用 loop が in+out を独占所有する。
        if port_name.contains("Roto") {
            let live = self.roto_task.as_ref().is_some_and(|t| !t.is_finished());
            return if live {
                (true, "roto")
            } else {
                (false, "idle")
            };
        }
        let held = self
            .input_listeners
            .read()
            .await
            .get(port_name)
            .is_some_and(|h| !h.is_finished());
        if held {
            (true, "listener")
        } else {
            (false, "idle")
        }
    }

    /// 登録中の全 device について `device_connected` を**現在の hold 状態つきで**撃ち直す。
    ///
    /// スイッチを切り替えた瞬間に Devices pane を追随させるための再配信。event は port_name で
    /// 冪等に畳まれる（daemon-device bridge の snapshot と同じ性質）ので、重複しても害はない。
    async fn republish_hold_state(&self) {
        let entries: Vec<(String, bool, bool)> = {
            let devs = self.devices.read().await;
            devs.values()
                .map(|d| (d.port_name.clone(), d.has_input, d.has_output))
                .collect()
        };
        for (port_name, has_input, has_output) in entries {
            let (held, reason) = self.hold_state(&port_name).await;
            let event = CapabilityEvent::new("devices.device_connected", "devices").with_payload(
                &serde_json::json!({
                    "port_name": port_name,
                    "has_input": has_input,
                    "has_output": has_output,
                    "held": held,
                    "hold_reason": reason,
                }),
            );
            self.event_bus.emit(event).await;
        }
    }

    /// 艦隊スイッチを切り替える。**状態が変わったときだけ `true`**（冪等）。
    ///
    /// - `false` へ: 握っている port を全部離す（input listener / output 接続 / ROTO）
    /// - `true` へ: 起動時と同じ手順で握り直す（enumeration + ROTO 再開）
    ///
    /// 切替は永続する（daemon 再起動をまたぐ）。ladyland 等の開発中に daemon を再起動
    /// （`app:swap` 等）しても port を奪い返さないため。
    pub async fn set_midi_enabled(&mut self, enabled: bool) -> bool {
        if self.midi_enabled.load(Ordering::Relaxed) == enabled {
            return false;
        }
        // ⚠️ 先に flag を倒してから release する。逆にすると、release 中に届いた
        // agent の hot-plug 報告が「まだ ON」を見て listener を張り直す。
        self.midi_enabled.store(enabled, Ordering::Relaxed);
        save_midi_enabled(enabled);
        if enabled {
            self.acquire_fleet().await;
        } else {
            self.release_fleet().await;
        }
        // Devices pane を即追随させる（掴んだ / 譲った が次の hot-plug を待たずに出る）。
        self.republish_hold_state().await;
        true
    }

    /// 握っている MIDI port を全部離す（艦隊 OFF の実体）。
    ///
    /// registry（device 一覧）は**触らない** — 見えなくなるのではなく、握らなくなるだけ。
    async fn release_fleet(&mut self) {
        // ① input listener（device ごとに 1 本の CoreMIDI input 接続）
        let aborted = {
            let mut map = self.input_listeners.write().await;
            let n = map.len();
            for (_, handle) in map.drain() {
                handle.abort();
            }
            n
        };
        // ② ROTO の in+out（専用 loop が独占所有している）
        self.stop_roto_control().await;
        // ③ feedback で開いた output 接続（drop = port を離す）
        let outputs = {
            let mut map = self.outputs.lock().await;
            let n = map.len();
            map.clear();
            n
        };
        tracing::info!(
            "🧲 艦隊 OFF — MIDI を他アプリへ譲りました（input {} / output {} / ROTO 停止）",
            aborted,
            outputs
        );
    }

    /// 艦隊を握り直す（OFF → ON）。起動時と同じ 2 段（input 張り直し + ROTO 再開）。
    async fn acquire_fleet(&mut self) {
        self.attach_fleet_inputs().await;
        // ROTO は起動時に控えた持ち物で張り直す。控えが無い = このプロセスで一度も
        // 起動していない（feature 無し / repo mode）ので何もしない。
        if let Some((daemon_cap, shutdown)) = self.roto_deps.clone() {
            self.start_roto_control(daemon_cap, shutdown).await;
        }
        tracing::info!("🧲 艦隊 ON — MIDI を握り直しました");
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
        // 艦隊 OFF の間は listener を張らない（task 側でも同じ 1 つの flag を見る）
        let midi_enabled = Arc::clone(&self.midi_enabled);
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
                        ensure_input_listener(&input_listeners, &event_bus, name, &midi_enabled)
                            .await;
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
        // ⚠️ 持ち物は **gate より前**に控える。OFF 状態で daemon が起動した回でも控えておかないと、
        // その後 `vp midi on` しても ROTO だけ戻せない（片道スイッチになる）。
        self.roto_deps = Some((daemon_cap.clone(), shutdown.clone()));
        // 艦隊 OFF の間は ROTO の in+out も握らない（ladyland 等に譲っている）。
        if !self.midi_enabled.load(Ordering::Relaxed) {
            tracing::info!("🧲 艦隊 OFF のため ROTO 常駐を見送りました（`vp midi on` で開始）");
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
        // input listener は is_new と独立に冪等 ensure（再報告 = 再接続の機会。
        // registry 更新だけで listener を張り忘れる取り残しの再発防止）
        //
        // ⚠️ **emit より先に張る**。逆順にすると `hold_state` が listener を見つけられず、
        // 初回配信の計器が必ず「掴んでいない」になる（次の hot-plug まで直らない）。
        if has_input {
            ensure_input_listener(
                &self.input_listeners,
                &self.event_bus,
                port_name,
                &self.midi_enabled,
            )
            .await;
        }
        if is_new {
            tracing::info!("🧲 device connected (agent report): {}", port_name);
            let (held, reason) = self.hold_state(port_name).await;
            let event = CapabilityEvent::new("devices.device_connected", "devices").with_payload(
                &serde_json::json!({
                    "port_name": port_name,
                    "has_input": has_input,
                    "has_output": has_output,
                    "held": held,
                    "hold_reason": reason,
                }),
            );
            self.event_bus.emit(event).await;
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
        let on = AtomicBool::new(true);
        // parser 対象外 device と ROTO（専用 loop 所有）は map に入らない
        ensure_input_listener(&listeners, &bus, "Unknown Device", &on).await;
        ensure_input_listener(&listeners, &bus, "Roto-Control", &on).await;
        assert!(listeners.read().await.is_empty());
    }

    #[tokio::test]
    async fn ensure_is_graceful_without_real_port() {
        let bus = Arc::new(EventBus::new());
        let listeners: InputListeners = Arc::new(RwLock::new(HashMap::new()));
        let on = AtomicBool::new(true);
        // parser 対応 device でも実 port が無ければ warn して no-op（CI = MIDI 無し環境）
        ensure_input_listener(&listeners, &bus, "LPD8 mk2 (absent)", &on).await;
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

    // ─── 艦隊スイッチ（MIDI を握る / 譲る）──────────────
    //
    // ⚠️ ここの test は `midi-switch.json` を読まずに済むよう、構築後に flag を直接倒す。
    // 永続の往復（file）は state zone を汚すので単体では触らない。

    /// ⚠️ **OFF は「握らない」であって「device が消える」ではない**。
    /// registry を空にしてしまうと GUI の一覧から機材が消え、「壊れた」に見える。
    #[tokio::test]
    async fn switch_off_keeps_registry_but_stops_acquiring() {
        let bus = Arc::new(EventBus::new());
        let devices = DeviceRegistry::new(bus);
        devices.midi_enabled.store(false, Ordering::Relaxed);

        // OFF でも agent 報告は registry に載る（見えることは保つ）
        devices
            .report_device_connected("LPD8 mk2", true, false)
            .await;
        assert_eq!(devices.device_count().await, 1, "一覧からは消さない");
        // hardware は掴まない（listener は 1 本も張らない）
        assert!(
            devices.input_listeners.read().await.is_empty(),
            "OFF の間は input を握らない"
        );
    }

    /// ⚠️ **片道スイッチにしない**。ROTO の持ち物（daemon_cap / shutdown）を控えていないと、
    /// 一度 off にした ROTO は daemon 再起動まで戻らない。
    #[tokio::test]
    async fn switch_records_roto_deps_even_when_off() {
        use crate::capability::RepoManagerCapability;
        let bus = Arc::new(EventBus::new());
        let mut devices = DeviceRegistry::new(bus);
        devices.midi_enabled.store(false, Ordering::Relaxed);

        let cap = Arc::new(RwLock::new(RepoManagerCapability::new()));
        let token = CancellationToken::new();
        // OFF 状態で daemon が起動した回でも、持ち物は控える（gate より前で控えるため）
        devices.start_roto_control(cap, token).await;
        assert!(
            devices.roto_deps.is_some(),
            "OFF 起動でも ROTO の持ち物を控える（そうしないと ON に戻せない）"
        );
        assert!(
            devices.roto_task.is_none(),
            "OFF の間は ROTO セッションを張らない"
        );
    }

    /// 同じ値への切替は no-op（冪等）。`vp midi off` を 2 回打っても離し直さない。
    #[tokio::test]
    async fn switch_is_idempotent() {
        let bus = Arc::new(EventBus::new());
        let mut devices = DeviceRegistry::new(bus);
        let start = devices.midi_enabled();
        assert!(!devices.set_midi_enabled(start).await, "同値は変化なし");
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
