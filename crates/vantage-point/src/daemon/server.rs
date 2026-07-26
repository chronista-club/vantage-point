//! Daemon の Unison QUIC サーバー
//!
//! daemon の live channel（daemon-process / events / wire / registry / device 等）を提供。
//! SP の自己登録を受け付け、process lifecycle / wire / event log を中継する。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use unison::network::quic::QuicServer;
use unison::network::{
    CertSource, MessageType, NetworkError, ProtocolServer, channel::UnisonChannel,
};

use super::protocol::{ChannelMessage, ProcessLifecycleEvent, ProcessSnapshot};
use crate::capability::{ProcessPresenceState, RunningProcess};

/// SP control channel registry（key = `path_key`、 value = SP の live control channel）。
///
/// daemon server の process-proxy / registry handler が populate し、 canvas/terminal_write の
/// reverse-routing に使う。 tmux decoupling PR1: Daemon-side の nudge loop（delivery_actor /
/// delegation reconcile）も同一 map を引いて `lane_nudge` を所有 SP に forward する（SSOT はここ）。
/// doc 44 P1 (fold-in): 旧 SP control channel registry (`path_key` → QUIC channel) の後継。
///
/// SP プロセスが消えたので「どう話すか」は QUIC channel ではなく **Daemon 内の
/// `Arc<AppState>` を引くこと**になった。型名は呼び出し側の意味 (= project へ話す口) を
/// 保つため据え置き、指す先だけを差し替えている。
pub(crate) type ControlChannels = Arc<crate::process::project_registry::ProjectRuntimes>;

/// Daemon の共有状態
pub struct DaemonState {
    /// Daemon 起動時刻（uptime計算用）
    pub started_at: Instant,
    /// 稼働中 Process 一覧（Registry チャネル経由で SP が自己登録）
    /// ProcessManagerCapability と共有される
    pub running_processes: Option<Arc<RwLock<HashMap<String, RunningProcess>>>>,
    /// プロジェクト情報（ProcessManagerCapability と共有、状態更新用）
    pub projects: Option<Arc<RwLock<HashMap<String, crate::capability::ProjectInfo>>>>,
    /// Phase 1b: 各 Project の Lane registry（ProcessManagerCapability と共有）
    /// SP が register payload に lanes を載せて push、 disconnect で全 Lane drop。
    /// agent (Echoes on Claude CLI) が `GET /api/lanes` で resolve するための cache。
    #[allow(clippy::type_complexity)]
    pub lane_registry:
        Option<Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>>,
    /// L1 lifecycle (Phase C): SP の接続 presence（ProcessManagerCapability と Arc 共有）。
    /// registry channel handler が register→Connected / unregister→Unregistered / 切断→Disconnected
    /// を書き、`/api/health` の `processes[]` が同一 Arc を読んで vp-app に expose する（doc 27 §3.2）。
    pub process_presence: Option<Arc<RwLock<HashMap<String, ProcessPresenceState>>>>,
    /// VP-154 PR-2: Process lifecycle event broadcast bus (= "daemon-process" channel の data plane)
    ///
    /// registry channel handler が SP register/unregister を受信したタイミングで `send` し、
    /// "daemon-process" subscribe handler の broadcast::Receiver が pump して client に
    /// `send_event` で push する経路。 capacity 64 = SP 同時 register が短時間に集中しても
    /// drop しない buffer (= 既存 system_event_tx と同サイズ)。
    pub process_lifecycle_tx: tokio::sync::broadcast::Sender<ProcessLifecycleEvent>,
    /// L0 SP-portless (lanes slice): lane_registry が変化した project の path_key を載せる
    /// broadcast bus。 registry channel handler が lane_registry を mutate した直後
    /// (register / lanes/add / lanes/remove / lanes/update) に `send(path_key)` し、
    /// per-project "lanes" channel の subscriber がこれを購読して当該 project の現 snapshot を
    /// vp-app に再 push する経路 (= SP "lanes" channel 直結の Daemon 集約版)。
    ///
    /// `process_lifecycle_tx` (process Add/Remove) と相補: あちらは SP の up/down、 こちらは
    /// SP 内 lane (Performer 等) の add/remove/update を realtime 配信する。 capacity 64 は
    /// 同上 (短時間に lane diff が集中しても drop しない buffer)。
    pub lane_change_tx: tokio::sync::broadcast::Sender<String>,
    /// L0 SP-portless (canvas slice): project ごとの Canvas (Board) TopicRouter。
    ///
    /// 各 SP が "canvas-ingest" channel で board の ProcessMessage を push し、 daemon は
    /// それを project の TopicRouter に `route()` する。 vp-app 向け "canvas" channel はこの
    /// TopicRouter を `subscribe("process/board/#")` して retained 初期配信 + live delta を
    /// 配る。 SP の "canvas" channel (`process/unison_server.rs`) と **同じ TopicRouter 型を再利用**
    /// することで、 retained/delta/atomicity を daemon 側で再実装せず委譲する (lanes の lane_registry
    /// に相当する canvas 版の per-project store)。 project_path (path_key) → TopicRouter。
    /// 初出 project は ingest / subscribe のどちらか早い方が get-or-create する。
    ///
    /// ライフサイクル: lane_registry と同じく **SP 切断では drop しない** (= retained canvas を
    /// SP restart 越しに保持 = 「前回の続き」)。 entry の回収は project remove (namespace ごと撤去)
    /// のタイミングが正で、 現状は未実装の follow-up (control_channels が SP 切断で remove するのと
    /// 非対称なのは意図的: control_channels は live 接続 handle、 canvas_routers は durable state)。
    /// daemon は long-lived なので project 数が大きく増える運用に入る前に project-remove cleanup を
    /// 入れる (現 dogfooding 規模では bounded growth 実害なし)。
    #[allow(clippy::type_complexity)]
    pub canvas_routers:
        Arc<RwLock<HashMap<String, Arc<crate::process::topic_router::TopicRouter>>>>,
    /// project ごとの実行状態 registry（旧「SP control channel handle」の後継）。
    ///
    /// doc 44 P1 (fold-in) 以前: 各 SP が起動時に "control" channel で daemon に outbound 接続し、
    /// daemon はその `UnisonChannel` を path_key で保持していた。"process-proxy" 経由で来た外部
    /// client (MCP/CLI) の process 操作は、この handle を**逆用**して当該 SP に forward していた
    /// (= Daemon→SP reverse-routing)。SP 切断で handle を除去 = reverse 不能、という寿命だった。
    ///
    /// fold-in 後: project は daemon と同一プロセスの `Arc<AppState>` なので、forward ではなく
    /// `ProjectRuntimes::dispatch` → `dispatch_process_method` の直呼びになる。「切断」という
    /// 状態が存在せず、map に居るか居ないかだけになった。
    pub(crate) control_channels: ControlChannels,
    /// projects 操作の権威 (= CLI → Daemon 直接 Unison "daemon-control" channel の data plane)。
    ///
    /// HTTP `routes/daemon.rs` と同一の `ProcessManagerCapability` 実体を Arc 共有し、
    /// add/remove/rename/set_enabled/reorder/list を Unison 経由でも受ける。
    /// control plane 一元化 (creo `mem_1CbmWjCGNi9z49s3r21TwQ`): projects は daemon 権威なので
    /// CLI は SP を経由せず daemon に直接 Unison RPC する (= projects.kdl 共有メモリの置換)。
    pub daemon_cap: Option<Arc<RwLock<crate::capability::ProcessManagerCapability>>>,
    /// doc 24 §10 Phase 2: lane descriptor の durable 永続先 (daemon-canonical 化)。
    ///
    /// registry channel handler が SP push (register snapshot / lanes diff) を受けた時、
    /// in-memory `lane_registry` への反映と並行して db に永続する。 これにより SP disconnect /
    /// daemon 再起動を越えて descriptor が生き残る (§3.3 re-animate / §4.1 喪失ゼロ)。
    pub vpdb: Option<crate::db::SharedVpDb>,
    /// DeviceRegistry 🧲 EventBus の参照 — "daemon-device" Unison channel の data plane。
    ///
    /// `MachineCapabilities.devices` が Some (= feature = "midi" + DeviceRegistry 稼働) のときのみ注入される。
    /// daemon-device channel handler がこれを subscribe して `devices.*` event (device 接続/切断/
    /// 操作入力) を `DeviceEvent` に変換し、 vp-app に push する。
    pub devices_event_bus: Option<Arc<crate::capability::eventbus::EventBus>>,
    /// DeviceRegistry 🧲 registry 本体の参照 — "device" Unison channel (agent → daemon) の data plane。
    ///
    /// M2 / doc 26 §2: macOS menu bar agent (Swift `CoreMIDIWatcher`) が hot-plug を `ReportDevice`
    /// で報告する。`device` channel handler がこの handle 越しに `report_device_*` を呼び、registry
    /// 更新 + `devices.*` emit を行う (emit は daemon-device bridge 経由で vp-app に届く)。
    #[cfg(feature = "midi")]
    pub devices: Option<Arc<RwLock<crate::devices::DeviceRegistry>>>,
    /// L0 portless B-4 (wire-unison): daemon 中央 wire store の参照 — "wire" Unison channel の data plane。
    ///
    /// 旧 `daemon_wire::call` の HTTP relay 先 (`POST /api/wire/*`) を unison channel に移行 (doc 27 §62
    /// 「全通信 unison」)。run_daemon が **daemon process AppState と同一 Arc** を `with_wire` で plumb する
    /// (同一プロセス)。`wire` channel handler がこれを使って wire の send/recv/thread/unread/latest/ack を
    /// 中央 store に直結する。SP mode の DaemonState では None (= wire は Daemon 専有)。
    pub wiremsg_store: Option<crate::capability::WiremsgStore>,
    /// wire long-poll (`wire_recv`) の起床通知器 — `wiremsg_store` と対で plumb される (同 AppState 由来)。
    pub wire_notifier: Option<crate::capability::WireNotifier>,
    /// command 着信時に delivery loop を即 wake する Notify — `wire/send` で category=command を検出して叩く。
    pub delivery_notify: Option<Arc<tokio::sync::Notify>>,
    /// 委譲 (delegation) の daemon 中央 store — "wire" channel の `delegation/*` method の data plane (doc 28 §6)。
    ///
    /// `daemon_wire::call("/api/delegation/*")` は wire と同じ transport を共有するため、unison 移行も同 channel に
    /// 相乗りする (path 分岐で dispatch)。run_daemon が AppState と同一 Arc を plumb する。
    /// (`DelegationStore` は pub(crate) なので本 field も crate 可視に揃える)
    pub(crate) delegation_store: Option<crate::capability::DelegationStore>,
    /// L2 (doc 27 §5-3): event log（agent の episodic memory）。always-on daemon が in-memory ring で
    /// 保持し、"events" channel の emit/query と auto-feed task（process lifecycle → event）が共有する。
    pub event_log: super::event_log::EventLog,
}

impl Default for DaemonState {
    fn default() -> Self {
        let (process_lifecycle_tx, _) = tokio::sync::broadcast::channel(64);
        let (lane_change_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            started_at: Instant::now(),
            running_processes: None,
            projects: None,
            lane_registry: None,
            process_presence: None,
            process_lifecycle_tx,
            lane_change_tx,
            canvas_routers: Arc::new(RwLock::new(HashMap::new())),
            control_channels: Arc::new(crate::process::project_registry::ProjectRuntimes::new()),
            daemon_cap: None,
            vpdb: None,
            devices_event_bus: None,
            #[cfg(feature = "midi")]
            devices: None,
            wiremsg_store: None,
            wire_notifier: None,
            delivery_notify: None,
            delegation_store: None,
            event_log: super::event_log::EventLog::new(),
        }
    }
}

impl DaemonState {
    /// 新しい DaemonState を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// ProcessManagerCapability の running_processes を共有する
    #[allow(clippy::type_complexity)]
    pub fn with_running_processes(
        mut self,
        running_processes: Arc<RwLock<HashMap<String, RunningProcess>>>,
        projects: Arc<RwLock<HashMap<String, crate::capability::ProjectInfo>>>,
        lane_registry: Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>,
        process_presence: Arc<RwLock<HashMap<String, ProcessPresenceState>>>,
    ) -> Self {
        self.running_processes = Some(running_processes);
        self.projects = Some(projects);
        self.lane_registry = Some(lane_registry);
        self.process_presence = Some(process_presence);
        self
    }

    /// projects 操作の権威 (`ProcessManagerCapability`) を共有する。
    ///
    /// HTTP `AppState.daemon` と同一の Arc を渡すことで、 Unison "daemon-control" channel から
    /// 受けた projects mutation を HTTP と同じ実体に反映する (= 入口は複数でも権威は 1 つ)。
    pub fn with_daemon_cap(
        mut self,
        daemon_cap: Arc<RwLock<crate::capability::ProcessManagerCapability>>,
    ) -> Self {
        self.daemon_cap = Some(daemon_cap);
        self
    }

    /// tmux decoupling PR1: SP control channel registry を外部の Arc と共有する。
    ///
    /// `control_channels` は SP 接続の live handle map（key = `path_key`）。 daemon server の
    /// process-proxy handler が populate し、 canvas/terminal_write の reverse-routing に使う。
    /// Daemon-side の nudge loop（delivery_actor / delegation reconcile）も同一 map を引いて
    /// `lane_nudge` を所有 SP に forward するため、 `run_daemon` が hoist した同一 Arc を
    /// DaemonState と両 loop の双方に注入する（別々に `new()` すると map が分裂して forward 不能）。
    pub(crate) fn with_control_channels(mut self, control_channels: ControlChannels) -> Self {
        self.control_channels = control_channels;
        self
    }

    /// canvas 集約 map を**外から**共有する（boot 窓の根治）。
    ///
    /// 既定では [`Default`] が内部で作るが、run_daemon は `ProjectRuntimes` と同一の map を
    /// 配る必要がある — project 起動が先行 subscribe の placeholder router を養子縁組する
    /// ため（別々に `new()` すると map が分裂し、placeholder 購読者が永遠に取り残される）。
    pub(crate) fn with_canvas_routers(
        mut self,
        canvas_routers: crate::process::topic_router::CanvasRouters,
    ) -> Self {
        self.canvas_routers = canvas_routers;
        self
    }

    /// doc 24 §10 Phase 2: lane descriptor の durable 永続先 (db/machine) を共有する。
    ///
    /// registry channel handler がこの db に SP push を永続して daemon-canonical 化する。
    /// capability の boot load (`load_config`) と同一の db を指す (= 書いた truth を起動時に読む)。
    /// vp-app への lanes push を起こす通知路を**外から**差し替える（doc 44 §11）。
    ///
    /// 既定では [`new`](Self::new) が内部で作るが、fold-in 後は **project 側の
    /// `publish_lanes` が生産者**になるため、daemon が先に channel を作って
    /// `ProjectRuntimes` と DaemonState の両方へ配る必要がある。
    /// `process_lifecycle_tx` を capability と共有しているのと同じ構図。
    pub fn with_lane_change_tx(mut self, tx: tokio::sync::broadcast::Sender<String>) -> Self {
        self.lane_change_tx = tx;
        self
    }

    pub fn with_vpdb(mut self, vpdb: crate::db::SharedVpDb) -> Self {
        self.vpdb = Some(vpdb);
        self
    }

    /// DeviceRegistry 🧲 EventBus を共有する (feature = "midi")。
    ///
    /// `run_daemon` が `MachineCapabilities.devices` の `event_bus()` を渡し、 daemon-device channel
    /// handler がこれを subscribe して device event を vp-app に push する。
    pub fn with_devices_event_bus(
        mut self,
        event_bus: Arc<crate::capability::eventbus::EventBus>,
    ) -> Self {
        self.devices_event_bus = Some(event_bus);
        self
    }

    /// DeviceRegistry 🧲 registry 本体を共有する (feature = "midi")。
    ///
    /// `device` channel handler が agent の `ReportDevice` を受けて registry を更新するために使う。
    /// `with_devices_event_bus` と同じ `MachineCapabilities.devices` を指す (event_bus は registry 内蔵)。
    #[cfg(feature = "midi")]
    pub fn with_devices(mut self, devices: Arc<RwLock<crate::devices::DeviceRegistry>>) -> Self {
        self.devices = Some(devices);
        self
    }

    /// L0 portless B-4 (wire-unison): daemon 中央 wire/delegation store を共有する。
    ///
    /// run_daemon が daemon process `AppState` 構築後に **同一 Arc** を渡す (同一プロセスなので clone で共有)。
    /// "wire" channel handler がこれらを使って `daemon_wire::call` の旧 HTTP relay 先を unison で serve する。
    /// `wiremsg_store` / `delegation_store` は DB 接続失敗時 None (= 当該 method は error を返す)。
    /// (`DelegationStore` が pub(crate) のため本 method も crate 可視。 caller は同一 crate の run_daemon)
    pub(crate) fn with_wire(
        mut self,
        wiremsg_store: Option<crate::capability::WiremsgStore>,
        wire_notifier: crate::capability::WireNotifier,
        delivery_notify: Arc<tokio::sync::Notify>,
        delegation_store: Option<crate::capability::DelegationStore>,
    ) -> Self {
        self.wiremsg_store = wiremsg_store;
        self.wire_notifier = Some(wire_notifier);
        self.delivery_notify = Some(delivery_notify);
        self.delegation_store = delegation_store;
        self
    }
}

/// `registry.list` の応答 body（稼働中 project の snapshot）。
///
/// ## doc 45 — processes 一覧の唯一の面
///
/// 旧 HTTP `GET /api/daemon/processes` は段 4 で撤去し、稼働中 project を配るのはここだけに
/// なった（vp-app は段 3、CLI は段 2 で移設済み）。map を JSON にする写し方は手書き object を
/// やめて `RunningProcess` の Serialize をそのまま使う —— field を足した時に写し漏れる、が
/// 構造的に起きない。振る舞いテスト: `registry_list_snapshot_carries_name_and_port`。
pub(crate) async fn registry_process_snapshot(
    running_processes: &Arc<RwLock<HashMap<String, RunningProcess>>>,
) -> Vec<serde_json::Value> {
    let procs = running_processes.read().await;
    procs
        .values()
        .map(|p| serde_json::to_value(p).unwrap_or_default())
        .collect()
}

// =========================================================================
// daemon control Channel ハンドラー（projects mutation: CLI → Daemon 直接 Unison）
// =========================================================================

/// "daemon-control" channel の method を `ProcessManagerCapability` に dispatch する。
///
/// 戻り値は成功時 result JSON、失敗時は `Err(String)`。 caller は Unison の慣習 (VP-163) に
/// 従い success frame に `{"error": ...}` を詰めて返す (= Unison は専用 error frame を持たない)。
///
/// ## doc 45 — control plane の唯一の入口
///
/// 段 1 で `routes/daemon.rs`（旧 HTTP）にしか無かった操作をここへ出し
/// （`projects/update` `projects/reload` `projects/sync` `projects/restart` `projects/pointview`
/// `lanes/create` `lanes/set_active`、および `lanes/list` の filter/sort）、段 2 で CLI・
/// 段 3 で vp-app を移設、**段 4 で HTTP route を撤去**した。projects CRUD / lifecycle / lanes を
/// 触れる面は現在ここだけで、HTTP に残るのは `/api/health` `/api/shutdown` の 2 本のみ（§2）。
///
/// route 層にしか無かった orchestration は `routes::daemon` の `pub(crate)` 関数に括り出して
/// ある（`apply_project_update` / `collect_lanes` / `resolve_create_lane_args`）。段 1 で
/// 1 実装に畳んであったので、段 4 の撤去は handler の殻を剥がすだけで済んだ。
///
/// `pub(crate)` なのは同 crate のテストから直接叩くため（Unison 経路を実際に張らずに
/// dispatch の振る舞いを固定する）。
pub(crate) async fn handle_daemon_control(
    daemon_cap: &Arc<RwLock<crate::capability::ProcessManagerCapability>>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Moody Blues PR-D review #2: read guard を arm ごとに取り直し、 mutation の await 完了後に
    // 即解放する (= 複数 Unison/HTTP リクエストが outer read guard を長時間共有しない)。
    // 内部 mutation は ProcessManagerCapability の Arc<RwLock> field で直列化される。
    match method {
        "projects/list" => {
            let list = daemon_cap.read().await.list_projects().await;
            serde_json::to_value(&list).map_err(|e| e.to_string())
        }
        "projects/add" => {
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let info = daemon_cap
                .read()
                .await
                .add_project(name, path)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(&info).map_err(|e| e.to_string())
        }
        "projects/remove" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            daemon_cap
                .read()
                .await
                .remove_project(path)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "removed", "path": path}))
        }
        "projects/rename" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            daemon_cap
                .read()
                .await
                .rename_project(path, name)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "renamed", "path": path, "name": name}))
        }
        "projects/set_enabled" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let enabled = payload["enabled"]
                .as_bool()
                .ok_or_else(|| "enabled is required".to_string())?;
            daemon_cap
                .read()
                .await
                .set_project_enabled(path, enabled)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "ok", "path": path, "enabled": enabled}))
        }
        // doc 45 段 1: HTTP `POST /api/daemon/projects/update` の Unison 版。
        // rename と set_enabled をまとめて適用する部分更新（vp-app の編集 dialog が使う形）。
        // 個別の `projects/rename` / `projects/set_enabled` と実体は同じで、こちらは
        // 「1 往復で両方直す」ための合成入口（HTTP と同じ `apply_project_update` を共有）。
        "projects/update" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let name = payload["name"].as_str();
            let enabled = payload["enabled"].as_bool();
            let cap = daemon_cap.read().await;
            crate::process::routes::daemon::apply_project_update(&cap, path, name, enabled).await?;
            Ok(serde_json::json!({"status": "updated", "path": path}))
        }
        // doc 45 段 1: HTTP `POST /api/daemon/projects/sync` の Unison 版。
        // projects.kdl / db から ghost project（dir が実在しない登録）を除去する。
        // `vp sync` / `vp app start` が叩く（daemon 不在時は CLI 側が kdl 直操作に落ちる）。
        "projects/sync" => {
            let outcome = daemon_cap
                .read()
                .await
                .sync_projects()
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({ "removed": outcome.removed }))
        }
        // doc 45 段 1: HTTP `POST /api/daemon/projects/reload` の Unison 版。
        // projects.kdl を読み直して in-memory projects に反映する（VP-189）。
        // CLI が projects.kdl を書き換えた後に稼働 daemon の乖離を解消する best-effort 通知。
        "projects/reload" => {
            daemon_cap.read().await.reload_config().await;
            Ok(serde_json::json!({"status": "reloaded"}))
        }
        "projects/reorder" => {
            let paths: Vec<String> = serde_json::from_value(payload["paths"].clone())
                .map_err(|e| format!("paths is required (string array): {}", e))?;
            daemon_cap
                .read()
                .await
                .reorder_projects(&paths)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "reordered", "count": paths.len()}))
        }
        // doc 44 P1 (fold-in): 単一 project の lifecycle 制御。旧 `vp sp start/stop` の後継で、
        // 名詞を「SP（プロセス）」から「project」へ移した（D2: project はプロセスではなく
        // daemon が抱える map のエントリ）。旧 `vp sp start` は project を daemon の外で
        // 二重に走らせる口だったが、本 RPC は Daemon 内の registry を操作するため
        // 二重起動が原理的に表現できない（既に居れば `start` は no-op になる）。
        "projects/start" => {
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            let proc = daemon_cap
                .read()
                .await
                .start_process(name)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "status": "started",
                "name": proc.project_name,
                "path": proc.project_path.to_string_lossy(),
            }))
        }
        "projects/stop" => {
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            daemon_cap
                .read()
                .await
                .stop_process(name)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "stopped", "name": name}))
        }
        // doc 45 段 1: HTTP `POST /api/daemon/processes/{name}/restart` の Unison 版。
        // stop + start を daemon 側で atomically に繋ぐ（MCP の `restart` tool が唯一の消費者）。
        //
        // 内部に grace sleep + 起動確認が入るので、HTTP handler と同じく **read guard を
        // 保持したまま await しない**（capability を clone してから解放する）。ここを guard 越しに
        // すると restart 中の数秒、他の daemon-control / HTTP リクエストが全部待たされる。
        "projects/restart" => {
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            let cap = {
                let guard = daemon_cap.read().await;
                guard.clone()
            };
            let proc = cap.restart_process(name).await.map_err(|e| e.to_string())?;
            serde_json::to_value(&proc).map_err(|e| e.to_string())
        }
        // doc 45 段 1: HTTP `POST /api/daemon/processes/{name}/pointview` の Unison 版。
        // project の PointView を開く（未起動なら内部で start_process する）。
        // restart と同じ理由で capability を clone してから await する。
        "projects/pointview" => {
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            let cap = {
                let guard = daemon_cap.read().await;
                guard.clone()
            };
            cap.open_pointview(name).await.map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "opened", "name": name}))
        }
        // 全 project 横断の lane 一覧（read-only）。
        //
        // 従来この面は HTTP `GET /api/daemon/lanes` にしか無く、CLI は Unison で繋いだ後に
        // わざわざ HTTP を叩く必要があった。control plane は Unison に寄せる方針（KDL schema +
        // drift テスト + MCP tool 合成が付いてくる）なので、read 面をここに置く。
        // project 単位の詳細は process-proxy の `lanes_list` が持つ。
        //
        // doc 45 段 1: HTTP 版の query filter (project / lane / stand) と表示順を取り込んだ。
        // ここが素の flatten のままだと、CLI を Unison に移した瞬間に一覧の並びが静かに変わる。
        // filter/sort は `routes::daemon::collect_lanes` を HTTP と共有する。
        "lanes/list" => {
            let query: crate::process::routes::daemon::LanesQuery =
                serde_json::from_value(payload).unwrap_or_default();
            let cap = daemon_cap.read().await;
            let lanes = crate::process::routes::daemon::collect_lanes(&cap, &query).await;
            Ok(serde_json::json!({ "count": lanes.len(), "lanes": lanes }))
        }
        // doc 45 段 1: HTTP `POST /api/daemon/lanes` の Unison 版（doc 24 §10 Phase 2 B-create）。
        // doc 44 §9.4: 実体は project runtime の lane 作成 core（`create_performer_orchestrated`）
        // 1 本で、`create_lane` はそこへの adapter。ここに残るのは「省略時 default の導出」だけ
        // （data/calc は route の責務 = `resolve_create_lane_args` を CLI/GUI と共有）。
        "lanes/create" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            let (branch, stand) = crate::process::routes::daemon::resolve_create_lane_args(
                path,
                name,
                payload["branch"].as_str(),
                payload["stand"].as_str(),
            );
            let info = daemon_cap
                .read()
                .await
                .create_lane(path, name, &branch, &stand)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(&info).map_err(|e| e.to_string())
        }
        // doc 45 段 1: HTTP `POST /api/daemon/lanes/active` の Unison 版。
        // project の active lane (presence、Model Q) を daemon-canonical に設定する。
        "lanes/set_active" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let address = payload["address"]
                .as_str()
                .ok_or_else(|| "address is required".to_string())?;
            daemon_cap
                .read()
                .await
                .set_active_lane(path, address)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "active_lane set", "path": path, "address": address}))
        }
        // doc 44 §7.5: Project Host の帳簿（見送りの記録）を CLI から触る 3 面。
        //
        // 帳簿は db/machine にあり surrealkv の OS 排他ロックで daemon が専有するので、
        // `vp lane cleanup` / `vp lane history` は直接読み書きできない（§8.4 と同じ理由）。
        //
        // ⚠️ **名前 → id の解決はここでしない**。CLI が観測と一緒に `lane_id` を送る。
        // 見送りの対象には「daemon が一度も見たことのない lane」（disk にだけ在る worktree）が
        // 含まれ、daemon の registry から引くとそれらが黙って記録から落ちるため — 落ちるのは
        // まさに放置されて溜まった lane なので、追いたいものだけが追えなくなる。
        // id の SSOT は lane_ids state file で、両プロセスが同じ derivation で引く。
        "host/farewell_observe" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let observations: Vec<crate::host::ledger::FarewellObservation> =
                serde_json::from_value(payload["observations"].clone())
                    .map_err(|e| format!("observations が不正: {e}"))?;
            let db = daemon_cap.read().await.vpdb().cloned();
            let now = chrono::Utc::now().to_rfc3339();
            let pending = crate::host::ledger::record_farewell_observations(
                db.as_ref(),
                path,
                &observations,
                &now,
            )
            .await;
            Ok(serde_json::json!({ "pending": pending }))
        }
        "host/farewell_reclaimed" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let lanes: Vec<crate::host::ledger::FarewellObservation> =
                serde_json::from_value(payload["lanes"].clone())
                    .map_err(|e| format!("lanes が不正: {e}"))?;
            let db = daemon_cap.read().await.vpdb().cloned();
            let now = chrono::Utc::now().to_rfc3339();
            let recorded =
                crate::host::ledger::record_farewell_reclaimed(db.as_ref(), path, &lanes, &now)
                    .await;
            Ok(serde_json::json!({ "recorded": recorded }))
        }
        "host/farewell_log" => {
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let limit = payload["limit"].as_u64().unwrap_or(0) as usize;
            let db = daemon_cap.read().await.vpdb().cloned();
            let entries = crate::host::ledger::farewell_history(db.as_ref(), path, limit).await;
            Ok(serde_json::json!({ "entries": entries }))
        }
        // chronista-hub federation: hub registry に居る daemon 一覧を取得する。
        // SSOT 原則により hub と話すのは daemon のみ。CLI / プログラム経路はこの RPC を叩く
        // (= 直接 hub に接続しない)。hub addr（env > config.kdl）未設定なら federation 無効を返す。
        "hub/discover" => {
            let Some(addr) = crate::daemon::hub_client::hub_addr() else {
                return Err(format!(
                    "hub addr 未設定（env {} / config.kdl hub-addr）— hub federation 無効",
                    crate::daemon::hub_client::HUB_ADDR_ENV
                ));
            };
            let client = crate::daemon::hub_client::HubClient::connect(&addr, 3)
                .await
                .map_err(|e| e.to_string())?;
            let nodes = client.discover().await.map_err(|e| e.to_string())?;
            // channel 慣習 (registry.list={processes} / events.query={events}) に合わせ
            // object で包む。unison-mcp synthesized tool の returns 記述 (vp-daemon.kdl) と一致。
            Ok(serde_json::json!({ "worlds": nodes }))
        }
        // F1b heartbeat: surface (vp-app) の共有 connection liveness probe。 client→server の
        // 一方向で、 server は応答するだけ (世界状態に触れない no-op)。 vp-app の
        // `spawn_daemon_conn_manager` が 15s ごとに送り、 応答が来なければ connection 死と判断して
        // 再接続する (passive subscriber だけだと dead 検知が QUIC idle timeout 60s 任せになる対策)。
        "ping" => Ok(serde_json::json!({ "pong": true })),
        other => Err(format!("不明なメソッド: daemon-control.{}", other)),
    }
}

// =========================================================================
// チャネルレスポンス送信ヘルパー
// =========================================================================

/// ChannelMessage を UnisonChannel 経由で送信する
///
/// ChannelMessage::Response は send_response() で、
/// ChannelMessage::Error は send_response() でエラーペイロードとして送信する。
/// device.report_device: agent (Swift menu bar) からの CoreMIDI hot-plug 報告を DeviceRegistry registry に反映する。
///
/// doc 26 §2 `ReportDevice` request。`state` = "connected" | "disconnected" で分岐し、
/// `DeviceRegistry::report_device_*` が registry 更新 + `devices.*` emit を行う (emit は既存 daemon-device
/// bridge 経由で vp-app に届く)。
#[cfg(feature = "midi")]
async fn handle_device_report(
    devices: &Arc<RwLock<crate::devices::DeviceRegistry>>,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: super::protocol::ReportDeviceRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    let b = devices.read().await;
    match req.state.as_str() {
        "connected" => {
            b.report_device_connected(&req.port_name, req.has_input, req.has_output)
                .await;
        }
        "disconnected" => {
            b.report_device_disconnected(&req.port_name).await;
        }
        other => return ChannelMessage::err(id, format!("不明な device state: {}", other)),
    }

    ChannelMessage::ok(id, serde_json::json!({ "ok": true }))
}

async fn send_channel_response(
    channel: &UnisonChannel,
    method: &str,
    response: ChannelMessage,
) -> Result<(), NetworkError> {
    match response {
        ChannelMessage::Response { id, payload } => {
            channel.send_response(id, method, &payload).await
        }
        ChannelMessage::Error { id, message } => {
            channel
                .send_response(id, method, &serde_json::json!({"error": message}))
                .await
        }
        // Event やその他の型はそのまま送信
        _ => Ok(()),
    }
}

/// `list_all_lanes` の cross-project join を純粋化した共有関数。
///
/// `running_processes`（port/name の SSOT）と `lane_registry` を project ごとに join し、
/// `daemon_cap` の project_order で安定ソートした projects 配列（`Vec<serde_json::Value>`）を返す。
/// "daemon-process" channel の `list_all_lanes` handler と、DeviceRegistry 常駐 ROTO loop の
/// `InProcessLaneSource`（in-process 直読み）が **同一ロジックを共有**することで、
/// CLI（QUIC 経由）と daemon（直読み）で lane 並びが完全一致する（doc 23 の重複回避）。
///
/// ロック順序: running_processes → lane_registry（register と同順、deadlock 回避）。
#[allow(clippy::type_complexity)]
pub(crate) async fn build_node_lanes(
    running_processes: &Arc<RwLock<HashMap<String, RunningProcess>>>,
    lane_registry: &Option<
        Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>,
    >,
    daemon_cap: &Option<Arc<RwLock<crate::capability::ProcessManagerCapability>>>,
) -> Vec<serde_json::Value> {
    // 並び順は sidebar と一致させる（= project_order）。物理 controller は位置 = 意味なので、
    // track button N の位置が sidebar の N 番目と対応する必要がある。
    let order: Vec<String> = match daemon_cap {
        Some(w) => w
            .read()
            .await
            .list_projects()
            .await
            .into_iter()
            .map(|p| p.name)
            .collect(),
        None => Vec::new(),
    };
    // ロック順序統一: running_processes → lane_registry（register と同順）。
    let mut entries: Vec<(usize, serde_json::Value)> = Vec::new();
    {
        let procs = running_processes.read().await;
        let lanes_map = match lane_registry {
            Some(lr) => Some(lr.read().await),
            None => None,
        };
        for (key, p) in procs.iter() {
            let lanes = lanes_map
                .as_ref()
                .and_then(|m| m.get(key))
                .cloned()
                .unwrap_or_default();
            // project_order 内の位置。未登録は末尾（usize::MAX）。
            let idx = order
                .iter()
                .position(|n| n == &p.project_name)
                .unwrap_or(usize::MAX);
            entries.push((
                idx,
                serde_json::json!({
                    "project_name": p.project_name,
                    "project_path": p.project_path.to_string_lossy(),
                    "port": p.port,
                    "lanes": lanes,
                }),
            ));
        }
    }
    // project_order 順に整列（= sidebar 順）。同 idx は安定ソートで維持。
    entries.sort_by_key(|(idx, _)| *idx);
    entries.into_iter().map(|(_, v)| v).collect()
}

/// L0 SP-portless (lanes slice): per-project "lanes" channel に現 lane snapshot を 1 回 push する。
///
/// `lane_registry[path_key]` の現値を `ProcessMessage::LanesSnapshot` に包み、 SP "lanes" channel と
/// **同一の event 形** (`method="snapshot"`、 payload = `{"type":"lanes_snapshot","lanes":[...]}`) で
/// 送る。 これにより vp-app の consumer (`run_lanes_session`) は接続先が SP→daemon に変わっても無改造。
/// 登録が無い project は空 Vec を送る (SP 未登録/lane 無しを「空」として正しく表現)。
///
/// FSM 投影 (2026-07-11): 送信直前に performer LaneInfo へ `flow_state` を enrich する
/// ([`enrich_lanes_flow_state`])。 送信時 derive であり `lane_registry` / db には書き戻さない。
#[allow(clippy::type_complexity)]
async fn send_lanes_snapshot(
    channel: &UnisonChannel,
    lane_registry: &Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>,
    path_key: &str,
    wiremsg_store: &Option<crate::capability::WiremsgStore>,
    running_processes: &Option<Arc<RwLock<HashMap<String, RunningProcess>>>>,
    vpdb: &Option<crate::db::SharedVpDb>,
) -> Result<(), NetworkError> {
    let mut lanes = lane_registry
        .read()
        .await
        .get(path_key)
        .cloned()
        .unwrap_or_default();
    if let Some(store) = wiremsg_store {
        // wire address の `<project>` は registry 登録名 (= SP の config 登録名) が SSOT
        // (`resolve::project_name_from_path` と同値)。 SP 未接続の窓は lane address の
        // project (basename 由来) で代用する (両者は通常一致)。
        let project_name = match running_processes {
            Some(rp) => rp
                .read()
                .await
                .get(path_key)
                .map(|p| p.project_name.clone()),
            None => None,
        }
        .or_else(|| lanes.first().map(|l| l.address.project.clone()));
        if let Some(project_name) = project_name {
            enrich_lanes_flow_state(&mut lanes, store, &project_name).await;
        }
    }
    // doc 44 D4: 開発起点を帳簿から解決して添える。project runtime 側の publish
    // (`process::server::publish_lanes`) と**同じ解決**を通す — 片方だけだと受け手が
    // 接続経路によって起点の有無で flicker する。
    let origin = crate::host::ledger::origin_name_for_lanes(vpdb.as_ref(), path_key, &lanes).await;
    let snapshot = crate::protocol::ProcessMessage::LanesSnapshot {
        lanes,
        origin: Some(origin),
    };
    let json = serde_json::to_value(&snapshot).unwrap_or_default();
    channel.send_event("snapshot", &json).await
}

/// FSM 投影 (2026-07-11): performer LaneInfo へ dev-flow FSM の現在 state を enrich する。
///
/// source は `vp flow progress` / MCP `flow_progress` と同一判定 (`flow::derive_flow_state`):
/// wire store の latest msg + 未 ack needs_user + `LaneInfo.performer_status`。 daemon は
/// wire store を in-process に持つため hop なしで derive できる (= 計算点を daemon に置く理由)。
/// conductor は dev-flow FSM の対象外 (spine の頭) で `None` のまま。 store クエリ失敗は
/// 当該 lane を `None` に留めて degrade (client 側は pid heuristic に fallback)。
async fn enrich_lanes_flow_state(
    lanes: &mut [crate::process::lanes_state::LaneInfo],
    store: &crate::capability::WiremsgStore,
    project_name: &str,
) {
    for lane in lanes.iter_mut() {
        if lane.address.is_root() {
            continue;
        }
        let agent_addr = format!("agent@{}/{}", project_name, lane.address.name);
        let latest = store
            .latest_msg_for_agent(&agent_addr)
            .await
            .unwrap_or_default();
        let needs_user = store
            .pending_needs_user(&agent_addr)
            .await
            .unwrap_or_default();
        // performer_status は SP push の LaneInfo に埋まっている typed 値をそのまま view 化
        // (PerformerStatusView::from_json と同じ判定規則)。
        let ps_view = lane
            .performer_status
            .as_ref()
            .map(|ps| crate::flow::PerformerStatusView {
                dirty: ps.dirty_count > 0,
                has_commit: !ps.last_commit.is_empty() && ps.last_commit != "-",
            })
            .unwrap_or_default();
        let latest_view = latest.as_ref().map(wire_msg_view);
        let needs_view = needs_user.as_ref().map(wire_msg_view);
        let fsm = crate::flow::derive_flow_state(
            latest_view.as_ref(),
            ps_view,
            &agent_addr,
            needs_view.as_ref(),
        );
        lane.flow_state = Some(fsm.state);
    }
}

/// `WireMessage` (typed、 Daemon in-process) → `LatestMsgView`。 JSON round-trip 不要の直変換。
fn wire_msg_view(m: &crate::capability::WireMessage) -> crate::flow::LatestMsgView {
    crate::flow::LatestMsgView {
        from_addr: m.from.clone(),
        body_kind: m
            .body
            .get("kind")
            .and_then(|v| v.as_str())
            .map(String::from),
        created_at_ms: m.created_at as i64,
    }
}

/// FSM 投影: wire payload から関与 project 名を抽出する (純関数)。
///
/// `wire/send` (from + to[]) / `wire/ack` (agent) の payload に現れる agent address
/// (`<actor>@<project>[/<lane>]`) から `<project>` を集める。 flow_state は wire 活動で
/// 変わるため、 send/ack 成功後にこれらの project の "lanes" subscriber へ再 push を促す。
fn collect_wire_projects(payload: &serde_json::Value) -> Vec<String> {
    let mut projects = std::collections::BTreeSet::new();
    let mut push_addr = |addr: &str| {
        if let Some((_, rest)) = addr.split_once('@') {
            let project = rest.split('/').next().unwrap_or("");
            if !project.is_empty() {
                projects.insert(project.to_string());
            }
        }
    };
    for key in ["from", "agent"] {
        if let Some(a) = payload.get(key).and_then(|v| v.as_str()) {
            push_addr(a);
        }
    }
    if let Some(to) = payload.get("to").and_then(|v| v.as_array()) {
        for a in to.iter().filter_map(|v| v.as_str()) {
            push_addr(a);
        }
    }
    projects.into_iter().collect()
}

/// FSM 投影: wire 活動 (send/ack) 後に、 関与 project の "lanes" subscriber へ snapshot
/// 再 push を促す。 flow_state は送信時 derive のため wire 活動がそのまま sidebar の
/// 更新トリガになる (= event-driven、 polling 無し)。 未登録 project は黙って skip (best-effort)。
async fn notify_lane_change_for_projects(state: &DaemonState, projects: &[String]) {
    let Some(rp) = &state.running_processes else {
        return;
    };
    if projects.is_empty() {
        return;
    }
    let procs = rp.read().await;
    for (path_key, proc) in procs.iter() {
        if projects.iter().any(|p| p == &proc.project_name) {
            // receiver 不在 (subscriber 0) の SendError は無害なので無視
            let _ = state.lane_change_tx.send(path_key.clone());
        }
    }
}

/// L0 SP-portless (canvas slice): project の Canvas TopicRouter を get-or-create する。
///
/// "canvas-ingest" (SP push) と "canvas" (vp-app subscribe) のどちらが先でも、 同じ project の
/// TopicRouter を共有する (= SP push が router に route し、 vp-app subscribe が同 router を購読)。
///
/// S2 (doc 27 §4.1): router 新規作成時に terminal demand hook を登録する。 surface が
/// `process/terminal/data/{lane}/out` を購読した瞬間 (0→1) / 最後に離れた瞬間 (1→0) に、
/// 当該 SP の control channel を逆用して `terminal_demand_start/stop {lane}` を撃つ
/// (= 購読者が居る間だけ SP pump を回す demand-driven production)。
#[allow(clippy::type_complexity)]
async fn canvas_router_for(
    canvas_routers: &Arc<RwLock<HashMap<String, Arc<crate::process::topic_router::TopicRouter>>>>,
    control_channels: &ControlChannels,
    path_key: &str,
) -> Arc<crate::process::topic_router::TopicRouter> {
    // doc 44 P1 (fold-in): project が起動していれば **その AppState の router が唯一の正**。
    // pump が route する先と surface が購読する先を同一にするため、cache に別 router が
    // 載っていたら差し替える（project 起動前に surface が subscribe して placeholder が
    // 作られていた場合の是正。placeholder には元々データが流れないので失うものは無い）。
    if let Some(state) = control_channels.get(path_key).await {
        let live = state.topic_router.clone();
        {
            let routers = canvas_routers.read().await;
            if let Some(existing) = routers.get(path_key)
                && Arc::ptr_eq(existing, &live)
            {
                return live;
            }
        }
        let mut routers = canvas_routers.write().await;
        // race recheck（他 task が先に差し替えていたらそれを使う）
        if let Some(existing) = routers.get(path_key)
            && Arc::ptr_eq(existing, &live)
        {
            return live;
        }
        tracing::info!(
            "canvas router を project の実 router に結線 (key={})",
            path_key
        );
        register_terminal_demand(&live, control_channels.clone(), path_key.to_string());
        register_echoes_demand(&live, control_channels.clone(), path_key.to_string());
        routers.insert(path_key.to_string(), live.clone());
        return live;
    }

    // fast path: 既存 router を read lock で取得（project 未起動時の placeholder 経路）
    if let Some(router) = canvas_routers.read().await.get(path_key) {
        return router.clone();
    }
    // slow path: write lock で get-or-create。 race recheck で 2 重作成を防ぐ
    // (entry().or_insert_with() は async な demand 登録を挟めないため手動 double-check)。
    let mut routers = canvas_routers.write().await;
    if let Some(router) = routers.get(path_key) {
        return router.clone();
    }
    // project 未起動時の placeholder。 起動後に上のブロックが実 router へ差し替える。
    let router = Arc::new(crate::process::topic_router::TopicRouter::new());
    register_terminal_demand(&router, control_channels.clone(), path_key.to_string());
    // Act II: chat lane の transcript replay-on-attach（terminal と対称）。
    register_echoes_demand(&router, control_channels.clone(), path_key.to_string());
    routers.insert(path_key.to_string(), router.clone());
    router
}

/// S2 (doc 27 §4.1 Cap2): project canvas router に terminal demand hook を登録する。
///
/// `process/terminal/data/+/out` の購読者増減 0↔1 で `terminal_demand_start/stop {lane}` を
/// 当該 SP に control reverse-route で撃つ（Act I: PtySlot の replay + live pump 起動）。
fn register_terminal_demand(
    router: &Arc<crate::process::topic_router::TopicRouter>,
    control_channels: ControlChannels,
    path_key: String,
) {
    register_lane_demand(
        router,
        control_channels,
        path_key,
        "process/terminal/data/+/out",
        "terminal_demand_start",
        "terminal_demand_stop",
    );
}

/// Act II replay-on-attach: project canvas router に echoes demand hook を登録する。
///
/// `process/echoes/data/+/event` の購読者 0↔1 で `echoes_demand_start/stop {lane}` を撃つ。
/// start を受けた SP は chat lane の transcript を `EchoesEvent` に起こして replay する
/// （非 retained topic なので、 これが無いと app 再起動後の ChatView が空になる）。
fn register_echoes_demand(
    router: &Arc<crate::process::topic_router::TopicRouter>,
    control_channels: ControlChannels,
    path_key: String,
) {
    register_lane_demand(
        router,
        control_channels,
        path_key,
        "process/echoes/data/+/event",
        "echoes_demand_start",
        "echoes_demand_stop",
    );
}

/// per-lane topic の demand hook を登録する共通実装（terminal / echoes で共有）。
///
/// `pattern` は `process/<x>/data/+/<y>` 形（lane key は segment 3）。 購読者の増減のたびに
/// **その時点の level**（`demand_active`）を読んで `start_method` / `stop_method` を当該
/// project に撃つ。 cb は sync で呼ばれるため、 reverse-route (async I/O) は `tokio::spawn` に逃がす。
///
/// ⚠️ **cb の第 2 引数（増減方向）で分岐しない**（doc 53 §2.3 — edge → level）。GUI が死んでも
/// daemon は QUIC の idle timeout（~60s）まで気づかないので、**新購読が先・旧購読の掃除が後**に
/// なる順序逆転が起きる。方向で分岐すると「count=2 の増加」を start と読んで撃つべきときに
/// 撃たない / 「count=1 に減っただけ」を stop と読んで撃つ、が起きうる。level を読めば
/// 順序に依存しない（受け手の reconcile は冪等なので、同じ結論に何度到達しても構わない）。
fn register_lane_demand(
    router: &Arc<crate::process::topic_router::TopicRouter>,
    control_channels: ControlChannels,
    path_key: String,
    pattern: &str,
    start_method: &'static str,
    stop_method: &'static str,
) {
    let router_for_level = Arc::downgrade(router);
    router.register_demand(pattern, move |topic, _added| {
        // topic = `process/<x>/data/{lanekey}/<y>` → lane address を復元
        // (topic key は LaneAddress の '/' を '~' に encode したもの。 逆変換する)。
        let Some(lane_key) = topic.split('/').nth(3) else {
            return;
        };
        let lane = lane_key.replace('~', "/");
        // **今 購読者が居るか**で決める（増減方向は見ない — 上記 ⚠️）。router が既に落ちて
        // いる場合は撃たない（level を読めない = 判断材料が無い）。
        let Some(router) = router_for_level.upgrade() else {
            return;
        };
        let method = if router.demand_active(&topic) {
            start_method
        } else {
            stop_method
        };
        let control_channels = control_channels.clone();
        let path_key = path_key.clone();
        // doc 44 P1 (fold-in): 旧実装は SP の control channel を逆引きして request を
        // 撃っていた。 channel 不在（SP 起動前に surface が subscribe した等）は無言で
        // 捨てられ、 救済は `refire_active_demands` 頼み — この取りこぼしが #817 の
        // 「Act II が復活しない」の根本原因 1 だった。
        //
        // 同一プロセスになった今、 demand は当該 project の dispatch を直接叩く。
        // 「購読が立った時点で project が居るか」だけが条件になり、 channel 接続状態と
        // 購読状態が別々に揺れることによるレースが構造的に消える。
        tokio::spawn(async move {
            let resp = control_channels
                .dispatch(&path_key, method, &serde_json::json!({ "lane": lane }))
                .await;
            if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                tracing::warn!(
                    "lane demand dispatch 失敗 (key={}, lane={}, {}): {}",
                    path_key,
                    lane,
                    method,
                    err
                );
            }
        });
    });
}

/// L0 SP-portless: project 単位 channel 共通の subscribe handshake を待つ。
///
/// client (SP canvas-ingest / SP control / vp-app lanes・canvas / MCP process-proxy) は接続後に
/// `request("subscribe", {project_path})` を送る。 project_path を path_key に正規化して返す。
/// canvas / lanes / control / process-proxy の 4 系統が同一 handshake protocol を共有する
/// (= 将来 channel 固有 field を足すなら、 その channel は本 helper から分離すること)。
/// 接続断 / 不正 payload で `None`。
async fn recv_subscribe_handshake(channel: &UnisonChannel) -> Option<String> {
    recv_subscribe_handshake_with_pattern(channel)
        .await
        .map(|(path_key, _pattern)| path_key)
}

/// `recv_subscribe_handshake` の pattern 付き版 (S2 / doc 27 §4.1 step 1)。
///
/// subscribe payload に任意の `pattern` field を許す。 canvas channel は購読対象 topic を
/// この pattern で指定する (例: terminal surface は `process/terminal/data/{lane}/out`)。
/// 既存 vp-app は `pattern` を送らないので `None` を返し、 caller 側で board default に
/// フォールバックする (= 既存 canvas 購読は無改造で動く)。
async fn recv_subscribe_handshake_with_pattern(
    channel: &UnisonChannel,
) -> Option<(String, Option<String>)> {
    loop {
        let msg = channel.recv().await.ok()?;
        if msg.msg_type != MessageType::Request || msg.method != "subscribe" {
            continue;
        }
        let payload = msg.payload_as_value().unwrap_or_default();
        let project_path = payload
            .get("project_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pattern = payload
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let path_key = crate::capability::normalize_path_key(std::path::Path::new(project_path));
        let _ = channel
            .send_response(msg.id, "subscribe", &serde_json::json!({"status": "ok"}))
            .await;
        return Some((path_key, pattern));
    }
}

/// L0 SP-portless: 外部 client の process method を当該 SP の control channel を逆用して forward する。
///
/// "process-proxy" channel (MCP/CLI) と bidirectional "canvas" channel の upstream request
/// (S3 terminal_write/terminal_resize) が共有する。 SP 未接続 / forward 失敗は error JSON で
/// 返し、 caller が `send_response` でそのまま client に relay する。
pub(crate) async fn forward_to_sp_control(
    runtimes: &ControlChannels,
    path_key: &str,
    method: &str,
    payload: &serde_json::Value,
) -> serde_json::Value {
    // doc 44 P1 (fold-in): 旧実装は path_key で SP の QUIC control channel を逆引きし
    // request を投げていた。SP プロセスが daemon に畳み込まれた今、同じ dispatch
    // (`dispatch_process_method`) を **同一プロセス内で直接呼ぶ**。
    // これで reverse-route の取りこぼし (SP 未接続の無言破棄 / refire の空振りレース /
    // start・stop の到着順非保証) というバグクラスが発生源ごと消える。
    runtimes.dispatch(path_key, method, payload).await
}

/// L0 portless B-4 (wire-unison): "wire" channel の method dispatch。
///
/// `daemon_wire::call` が path `"/api/<rest>"` を method=`"<rest>"` (= `"wire/send"` /
/// `"delegation/create"` 等) にして本 channel に投げてくる。prefix で wire / delegation を切り分け、
/// `routes::{wire,delegation}::dispatch_*` に委譲する。store は `with_wire` で plumb された
/// daemon process AppState 由来の Arc。未初期化 (SP mode / DB 接続失敗) は Err を返し、channel
/// handler が `{"error": ...}` フレームに詰める (旧 HTTP handler の error JSON と等価)。
async fn handle_wire_channel(
    state: &DaemonState,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // 供給 push 根治（session chip 凍結、2026-07-17）: claude UserPromptSubmit hook が
    // cc_session pointer を動かした時の変化通知。SP は portless で hook は Daemon しか
    // 知らないため、ここで project 名 → path_key を lane_registry から逆引きし、当該 SP の
    // control channel に `lane_session_changed` を forward する。SP が真値を re-enrich して
    // `Diff::Update` を push → 本 daemon の "lanes/update" 受信 → registry replace +
    // lanes snapshot 再 push、という既存経路に乗る（daemon は routing のみ、真実源は SP）。
    // SP 不在 / 未接続は Err（hook 側は fail-open で握る）。
    if method == "lane/session-changed" {
        let project = payload
            .get("project")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "lane/session-changed: 'project' (project 名) required".to_string())?;
        let label = payload
            .get("lane")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "lane/session-changed: 'lane' (lane label) required".to_string())?;
        let lr = state
            .lane_registry
            .as_ref()
            .ok_or_else(|| "lane registry not initialized".to_string())?;
        let path_key = {
            let registry = lr.read().await;
            registry
                .iter()
                .find(|(_k, lanes)| lanes.iter().any(|l| l.address.project == project))
                .map(|(k, _)| k.clone())
        }
        .ok_or_else(|| {
            format!("lane/session-changed: project '{project}' の SP が registry に無い")
        })?;
        // hook env の VP_LANE は label（"root" / performer 名）。SP method は表示形を取る。
        let display = if label == "root" || label == "lead" {
            format!("{project}/root")
        } else {
            format!("{project}/performer/{label}")
        };
        // doc 40 §4: hook の会話報告（session_id + event + 報告者が名乗る session）を SP へ
        // 透過する。無い場合は従来の「変化通知のみ」（re-enrich + push）として振る舞う =
        // 新旧 binary 混在に安全。`session` 不在も同様で、SP 側が root 宛の後方互換に倒す
        // （daemon は routing のみ — ここで欠けた値を補完しない）。
        let mut fwd = serde_json::json!({ "lane": display });
        if let Some(sid) = payload.get("session_id").and_then(|v| v.as_str()) {
            fwd["session_id"] = sid.into();
        }
        if let Some(ev) = payload.get("event").and_then(|v| v.as_str()) {
            fwd["event"] = ev.into();
        }
        if let Some(session) = payload.get("session").and_then(|v| v.as_u64()) {
            fwd["session"] = session.into();
        }
        let resp = forward_to_sp_control(
            &state.control_channels,
            &path_key,
            "lane_session_changed",
            &fwd,
        )
        .await;
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(err.to_string());
        }
        return Ok(resp);
    }
    if let Some(sub) = method.strip_prefix("wire/") {
        // flow ③: federation 送信。宛先 daemon が remote なら relay 経由で送る（ローカル store は使わない）。
        // payload = `{daemon: <宛先 daemon handle>, from, to, body, reply_to?}`。SSOT 原則で hub と話すのは
        // daemon のみなので、CLI ではなくここ（daemon）が federate_wire_send を呼ぶ。
        if sub == "federate" {
            let daemon = payload
                .get("node")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "wire/federate: 'daemon' (宛先 daemon handle) required".to_string())?
                .to_string();
            let hub_addr = crate::daemon::hub_client::hub_addr().ok_or_else(|| {
                "wire/federate: hub addr 未設定（env CHRONISTA_HUB_ADDR / config.kdl hub-addr）— federation 無効"
                    .to_string()
            })?;
            let from_label = crate::daemon::hub_client::resolve_handle(None);
            // envelope = payload から `daemon`（transport 用 routing key）を除いた wire 本体。
            let mut envelope = payload;
            if let Some(obj) = envelope.as_object_mut() {
                obj.remove("node");
            }
            crate::daemon::hub_client::federate_wire_send(
                &hub_addr,
                &daemon,
                &from_label,
                &envelope,
            )
            .await
            .map_err(|e| e.to_string())?;
            return Ok(serde_json::json!({ "status": "ok", "federated": daemon }));
        }
        // discovery（flow step 2）: 遠方 node の lane 一覧を問い合わせる（relay 上の request-response）。
        // payload = `{daemon: <宛先 daemon handle>}`。「在庫確認」: 宛先を知らないときに lane を列挙する。
        if sub == "discover-lanes" {
            let daemon = payload
                .get("node")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "wire/discover-lanes: 'daemon' (宛先 daemon handle) required".to_string()
                })?
                .to_string();
            let hub_addr = crate::daemon::hub_client::hub_addr().ok_or_else(|| {
                "wire/discover-lanes: hub addr 未設定（env CHRONISTA_HUB_ADDR / config.kdl hub-addr）— federation 無効"
                    .to_string()
            })?;
            let lanes = crate::daemon::hub_client::federate_discover_lanes(&hub_addr, &daemon)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(serde_json::json!({ "status": "ok", "node": daemon, "lanes": lanes }));
        }
        let store = state.wiremsg_store.as_ref().ok_or_else(|| {
            "wire store not initialized (daemon DB 接続失敗 or SP mode)".to_string()
        })?;
        let notifier = state
            .wire_notifier
            .as_ref()
            .ok_or_else(|| "wire notifier not initialized".to_string())?;
        let delivery = state
            .delivery_notify
            .as_ref()
            .ok_or_else(|| "wire delivery_notify not initialized".to_string())?;
        // FSM 投影: send/ack は flow_state を変え得るので、 関与 project を dispatch 前に
        // 控えておき (payload は dispatch に move される)、 成功後に lanes 再 push を促す。
        let wire_projects = if matches!(sub, "send" | "ack") {
            collect_wire_projects(&payload)
        } else {
            Vec::new()
        };
        let result =
            crate::process::routes::wire::dispatch_wire(store, notifier, delivery, sub, payload)
                .await;
        if result.is_ok() {
            notify_lane_change_for_projects(state, &wire_projects).await;
        }
        result
    } else if let Some(sub) = method.strip_prefix("delegation/") {
        let store = state.delegation_store.as_ref().ok_or_else(|| {
            "delegation store not initialized (daemon DB 接続失敗 or SP mode)".to_string()
        })?;
        crate::process::routes::delegation::dispatch_delegation(store, sub, payload).await
    } else {
        Err(format!("不明な wire channel method: {method}"))
    }
}

/// Daemon の Unison QUIC サーバーを起動する
///
/// daemon-process / events / wire / registry / device 等の live channel ハンドラーを登録し、
/// 指定ポートで QUIC 接続を待ち受ける。
pub async fn start_daemon_server(state: Arc<DaemonState>, port: u16) {
    // doc 44 P1 の後始末: fold-in で読まれなくなった旧 per-project DB (`db/sp_*`) を回収する。
    // 撤去されたのは「開くコード」だけで、disk 上の残骸はそのままだった（実機 23 dir / 約 1.2 GB）。
    let reclaimed = crate::db::reclaim_legacy_project_dbs();
    if reclaimed > 0 {
        tracing::info!("旧 project DB を回収: {reclaimed} dir（doc 44 §5.2 で破棄と確認済み）");
    }

    // 予約 lane 名の改名（`conductor` → `root`、2026-07-21）に伴う state file の付け替え。
    // lane を spawn する前に済ませる — 先に boot すると新名で空の state を作ってしまい、
    // 旧名の会話 id / 安定 id が「衝突時は上書きしない」規則で永久に取り残される。
    let renamed = vp_paths::migrate_root_lane_state_files(&crate::config::vp_state_dir());
    if renamed > 0 {
        tracing::info!("予約 lane 名 migration: state file {renamed} 件を root へ改名");
    }

    // doc 47 §4: 旧 `console_modes/` を root session の act へ畳む one-shot migration。
    // lane を spawn する前に済ませる — 畳む前に boot すると chat lane が Tui として立ち上がり、
    // その lane で 1 会話 2 エンジンになる（この移設が塞ごうとしている当のもの）。
    crate::lane::session_registry::migrate_console_modes();

    // [::]: dual-stack (IPv6 + IPv4) bind on all interfaces (WSL2/LAN 経由アクセス対応)
    let addr = format!("[::]:{}", port);
    let server =
        ProtocolServer::with_identity("vp-daemon", env!("CARGO_PKG_VERSION"), "vantage-point");

    // VP × unison-mcp Phase 1: daemon channel の wire protocol を KDL で記述し、unison.discovery
    // channel を有効化する。無改造の unison-mcp がこの KDL を runtime fetch し、registry / events の
    // typed tool（`unison_<channel>_<method>`）を合成できる（= VP 開発のデバッグ機）。KDL は記述的
    // スキーマで、request 名 = wire の msg.method と文字列一致する。SSOT: schema/vp-daemon.kdl、
    // drift 検出は tests/vp_daemon_kdl.rs。start_daemon_server は () 返しのため `?` は使えず、失敗は
    // log で握る（discovery 有効化の失敗は致命ではない = 既存 channel は無影響で動く。KDL 破損時に
    // 起動ログで気付ける。健全性は上記 drift テストが CI で先取りする）。
    // starter は registry + events のみ。process-proxy は subscribe handshake 前提で無改造
    // unison-mcp から駆動不可（ハング）＋要 SP 起動のため follow-up（creo mem_1CcuR73WSNFkyAgzVJvWyF）。
    if let Err(e) = server
        .enable_discovery(include_str!("../../schema/vp-daemon.kdl"))
        .await
    {
        tracing::error!(
            "vp-daemon discovery の有効化に失敗（KDL parse エラー等、schema/vp-daemon.kdl を確認）: {e}"
        );
    }

    // L2 (doc 27 §5-3): event log auto-feed — process lifecycle を baseline event として log に流す。
    // SP register → "process.up" / unregister・切断 → "process.down"。これで `vp events` が
    // emit なしでも「SP が上がった/落ちた」を最初から持つ（build/test 等の追加 source は follow-up）。
    {
        let event_log = state.event_log.clone();
        let mut rx = state.process_lifecycle_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ProcessLifecycleEvent::Add {
                        project_path,
                        project_name,
                        port,
                        pid,
                    }) => {
                        event_log
                            .emit(
                                "process.up",
                                Some(project_name),
                                serde_json::json!({ "path": project_path, "port": port, "pid": pid }),
                            )
                            .await;
                    }
                    Ok(ProcessLifecycleEvent::Remove { project_path }) => {
                        event_log
                            .emit(
                                "process.down",
                                None,
                                serde_json::json!({ "path": project_path }),
                            )
                            .await;
                    }
                    // lagged: broadcast buffer 溢れ。次の event から再開（log は best-effort）。
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // =========================================================================
    // daemon-process Channel (VP-154 PR-2)
    //
    // Daemon 内側 hub の data plane を Unison 経由で expose。
    //   - `list`      : RPC、 現在の running_processes snapshot を JSON で返す
    //   - `subscribe` : push stream、 register/unregister/disconnect の lifecycle event を
    //                   `send_event("event", ProcessLifecycleEvent)` で client に realtime push
    //
    // 経路: start_process / stop_process (in-process 起動元) → process_lifecycle_tx broadcast
    //       → 本 channel の subscribe handler → client (vp-app / 別 Daemon / 将来 hub gateway)。
    //   doc 44 P1 (fold-in) 以前は「SP register/heartbeat (QUIC Push) → registry channel」が
    //   生産者だったが、SP 消滅で in-process の start/stop_process が daemon-canonical に引き継いだ。
    //
    // SSOT 規約: Unison-first。 既存 HTTP /api/health の stands field は legacy fallback として
    // 温存するが、 新規 control plane の主経路は本 channel に集約。
    // =========================================================================
    if let Some(ref running_processes) = state.running_processes {
        let running_processes_snapshot = running_processes.clone();
        let process_lifecycle_tx_for_channel = state.process_lifecycle_tx.clone();
        // cross-project lane view (ROTO `list_all_lanes`) のため lane_registry も capture。
        let lane_registry_for_channel = state.lane_registry.clone();
        // sidebar と同じ project 順 (project_order) を引くため daemon_cap も capture。
        let daemon_cap_for_channel = state.daemon_cap.clone();
        server
            .register_channel("daemon-process", {
                move |_ctx, stream| {
                    let running_processes = running_processes_snapshot.clone();
                    let process_lifecycle_tx = process_lifecycle_tx_for_channel.clone();
                    let lane_registry = lane_registry_for_channel.clone();
                    let daemon_cap = daemon_cap_for_channel.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        loop {
                            let msg = match channel.recv().await {
                                Ok(msg) => msg,
                                Err(_) => break,
                            };

                            if msg.msg_type != MessageType::Request {
                                continue;
                            }

                            let method = msg.method.clone();
                            let request_id = msg.id;

                            match method.as_str() {
                                "list" => {
                                    let snapshot: Vec<ProcessSnapshot> = running_processes
                                        .read()
                                        .await
                                        .values()
                                        .map(|p| ProcessSnapshot {
                                            project_path: p
                                                .project_path
                                                .to_string_lossy()
                                                .to_string(),
                                            project_name: p.project_name.clone(),
                                            port: p.port,
                                            pid: p.pid,
                                        })
                                        .collect();
                                    if channel
                                        .send_response(
                                            request_id,
                                            "list",
                                            &serde_json::json!({"processes": snapshot}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                "list_all_lanes" => {
                                    // cross-project lane view: running_processes (port/name の SSOT)
                                    // と lane_registry を join し、project ごとに lanes を束ねて返す。
                                    // ROTO の cross-project 8-slot LCD が consumer。join 本体は
                                    // build_node_lanes に抽出し、DeviceRegistry 常駐 ROTO loop の
                                    // InProcessLaneSource と共有する (lane 並び一致)。
                                    let projects =
                                        build_node_lanes(&running_processes, &lane_registry, &daemon_cap)
                                            .await;
                                    if channel
                                        .send_response(
                                            request_id,
                                            "list_all_lanes",
                                            &serde_json::json!({"projects": projects}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                "subscribe" => {
                                    // ack 応答 (= subscribe 受け付け確認)
                                    if channel
                                        .send_response(
                                            request_id,
                                            "subscribe",
                                            &serde_json::json!({"status": "ok"}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                    // event push loop。 client 切断 (= channel send 失敗) で break、
                                    // broadcast lag は警告のみ (= 監視 client は独自に sync 必要)。
                                    let mut rx = process_lifecycle_tx.subscribe();
                                    loop {
                                        match rx.recv().await {
                                            Ok(event) => {
                                                let payload = match serde_json::to_value(&event) {
                                                    Ok(v) => v,
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            "daemon-process event serialize 失敗: {}",
                                                            e
                                                        );
                                                        continue;
                                                    }
                                                };
                                                if channel
                                                    .send_event("event", &payload)
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(
                                                tokio::sync::broadcast::error::RecvError::Lagged(n),
                                            ) => {
                                                tracing::warn!(
                                                    "daemon-process subscribe lagged: {} events dropped",
                                                    n
                                                );
                                            }
                                            Err(
                                                tokio::sync::broadcast::error::RecvError::Closed,
                                            ) => break,
                                        }
                                    }
                                    // subscribe loop 終了 = client 切断 → channel 自体も終わる
                                    break;
                                }
                                _ => {
                                    let _ = channel
                                        .send_response(
                                            request_id,
                                            &method,
                                            &serde_json::json!({
                                                "error": format!(
                                                    "不明なメソッド: daemon-process.{}",
                                                    method
                                                )
                                            }),
                                        )
                                        .await;
                                }
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // "lanes" Channel（L0 SP-portless lanes slice — vp-app per-project lane 購読の Daemon 集約版）
    // =========================================================================
    // 旧経路では vp-app が各 SP (:33000+) の "lanes" channel に直結して project ごとの lane
    // snapshot を購読していた。 SP-portless 化により、 vp-app は Daemon :32000 の本 channel 1 本に
    // 集約する。
    //
    // 経路: SP register/lanes-diff (QUIC Push) → registry channel → lane_registry + lane_change_tx
    //       → 本 channel の subscriber → vp-app。
    //
    // プロトコル:
    //   1. client: open_channel("lanes") → request("subscribe", {"project_path": "<dir>"})
    //   2. server: ack 応答後、 lane_change_tx を購読 → 当該 project の現 snapshot を初期配信
    //      (`send_event("snapshot", LanesSnapshot)`)、 以降 lane_change のたび再 push。
    //
    // event 形は SP "lanes" channel と一致させ、 vp-app consumer を無改造に保つ (send_lanes_snapshot)。
    if let Some(ref lane_registry) = state.lane_registry {
        let lane_registry = lane_registry.clone();
        let lane_change_tx = state.lane_change_tx.clone();
        // FSM 投影: snapshot 送信時の flow_state enrich に使う (store 不在 = enrich skip)。
        let wiremsg_store = state.wiremsg_store.clone();
        let running_processes = state.running_processes.clone();
        // doc 44 D4: snapshot に添える開発起点は帳簿 (db) が真実源。
        let vpdb = state.vpdb.clone();
        server
            .register_channel("lanes", {
                move |_ctx, stream| {
                    let lane_registry = lane_registry.clone();
                    let lane_change_tx = lane_change_tx.clone();
                    let wiremsg_store = wiremsg_store.clone();
                    let running_processes = running_processes.clone();
                    let vpdb = vpdb.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);

                        // handshake: 全 project 単位 channel 共通の subscribe ({project_path}→path_key)。
                        // canvas / control / process-proxy と同一 helper に統一 (bespoke 重複を排除、
                        // doc 27 §3.4.4「1 protocol」方向)。
                        let Some(path_key) = recv_subscribe_handshake(&channel).await else {
                            return Ok(()); // 接続断
                        };

                        // lane_change の購読を初期 snapshot の **前** に張る (subscribe→snapshot 順なので
                        // この間の diff を取りこぼさない。 snapshot は全置換なので diff と重複しても冪等)。
                        let mut rx = lane_change_tx.subscribe();

                        // 初期 snapshot 配信
                        if send_lanes_snapshot(&channel, &lane_registry, &path_key, &wiremsg_store, &running_processes, &vpdb)
                            .await
                            .is_err()
                        {
                            return Ok(()); // client 切断
                        }

                        // 変更 push loop: 当該 project の lane_change のたび現 snapshot を再配信
                        loop {
                            match rx.recv().await {
                                Ok(changed_key) => {
                                    if changed_key != path_key {
                                        continue; // 別 project の変更は無視
                                    }
                                    if send_lanes_snapshot(&channel, &lane_registry, &path_key, &wiremsg_store, &running_processes, &vpdb)
                                        .await
                                        .is_err()
                                    {
                                        break; // client 切断
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    // lag 後は最新 snapshot を送って resync (全置換なので diff 欠落を吸収)。
                                    tracing::warn!(
                                        "lanes channel subscribe lagged: {} events dropped (resync)",
                                        n
                                    );
                                    if send_lanes_snapshot(&channel, &lane_registry, &path_key, &wiremsg_store, &running_processes, &vpdb)
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // "gui-ingest" Channel（SP → daemon の GUI-bound content push 受け口。旧名 "canvas-ingest"）
    // =========================================================================
    // doc 44 P1 (fold-in) 以前は、各 SP の pusher (`discovery::spawn_daemon_uplink`) が
    // board topic の ProcessMessage をこの channel に push していた。fold-in 後は
    // project の TopicRouter を daemon が直接購読するため、この受け口に来る push は無い
    // （`canvas_router_for` が project 起動時に実 router へ差し替える）。channel 自体は
    // 外部からの ingest 口として残置（doc 52 §6: 対の配信 channel は "gui"）。
    //
    // プロトコル: SP が open_channel("gui-ingest") → request("subscribe", {project_path}) →
    //   以降 send_event("pane", <ProcessMessage JSON>) を流す。 daemon は route() するのみ (応答不要)。
    {
        let canvas_routers = state.canvas_routers.clone();
        let control_channels = state.control_channels.clone();
        server
            .register_channel("gui-ingest", {
                move |_ctx, stream| {
                    let canvas_routers = canvas_routers.clone();
                    let control_channels = control_channels.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        let Some(path_key) = recv_subscribe_handshake(&channel).await else {
                            return Ok(());
                        };
                        let router =
                            canvas_router_for(&canvas_routers, &control_channels, &path_key).await;

                        // SP から push される board ProcessMessage を router に route。
                        // event は method="pane"、 payload = ProcessMessage JSON (SP の canvas
                        // channel と同形)。
                        loop {
                            let msg = match channel.recv().await {
                                Ok(m) => m,
                                Err(_) => break, // SP 切断 (canvas pusher が backoff 再接続する)
                            };
                            if msg.msg_type != MessageType::Event || msg.method != "pane" {
                                continue;
                            }
                            let value = match msg.payload_as_value() {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            match serde_json::from_value::<crate::protocol::ProcessMessage>(value) {
                                Ok(pm) => router.route(pm).await,
                                Err(e) => {
                                    tracing::warn!(
                                        "canvas-ingest: ProcessMessage decode 失敗 (key={}): {}",
                                        path_key,
                                        e
                                    );
                                }
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // "gui" Channel（vp-app への配信バス — board / terminal / echoes / editor を一本で運ぶ）
    // =========================================================================
    // doc 52 §6: 旧名 "canvas" から改名（実態は board 専用でなく GUI への配信路の総称）。
    // vp-app は SP 直結ではなく Daemon :32000 の本 channel に集約する。 project の TopicRouter を
    // `subscribe("process/board/#")` し、 retained 初期配信 (最新 board 等) + live delta を
    // `send_event("pane", <ProcessMessage JSON>)` 形で配る。
    // → vp-app の consumer (`run_canvas_session`) は接続先が変わっても無改造。
    {
        let canvas_routers = state.canvas_routers.clone();
        let control_channels = state.control_channels.clone();
        server
            .register_channel("gui", {
                move |_ctx, stream| {
                    let canvas_routers = canvas_routers.clone();
                    let control_channels = control_channels.clone();
                    async move {
                        // S3: canvas channel は full-duplex。 1 本の Unison channel で
                        // 下り (topic event push) と上り (terminal_write/resize request) を兼ねる
                        // (surface 視点で channel を増やさない)。 channel.recv() と send_event を
                        // 同一 task の select! で混ぜると cancel-safety が怪しいので、 下り push を
                        // 別 task に分け、 main task は上り request 専従にする (control handler +
                        // process-proxy が実証済の並行 send/recv パターン)。
                        let channel = Arc::new(UnisonChannel::new(stream));
                        // S2: handshake で購読 pattern を受領 (省略時 board default で
                        // 既存 vp-app を無改造に保つ)。 terminal surface は
                        // `process/terminal/data/{lane}/out` を指定して demand を立てる。
                        let Some((path_key, pattern)) =
                            recv_subscribe_handshake_with_pattern(&channel).await
                        else {
                            return Ok(());
                        };
                        let router =
                            canvas_router_for(&canvas_routers, &control_channels, &path_key).await;

                        // pattern 指定があればそれを、 無ければ board default を購読。
                        let pattern = pattern.unwrap_or_else(|| "process/board/#".to_string());
                        let (sub_id, mut rx) = router.subscribe(&pattern).await;

                        // 下り push task: topic event → surface (`pane` event)。
                        let push_channel = channel.clone();
                        let pusher = tokio::spawn(async move {
                            while let Some((_topic, msg)) = rx.recv().await {
                                let json = serde_json::to_value(&msg).unwrap_or_default();
                                if push_channel.send_event("pane", &json).await.is_err() {
                                    break; // surface 切断
                                }
                            }
                        });

                        // 上り: surface → Daemon → SP control へ forward (S3 terminal_write/resize)。
                        // 既存 vp-app canvas は request を送らないので、 ここは切断まで block するだけ
                        // (= 従来の downstream-only と同じ lifecycle)。
                        loop {
                            let msg = match channel.recv().await {
                                Ok(m) => m,
                                Err(_) => break, // surface 切断
                            };
                            if msg.msg_type != MessageType::Request {
                                continue;
                            }
                            let id = msg.id;
                            let method = msg.method.clone();
                            let payload = msg.payload_as_value().unwrap_or_default();
                            let response = forward_to_sp_control(
                                &control_channels,
                                &path_key,
                                &method,
                                &payload,
                            )
                            .await;
                            if channel.send_response(id, &method, &response).await.is_err() {
                                break;
                            }
                        }

                        pusher.abort();
                        router.unsubscribe(sub_id).await;
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // "control" Channel — doc 44 P1 (fold-in) で退役
    // =========================================================================
    // 旧: 各 SP が outbound で "control" channel を張り、daemon が UnisonChannel を
    // control_channels[path_key] に保持して process 操作を reverse-route していた。
    // SP プロセスが daemon に畳み込まれたため channel 自体が不要になり、同じ dispatch を
    // `ProjectRuntimes::dispatch` が in-process で直接呼ぶ。
    // これに伴い「SP 未接続で無言破棄」「refire_active_demands の空振りレース」
    // 「高速再接続で旧 handler が新 channel を clobber」の 3 バグクラスが消滅した。

    // =========================================================================
    // "process-proxy" Channel（L0 SP-portless control slice — 外部 client → Daemon → SP reverse）
    // =========================================================================
    // MCP/CLI は SP listen port ではなく本 channel に繋ぎ、 handshake {project_path} 後に
    // process method (show/clear/tmux/process/wire 等) を request する。 daemon は当該 SP の
    // control channel を逆用して forward し、 応答を client に relay する。 SP "process" channel と
    // 同一 method・同一 dispatch (SP 側 `dispatch_process_method`) なので、 client から見た挙動は
    // SP 直結と不変 (= SP portless 化しても透過)。
    {
        let control_channels = state.control_channels.clone();
        server
            .register_channel("process-proxy", {
                move |_ctx, stream| {
                    let control_channels = control_channels.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        let Some(path_key) = recv_subscribe_handshake(&channel).await else {
                            return Ok(());
                        };

                        loop {
                            let msg = match channel.recv().await {
                                Ok(m) => m,
                                Err(_) => break, // client 切断
                            };
                            if msg.msg_type != MessageType::Request {
                                continue;
                            }
                            let id = msg.id;
                            let method = msg.method.clone();
                            let payload = msg.payload_as_value().unwrap_or_default();

                            // 当該 SP の control channel を逆用して forward (= Daemon→SP reverse)。
                            let response = forward_to_sp_control(
                                &control_channels,
                                &path_key,
                                &method,
                                &payload,
                            )
                            .await;
                            if channel.send_response(id, &method, &response).await.is_err() {
                                break;
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // Daemon-Device Channel（DeviceRegistry 🧲 device event → vp-app への bridge）
    // =========================================================================
    // EventBus の `devices.*` event (device 接続/切断/操作入力) を Unison wire の `DeviceEvent` に
    // 変換して push する単機能 channel。 daemon-process と違い method 分岐は無く、 接続 = 購読
    // (canvas channel 方式)。 `devices_event_bus` が Some (= feature midi + DeviceRegistry 稼働) のときのみ登録。
    if let Some(ref devices_event_bus) = state.devices_event_bus {
        let devices_event_bus = devices_event_bus.clone();
        // M2 follow-up: subscribe 時の registry snapshot 送信用に registry 本体も capture (midi のみ)。
        #[cfg(feature = "midi")]
        let devices = state.devices.clone();
        server
            .register_channel("daemon-device", {
                move |_ctx, stream| {
                    let event_bus = devices_event_bus.clone();
                    #[cfg(feature = "midi")]
                    let devices = devices.clone();
                    async move {
                        // フィードバック方向（LE-19）で full-duplex 化: 下り（DeviceEvent push）を
                        // 別 task に分け、main task は上り（webview → 機材の feedback event）専従。
                        // canvas channel の並行 send/recv と同じ実証済みパターン。
                        let channel = std::sync::Arc::new(UnisonChannel::new(stream));
                        // 接続即購読: devices.* を FilteredSubscription で受け、 DeviceEvent に変換して push。
                        // subscriber id は接続ごとにユニーク化する (= 複数 vp-app instance が同時購読
                        // しても EventBus の subscriptions メタデータが last-write-wins で衝突しない。
                        // broadcast 配信自体は receiver 独立で元々壊れないが、 subscriber_count を正確に保つ)。
                        let sub_id = format!("daemon-device-bridge-{}", uuid::Uuid::new_v4());
                        let sub = event_bus.subscribe(&sub_id, "devices.*").await;
                        let mut filtered =
                            crate::capability::eventbus::FilteredSubscription::new(sub);

                        // M2 follow-up: subscribe の「後」に現 registry を device_connected として snapshot
                        // 送信する。 これで vp-app は (再)接続直後に device 一覧を即得る (従来は次の hot-plug
                        // まで空)。 順序が subscribe→snapshot なので delta の取りこぼしは無く、 snapshot と
                        // delta が重複し得るが、 vp-app の apply_device_event は port_name で retain-then-push
                        // = 冪等なので吸収される。 registry lock は collect で解放してから送る (send を跨いで
                        // 保持しない)。
                        #[cfg(feature = "midi")]
                        if let Some(devices) = devices.as_ref() {
                            let devices_arc = {
                                let b = devices.read().await;
                                std::sync::Arc::clone(b.devices())
                            };
                            let snapshot: Vec<crate::daemon::protocol::DeviceEvent> = {
                                let devs = devices_arc.read().await;
                                devs.values()
                                    .map(|d| {
                                        crate::daemon::protocol::DeviceEvent::DeviceConnected {
                                            port_name: d.port_name.clone(),
                                            has_input: d.has_input,
                                            has_output: d.has_output,
                                        }
                                    })
                                    .collect()
                            };
                            for device_event in snapshot {
                                let Ok(payload) = serde_json::to_value(&device_event) else {
                                    continue;
                                };
                                if channel.send_event("event", &payload).await.is_err() {
                                    return Ok(()); // client 切断
                                }
                            }
                        }
                        // 下り push task。
                        // TODO(Phase 2): FilteredSubscription は lag を silent skip する (eventbus.rs:39)。
                        // ControlEvent 高頻度時に lag 警告が出ないため、 必要なら Lagged 警告付きの
                        // 購読に差し替えるか buffer_size を調整する (daemon-process は lag を warn 可視化)。
                        let push_channel = channel.clone();
                        let pusher = tokio::spawn(async move {
                            while let Some(cap_event) = filtered.recv().await {
                                let Some(device_event) =
                                    crate::daemon::protocol::DeviceEvent::from_capability_event(
                                        &cap_event.event_type,
                                        &cap_event.payload,
                                    )
                                else {
                                    continue;
                                };
                                let payload = match serde_json::to_value(&device_event) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        tracing::warn!("DeviceEvent serialize 失敗: {}", e);
                                        continue;
                                    }
                                };
                                if push_channel.send_event("event", &payload).await.is_err() {
                                    break; // client 切断
                                }
                            }
                        });

                        // 上り: vp-app からの `feedback` event（webview の場の状態 → 機材投影）。
                        // 切断（recv Err）で handler を畳む（pusher も道連れ）。
                        loop {
                            let msg = match channel.recv().await {
                                Ok(m) => m,
                                Err(_) => break, // client 切断
                            };
                            if msg.msg_type != MessageType::Event || msg.method != "feedback" {
                                continue;
                            }
                            #[cfg(feature = "midi")]
                            if let Some(devices) = devices.as_ref() {
                                let Ok(value) = msg.payload_as_value() else {
                                    continue;
                                };
                                match serde_json::from_value::<
                                    crate::daemon::protocol::FleetFeedback,
                                >(value)
                                {
                                    Ok(fb) => devices.read().await.apply_feedback(&fb).await,
                                    Err(e) => {
                                        tracing::warn!("fleet feedback decode 失敗: {}", e);
                                    }
                                }
                            }
                        }
                        pusher.abort();
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // Device Channel（agent → daemon: CoreMIDI hot-plug 報告、doc 26 §2 channel_id=2）
    // =========================================================================
    // request-dispatch 型 channel。macOS menu bar agent (Swift
    // `CoreMIDIWatcher`) が `ReportDevice` を送り、DeviceRegistry registry を更新する。
    // Model D (doc 25): hot-plug authority = agent。daemon は polling を回さない。
    #[cfg(feature = "midi")]
    if let Some(ref devices) = state.devices {
        let devices = devices.clone();
        server
            .register_channel("device", {
                move |_ctx, stream| {
                    let devices = devices.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        loop {
                            let msg = match channel.recv().await {
                                Ok(msg) => msg,
                                Err(_) => break,
                            };

                            if msg.msg_type != MessageType::Request {
                                continue;
                            }

                            let payload = msg.payload_as_value().unwrap_or_default();
                            let method = msg.method.clone();
                            let request_id = msg.id;

                            let response = match method.as_str() {
                                "report_device" => {
                                    handle_device_report(&devices, request_id, payload).await
                                }
                                _ => ChannelMessage::err(
                                    request_id,
                                    format!("不明なメソッド: device.{}", method),
                                ),
                            };

                            if send_channel_response(&channel, &method, response)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // Registry Channel（稼働 project の read-only 照会）
    // =========================================================================
    //
    // doc 44 P1 (fold-in) 以前は「SP 自己登録 — QUIC 永続接続による即時登録・即時死亡検出」
    // の control plane で、register / unregister / heartbeat / lanes/* を受けて daemon 側の
    // running_processes / lane_registry / process_presence を維持していた。
    //
    // fold-in で project が daemon プロセス内に入り、自己登録しに来る SP が消滅した。
    // 上記 state の維持は起動元（`start_process` / `publish_lanes`）が直接行う daemon-canonical
    // に移ったため、SP 向け method 群と切断時の後始末は撤去した。
    //
    // channel 自体は `list`（`vp ps` 相当を MCP / 外部 client から引く read-only 面。
    // schema/vp-daemon.kdl に記述あり）のために残す。
    if let Some(ref running_processes) = state.running_processes {
        let running_processes = running_processes.clone();
        server
            .register_channel("registry", {
                move |_ctx, stream| {
                    // doc 44 P1 (fold-in): SP 向けの register / unregister / heartbeat /
                    // lanes/* は撤去した（自己登録しに来る SP プロセスが存在しない）。
                    // 残るのは read-only の `list` だけなので、依存も running_processes 1 本。
                    let running_processes = running_processes.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);

                        loop {
                            let msg = match channel.recv().await {
                                Ok(msg) => msg,
                                Err(_) => break, // 切断
                            };

                            if msg.msg_type != MessageType::Request {
                                continue;
                            }

                            let method = msg.method.clone();
                            let request_id = msg.id;

                            match method.as_str() {
                                "list" => {
                                    let list =
                                        registry_process_snapshot(&running_processes).await;
                                    if channel
                                        .send_response(
                                            request_id,
                                            "list",
                                            &serde_json::json!({"processes": list}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                _ => {
                                    let _ = channel
                                        .send_response(
                                            request_id,
                                            &method,
                                            &serde_json::json!({
                                                "error": format!("不明なメソッド: registry.{}", method)
                                            }),
                                        )
                                        .await;
                                }
                            }
                        }

                        // doc 44 P1 (fold-in): SP 切断時の自動除去は撤去した。
                        // `registered_name` を立てる `register` が無くなったため到達不能で、
                        // かつ project の生存は Daemon 自身の `ProjectRuntimes` が持つ
                        // （切断という状態が存在しない = map に居るか居ないか）。

                        Ok(())
                    }
                }
            })
            .await;
    }

    // L2 (doc 27 §5-3): events channel — event log の emit / query。
    // agent の episodic memory（CLI `vp events` / 将来 agent peer / vp-app が consume）。always-on
    // daemon が in-memory ring を保持し、emit は誰でも push、query は `since` cursor 以降を古い順に返す。
    {
        let event_log = state.event_log.clone();
        server
            .register_channel("events", {
                move |_ctx, stream| {
                    let event_log = event_log.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        loop {
                            let msg = match channel.recv().await {
                                Ok(msg) => msg,
                                Err(_) => break, // 切断
                            };
                            if msg.msg_type != MessageType::Request {
                                continue;
                            }
                            let payload = msg.payload_as_value().unwrap_or_default();
                            let request_id = msg.id;
                            match msg.method.as_str() {
                                "emit" => {
                                    let kind = payload["kind"].as_str().unwrap_or("").to_string();
                                    let source = payload["source"].as_str().map(|s| s.to_string());
                                    let data = payload
                                        .get("data")
                                        .cloned()
                                        .unwrap_or(serde_json::Value::Null);
                                    if kind.is_empty() {
                                        let _ = channel
                                            .send_response(
                                                request_id,
                                                "emit",
                                                &serde_json::json!({"error": "kind は必須"}),
                                            )
                                            .await;
                                        continue;
                                    }
                                    let seq = event_log.emit(kind, source, data).await;
                                    if channel
                                        .send_response(
                                            request_id,
                                            "emit",
                                            &serde_json::json!({ "seq": seq }),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                "query" => {
                                    let since = payload["since"].as_u64().unwrap_or(0);
                                    let limit = payload["limit"].as_u64().unwrap_or(0) as usize;
                                    let events = event_log.query(since, limit).await;
                                    if channel
                                        .send_response(
                                            request_id,
                                            "query",
                                            &serde_json::json!({ "events": events }),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                other => {
                                    let _ = channel
                                        .send_response(
                                            request_id,
                                            other,
                                            &serde_json::json!({
                                                "error": format!("不明なメソッド: events.{}", other)
                                            }),
                                        )
                                        .await;
                                }
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // daemon control Channel（projects mutation: CLI → Daemon 直接 Unison）
    //
    // control plane 一元化: projects は daemon 権威 (db/machine) なので、 CLI は SP を経由せず
    // daemon に直接 Unison RPC する。 registry (SP 自己登録専用) とは責務を分離した
    // 別 channel にする。 daemon_cap 不在 (= 非 daemon mode) なら登録しない。
    if let Some(ref daemon_cap) = state.daemon_cap {
        let daemon_cap = daemon_cap.clone();
        server
            .register_channel("daemon-control", {
                move |_ctx, stream| {
                    let daemon_cap = daemon_cap.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        loop {
                            let msg = match channel.recv().await {
                                Ok(msg) => msg,
                                Err(_) => break, // 切断
                            };
                            if msg.msg_type != MessageType::Request {
                                continue;
                            }
                            let payload = msg.payload_as_value().unwrap_or_default();
                            let method = msg.method.clone();
                            let request_id = msg.id;
                            // 成功時 result JSON、 失敗時は success frame に {"error": ...}
                            // を詰める (= Unison は専用 error frame を持たない、 VP-163 慣習)。
                            let response =
                                match handle_daemon_control(&daemon_cap, &method, payload).await {
                                    Ok(v) => v,
                                    Err(e) => serde_json::json!({ "error": e }),
                                };
                            if channel
                                .send_response(request_id, &method, &response)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // Wire Channel (L0 portless B-4: 旧 daemon_wire::call の HTTP relay 先を unison 化)
    //
    // `daemon_wire::call` (SP→daemon の wire/delegation transport) を HTTP `POST /api/wire/*`
    // `/api/delegation/*` から本 channel に移行 (doc 27 §62「全通信 unison」)。method は
    // "wire/<m>" / "delegation/<m>" の prefix 分岐で各 store dispatch に振る (`handle_wire_channel`)。
    // store は daemon process AppState と共有 (`with_wire` で plumb)。SP mode (store=None) では
    // 各 method が error を返す (= 旧 HTTP handler の「store not initialized」と等価)。
    // =========================================================================
    server
        .register_channel("wire", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
                async move {
                    // SP は永続 link の 1 channel に全 wire request を多重化する (daemon_wire.rs の
                    // fd leak 根治)。 直列 loop のままだと wire/recv long-poll (≤25s) の await 中に
                    // 同一 channel の後続 request (wire/send 等) を読めず塞ぐため、 request ごとに
                    // spawn して並行処理する。 応答は message id で対応付くので順序保証は不要、
                    // 送信は UnisonStream 内部の Mutex で frame 単位に直列化される。
                    let channel = Arc::new(UnisonChannel::new(stream));
                    loop {
                        let msg = match channel.recv().await {
                            Ok(msg) => msg,
                            Err(_) => break, // 切断
                        };
                        if msg.msg_type != MessageType::Request {
                            continue;
                        }
                        let payload = msg.payload_as_value().unwrap_or_default();
                        let method = msg.method.clone();
                        let request_id = msg.id;
                        let state = state.clone();
                        let channel = Arc::clone(&channel);
                        tokio::spawn(async move {
                            // 成功時 result JSON、 失敗時は success frame に {"error": ...} を詰める
                            // (Unison は専用 error frame を持たない、 VP-163 慣習)。
                            let response = match handle_wire_channel(&state, &method, payload).await
                            {
                                Ok(v) => v,
                                Err(e) => serde_json::json!({ "error": e }),
                            };
                            // 送信失敗 = 接続断。 loop 側の recv Err で channel ごと終了するため
                            // ここでは無視してよい。
                            let _ = channel.send_response(request_id, &method, &response).await;
                        });
                    }
                    Ok(())
                }
            }
        })
        .await;

    // サーバー起動
    // VP-185: listen は内部で QuicServer::new() (= cert なし固定) を使うため、
    // CertSource を明示するには QuicServer::builder 経由が必須。 daemon は shutdown
    // 連携を持たない (= listen が永久 block する設計) ため start() を使う。
    // PR-3 で cert_source を InternalMeshKeypair の server 半分に差し替える。
    tracing::info!("Daemon Unison QUIC サーバー起動: {}", addr);
    let server = Arc::new(server);
    let mut quic = QuicServer::builder(server)
        .cert_source(CertSource::dev_localhost())
        .build();
    if let Err(e) = quic.bind(&addr).await {
        tracing::error!("Daemon Unison サーバー bind 失敗: {}", e);
        return;
    }
    tracing::info!("Daemon Unison QUIC listening on {:?}", quic.local_addr());
    // QUIC 面 liveness watchdog を起動 (self-heal、 mem_1CcvYA5TRF4EcFafbyKqPg)。
    // quic.start() は永久 block するため、 watchdog は別 task で並走させる。
    tokio::spawn(quic_liveness_watchdog(port));
    if let Err(e) = quic.start().await {
        tracing::error!("Daemon Unison サーバーエラー: {}", e);
    }
}

/// QUIC 面 liveness watchdog + self-heal (2026-07-12、 mem_1CcvYA5TRF4EcFafbyKqPg)。
///
/// 観測された故障モード: HTTP 面は生存したまま QUIC accept が wedge し、 SP uplink /
/// wire / process-proxy が全滅する「片肺死」。 `quic.start()` は club-unison 内で永久 block
/// するため VP 側で Err として catch できず、 HTTP ベースの health check でも検知不能だった。
///
/// この watchdog は自プロセスの QUIC :port へ周期的に fresh self-connect し、 accept path の
/// 生存を probe する。 連続 `MAX_FAILURES` 回失敗 = 片肺死と判定し、 loud log の後 process::exit
/// する。 supervisor (macOS launchd KeepAlive / systemd) が fresh 再起動して回復する
/// (「プロセスは死ぬがコンテキストは蘇る」 — SP は setsid 分離で生存、 lane claude は --resume)。
///
/// false-positive で健全な daemon を殺さないための保険:
/// - 起動直後は `STARTUP_GRACE` の猶予 (bind 完了直後の未 ready 状態を数えない)
/// - `MAX_FAILURES` **連続** 失敗のみ発火 (単発 transient は counter を reset)
/// - probe は `PROBE_TIMEOUT` で cap
/// - env `VP_DISABLE_QUIC_WATCHDOG` で無効化 (誤検知時の escape hatch)
async fn quic_liveness_watchdog(port: u16) {
    if std::env::var_os("VP_DISABLE_QUIC_WATCHDOG").is_some() {
        tracing::info!(
            "QUIC liveness watchdog は VP_DISABLE_QUIC_WATCHDOG により無効化されています"
        );
        return;
    }
    const STARTUP_GRACE: Duration = Duration::from_secs(30);
    const PROBE_INTERVAL: Duration = Duration::from_secs(30);
    const MAX_FAILURES: u32 = 3;

    tokio::time::sleep(STARTUP_GRACE).await;
    let addr = format!("[::1]:{port}");
    tracing::info!(
        "QUIC liveness watchdog 起動 (probe 先={addr}、 間隔={}s)",
        PROBE_INTERVAL.as_secs()
    );
    let mut consecutive: u32 = 0;
    loop {
        tokio::time::sleep(PROBE_INTERVAL).await;
        // probe_quic_once は内部 timeout で自己完結する (ハング時も disconnect まで到達し
        // socket を leak しない)。 ここで外側 timeout を重ねない。
        match probe_quic_once(&addr).await {
            Ok(()) => {
                if consecutive > 0 {
                    tracing::info!("QUIC liveness probe 回復 ({} 回連続失敗の後)", consecutive);
                }
                consecutive = 0;
            }
            Err(e) => {
                consecutive += 1;
                tracing::warn!("QUIC liveness probe 失敗 ({consecutive}/{MAX_FAILURES}): {e}");
            }
        }
        if consecutive >= MAX_FAILURES {
            tracing::error!(
                "QUIC 面の wedge を検知 ({MAX_FAILURES} 回連続 probe 失敗)。 self-heal: supervisor \
                 による fresh 再起動のため exit します (mem_1CcvYA5TRF4EcFafbyKqPg)"
            );
            // tracing subscriber の flush 猶予を少し与えてから exit する。
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::process::exit(70); // EX_SOFTWARE — supervisor (launchd KeepAlive) が relaunch する
        }
    }
}

/// 自プロセスの QUIC `:port` へ fresh connect + channel open で accept path の生存を確認する。
///
/// 観測された故障は `client.connect()` レベルの「Failed to establish QUIC connection」だったため、
/// connect が最も忠実な liveness 信号。 加えて `open_channel` で stream accept path も exercise する
/// (registry handler は subscribe 前の切断を `Ok(())` として扱うので即 disconnect は無害)。
///
/// ⚠️ QUIC(UDP) は TCP と違い dead port への connect が即失敗せず handshake timeout まで
/// ハングする (RST が無い)。 そのため timeout は **内部** に持ち、 ハング時も必ず `disconnect()`
/// に到達させて UDP socket leak を防ぐ (drop 任せでは socket が残る、 daemon_wire.rs module doc 参照)。
async fn probe_quic_once(addr: &str) -> Result<(), String> {
    const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
    const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(3);

    // QUIC(rustls) は CryptoProvider install が前提 (install 済みなら no-op)。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("probe client build 失敗: {e}"))?;
    let client = unison::ProtocolClient::new(transport);
    let inner = async {
        client
            .connect(addr)
            .await
            .map_err(|e| format!("connect 失敗: {e}"))?;
        client
            .open_channel("registry")
            .await
            .map_err(|e| format!("open_channel 失敗: {e}"))?;
        Ok::<(), String>(())
    };
    let result = match tokio::time::timeout(PROBE_TIMEOUT, inner).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "probe timeout ({}s 無応答)",
            PROBE_TIMEOUT.as_secs()
        )),
    };
    // 成否に関わらず接続を解放 (UDP socket leak 防止)。 disconnect 自体のハングにも上限。
    let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, client.disconnect()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // =====================================================================
    // daemon control Channel — projects mutation dispatch (handle_daemon_control)
    //
    // QUIC server を立てず handler 関数を直接呼ぶ Small test。 dispatch が
    // ProcessManagerCapability を正しく叩き、 in-memory 状態に反映されることを検証する。
    // (DB 真実源化は PR-C、 ここでは vpdb=None なので persist は projects.kdl no-op)
    // =====================================================================

    fn new_daemon_cap() -> Arc<RwLock<crate::capability::ProcessManagerCapability>> {
        Arc::new(RwLock::new(
            crate::capability::ProcessManagerCapability::new(),
        ))
    }

    // =====================================================================
    // QUIC liveness watchdog — probe の故障検知方向 (safety-critical)
    //
    // watchdog の最悪の regression は「健全な daemon を false-positive で殺す」こと。
    // それを防ぐ根幹は「probe が本当に死んでいる時だけ Err を返す」= 死を正しく死と
    // 判定できること。 誰も listen していない port への probe が、 probe **内部** の timeout で
    // 自己完結して Err を返す (= 外側でハングしない) ことを検証する。 QUIC(UDP) は dead port へ
    // の connect が RST を返さずハングするため、 内部 timeout が効くことがこのテストの主眼。
    // 逆方向 (生きた server で Ok) は実 daemon 差替で検証する。
    // =====================================================================
    #[tokio::test]
    async fn probe_quic_fails_on_unbound_port() {
        // 誰も bind していない高位 port。 QUIC connect はハングするが probe 内部 timeout (8s) で
        // Err に落ちるはず。 外側 15s guard は「内部 timeout が効かず無限ハング」の回帰検出用。
        let addr = "[::1]:59321";
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            super::probe_quic_once(addr),
        )
        .await;
        assert!(
            result.is_ok(),
            "probe が 15s 以内に返らなかった — 内部 timeout が効いていない (socket leak / 無限ハングの回帰)"
        );
        assert!(
            result.unwrap().is_err(),
            "unbound port への probe は Err を返すべき (死を死と判定できる = self-heal の前提)"
        );
    }

    #[tokio::test]
    async fn daemon_control_add_list_remove() {
        let cap = new_daemon_cap();
        // add_project は path.is_dir() を要求するので実在 dir (temp_dir) を使う
        let path = std::env::temp_dir().to_string_lossy().to_string();

        // add → 追加された ProjectInfo が返る
        let added = handle_daemon_control(
            &cap,
            "projects/add",
            serde_json::json!({"name": "wc-test", "path": path}),
        )
        .await
        .expect("add ok");
        assert_eq!(added["name"], "wc-test");

        // list に反映される
        let list = handle_daemon_control(&cap, "projects/list", serde_json::json!({}))
            .await
            .expect("list ok");
        let arr = list.as_array().expect("list is array");
        assert!(
            arr.iter().any(|p| p["name"] == "wc-test"),
            "added project が list に出る"
        );

        // remove (add と同じ path → 同じ正規化キーで削除)
        handle_daemon_control(&cap, "projects/remove", serde_json::json!({"path": path}))
            .await
            .expect("remove ok");
        let list2 = handle_daemon_control(&cap, "projects/list", serde_json::json!({}))
            .await
            .expect("list ok");
        assert!(list2.as_array().unwrap().is_empty(), "remove 後は空になる");
    }

    #[tokio::test]
    async fn daemon_control_unknown_method_errors() {
        let cap = new_daemon_cap();
        let r = handle_daemon_control(&cap, "projects/bogus", serde_json::json!({})).await;
        assert!(r.is_err(), "未知 method は Err");
    }

    // =====================================================================
    // doc 45 段 4 — HTTP 撤去後の daemon-control 振る舞い固定
    //
    // 段 2 / 段 3 では「新面（Unison）が旧面（HTTP route）と同じ答えを返す」を
    // 突き合わせで担保していた。段 4 で HTTP route を落としたので、突き合わせる
    // 相手が消える。**parity テストが担保していた中身（合成 update の意味論 /
    // lanes の filter+sort / reorder の並び / snapshot の写し方）は Unison 入口に
    // 対して直接固定し直す** — 旧面が消えたからといって期待値まで消すと、
    // 移行で守ったものが黙って外れる。
    //
    // 実装は `routes::daemon` の共有関数（apply_project_update / collect_lanes /
    // resolve_create_lane_args）1 本なので、ここが落ちるのは振る舞いが動いた時。
    // =====================================================================

    /// `projects/update` が rename + enabled を 1 往復で適用する（旧 HTTP の合成 update）。
    #[tokio::test]
    async fn daemon_control_projects_update_applies_rename_and_enabled() {
        let path = std::env::temp_dir().to_string_lossy().to_string();
        let cap = new_daemon_cap();
        handle_daemon_control(
            &cap,
            "projects/add",
            serde_json::json!({"name": "update-target", "path": path}),
        )
        .await
        .expect("add ok");

        handle_daemon_control(
            &cap,
            "projects/update",
            serde_json::json!({"path": path, "name": "renamed", "enabled": false}),
        )
        .await
        .expect("update ok");

        let after = handle_daemon_control(&cap, "projects/list", serde_json::json!({}))
            .await
            .expect("list ok");
        assert_eq!(after[0]["name"], "renamed", "rename が効いている");
        assert_eq!(after[0]["enabled"], false, "disable が効いている");

        // 「何も指定しない update」は Err（黙って成功にしない）。旧 HTTP の 400 と同じ意味論。
        let empty =
            handle_daemon_control(&cap, "projects/update", serde_json::json!({"path": path})).await;
        assert_eq!(
            empty.unwrap_err(),
            "No fields to update",
            "field 無し update は明示エラー"
        );
    }

    /// `lanes/list` の filter（project / lane / stand）と表示順を固定する。
    ///
    /// **空 registry では filter も sort も無仕事になる**ので、複数 project × 複数 lane を
    /// 実際に積んでから確認する。並びは project 名昇順 → 同 project 内は開発起点 (root) 先 →
    /// 続いて created_at 昇順。
    #[tokio::test]
    async fn daemon_control_lanes_list_filters_and_sorts() {
        use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneState};

        let cap = new_daemon_cap();

        // registry を直接埋める（project の publish を模す）。project 名 / created_at / stand を
        // わざと逆順・混在で入れて、sort と filter が実際に仕事をする状態を作る。
        let mk = |project: &str, name: &str, created_at: &str, stand: &str| LaneInfo {
            id: Default::default(),
            address: LaneAddress::new(project, name),
            state: LaneState::Running,
            stand: stand.to_string(),
            created_at: created_at.to_string(),
            pid: Some(4321),
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        };
        {
            let registry = cap.read().await.lane_registry_ref();
            let mut registry = registry.write().await;
            registry.insert(
                "/repos/zeta".to_string(),
                vec![
                    mk("zeta", "later", "2026-07-02T00:00:00Z", "shell"),
                    mk("zeta", "root", "2026-07-03T00:00:00Z", "echoes"),
                    mk("zeta", "earlier", "2026-07-01T00:00:00Z", "echoes"),
                ],
            );
            registry.insert(
                "/repos/alpha".to_string(),
                vec![mk("alpha", "root", "2026-07-01T00:00:00Z", "echoes")],
            );
        }

        // (payload, 期待する lane 名の並び)
        let cases: [(serde_json::Value, Vec<&str>); 5] = [
            (
                serde_json::json!({}),
                // alpha/root → zeta/root（開発起点先）→ earlier → later（created_at 昇順）
                vec!["root", "root", "earlier", "later"],
            ),
            (
                serde_json::json!({"project": "zeta"}),
                vec!["root", "earlier", "later"],
            ),
            (
                serde_json::json!({"stand": "echoes"}),
                vec!["root", "root", "earlier"],
            ),
            (serde_json::json!({"lane": "root"}), vec!["root", "root"]),
            (serde_json::json!({"project": "nonexistent"}), vec![]),
        ];

        for (payload, expected) in cases {
            let resp = handle_daemon_control(&cap, "lanes/list", payload.clone())
                .await
                .expect("lanes/list ok");
            let names: Vec<&str> = resp["lanes"]
                .as_array()
                .expect("lanes は配列")
                .iter()
                .map(|l| l["address"]["name"].as_str().unwrap_or("?"))
                .collect();
            assert_eq!(names, expected, "lanes/list {payload} の filter + 並び");
            assert_eq!(
                resp["count"],
                serde_json::json!(expected.len()),
                "count は lanes の長さと一致する"
            );
        }
    }

    /// `projects/sync` は ghost 無しなら `{removed: []}` を返す（旧 HTTP と同じ形）。
    #[tokio::test]
    async fn daemon_control_projects_sync_returns_removed_list() {
        let cap = new_daemon_cap();
        let resp = handle_daemon_control(&cap, "projects/sync", serde_json::json!({}))
            .await
            .expect("sync ok");
        assert_eq!(resp["removed"], serde_json::json!([]), "ghost 無しなら空");
    }

    /// `registry.list` の snapshot が `RunningProcess` の Serialize をそのまま写すこと。
    ///
    /// vp-app の `list_processes`（sidebar の runtime port join + Activity の稼働数）が
    /// 依存する面。旧 HTTP route と同じ `running_processes` map / 同じ Serialize を通るので、
    /// ここが割れる時は「JSON への写し方に手が入った」場合に限られる。
    #[tokio::test]
    async fn registry_list_snapshot_carries_name_and_port() {
        let cap = new_daemon_cap();
        // capability と registry channel が共有する map に直接置く（start_process は
        // 実際に project を起こしてしまうので、ここでは snapshot の写し方だけを見る）。
        let running = cap.read().await.running_processes_ref();
        running.write().await.insert(
            "/tmp/snapshot-proc".to_string(),
            RunningProcess {
                project_name: "snapshot-proc".to_string(),
                port: 33000,
                pid: 4242,
                project_path: "/tmp/snapshot-proc".into(),
            },
        );

        let snapshot = registry_process_snapshot(&running).await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0]["project_name"], "snapshot-proc");
        assert_eq!(snapshot[0]["port"], 33000);
        assert_eq!(snapshot[0]["pid"], 4242);
    }

    /// `projects/reorder` が指定した順に project を並べ替える。
    ///
    /// vp-app の sidebar D&D が通る面。`ord` は表示順の canonical なので、
    /// ここが壊れると「並べ替えたのに戻る」形で表に出る。
    #[tokio::test]
    async fn daemon_control_reorder_orders_projects() {
        // 実在 dir が 2 つ要る（add_project は path.is_dir() を要求する）。
        // TempDir なので test 終了時に消える（固定 path にすると再実行で残骸を拾う）。
        let base = tempfile::tempdir().expect("tempdir");
        let first = base.path().join("alpha");
        let second = base.path().join("beta");
        std::fs::create_dir_all(&first).expect("mkdir alpha");
        std::fs::create_dir_all(&second).expect("mkdir beta");
        let first = first.to_string_lossy().to_string();
        let second = second.to_string_lossy().to_string();

        let cap = new_daemon_cap();
        for (name, path) in [("alpha", &first), ("beta", &second)] {
            handle_daemon_control(
                &cap,
                "projects/add",
                serde_json::json!({"name": name, "path": path}),
            )
            .await
            .expect("add ok");
        }

        // 逆順に並べ替える。
        handle_daemon_control(
            &cap,
            "projects/reorder",
            serde_json::json!({ "paths": [second, first] }),
        )
        .await
        .expect("reorder ok");

        let list = handle_daemon_control(&cap, "projects/list", serde_json::json!({}))
            .await
            .expect("list ok");
        let names: Vec<&str> = list
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect();
        assert_eq!(names, ["beta", "alpha"], "指定した順に並ぶこと");
    }

    /// mutation 系 RPC の必須 field 欠落は Err（daemon の状態を触る前に弾く）。
    #[tokio::test]
    async fn daemon_control_new_methods_require_their_fields() {
        let cap = new_daemon_cap();
        for (method, payload) in [
            ("projects/update", serde_json::json!({})),
            ("projects/restart", serde_json::json!({})),
            ("projects/pointview", serde_json::json!({})),
            ("lanes/create", serde_json::json!({"path": "/tmp"})),
            ("lanes/set_active", serde_json::json!({"path": "/tmp"})),
        ] {
            assert!(
                handle_daemon_control(&cap, method, payload).await.is_err(),
                "{method} は必須 field 欠落を Err にする"
            );
        }
    }

    /// `lanes/create` の省略時 default 導出（旧 HTTP route と共有していた calc）。
    #[test]
    fn create_lane_defaults_are_derived() {
        use crate::process::routes::daemon::resolve_create_lane_args;

        let (branch, stand) = resolve_create_lane_args("/tmp/parity", "sub", None, None);
        assert!(
            branch.ends_with("/sub"),
            "branch 未指定なら `<user>/<name>` を derive する: {branch}"
        );
        assert!(
            !stand.is_empty(),
            "stand 未指定でも default が入る: {stand}"
        );

        // 明示指定はそのまま通る。空白のみの branch は未指定と同じ扱い。
        let (branch, stand) =
            resolve_create_lane_args("/tmp/parity", "sub", Some("feat/x"), Some("shell"));
        assert_eq!((branch.as_str(), stand.as_str()), ("feat/x", "shell"));
        let (branch, _) = resolve_create_lane_args("/tmp/parity", "sub", Some("   "), None);
        assert!(branch.ends_with("/sub"), "空白 branch は derive に落ちる");
    }

    #[tokio::test]
    async fn daemon_control_add_missing_field_errors() {
        let cap = new_daemon_cap();
        // name 欠落 → Err
        let r =
            handle_daemon_control(&cap, "projects/add", serde_json::json!({"path": "/tmp"})).await;
        assert!(r.is_err(), "name 欠落は Err");
    }

    #[test]
    fn test_daemon_state_new() {
        let state = DaemonState::new();
        // 起動時刻が現在に近いことを確認
        assert!(
            state.started_at.elapsed().as_secs() < 1,
            "started_at が現在時刻から離れすぎている"
        );
    }

    #[test]
    fn test_daemon_state_has_process_lifecycle_tx() {
        // VP-154 PR-2: DaemonState::new() で process_lifecycle_tx が初期化されてる (= capacity 64)
        let state = DaemonState::new();
        // subscribe できる = Sender が active
        let _rx = state.process_lifecycle_tx.subscribe();
        // 既存 receiver は 1 (= 上で作った _rx)
        assert_eq!(state.process_lifecycle_tx.receiver_count(), 1);
    }

    #[tokio::test]
    async fn test_process_lifecycle_broadcast_add_remove() {
        // VP-154 PR-2: registry channel が publish した event が subscribe で受信できる
        let state = DaemonState::new();
        let mut rx = state.process_lifecycle_tx.subscribe();

        let add = ProcessLifecycleEvent::Add {
            project_path: "/x".to_string(),
            project_name: "creo".to_string(),
            port: 33000,
            pid: 1,
        };
        state.process_lifecycle_tx.send(add.clone()).unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received, add);

        let remove = ProcessLifecycleEvent::Remove {
            project_path: "/x".to_string(),
        };
        state.process_lifecycle_tx.send(remove.clone()).unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received, remove);
    }

    #[tokio::test]
    async fn test_process_lifecycle_broadcast_no_subscriber_is_ok() {
        // subscriber 不在で send しても error にならず安全 (= 既存 .send() の `let _ =` 経路と整合)
        // broadcast::Sender は no-receiver で SendError を返すが、 Sender 自体は alive。
        let state = DaemonState::new();
        let event = ProcessLifecycleEvent::Add {
            project_path: "/x".to_string(),
            project_name: "vp".to_string(),
            port: 33002,
            pid: 99,
        };
        // receiver 不在 → SendError (= caller 側で `let _ =` で無視されてる)
        let result = state.process_lifecycle_tx.send(event);
        assert!(
            result.is_err(),
            "subscriber 不在では SendError が想定通り返る"
        );
    }

    /// FSM 投影: wire payload からの関与 project 抽出 (send = from + to[]、 ack = agent)
    #[test]
    fn collect_wire_projects_extracts_from_send_and_ack_payloads() {
        // wire/send: from + to[] (cross-project 宛先も拾う)
        let send = serde_json::json!({
            "from": "agent@vantage-point",
            "to": ["agent@vantage-point/fsm-projection", "notify@creo-memories"],
            "body": {"kind": "task"}
        });
        assert_eq!(
            collect_wire_projects(&send),
            vec!["creo-memories".to_string(), "vantage-point".to_string()],
            "from + to[] の project を dedup して抽出 (BTreeSet = 辞書順)"
        );

        // wire/ack: agent のみ
        let ack = serde_json::json!({
            "message_id": "x",
            "agent": "agent@vantage-point"
        });
        assert_eq!(
            collect_wire_projects(&ack),
            vec!["vantage-point".to_string()]
        );

        // address 無し / 不正形は空
        assert!(collect_wire_projects(&serde_json::json!({})).is_empty());
        assert!(
            collect_wire_projects(&serde_json::json!({"from": "bare-name"})).is_empty(),
            "@ 無しの address は project を持たない"
        );
    }
}
