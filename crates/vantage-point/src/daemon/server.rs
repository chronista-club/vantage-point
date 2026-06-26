//! Daemon の Unison QUIC サーバー
//!
//! session / terminal / system の3チャネルを提供。
//! Console (vp hd attach) からの接続を受け付け、PTY I/O を中継する。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, RwLock};
use unison::network::quic::QuicServer;
use unison::network::{
    CertSource, MessageType, NetworkError, ProtocolServer, channel::UnisonChannel,
};

use super::protocol::{
    AttachRequest, ChannelMessage, CreatePaneRequest, CreateSessionRequest, DetachRequest,
    KillPaneRequest, ProcessLifecycleEvent, ProcessSnapshot, ReadOutputRequest, ResizeRequest,
    WriteRequest,
};
use super::pty_slot::PtySlot;
use super::registry::{PaneKind, SessionRegistry};
use crate::capability::RunningProcess;

/// ペイン識別子: (session_id, pane_id)
type PaneKey = (String, u32);

/// PTY 出力の broadcast receiver マップ
type OutputReceiverMap = HashMap<PaneKey, tokio::sync::broadcast::Receiver<Vec<u8>>>;

/// Daemon の共有状態
///
/// `pty_slots` は `Mutex` を使用する（`PtySlot` が `Sync` を実装しないため）。
/// `registry` は純粋なデータ構造なので `RwLock` で読み取り並行性を確保。
pub struct DaemonState {
    /// セッション・ペインのレジストリ
    pub registry: Arc<RwLock<SessionRegistry>>,
    /// PTYスロット: (session_id, pane_id) → PtySlot
    /// PtySlot は Send だが Sync ではないため Mutex を使用
    pub pty_slots: Arc<Mutex<HashMap<PaneKey, PtySlot>>>,
    /// PTY出力の broadcast receiver: ペインごとに保持
    /// terminal.read_output で消費される
    pub output_receivers: Arc<Mutex<OutputReceiverMap>>,
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
    /// VP-154 PR-2: Process lifecycle event broadcast bus (= "world-process" channel の data plane)
    ///
    /// registry channel handler が SP register/unregister を受信したタイミングで `send` し、
    /// "world-process" subscribe handler の broadcast::Receiver が pump して client に
    /// `send_event` で push する経路。 capacity 64 = SP 同時 register が短時間に集中しても
    /// drop しない buffer (= 既存 system_event_tx と同サイズ)。
    pub process_lifecycle_tx: tokio::sync::broadcast::Sender<ProcessLifecycleEvent>,
    /// L0 SP-portless (lanes slice): lane_registry が変化した project の path_key を載せる
    /// broadcast bus。 registry channel handler が lane_registry を mutate した直後
    /// (register / lanes/add / lanes/remove / lanes/update) に `send(path_key)` し、
    /// per-project "lanes" channel の subscriber がこれを購読して当該 project の現 snapshot を
    /// vp-app に再 push する経路 (= SP "lanes" channel 直結の World 集約版)。
    ///
    /// `process_lifecycle_tx` (process Add/Remove) と相補: あちらは SP の up/down、 こちらは
    /// SP 内 lane (Performer 等) の add/remove/update を realtime 配信する。 capacity 64 は
    /// 同上 (短時間に lane diff が集中しても drop しない buffer)。
    pub lane_change_tx: tokio::sync::broadcast::Sender<String>,
    /// L0 SP-portless (canvas slice): project ごとの Canvas (Paisley Park) TopicRouter。
    ///
    /// 各 SP が "canvas-ingest" channel で paisley-park の ProcessMessage を push し、 World は
    /// それを project の TopicRouter に `route()` する。 vp-app 向け "canvas" channel はこの
    /// TopicRouter を `subscribe("process/paisley-park/#")` して retained 初期配信 + live delta を
    /// 配る。 SP の "canvas" channel (`process/unison_server.rs`) と **同じ TopicRouter 型を再利用**
    /// することで、 retained/delta/atomicity を World 側で再実装せず委譲する (lanes の lane_registry
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
    /// L0 SP-portless (control slice): project ごとの SP "control" channel handle。
    ///
    /// 各 SP が起動時に "control" channel で World に outbound 接続し (`spawn_control_keepalive`)、
    /// World はその `UnisonChannel` を path_key で保持する。 "process-proxy" channel 経由で来た
    /// 外部 client (MCP/CLI) の process 操作を、 この handle を**逆用** (`request()`) して当該 SP に
    /// forward する (= World→SP reverse-routing)。 UnisonChannel は双方向対称なので、 SP が QUIC
    /// client でありながら本接続上で RPC server として振る舞える。 SP 切断で除去 (= reverse 不能)。
    pub control_channels: Arc<RwLock<HashMap<String, Arc<UnisonChannel>>>>,
    /// projects 操作の権威 (= CLI → World 直接 Unison "world-control" channel の data plane)。
    ///
    /// HTTP `routes/world.rs` と同一の `ProcessManagerCapability` 実体を Arc 共有し、
    /// add/remove/rename/set_enabled/reorder/list を Unison 経由でも受ける。
    /// control plane 一元化 (creo `mem_1CbmWjCGNi9z49s3r21TwQ`): projects は World 権威なので
    /// CLI は SP を経由せず World daemon に直接 Unison RPC する (= projects.kdl 共有メモリの置換)。
    pub world_cap: Option<Arc<RwLock<crate::capability::ProcessManagerCapability>>>,
    /// doc 24 §10 Phase 2: lane descriptor の durable 永続先 (daemon-canonical 化)。
    ///
    /// registry channel handler が SP push (register snapshot / lanes diff) を受けた時、
    /// in-memory `lane_registry` への反映と並行して db に永続する。 これにより SP disconnect /
    /// daemon 再起動を越えて descriptor が生き残る (§3.3 re-animate / §4.1 喪失ゼロ)。
    pub vpdb: Option<crate::db::SharedVpDb>,
    /// Bastet 🧲 EventBus の参照 — "world-device" Unison channel の data plane。
    ///
    /// `WorldCapabilities.bastet` が Some (= feature = "midi" + Bastet 稼働) のときのみ注入される。
    /// world-device channel handler がこれを subscribe して `bastet.*` event (device 接続/切断/
    /// 操作入力) を `DeviceEvent` に変換し、 vp-app に push する。
    pub bastet_event_bus: Option<Arc<crate::capability::eventbus::EventBus>>,
    /// Bastet 🧲 registry 本体の参照 — "device" Unison channel (agent → daemon) の data plane。
    ///
    /// M2 / doc 26 §2: macOS menu bar agent (Swift `CoreMIDIWatcher`) が hot-plug を `ReportDevice`
    /// で報告する。`device` channel handler がこの handle 越しに `report_device_*` を呼び、registry
    /// 更新 + `bastet.*` emit を行う (emit は world-device bridge 経由で vp-app に届く)。
    #[cfg(feature = "midi")]
    pub bastet: Option<Arc<RwLock<crate::bastet::Bastet>>>,
}

impl Default for DaemonState {
    fn default() -> Self {
        let (process_lifecycle_tx, _) = tokio::sync::broadcast::channel(64);
        let (lane_change_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            registry: Arc::new(RwLock::new(SessionRegistry::default())),
            pty_slots: Arc::new(Mutex::new(HashMap::new())),
            output_receivers: Arc::new(Mutex::new(HashMap::new())),
            started_at: Instant::now(),
            running_processes: None,
            projects: None,
            lane_registry: None,
            process_lifecycle_tx,
            lane_change_tx,
            canvas_routers: Arc::new(RwLock::new(HashMap::new())),
            control_channels: Arc::new(RwLock::new(HashMap::new())),
            world_cap: None,
            vpdb: None,
            bastet_event_bus: None,
            #[cfg(feature = "midi")]
            bastet: None,
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
    ) -> Self {
        self.running_processes = Some(running_processes);
        self.projects = Some(projects);
        self.lane_registry = Some(lane_registry);
        self
    }

    /// projects 操作の権威 (`ProcessManagerCapability`) を共有する。
    ///
    /// HTTP `AppState.world` と同一の Arc を渡すことで、 Unison "world-control" channel から
    /// 受けた projects mutation を HTTP と同じ実体に反映する (= 入口は複数でも権威は 1 つ)。
    pub fn with_world_cap(
        mut self,
        world_cap: Arc<RwLock<crate::capability::ProcessManagerCapability>>,
    ) -> Self {
        self.world_cap = Some(world_cap);
        self
    }

    /// doc 24 §10 Phase 2: lane descriptor の durable 永続先 (db/world) を共有する。
    ///
    /// registry channel handler がこの db に SP push を永続して daemon-canonical 化する。
    /// capability の boot load (`load_config`) と同一の db を指す (= 書いた truth を起動時に読む)。
    pub fn with_vpdb(mut self, vpdb: crate::db::SharedVpDb) -> Self {
        self.vpdb = Some(vpdb);
        self
    }

    /// Bastet 🧲 EventBus を共有する (feature = "midi")。
    ///
    /// `run_world` が `WorldCapabilities.bastet` の `event_bus()` を渡し、 world-device channel
    /// handler がこれを subscribe して device event を vp-app に push する。
    pub fn with_bastet_event_bus(
        mut self,
        event_bus: Arc<crate::capability::eventbus::EventBus>,
    ) -> Self {
        self.bastet_event_bus = Some(event_bus);
        self
    }

    /// Bastet 🧲 registry 本体を共有する (feature = "midi")。
    ///
    /// `device` channel handler が agent の `ReportDevice` を受けて registry を更新するために使う。
    /// `with_bastet_event_bus` と同じ `WorldCapabilities.bastet` を指す (event_bus は registry 内蔵)。
    #[cfg(feature = "midi")]
    pub fn with_bastet(mut self, bastet: Arc<RwLock<crate::bastet::Bastet>>) -> Self {
        self.bastet = Some(bastet);
        self
    }
}

/// 許可されたシェルコマンドのホワイトリスト
const ALLOWED_SHELLS: &[&str] = &[
    "/bin/bash",
    "/bin/zsh",
    "/bin/sh",
    "/usr/bin/bash",
    "/usr/bin/zsh",
    "/usr/local/bin/bash",
    "/usr/local/bin/zsh",
    "/usr/local/bin/fish",
    "/opt/homebrew/bin/bash",
    "/opt/homebrew/bin/zsh",
    "/opt/homebrew/bin/fish",
    "bash",
    "zsh",
    "sh",
    "fish",
];

/// シェルコマンドのバリデーション（コマンドインジェクション防止）
fn validate_shell_cmd(shell_cmd: &str) -> Result<(), NetworkError> {
    if !ALLOWED_SHELLS.contains(&shell_cmd) {
        return Err(NetworkError::Protocol(format!(
            "許可されていないシェルコマンド: {}",
            shell_cmd
        )));
    }
    Ok(())
}

// =========================================================================
// World Control Channel ハンドラー（projects mutation: CLI → World 直接 Unison）
// =========================================================================

/// "world-control" channel の method を `ProcessManagerCapability` に dispatch する。
///
/// HTTP `routes/world.rs` と同じ `world_cap` メソッドを呼ぶため、 永続化 (db/world) や
/// project_order 管理ロジックは共有される (= 二重実装を避ける)。 戻り値は成功時 result JSON、
/// 失敗時は `Err(String)`。 caller は Unison の慣習 (VP-163) に従い success frame に
/// `{"error": ...}` を詰めて返す (= Unison は専用 error frame を持たない)。
async fn handle_world_control(
    world_cap: &Arc<RwLock<crate::capability::ProcessManagerCapability>>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Moody Blues PR-D review #2: read guard を arm ごとに取り直し、 mutation の await 完了後に
    // 即解放する (= 複数 Unison/HTTP リクエストが outer read guard を長時間共有しない)。
    // 内部 mutation は ProcessManagerCapability の Arc<RwLock> field で直列化される。
    match method {
        "projects/list" => {
            let list = world_cap.read().await.list_projects().await;
            serde_json::to_value(&list).map_err(|e| e.to_string())
        }
        "projects/add" => {
            let name = payload["name"]
                .as_str()
                .ok_or_else(|| "name is required".to_string())?;
            let path = payload["path"]
                .as_str()
                .ok_or_else(|| "path is required".to_string())?;
            let info = world_cap
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
            world_cap
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
            world_cap
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
            world_cap
                .read()
                .await
                .set_project_enabled(path, enabled)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "ok", "path": path, "enabled": enabled}))
        }
        "projects/reorder" => {
            let paths: Vec<String> = serde_json::from_value(payload["paths"].clone())
                .map_err(|e| format!("paths is required (string array): {}", e))?;
            world_cap
                .read()
                .await
                .reorder_projects(&paths)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"status": "reordered", "count": paths.len()}))
        }
        // chronista-hub federation: hub registry に居る world 一覧を取得する。
        // SSOT 原則により hub と話すのは TheWorld のみ。CLI / プログラム経路はこの RPC を叩く
        // (= 直接 hub に接続しない)。`CHRONISTA_HUB_ADDR` 未設定なら federation 無効を返す。
        "hub/discover" => {
            let Some(addr) = crate::daemon::hub_client::hub_addr() else {
                return Err(format!(
                    "{} 未設定 — hub federation 無効",
                    crate::daemon::hub_client::HUB_ADDR_ENV
                ));
            };
            let client = crate::daemon::hub_client::HubClient::connect(&addr, 3)
                .await
                .map_err(|e| e.to_string())?;
            let worlds = client.discover().await.map_err(|e| e.to_string())?;
            serde_json::to_value(&worlds).map_err(|e| e.to_string())
        }
        other => Err(format!("不明なメソッド: world-control.{}", other)),
    }
}

// =========================================================================
// Session Channel ハンドラー
// =========================================================================

/// session.create: セッション作成
async fn handle_session_create(
    state: &DaemonState,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: CreateSessionRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    let mut registry = state.registry.write().await;

    // 既存セッションがあればエラー
    if registry.get_session(&req.session_id).is_some() {
        return ChannelMessage::err(
            id,
            format!("セッション '{}' は既に存在します", req.session_id),
        );
    }

    let info = registry.create_session(&req.session_id);
    ChannelMessage::ok(
        id,
        serde_json::json!({
            "status": "ok",
            "session_id": info.id,
            "created_at": info.created_at,
        }),
    )
}

/// session.list: セッション一覧
async fn handle_session_list(state: &DaemonState, id: u64) -> ChannelMessage {
    let registry = state.registry.read().await;
    let sessions: Vec<_> = registry
        .list_sessions()
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "pane_count": s.panes.len(),
                "created_at": s.created_at,
            })
        })
        .collect();

    ChannelMessage::ok(
        id,
        serde_json::json!({
            "status": "ok",
            "sessions": sessions,
        }),
    )
}

/// session.attach: セッションにアタッチ
async fn handle_session_attach(
    state: &DaemonState,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: AttachRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    let registry = state.registry.read().await;
    let session = match registry.get_session(&req.session_id) {
        Some(s) => s,
        None => {
            return ChannelMessage::err(
                id,
                format!("セッション '{}' が見つかりません", req.session_id),
            );
        }
    };

    // セッション情報を返す（PTY output streaming は後続タスクで追加）
    let panes: Vec<_> = session
        .panes
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "cols": p.cols,
                "rows": p.rows,
                "active": p.active,
            })
        })
        .collect();

    ChannelMessage::ok(
        id,
        serde_json::json!({
            "status": "ok",
            "session_id": session.id,
            "panes": panes,
        }),
    )
}

/// session.detach: セッションからデタッチ
async fn handle_session_detach(id: u64, payload: serde_json::Value) -> ChannelMessage {
    let _req: DetachRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    // デタッチは接続側の状態変更のみ（Daemon 側では特に処理なし）
    ChannelMessage::ok(id, serde_json::json!({"status": "ok"}))
}

// =========================================================================
// Terminal Channel ハンドラー
// =========================================================================

/// terminal.create_pane: ペイン作成
async fn handle_terminal_create_pane(
    state: &DaemonState,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: CreatePaneRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    // 作業ディレクトリはホームディレクトリをデフォルトに
    let cwd = dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp".to_string());

    // シェルコマンドのバリデーション（コマンドインジェクション防止）
    if let Err(e) = validate_shell_cmd(&req.shell_cmd) {
        return ChannelMessage::err(id, format!("{}", e));
    }

    // PTYスロット起動（初期 receiver を取得し、シェルプロンプト等の初期出力をロストしない）
    // shell args は req に含まれない (古い protocol)、空 args で起動。将来 protocol 拡張時に変更。
    let (slot, output_rx) = match PtySlot::spawn(&cwd, &req.shell_cmd, &[], &[], req.cols, req.rows)
    {
        Ok(s) => s,
        Err(e) => return ChannelMessage::err(id, format!("PTY起動失敗: {}", e)),
    };

    let pid = slot.pid();

    // レジストリにペイン追加
    let mut registry = state.registry.write().await;
    let pane_id = match registry.add_pane(
        &req.session_id,
        PaneKind::Pty {
            pid,
            shell_cmd: req.shell_cmd.clone(),
        },
        req.cols,
        req.rows,
    ) {
        Some(id) => id,
        None => {
            return ChannelMessage::err(
                id,
                format!("セッション '{}' が見つかりません", req.session_id),
            );
        }
    };

    // PTYスロットを保存（output_rx は spawn 時点から全出力を受信済み）
    let mut slots = state.pty_slots.lock().await;
    slots.insert((req.session_id.clone(), pane_id), slot);
    drop(slots);

    // Output receiver を保存
    let mut receivers = state.output_receivers.lock().await;
    receivers.insert((req.session_id.clone(), pane_id), output_rx);

    tracing::info!(
        "ペイン作成: session={}, pane_id={}, pid={}, shell={}",
        req.session_id,
        pane_id,
        pid,
        req.shell_cmd
    );

    ChannelMessage::ok(
        id,
        serde_json::json!({
            "status": "ok",
            "pane_id": pane_id,
            "pid": pid,
        }),
    )
}

/// terminal.write: PTY入力書き込み
async fn handle_terminal_write(
    state: &DaemonState,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: WriteRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    // base64 デコード
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let data = match engine.decode(&req.data) {
        Ok(d) => d,
        Err(e) => return ChannelMessage::err(id, format!("base64 デコード失敗: {}", e)),
    };

    let mut slots = state.pty_slots.lock().await;
    let key = (req.session_id.clone(), req.pane_id);
    let slot = match slots.get_mut(&key) {
        Some(s) => s,
        None => {
            return ChannelMessage::err(
                id,
                format!(
                    "ペインが見つかりません: session={}, pane_id={}",
                    req.session_id, req.pane_id
                ),
            );
        }
    };

    if let Err(e) = slot.write(&data) {
        return ChannelMessage::err(id, format!("PTY書き込み失敗: {}", e));
    }

    ChannelMessage::ok(id, serde_json::json!({"status": "ok"}))
}

/// terminal.resize: ペインリサイズ
async fn handle_terminal_resize(
    state: &DaemonState,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: ResizeRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    let slots = state.pty_slots.lock().await;
    let key = (req.session_id.clone(), req.pane_id);
    let slot = match slots.get(&key) {
        Some(s) => s,
        None => {
            return ChannelMessage::err(
                id,
                format!(
                    "ペインが見つかりません: session={}, pane_id={}",
                    req.session_id, req.pane_id
                ),
            );
        }
    };

    if let Err(e) = slot.resize(req.cols, req.rows) {
        return ChannelMessage::err(id, format!("リサイズ失敗: {}", e));
    }

    tracing::debug!(
        "ペインリサイズ: session={}, pane_id={}, {}x{}",
        req.session_id,
        req.pane_id,
        req.cols,
        req.rows
    );

    ChannelMessage::ok(id, serde_json::json!({"status": "ok"}))
}

/// terminal.read_output: PTY出力読み取り
async fn handle_terminal_read_output(
    state: &DaemonState,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: ReadOutputRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    let key = (req.session_id.clone(), req.pane_id);

    // 1. receiver をマップから取り出す（ロックを短時間で解放）
    let mut receivers = state.output_receivers.lock().await;
    let rx = receivers.remove(&key);
    drop(receivers); // ロックを即座に解放

    let Some(mut rx) = rx else {
        return ChannelMessage::err(
            id,
            format!(
                "出力 receiver が見つかりません: session={}, pane_id={}",
                req.session_id, req.pane_id
            ),
        );
    };

    // 2. ロックを保持せずにタイムアウト付きで出力を読み取り
    let timeout = std::time::Duration::from_millis(req.timeout_ms);
    let mut all_data: Vec<u8> = Vec::new();

    match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Ok(data)) => {
            all_data.extend_from_slice(&data);
            // バッファに溜まっている追加データも取得（非ブロッキング）
            while let Ok(more) = rx.try_recv() {
                all_data.extend_from_slice(&more);
            }
        }
        Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
            tracing::warn!("出力 receiver lagged: {} メッセージスキップ", n);
            // lagged の後、次のメッセージは読める
            if let Ok(data) = rx.try_recv() {
                all_data.extend_from_slice(&data);
            }
        }
        Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
            // チャネルがクローズされた（PTYプロセス終了）
        }
        Err(_) => {
            // タイムアウト（出力なし）
        }
    }

    // 3. receiver をマップに戻す（ロックを短時間で取得）
    let mut receivers = state.output_receivers.lock().await;
    receivers.insert(key, rx);

    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&all_data);

    ChannelMessage::ok(
        id,
        serde_json::json!({
            "data": encoded,
            "bytes_read": all_data.len(),
        }),
    )
}

/// terminal.kill_pane: ペイン終了
async fn handle_terminal_kill_pane(
    state: &DaemonState,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: KillPaneRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    let key = (req.session_id.clone(), req.pane_id);

    // PTYスロットを削除（drop でプロセスも終了）
    let mut slots = state.pty_slots.lock().await;
    let removed_slot = slots.remove(&key).is_some();
    drop(slots);

    // Output receiver も削除
    let mut receivers = state.output_receivers.lock().await;
    receivers.remove(&key);
    drop(receivers);

    // レジストリからペイン削除
    let mut registry = state.registry.write().await;
    let removed_pane = registry.remove_pane(&req.session_id, req.pane_id);

    if !removed_slot && !removed_pane {
        return ChannelMessage::err(
            id,
            format!(
                "ペインが見つかりません: session={}, pane_id={}",
                req.session_id, req.pane_id
            ),
        );
    }

    tracing::info!(
        "ペイン終了: session={}, pane_id={}",
        req.session_id,
        req.pane_id
    );

    ChannelMessage::ok(id, serde_json::json!({"status": "ok"}))
}

// =========================================================================
// System Channel ハンドラー
// =========================================================================

/// system.health: ヘルスチェック
async fn handle_system_health(state: &DaemonState, id: u64) -> ChannelMessage {
    let registry = state.registry.read().await;
    let sessions_count = registry.list_sessions().len();
    let uptime_secs = state.started_at.elapsed().as_secs();

    ChannelMessage::ok(
        id,
        serde_json::json!({
            "status": "ok",
            "sessions_count": sessions_count,
            "uptime_secs": uptime_secs,
        }),
    )
}

/// system.shutdown: シャットダウン
fn handle_system_shutdown(id: u64) -> ChannelMessage {
    tracing::info!("system.shutdown リクエスト受信");

    // シャットダウンはプロセス終了で実現
    // Daemon の tokio::select! がシグナルをキャッチしてクリーンアップする
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let pid = std::process::id();
        if !crate::platform::process_terminate(pid) {
            tracing::warn!("system.shutdown: terminate 送信が失敗しました");
            std::process::exit(1);
        }
    });

    ChannelMessage::ok(
        id,
        serde_json::json!({"status": "ok", "message": "shutting down"}),
    )
}

// =========================================================================
// チャネルレスポンス送信ヘルパー
// =========================================================================

/// ChannelMessage を UnisonChannel 経由で送信する
///
/// ChannelMessage::Response は send_response() で、
/// ChannelMessage::Error は send_response() でエラーペイロードとして送信する。
/// device.report_device: agent (Swift menu bar) からの CoreMIDI hot-plug 報告を Bastet registry に反映する。
///
/// doc 26 §2 `ReportDevice` request。`state` = "connected" | "disconnected" で分岐し、
/// `Bastet::report_device_*` が registry 更新 + `bastet.*` emit を行う (emit は既存 world-device
/// bridge 経由で vp-app に届く)。
#[cfg(feature = "midi")]
async fn handle_device_report(
    bastet: &Arc<RwLock<crate::bastet::Bastet>>,
    id: u64,
    payload: serde_json::Value,
) -> ChannelMessage {
    let req: super::protocol::ReportDeviceRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => return ChannelMessage::err(id, format!("Invalid payload: {}", e)),
    };

    let b = bastet.read().await;
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
/// `world_cap` の project_order で安定ソートした projects 配列（`Vec<serde_json::Value>`）を返す。
/// "world-process" channel の `list_all_lanes` handler と、Bastet 常駐 ROTO loop の
/// `InProcessLaneSource`（in-process 直読み）が **同一ロジックを共有**することで、
/// CLI（QUIC 経由）と daemon（直読み）で lane 並びが完全一致する（doc 23 の重複回避）。
///
/// ロック順序: running_processes → lane_registry（register と同順、deadlock 回避）。
#[allow(clippy::type_complexity)]
pub(crate) async fn build_world_lanes(
    running_processes: &Arc<RwLock<HashMap<String, RunningProcess>>>,
    lane_registry: &Option<
        Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>,
    >,
    world_cap: &Option<Arc<RwLock<crate::capability::ProcessManagerCapability>>>,
) -> Vec<serde_json::Value> {
    // 並び順は sidebar と一致させる（= project_order）。物理 controller は位置 = 意味なので、
    // track button N の位置が sidebar の N 番目と対応する必要がある。
    let order: Vec<String> = match world_cap {
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
/// 送る。 これにより vp-app の consumer (`run_lanes_session`) は接続先が SP→World に変わっても無改造。
/// 登録が無い project は空 Vec を送る (SP 未登録/lane 無しを「空」として正しく表現)。
#[allow(clippy::type_complexity)]
async fn send_lanes_snapshot(
    channel: &UnisonChannel,
    lane_registry: &Arc<RwLock<HashMap<String, Vec<crate::process::lanes_state::LaneInfo>>>>,
    path_key: &str,
) -> Result<(), NetworkError> {
    let lanes = lane_registry
        .read()
        .await
        .get(path_key)
        .cloned()
        .unwrap_or_default();
    let snapshot = crate::protocol::ProcessMessage::LanesSnapshot { lanes };
    let json = serde_json::to_value(&snapshot).unwrap_or_default();
    channel.send_event("snapshot", &json).await
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
    control_channels: &Arc<RwLock<HashMap<String, Arc<UnisonChannel>>>>,
    path_key: &str,
) -> Arc<crate::process::topic_router::TopicRouter> {
    // fast path: 既存 router を read lock で取得
    if let Some(router) = canvas_routers.read().await.get(path_key) {
        return router.clone();
    }
    // slow path: write lock で get-or-create。 race recheck で 2 重作成を防ぐ
    // (entry().or_insert_with() は async な demand 登録を挟めないため手動 double-check)。
    let mut routers = canvas_routers.write().await;
    if let Some(router) = routers.get(path_key) {
        return router.clone();
    }
    let router = Arc::new(crate::process::topic_router::TopicRouter::new());
    register_terminal_demand(&router, control_channels.clone(), path_key.to_string());
    routers.insert(path_key.to_string(), router.clone());
    router
}

/// S2 (doc 27 §4.1 Cap2): project canvas router に terminal demand hook を登録する。
///
/// `process/terminal/data/+/out` の購読者増減を監視し、 0↔1 遷移で当該 SP に
/// `terminal_demand_start/stop {lane}` を control reverse-route で撃つ。 cb は sync で呼ばれる
/// ため、 reverse-route (async I/O) は `tokio::spawn` に逃がす。
fn register_terminal_demand(
    router: &Arc<crate::process::topic_router::TopicRouter>,
    control_channels: Arc<RwLock<HashMap<String, Arc<UnisonChannel>>>>,
    path_key: String,
) {
    router.register_demand("process/terminal/data/+/out", move |topic, active| {
        // topic = `process/terminal/data/{lanekey}/out` → lane address を復元
        // (topic key は LaneAddress の '/' を '~' に encode したもの。 逆変換する)。
        let Some(lane_key) = topic.split('/').nth(3) else {
            return;
        };
        let lane = lane_key.replace('~', "/");
        let method = if active {
            "terminal_demand_start"
        } else {
            "terminal_demand_stop"
        };
        let control_channels = control_channels.clone();
        let path_key = path_key.clone();
        tokio::spawn(async move {
            let sp_ch = control_channels.read().await.get(&path_key).cloned();
            match sp_ch {
                Some(ch) => {
                    if let Err(e) = ch
                        .request::<serde_json::Value, serde_json::Value>(
                            method,
                            &serde_json::json!({ "lane": lane }),
                        )
                        .await
                    {
                        tracing::warn!(
                            "terminal demand reverse-route 失敗 ({} lane={}): {}",
                            method,
                            lane,
                            e
                        );
                    }
                }
                None => {
                    // SP control channel 未接続 (SP 起動前に surface が subscribe した等)。
                    // SP 接続後の再 demand 機構は follow-up (S2 polish)。
                    tracing::debug!(
                        "terminal demand: SP control channel 不在 (key={}, lane={}, {})",
                        path_key,
                        lane,
                        method
                    );
                }
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
/// 既存 vp-app は `pattern` を送らないので `None` を返し、 caller 側で paisley-park default に
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
async fn forward_to_sp_control(
    control_channels: &Arc<RwLock<HashMap<String, Arc<UnisonChannel>>>>,
    path_key: &str,
    method: &str,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let sp_ch = control_channels.read().await.get(path_key).cloned();
    match sp_ch {
        Some(sp_ch) => match sp_ch
            .request::<serde_json::Value, serde_json::Value>(method, payload)
            .await
        {
            Ok(v) => v,
            Err(e) => serde_json::json!({
                "error": format!("SP forward 失敗 (key={}): {}", path_key, e)
            }),
        },
        None => serde_json::json!({
            "error": format!("SP 未接続 (key={})", path_key)
        }),
    }
}

/// Daemon の Unison QUIC サーバーを起動する
///
/// session / terminal / system の各チャネルハンドラーを登録し、
/// 指定ポートで QUIC 接続を待ち受ける。
pub async fn start_daemon_server(state: Arc<DaemonState>, port: u16) {
    // [::]: dual-stack (IPv6 + IPv4) bind on all interfaces (WSL2/LAN 経由アクセス対応)
    let addr = format!("[::]:{}", port);
    let server =
        ProtocolServer::with_identity("vp-daemon", env!("CARGO_PKG_VERSION"), "vantage-point");

    // =========================================================================
    // Session Channel
    // =========================================================================
    server
        .register_channel("session", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
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
                            "create" => handle_session_create(&state, request_id, payload).await,
                            "list" => handle_session_list(&state, request_id).await,
                            "attach" => handle_session_attach(&state, request_id, payload).await,
                            "detach" => handle_session_detach(request_id, payload).await,
                            _ => ChannelMessage::err(
                                request_id,
                                format!("不明なメソッド: session.{}", method),
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

    // =========================================================================
    // Terminal Channel
    // =========================================================================
    server
        .register_channel("terminal", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
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
                            "create_pane" => {
                                handle_terminal_create_pane(&state, request_id, payload).await
                            }
                            "write" => handle_terminal_write(&state, request_id, payload).await,
                            "resize" => handle_terminal_resize(&state, request_id, payload).await,
                            "read_output" => {
                                handle_terminal_read_output(&state, request_id, payload).await
                            }
                            "kill_pane" => {
                                handle_terminal_kill_pane(&state, request_id, payload).await
                            }
                            _ => ChannelMessage::err(
                                request_id,
                                format!("不明なメソッド: terminal.{}", method),
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

    // =========================================================================
    // System Channel
    // =========================================================================
    server
        .register_channel("system", {
            let state = state.clone();
            move |_ctx, stream| {
                let state = state.clone();
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

                        let response = match method.as_str() {
                            "health" => handle_system_health(&state, request_id).await,
                            "shutdown" => handle_system_shutdown(request_id),
                            _ => ChannelMessage::err(
                                request_id,
                                format!("不明なメソッド: system.{}", method),
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

    // =========================================================================
    // world-process Channel (VP-154 PR-2)
    //
    // World 内側 hub の data plane を Unison 経由で expose。
    //   - `list`      : RPC、 現在の running_processes snapshot を JSON で返す
    //   - `subscribe` : push stream、 register/unregister/disconnect の lifecycle event を
    //                   `send_event("event", ProcessLifecycleEvent)` で client に realtime push
    //
    // 経路: SP register/heartbeat (QUIC Push) → registry channel → process_lifecycle_tx broadcast
    //       → 本 channel の subscribe handler → client (vp-app / 別 World / 将来 hub gateway)。
    //
    // SSOT 規約: Unison-first。 既存 HTTP /api/health の stands field は legacy fallback として
    // 温存するが、 新規 control plane の主経路は本 channel に集約。
    // =========================================================================
    if let Some(ref running_processes) = state.running_processes {
        let running_processes_snapshot = running_processes.clone();
        let process_lifecycle_tx_for_channel = state.process_lifecycle_tx.clone();
        // cross-project lane view (ROTO `list_all_lanes`) のため lane_registry も capture。
        let lane_registry_for_channel = state.lane_registry.clone();
        // sidebar と同じ project 順 (project_order) を引くため world_cap も capture。
        let world_cap_for_channel = state.world_cap.clone();
        server
            .register_channel("world-process", {
                move |_ctx, stream| {
                    let running_processes = running_processes_snapshot.clone();
                    let process_lifecycle_tx = process_lifecycle_tx_for_channel.clone();
                    let lane_registry = lane_registry_for_channel.clone();
                    let world_cap = world_cap_for_channel.clone();
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
                                            tmux_session: p.tmux_session.clone(),
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
                                    // build_world_lanes に抽出し、Bastet 常駐 ROTO loop の
                                    // InProcessLaneSource と共有する (lane 並び一致)。
                                    let projects =
                                        build_world_lanes(&running_processes, &lane_registry, &world_cap)
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
                                                            "world-process event serialize 失敗: {}",
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
                                                    "world-process subscribe lagged: {} events dropped",
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
                                                    "不明なメソッド: world-process.{}",
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
    // "lanes" Channel（L0 SP-portless lanes slice — vp-app per-project lane 購読の World 集約版）
    // =========================================================================
    // 旧経路では vp-app が各 SP (:33000+) の "lanes" channel に直結して project ごとの lane
    // snapshot を購読していた。 SP-portless 化により、 vp-app は World :32000 の本 channel 1 本に
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
        server
            .register_channel("lanes", {
                move |_ctx, stream| {
                    let lane_registry = lane_registry.clone();
                    let lane_change_tx = lane_change_tx.clone();
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
                        if send_lanes_snapshot(&channel, &lane_registry, &path_key)
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
                                    if send_lanes_snapshot(&channel, &lane_registry, &path_key)
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
                                    if send_lanes_snapshot(&channel, &lane_registry, &path_key)
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
    // "canvas-ingest" Channel（L0 SP-portless canvas slice — SP → World の canvas push 受け口）
    // =========================================================================
    // 各 SP の canvas pusher (`discovery::spawn_canvas_keepalive`) が paisley-park topic の
    // ProcessMessage を push する。 World は project ごとの TopicRouter に `route()` して
    // retained 保持 + 購読者 (= "canvas" channel 経由の vp-app) へ配信する。
    //
    // プロトコル: SP が open_channel("canvas-ingest") → request("subscribe", {project_path}) →
    //   以降 send_event("pane", <ProcessMessage JSON>) を流す。 World は route() するのみ (応答不要)。
    {
        let canvas_routers = state.canvas_routers.clone();
        let control_channels = state.control_channels.clone();
        server
            .register_channel("canvas-ingest", {
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

                        // SP から push される paisley-park ProcessMessage を router に route。
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
    // "canvas" Channel（L0 SP-portless canvas slice — vp-app per-project canvas 購読の World 集約版）
    // =========================================================================
    // vp-app は SP 直結ではなく World :32000 の本 channel に集約する。 project の TopicRouter を
    // `subscribe("process/paisley-park/#")` し、 retained 初期配信 (最新 Show 等) + live delta を
    // SP "canvas" channel と同形 (`send_event("pane", <ProcessMessage JSON>)`) で配る。
    // → vp-app の consumer (`run_canvas_session`) は接続先が変わっても無改造。
    {
        let canvas_routers = state.canvas_routers.clone();
        let control_channels = state.control_channels.clone();
        server
            .register_channel("canvas", {
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
                        // S2: handshake で購読 pattern を受領 (省略時 paisley-park default で
                        // 既存 vp-app を無改造に保つ)。 terminal surface は
                        // `process/terminal/data/{lane}/out` を指定して demand を立てる。
                        let Some((path_key, pattern)) =
                            recv_subscribe_handshake_with_pattern(&channel).await
                        else {
                            return Ok(());
                        };
                        let router =
                            canvas_router_for(&canvas_routers, &control_channels, &path_key).await;

                        // pattern 指定があればそれを、 無ければ paisley-park default を購読。
                        let pattern =
                            pattern.unwrap_or_else(|| "process/paisley-park/#".to_string());
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

                        // 上り: surface → World → SP control へ forward (S3 terminal_write/resize)。
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
    // "control" Channel（L0 SP-portless control slice — SP の reverse-routing 受け口を登録）
    // =========================================================================
    // 各 SP が "control" channel で World に outbound 接続する (`spawn_control_keepalive`)。
    // World は handshake {project_path} 後にその UnisonChannel を control_channels[path_key] に
    // 保持し、 "process-proxy" channel から逆用 (request) して SP に process 操作を forward する。
    // 本 handler 自体は channel を保持して接続維持を監視するだけ (request を撃つのは process-proxy)。
    {
        let control_channels = state.control_channels.clone();
        let canvas_routers = state.canvas_routers.clone();
        server
            .register_channel("control", {
                move |_ctx, stream| {
                    let control_channels = control_channels.clone();
                    let canvas_routers = canvas_routers.clone();
                    async move {
                        let channel = Arc::new(UnisonChannel::new(stream));
                        let Some(path_key) = recv_subscribe_handshake(&channel).await else {
                            return Ok(());
                        };
                        control_channels
                            .write()
                            .await
                            .insert(path_key.clone(), channel.clone());
                        tracing::info!("Control: SP 登録 (key={})", path_key);

                        // S2 polish: SP (再)接続 catch-up。 SP 起動前に surface が terminal topic を
                        // subscribe して demand_start を撃った場合、 control channel 不在で reverse-route
                        // が捨てられている。 SP 登録直後に当該 project の active demand を撃ち直し、
                        // 取りこぼした pump start を補填する (= SP restart で terminal が暗転しない)。
                        if let Some(router) = canvas_routers.read().await.get(&path_key) {
                            router.refire_active_demands();
                        }

                        // 切断検知まで待つ。 World は本 channel に request を撃つだけで SP は
                        // event/request を送らないため、 recv() は切断時のみ Err で返る
                        // (= reverse request の Response は pending 経由で解決され、 ここには来ない)。
                        loop {
                            if channel.recv().await.is_err() {
                                break;
                            }
                        }
                        control_channels.write().await.remove(&path_key);
                        tracing::info!("Control: SP 除去 (key={})", path_key);
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // "process-proxy" Channel（L0 SP-portless control slice — 外部 client → World → SP reverse）
    // =========================================================================
    // MCP/CLI は SP listen port ではなく本 channel に繋ぎ、 handshake {project_path} 後に
    // process method (show/clear/tmux/process/wire 等) を request する。 World は当該 SP の
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

                            // 当該 SP の control channel を逆用して forward (= World→SP reverse)。
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
    // World-Device Channel（Bastet 🧲 device event → vp-app への bridge）
    // =========================================================================
    // EventBus の `bastet.*` event (device 接続/切断/操作入力) を Unison wire の `DeviceEvent` に
    // 変換して push する単機能 channel。 world-process と違い method 分岐は無く、 接続 = 購読
    // (canvas channel 方式)。 `bastet_event_bus` が Some (= feature midi + Bastet 稼働) のときのみ登録。
    if let Some(ref bastet_event_bus) = state.bastet_event_bus {
        let bastet_event_bus = bastet_event_bus.clone();
        // M2 follow-up: subscribe 時の registry snapshot 送信用に registry 本体も capture (midi のみ)。
        #[cfg(feature = "midi")]
        let bastet = state.bastet.clone();
        server
            .register_channel("world-device", {
                move |_ctx, stream| {
                    let event_bus = bastet_event_bus.clone();
                    #[cfg(feature = "midi")]
                    let bastet = bastet.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        // 接続即購読: bastet.* を FilteredSubscription で受け、 DeviceEvent に変換して push。
                        // subscriber id は接続ごとにユニーク化する (= 複数 vp-app instance が同時購読
                        // しても EventBus の subscriptions メタデータが last-write-wins で衝突しない。
                        // broadcast 配信自体は receiver 独立で元々壊れないが、 subscriber_count を正確に保つ)。
                        let sub_id = format!("world-device-bridge-{}", uuid::Uuid::new_v4());
                        let sub = event_bus.subscribe(&sub_id, "bastet.*").await;
                        let mut filtered =
                            crate::capability::eventbus::FilteredSubscription::new(sub);

                        // M2 follow-up: subscribe の「後」に現 registry を device_connected として snapshot
                        // 送信する。 これで vp-app は (再)接続直後に device 一覧を即得る (従来は次の hot-plug
                        // まで空)。 順序が subscribe→snapshot なので delta の取りこぼしは無く、 snapshot と
                        // delta が重複し得るが、 vp-app の apply_device_event は port_name で retain-then-push
                        // = 冪等なので吸収される。 registry lock は collect で解放してから送る (send を跨いで
                        // 保持しない)。
                        #[cfg(feature = "midi")]
                        if let Some(bastet) = bastet.as_ref() {
                            let devices_arc = {
                                let b = bastet.read().await;
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
                        // TODO(Phase 2): FilteredSubscription は lag を silent skip する (eventbus.rs:39)。
                        // ControlEvent 高頻度時に lag 警告が出ないため、 必要なら Lagged 警告付きの
                        // 購読に差し替えるか buffer_size を調整する (world-process は lag を warn 可視化)。
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
                            if channel.send_event("event", &payload).await.is_err() {
                                break; // client 切断
                            }
                        }
                        Ok(())
                    }
                }
            })
            .await;
    }

    // =========================================================================
    // Device Channel（agent → daemon: CoreMIDI hot-plug 報告、doc 26 §2 channel_id=2）
    // =========================================================================
    // session/terminal と同じ request-dispatch 型 channel。macOS menu bar agent (Swift
    // `CoreMIDIWatcher`) が `ReportDevice` を送り、Bastet registry を更新する。
    // Model D (doc 25): hot-plug authority = agent。daemon は polling を回さない。
    #[cfg(feature = "midi")]
    if let Some(ref bastet) = state.bastet {
        let bastet = bastet.clone();
        server
            .register_channel("device", {
                move |_ctx, stream| {
                    let bastet = bastet.clone();
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
                                    handle_device_report(&bastet, request_id, payload).await
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
    // Registry Channel（SP 自己登録 — QUIC 永続接続による即時登録・即時死亡検出）
    // =========================================================================
    if let Some(ref running_processes) = state.running_processes {
        let running_processes = running_processes.clone();
        let projects = state.projects.clone();
        // Phase 1b: lane_registry も capture (register payload の lanes を cache する)
        let lane_registry = state.lane_registry.clone();
        // doc 24 §10 Phase 2: lane descriptor の durable 永続先 (daemon-canonical 化)。
        let vpdb = state.vpdb.clone();
        // VP-154 PR-2: lifecycle event を broadcast する Sender (= "world-process" subscriber へ)
        let process_lifecycle_tx = state.process_lifecycle_tx.clone();
        // L0 SP-portless (lanes slice): lane_registry mutate 後に project path_key を流す Sender
        // (= per-project "lanes" channel subscriber へ realtime 再 push を促す)。
        let lane_change_tx = state.lane_change_tx.clone();
        server
            .register_channel("registry", {
                move |_ctx, stream| {
                    let running_processes = running_processes.clone();
                    let projects = projects.clone();
                    let lane_registry = lane_registry.clone();
                    let vpdb = vpdb.clone();
                    let process_lifecycle_tx = process_lifecycle_tx.clone();
                    let lane_change_tx = lane_change_tx.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        let mut registered_name: Option<String> = None;
                        let mut _registered_project_dir: Option<String> = None;

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

                            match method.as_str() {
                                "register" => {
                                    let project_name = payload["project_name"]
                                        .as_str()
                                        .unwrap_or("unknown")
                                        .to_string();
                                    let port =
                                        payload["port"].as_u64().unwrap_or(0) as u16;
                                    let pid =
                                        payload["pid"].as_u64().unwrap_or(0) as u32;
                                    let project_dir = payload["project_dir"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();

                                    let tmux_session = payload["tmux_session"]
                                        .as_str()
                                        .map(|s| s.to_string());

                                    let process = RunningProcess {
                                        project_name: project_name.clone(),
                                        port,
                                        pid,
                                        project_path: project_dir.clone().into(),
                                        tmux_session,
                                    };

                                    // パスキーで一意識別
                                    let path_key = crate::capability::normalize_path_key(
                                        std::path::Path::new(&project_dir),
                                    );

                                    registered_name = Some(path_key.clone());
                                    _registered_project_dir = Some(project_dir.clone());

                                    // ロック順序統一: projects → running_processes
                                    // プロジェクト状態を Running に更新
                                    if let Some(ref projects) = projects {
                                        let mut projs = projects.write().await;
                                        if let Some(p) = projs.get_mut(&path_key) {
                                            p.process_status =
                                                crate::capability::ProcessStatus::Running;
                                        }
                                    }

                                    // running_processes に挿入（projects の後）
                                    running_processes
                                        .write()
                                        .await
                                        .insert(path_key.clone(), process);

                                    // lanes payload を lane_registry + db に snapshot 反映。
                                    //
                                    // doc 24 §10 Phase 2 (team-b review #1): lanes フィールド
                                    // **不在 (null)** は「lanes を知らない古 SP」を意味するので
                                    // **何もしない** (db の boot-loaded descriptor を保持 = §4.1
                                    // 喪失ゼロ)。 旧 cache 時代は不在を空 Vec 扱いで wipe していたが、
                                    // durable truth 化した今 wipe すると永続 descriptor を破壊する。
                                    // 明示的な空配列 `[]` は「lane を持たない」意思表示なので replace する。
                                    if let Some(ref lr) = lane_registry {
                                        let lanes_value = &payload["lanes"];
                                        if lanes_value.is_null() {
                                            tracing::debug!(
                                                "Registry: SP '{}' lanes フィールドなし (古 SP 互換、 db descriptor を保持)",
                                                project_name
                                            );
                                        } else {
                                            let lanes: Vec<
                                                crate::process::lanes_state::LaneInfo,
                                            > = serde_json::from_value(lanes_value.clone())
                                                .unwrap_or_default();
                                            let lane_count = lanes.len();
                                            lr.write().await.insert(path_key.clone(), lanes.clone());
                                            // doc 24 §10 Phase 2: snapshot を db に durable 永続
                                            // (project 単位 replace = SP reconnect 時の reconcile)。
                                            // lock は上の insert で解放済 (db await 中は保持しない)。
                                            if let Some(ref db) = vpdb
                                                && let Err(e) = db
                                                    .replace_lanes_for_project(&path_key, &lanes)
                                                    .await
                                            {
                                                tracing::warn!(
                                                    "lane snapshot の db 永続に失敗 (in-memory は反映済): {}",
                                                    e
                                                );
                                            }
                                            tracing::debug!(
                                                "Registry: SP '{}' lanes 登録 ({} entries)",
                                                project_name,
                                                lane_count
                                            );
                                        }
                                    }

                                    tracing::info!(
                                        "Registry: SP '{}' 登録 (port={}, pid={}, key={})",
                                        project_name,
                                        port,
                                        pid,
                                        path_key
                                    );

                                    // VP-154 PR-2: lifecycle event を broadcast (= "world-process"
                                    // subscriber に push)。 receiver 不在は OK (= 誰も subscribe して
                                    // ない時は send が SendError を返すが無視)。
                                    let _ = process_lifecycle_tx.send(ProcessLifecycleEvent::Add {
                                        project_path: path_key.clone(),
                                        project_name: project_name.clone(),
                                        port,
                                        pid,
                                    });

                                    // L0 lanes slice: register は lanes snapshot を載せる (= SP 起動
                                    // 直後の初期 lane 集合)。 per-project "lanes" channel の subscriber に
                                    // 再 push を促す (lanes 不在の register でも空→空で無害)。
                                    let _ = lane_change_tx.send(path_key.clone());

                                    if channel
                                        .send_response(
                                            request_id,
                                            "register",
                                            &serde_json::json!({"status": "ok"}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                "unregister" => {
                                    if let Some(ref path_key) = registered_name {
                                        // ロック順序統一: projects → running_processes
                                        // スコープブロックで projects ロックを先に解放
                                        if let Some(ref projects) = projects {
                                            let mut projs = projects.write().await;
                                            if let Some(p) = projs.get_mut(path_key) {
                                                p.process_status =
                                                    crate::capability::ProcessStatus::Stopped;
                                            }
                                        } // ← projects ロック解放
                                        {
                                            running_processes.write().await.remove(path_key);
                                        }
                                        // doc 24 §10 Phase 2 (authority 反転): graceful unregister
                                        // (SP shutdown) でも lane_registry を **drop しない**。
                                        // descriptor は durable truth で、 app quit = 喪失ゼロ (§4.1)。
                                        // descriptor の回収は project remove (= namespace ごと倒す)
                                        // のみが行う (capability::remove_project)。

                                        tracing::info!(
                                            "Registry: SP 登録解除 (key={})",
                                            path_key
                                        );

                                        // VP-154 PR-2: 明示 unregister 経由の lifecycle event。
                                        // 切断検知 (= channel 切断による Drop パス) も別途 publish が
                                        // 必要 (= 後続の disconnect handler で対応)。
                                        let _ = process_lifecycle_tx.send(
                                            ProcessLifecycleEvent::Remove {
                                                project_path: path_key.clone(),
                                            },
                                        );
                                    } else {
                                        tracing::debug!(
                                            "Registry: unregister 受信したが未登録"
                                        );
                                    }
                                    let _ = channel
                                        .send_response(
                                            request_id,
                                            "unregister",
                                            &serde_json::json!({"status": "ok"}),
                                        )
                                        .await;
                                    break; // チャネル終了
                                }
                                "heartbeat" => {
                                    if channel
                                        .send_response(
                                            request_id,
                                            "heartbeat",
                                            &serde_json::json!({"status": "ok"}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                "list" => {
                                    let procs = running_processes.read().await;
                                    let list: Vec<_> = procs
                                        .values()
                                        .map(|p| {
                                            serde_json::json!({
                                                "project_name": p.project_name,
                                                "port": p.port,
                                                "pid": p.pid,
                                                "project_path": p.project_path,
                                            })
                                        })
                                        .collect();
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
                                "lanes/add" => {
                                    // Phase 2 (Step E): SP push の Diff::Add 反映 (Performer spawn 完了 等)。
                                    // payload["payload"] が LaneInfo serde 結果。
                                    if let Some(ref lr) = lane_registry
                                        && let Some(ref path_key) = registered_name
                                        && let Ok(lane) = serde_json::from_value::<
                                            crate::process::lanes_state::LaneInfo,
                                        >(payload["payload"].clone())
                                    {
                                        {
                                            let mut registry = lr.write().await;
                                            let entry =
                                                registry.entry(path_key.clone()).or_default();
                                            // address 重複なら replace、 無ければ push (race 防御)
                                            if let Some(idx) =
                                                entry.iter().position(|l| l.address == lane.address)
                                            {
                                                entry[idx] = lane.clone();
                                            } else {
                                                entry.push(lane.clone());
                                            }
                                        } // ← lane_registry lock 解放 (db await 前)
                                        // doc 24 §10 Phase 2: descriptor を db に durable 永続。
                                        if let Some(ref db) = vpdb
                                            && let Err(e) = db.upsert_lane(path_key, &lane).await
                                        {
                                            tracing::warn!(
                                                "lanes/add の db 永続に失敗 (in-memory は反映済): {}",
                                                e
                                            );
                                        }
                                        tracing::debug!(
                                            "Registry: lanes/add 反映 (key={})",
                                            path_key
                                        );
                                        // L0 lanes slice: 当該 project の lane snapshot を再 push。
                                        let _ = lane_change_tx.send(path_key.clone());
                                    }
                                    if channel
                                        .send_response(
                                            request_id,
                                            "lanes/add",
                                            &serde_json::json!({"status": "ok"}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                "lanes/remove" => {
                                    // Phase 2 (Step E): SP push の Diff::Remove 反映 (Performer delete 等)。
                                    // payload["id"] が LaneAddress serde 結果。
                                    if let Some(ref lr) = lane_registry
                                        && let Some(ref path_key) = registered_name
                                        && let Ok(addr) = serde_json::from_value::<
                                            crate::process::lanes_state::LaneAddress,
                                        >(payload["id"].clone())
                                    {
                                        {
                                            let mut registry = lr.write().await;
                                            if let Some(entry) = registry.get_mut(path_key) {
                                                entry.retain(|l| l.address != addr);
                                            }
                                        } // ← lane_registry lock 解放 (db await 前)
                                        // doc 24 §10 Phase 2: descriptor を db からも回収。
                                        if let Some(ref db) = vpdb
                                            && let Err(e) = db
                                                .delete_lane(path_key, &addr.to_string())
                                                .await
                                        {
                                            tracing::warn!(
                                                "lanes/remove の db 永続に失敗 (in-memory は反映済): {}",
                                                e
                                            );
                                        }
                                        tracing::debug!(
                                            "Registry: lanes/remove 反映 (key={}, addr={})",
                                            path_key,
                                            addr
                                        );
                                        // L0 lanes slice: 当該 project の lane snapshot を再 push。
                                        let _ = lane_change_tx.send(path_key.clone());
                                    }
                                    if channel
                                        .send_response(
                                            request_id,
                                            "lanes/remove",
                                            &serde_json::json!({"status": "ok"}),
                                        )
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                "lanes/update" => {
                                    // Phase 2 (Step E): SP push の Diff::Update 反映 (state 変更 / restart 完了 等)。
                                    // payload["payload"] が LaneInfo serde 結果。 同 address の entry を replace。
                                    if let Some(ref lr) = lane_registry
                                        && let Some(ref path_key) = registered_name
                                        && let Ok(lane) = serde_json::from_value::<
                                            crate::process::lanes_state::LaneInfo,
                                        >(payload["payload"].clone())
                                    {
                                        // 既存 entry がある時だけ replace (defensive)。 db 永続も
                                        // in-memory に合わせ applied 時のみ (= 両者の真実を一致させる)。
                                        let mut applied = false;
                                        {
                                            let mut registry = lr.write().await;
                                            if let Some(entry) = registry.get_mut(path_key)
                                                && let Some(idx) = entry
                                                    .iter()
                                                    .position(|l| l.address == lane.address)
                                            {
                                                entry[idx] = lane.clone();
                                                applied = true;
                                            }
                                        } // ← lane_registry lock 解放 (db await 前)
                                        // team-b review #2: applied=false は「register 前の
                                        // update」等の SP protocol 違反 or in-memory/db divergence を
                                        // 示す異常状態。 正常経路では起きないので warn で可視化する
                                        // (無音で握り潰すと divergence を追えない)。
                                        if !applied {
                                            tracing::warn!(
                                                "lanes/update: in-memory に対象 lane なし (SP protocol 違反? key={}, addr={})",
                                                path_key,
                                                lane.address
                                            );
                                        }
                                        // doc 24 §10 Phase 2: descriptor を db に durable 永続。
                                        if applied
                                            && let Some(ref db) = vpdb
                                            && let Err(e) = db.upsert_lane(path_key, &lane).await
                                        {
                                            tracing::warn!(
                                                "lanes/update の db 永続に失敗 (in-memory は反映済): {}",
                                                e
                                            );
                                        }
                                        tracing::debug!(
                                            "Registry: lanes/update 反映 (key={})",
                                            path_key
                                        );
                                        // L0 lanes slice: 実際に replace された時のみ snapshot を再 push
                                        // (applied=false は no-op なので無駄打ちを避ける)。
                                        if applied {
                                            let _ = lane_change_tx.send(path_key.clone());
                                        }
                                    }
                                    if channel
                                        .send_response(
                                            request_id,
                                            "lanes/update",
                                            &serde_json::json!({"status": "ok"}),
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

                        // 切断時に自動除去（unregister なしで切断された場合）
                        // ロック順序統一: projects → running_processes
                        if let Some(name) = registered_name {
                            // プロジェクト状態を Stopped に更新（projects 先）
                            if let Some(ref projects) = projects {
                                let mut projs = projects.write().await;
                                if let Some(p) = projs.get_mut(&name) {
                                    p.process_status =
                                        crate::capability::ProcessStatus::Stopped;
                                }
                            }

                            // running_processes から除去（projects の後）
                            let removed = {
                                let mut procs = running_processes.write().await;
                                procs.remove(&name).is_some()
                            };

                            // doc 24 §10 Phase 2 (authority 反転の核心): SP 切断では lane_registry を
                            // **drop しない**。 descriptor は daemon-canonical な durable truth に
                            // なったので、 SP quit/crash を越えて生き残る (§4.1 app quit = 喪失ゼロ)。
                            // 失われるのは live engagement (SP の PtySlot/PTY) だけで、 descriptor は
                            // 残り、 SP reconnect で register snapshot が最新を上書きする (reconcile)。
                            // 旧挙動「disconnect = 全 Lane drop」(Phase 1b の cache 前提) はここで撤回。
                            // NOTE: live 値 (pid/state) の cold 化 (= §4.6 boot reconcile heal) は
                            // 後続 increment。 現状は last-known descriptor をそのまま保持する。

                            if removed {
                                tracing::info!(
                                    "Registry: SP 切断 → 自動除去 (key={})",
                                    name
                                );

                                // VP-154 PR-2: 切断由来の lifecycle remove event。 明示
                                // unregister と異なり、 ここは QUIC 切断検出 (= D10 Push パスの
                                // 即時死亡検出) を世界に伝える経路。
                                let _ = process_lifecycle_tx.send(
                                    ProcessLifecycleEvent::Remove {
                                        project_path: name.clone(),
                                    },
                                );
                            }
                        }

                        Ok(())
                    }
                }
            })
            .await;
    }

    // World Control Channel（projects mutation: CLI → World 直接 Unison）
    //
    // control plane 一元化: projects は World 権威 (db/world) なので、 CLI は SP を経由せず
    // World daemon に直接 Unison RPC する。 registry (SP 自己登録専用) とは責務を分離した
    // 別 channel にする。 world_cap 不在 (= 非 World mode) なら登録しない。
    if let Some(ref world_cap) = state.world_cap {
        let world_cap = world_cap.clone();
        server
            .register_channel("world-control", {
                move |_ctx, stream| {
                    let world_cap = world_cap.clone();
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
                                match handle_world_control(&world_cap, &method, payload).await {
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
    if let Err(e) = quic.start().await {
        tracing::error!("Daemon Unison サーバーエラー: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // =====================================================================
    // World Control Channel — projects mutation dispatch (handle_world_control)
    //
    // QUIC server を立てず handler 関数を直接呼ぶ Small test。 dispatch が
    // ProcessManagerCapability を正しく叩き、 in-memory 状態に反映されることを検証する。
    // (DB 真実源化は PR-C、 ここでは vpdb=None なので persist は projects.kdl no-op)
    // =====================================================================

    fn new_world_cap() -> Arc<RwLock<crate::capability::ProcessManagerCapability>> {
        Arc::new(RwLock::new(
            crate::capability::ProcessManagerCapability::new(),
        ))
    }

    #[tokio::test]
    async fn world_control_add_list_remove() {
        let cap = new_world_cap();
        // add_project は path.is_dir() を要求するので実在 dir (temp_dir) を使う
        let path = std::env::temp_dir().to_string_lossy().to_string();

        // add → 追加された ProjectInfo が返る
        let added = handle_world_control(
            &cap,
            "projects/add",
            serde_json::json!({"name": "wc-test", "path": path}),
        )
        .await
        .expect("add ok");
        assert_eq!(added["name"], "wc-test");

        // list に反映される
        let list = handle_world_control(&cap, "projects/list", serde_json::json!({}))
            .await
            .expect("list ok");
        let arr = list.as_array().expect("list is array");
        assert!(
            arr.iter().any(|p| p["name"] == "wc-test"),
            "added project が list に出る"
        );

        // remove (add と同じ path → 同じ正規化キーで削除)
        handle_world_control(&cap, "projects/remove", serde_json::json!({"path": path}))
            .await
            .expect("remove ok");
        let list2 = handle_world_control(&cap, "projects/list", serde_json::json!({}))
            .await
            .expect("list ok");
        assert!(list2.as_array().unwrap().is_empty(), "remove 後は空になる");
    }

    #[tokio::test]
    async fn world_control_unknown_method_errors() {
        let cap = new_world_cap();
        let r = handle_world_control(&cap, "projects/bogus", serde_json::json!({})).await;
        assert!(r.is_err(), "未知 method は Err");
    }

    #[tokio::test]
    async fn world_control_add_missing_field_errors() {
        let cap = new_world_cap();
        // name 欠落 → Err
        let r =
            handle_world_control(&cap, "projects/add", serde_json::json!({"path": "/tmp"})).await;
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

    #[test]
    fn test_validate_shell_cmd_allowed() {
        // 許可されたシェル（絶対パス）
        assert!(validate_shell_cmd("/bin/bash").is_ok());
        assert!(validate_shell_cmd("/bin/zsh").is_ok());
        assert!(validate_shell_cmd("/bin/sh").is_ok());
        assert!(validate_shell_cmd("/usr/bin/bash").is_ok());
        assert!(validate_shell_cmd("/usr/local/bin/fish").is_ok());
        assert!(validate_shell_cmd("/opt/homebrew/bin/zsh").is_ok());
    }

    #[test]
    fn test_validate_shell_cmd_allowed_bare() {
        // 許可されたシェル（コマンド名のみ）
        assert!(validate_shell_cmd("bash").is_ok());
        assert!(validate_shell_cmd("zsh").is_ok());
        assert!(validate_shell_cmd("sh").is_ok());
        assert!(validate_shell_cmd("fish").is_ok());
    }

    #[test]
    fn test_validate_shell_cmd_rejected() {
        // 拒否されるべきコマンド
        assert!(validate_shell_cmd("python").is_err());
        assert!(validate_shell_cmd("node").is_err());
        assert!(validate_shell_cmd("/usr/bin/python3").is_err());
        assert!(validate_shell_cmd("rm -rf /").is_err());
        assert!(validate_shell_cmd("bash -c 'malicious'").is_err());
        assert!(validate_shell_cmd("").is_err());
        assert!(validate_shell_cmd("/bin/bash; rm -rf /").is_err());
        assert!(validate_shell_cmd("zsh\nmalicious").is_err());
    }

    // =========================================================================
    // read_output の take-restore パターンのテスト
    // =========================================================================

    #[tokio::test]
    async fn test_read_output_take_restore_pattern() {
        // take-restore パターンの基本動作:
        // receiver を取り出し、データを受信し、元に戻す
        let state = DaemonState::new();
        let (tx, rx) = tokio::sync::broadcast::channel::<Vec<u8>>(16);
        let key = ("test-session".to_string(), 0u32);

        state.output_receivers.lock().await.insert(key.clone(), rx);

        // 1. receiver を取り出す
        let mut receivers = state.output_receivers.lock().await;
        let rx = receivers.remove(&key);
        drop(receivers); // ロック即解放

        let mut rx = rx.expect("receiver が存在するはず");

        // 2. ロック非保持の状態でデータ送受信
        tx.send(b"hello".to_vec()).unwrap();
        let data = rx.recv().await.unwrap();
        assert_eq!(data, b"hello");

        // 3. receiver を戻す
        state.output_receivers.lock().await.insert(key.clone(), rx);

        // 4. 戻った receiver がマップに存在することを確認
        assert!(
            state.output_receivers.lock().await.contains_key(&key),
            "receiver が復元されていない"
        );
    }

    #[tokio::test]
    async fn test_read_output_concurrent_different_panes() {
        // 異なるペインへの同時 read_output がデッドロックしないことを検証
        // 旧実装（Mutex保持のまま await）ではタスク2がタスク1のタイムアウト完了まで
        // ブロックされていた。新実装では両方が独立に進行する。
        let state = Arc::new(DaemonState::new());

        let (tx1, rx1) = tokio::sync::broadcast::channel::<Vec<u8>>(16);
        let (tx2, rx2) = tokio::sync::broadcast::channel::<Vec<u8>>(16);
        let key1 = ("session".to_string(), 0u32);
        let key2 = ("session".to_string(), 1u32);

        {
            let mut receivers = state.output_receivers.lock().await;
            receivers.insert(key1.clone(), rx1);
            receivers.insert(key2.clone(), rx2);
        }

        // ペイン1: 50ms後にデータ受信（100msタイムアウト）
        let state1 = state.clone();
        let key1c = key1.clone();
        let task1 = tokio::spawn(async move {
            let mut receivers = state1.output_receivers.lock().await;
            let rx = receivers.remove(&key1c);
            drop(receivers);

            let mut rx = rx.unwrap();
            let result =
                tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;

            let mut receivers = state1.output_receivers.lock().await;
            receivers.insert(key1c, rx);
            result.is_ok()
        });

        // ペイン2: 即座にデータ受信（ペイン1にブロックされないことを検証）
        let state2 = state.clone();
        let key2c = key2.clone();
        let task2 = tokio::spawn(async move {
            // 少し遅延してからtakeを試みる（task1がtakeした後）
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;

            let mut receivers = state2.output_receivers.lock().await;
            let rx = receivers.remove(&key2c);
            drop(receivers);

            let mut rx = rx.unwrap();
            let result =
                tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;

            let mut receivers = state2.output_receivers.lock().await;
            receivers.insert(key2c, rx);
            result.is_ok()
        });

        // 両ペインにデータ送信
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        tx1.send(b"pane1".to_vec()).unwrap();
        tx2.send(b"pane2".to_vec()).unwrap();

        let (r1, r2) = tokio::join!(task1, task2);
        assert!(r1.unwrap(), "ペイン1がデータを受信できなかった");
        assert!(
            r2.unwrap(),
            "ペイン2がデータを受信できなかった（デッドロックの可能性）"
        );
    }

    #[tokio::test]
    async fn test_read_output_same_pane_second_reader_sees_missing() {
        // 同一ペインへの同時アクセス:
        // 1つ目の reader が receiver を take 中、2つ目は receiver が見つからない
        let state = Arc::new(DaemonState::new());
        let (_tx, rx) = tokio::sync::broadcast::channel::<Vec<u8>>(16);
        let key = ("session".to_string(), 0u32);

        state.output_receivers.lock().await.insert(key.clone(), rx);

        // 1つ目: receiver を取り出す
        let mut receivers = state.output_receivers.lock().await;
        let _rx = receivers.remove(&key);
        drop(receivers);

        // 2つ目: 同じキーで取得を試みる → None（取り出し済み）
        let receivers = state.output_receivers.lock().await;
        assert!(
            !receivers.contains_key(&key),
            "take中のペインに receiver が残っている"
        );
    }
}
