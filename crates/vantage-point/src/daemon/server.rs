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
}

impl Default for DaemonState {
    fn default() -> Self {
        let (process_lifecycle_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            registry: Arc::new(RwLock::new(SessionRegistry::default())),
            pty_slots: Arc::new(Mutex::new(HashMap::new())),
            output_receivers: Arc::new(Mutex::new(HashMap::new())),
            started_at: Instant::now(),
            running_processes: None,
            projects: None,
            lane_registry: None,
            process_lifecycle_tx,
            world_cap: None,
            vpdb: None,
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
                                    // ROTO の cross-project 8-slot LCD が唯一の consumer (現状)。
                                    //
                                    // 並び順は sidebar と一致させる (= project_order)。物理 controller は
                                    // 位置 = 意味なので、track button N の位置が sidebar の N 番目と
                                    // 対応する必要がある。project_name で order を引く (path 正規化不要)。
                                    let order: Vec<String> = match world_cap {
                                        Some(ref w) => w
                                            .read()
                                            .await
                                            .list_projects()
                                            .await
                                            .into_iter()
                                            .map(|p| p.name)
                                            .collect(),
                                        None => Vec::new(),
                                    };
                                    // ロック順序統一: running_processes → lane_registry (register と同順)。
                                    let mut entries: Vec<(usize, serde_json::Value)> = Vec::new();
                                    {
                                        let procs = running_processes.read().await;
                                        let lanes_map = match lane_registry {
                                            Some(ref lr) => Some(lr.read().await),
                                            None => None,
                                        };
                                        for (key, p) in procs.iter() {
                                            let lanes = lanes_map
                                                .as_ref()
                                                .and_then(|m| m.get(key))
                                                .cloned()
                                                .unwrap_or_default();
                                            // project_order 内の位置。未登録は末尾 (usize::MAX)。
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
                                    // project_order 順に整列 (= sidebar 順)。同 idx は安定ソートで維持。
                                    entries.sort_by_key(|(idx, _)| *idx);
                                    let projects: Vec<serde_json::Value> =
                                        entries.into_iter().map(|(_, v)| v).collect();
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
        server
            .register_channel("registry", {
                move |_ctx, stream| {
                    let running_processes = running_processes.clone();
                    let projects = projects.clone();
                    let lane_registry = lane_registry.clone();
                    let vpdb = vpdb.clone();
                    let process_lifecycle_tx = process_lifecycle_tx.clone();
                    async move {
                        let channel = UnisonChannel::new(stream);
                        let mut registered_name: Option<String> = None;
                        let mut registered_port: Option<u16> = None;
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
                                    registered_port = Some(port);
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

                                // メニューバーアプリに通知
                                if let Some(port) = registered_port {
                                    crate::notify::post_process_changed(
                                        port,
                                        "stopped",
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
