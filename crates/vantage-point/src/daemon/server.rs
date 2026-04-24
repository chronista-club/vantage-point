//! Daemon の Unison QUIC サーバー
//!
//! session / terminal / system の3チャネルを提供。
//! Console (vp hd attach) からの接続を受け付け、PTY I/O を中継する。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, RwLock};
use unison::network::{MessageType, NetworkError, ProtocolServer, channel::UnisonChannel};

use super::protocol::{
    AttachRequest, ChannelMessage, CreatePaneRequest, CreateSessionRequest, DetachRequest,
    KillPaneRequest, ReadOutputRequest, ResizeRequest, WriteRequest,
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
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            registry: Arc::new(RwLock::new(SessionRegistry::default())),
            pty_slots: Arc::new(Mutex::new(HashMap::new())),
            output_receivers: Arc::new(Mutex::new(HashMap::new())),
            started_at: Instant::now(),
            running_processes: None,
            projects: None,
        }
    }
}

impl DaemonState {
    /// 新しい DaemonState を作成
    pub fn new() -> Self {
        Self::default()
    }

    /// ProcessManagerCapability の running_processes を共有する
    pub fn with_running_processes(
        mut self,
        running_processes: Arc<RwLock<HashMap<String, RunningProcess>>>,
        projects: Arc<RwLock<HashMap<String, crate::capability::ProjectInfo>>>,
    ) -> Self {
        self.running_processes = Some(running_processes);
        self.projects = Some(projects);
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
    let (slot, output_rx) = match PtySlot::spawn(&cwd, &req.shell_cmd, req.cols, req.rows) {
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
            channel.send_response(id, method, payload).await
        }
        ChannelMessage::Error { id, message } => {
            channel
                .send_response(id, method, serde_json::json!({"error": message}))
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
    // Registry Channel（SP 自己登録 — QUIC 永続接続による即時登録・即時死亡検出）
    // =========================================================================
    if let Some(ref running_processes) = state.running_processes {
        let running_processes = running_processes.clone();
        let projects = state.projects.clone();
        server
            .register_channel("registry", {
                move |_ctx, stream| {
                    let running_processes = running_processes.clone();
                    let projects = projects.clone();
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

                                    tracing::info!(
                                        "Registry: SP '{}' 登録 (port={}, pid={}, key={})",
                                        project_name,
                                        port,
                                        pid,
                                        path_key
                                    );

                                    if channel
                                        .send_response(
                                            request_id,
                                            "register",
                                            serde_json::json!({"status": "ok"}),
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

                                        tracing::info!(
                                            "Registry: SP 登録解除 (key={})",
                                            path_key
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
                                            serde_json::json!({"status": "ok"}),
                                        )
                                        .await;
                                    break; // チャネル終了
                                }
                                "heartbeat" => {
                                    if channel
                                        .send_response(
                                            request_id,
                                            "heartbeat",
                                            serde_json::json!({"status": "ok"}),
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
                                            serde_json::json!({"processes": list}),
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
                                            serde_json::json!({
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

                            if removed {
                                tracing::info!(
                                    "Registry: SP 切断 → 自動除去 (key={})",
                                    name
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

    // サーバー起動
    tracing::info!("Daemon Unison QUIC サーバー起動: {}", addr);
    if let Err(e) = server.listen(&addr).await {
        tracing::error!("Daemon Unison サーバー起動失敗: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
