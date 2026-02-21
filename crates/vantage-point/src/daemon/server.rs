//! Daemon の Unison QUIC サーバー
//!
//! session / terminal / system の3チャネルを提供。
//! Console (vp start) からの接続を受け付け、PTY I/O を中継する。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, RwLock};
use unison::network::{NetworkError, ProtocolServer, UnisonServer, UnisonServerExt};

use super::protocol::{
    AttachRequest, CreatePaneRequest, CreateSessionRequest, DetachRequest, KillPaneRequest,
    ReadOutputRequest, ResizeRequest, WriteRequest,
};
use super::pty_slot::PtySlot;
use super::registry::{PaneKind, SessionRegistry};

/// Daemon の共有状態
///
/// `pty_slots` は `Mutex` を使用する（`PtySlot` が `Sync` を実装しないため）。
/// `registry` は純粋なデータ構造なので `RwLock` で読み取り並行性を確保。
pub struct DaemonState {
    /// セッション・ペインのレジストリ
    pub registry: Arc<RwLock<SessionRegistry>>,
    /// PTYスロット: (session_id, pane_id) → PtySlot
    /// PtySlot は Send だが Sync ではないため Mutex を使用
    pub pty_slots: Arc<Mutex<HashMap<(String, u32), PtySlot>>>,
    /// PTY出力の broadcast receiver: ペインごとに保持
    /// terminal.read_output で消費される
    pub output_receivers:
        Arc<Mutex<HashMap<(String, u32), tokio::sync::broadcast::Receiver<Vec<u8>>>>>,
    /// Daemon 起動時刻（uptime計算用）
    pub started_at: Instant,
}

impl DaemonState {
    /// 新しい DaemonState を作成
    pub fn new() -> Self {
        Self {
            registry: Arc::new(RwLock::new(SessionRegistry::new())),
            pty_slots: Arc::new(Mutex::new(HashMap::new())),
            output_receivers: Arc::new(Mutex::new(HashMap::new())),
            started_at: Instant::now(),
        }
    }
}

/// Daemon の Unison QUIC サーバーを起動する
///
/// session / terminal / system の各ハンドラを登録し、
/// 指定ポートで QUIC 接続を待ち受ける。
pub async fn start_daemon_server(state: Arc<DaemonState>, port: u16) {
    let addr = format!("[::1]:{}", port);
    let mut server = ProtocolServer::new();

    // =========================================================================
    // Session Channel
    // =========================================================================

    // --- session.create ---
    {
        let state = state.clone();
        server.register_handler("session.create", move |payload| {
            let req: CreateSessionRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid session.create payload: {}", e))
            })?;

            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                let mut registry = state.registry.write().await;

                // 既存セッションがあればエラー
                if registry.get_session(&req.session_id).is_some() {
                    return Err(NetworkError::Protocol(format!(
                        "セッション '{}' は既に存在します",
                        req.session_id
                    )));
                }

                let info = registry.create_session(&req.session_id);
                let response = serde_json::json!({
                    "status": "ok",
                    "session_id": info.id,
                    "created_at": info.created_at,
                });

                Ok(response)
            })
        });
    }

    // --- session.list ---
    {
        let state = state.clone();
        server.register_handler("session.list", move |_payload| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
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

                Ok(serde_json::json!({
                    "status": "ok",
                    "sessions": sessions,
                }))
            })
        });
    }

    // --- session.attach ---
    {
        let state = state.clone();
        server.register_handler("session.attach", move |payload| {
            let req: AttachRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid session.attach payload: {}", e))
            })?;

            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                let registry = state.registry.read().await;
                let session = registry.get_session(&req.session_id).ok_or_else(|| {
                    NetworkError::Protocol(format!(
                        "セッション '{}' が見つかりません",
                        req.session_id
                    ))
                })?;

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

                Ok(serde_json::json!({
                    "status": "ok",
                    "session_id": session.id,
                    "panes": panes,
                }))
            })
        });
    }

    // --- session.detach ---
    {
        server.register_handler("session.detach", move |payload| {
            let _req: DetachRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid session.detach payload: {}", e))
            })?;

            // デタッチは接続側の状態変更のみ（Daemon 側では特に処理なし）
            Ok(serde_json::json!({"status": "ok"}))
        });
    }

    // =========================================================================
    // Terminal Channel
    // =========================================================================

    // --- terminal.create_pane ---
    {
        let state = state.clone();
        server.register_handler("terminal.create_pane", move |payload| {
            let req: CreatePaneRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid terminal.create_pane payload: {}", e))
            })?;

            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                // 作業ディレクトリはホームディレクトリをデフォルトに
                let cwd = dirs::home_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "/tmp".to_string());

                // シェルコマンドのバリデーション（コマンドインジェクション防止）
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

                if !ALLOWED_SHELLS.contains(&req.shell_cmd.as_str()) {
                    return Err(NetworkError::Protocol(format!(
                        "許可されていないシェルコマンド: {}",
                        req.shell_cmd
                    )));
                }

                // PTYスロット起動
                let slot = PtySlot::spawn(&cwd, &req.shell_cmd, req.cols, req.rows)
                    .map_err(|e| NetworkError::Protocol(format!("PTY起動失敗: {}", e)))?;

                let pid = slot.pid();

                // レジストリにペイン追加
                let mut registry = state.registry.write().await;
                let pane_id = registry
                    .add_pane(
                        &req.session_id,
                        PaneKind::Pty {
                            pid,
                            shell_cmd: req.shell_cmd.clone(),
                        },
                        req.cols,
                        req.rows,
                    )
                    .ok_or_else(|| {
                        NetworkError::Protocol(format!(
                            "セッション '{}' が見つかりません",
                            req.session_id
                        ))
                    })?;

                // PTY出力の receiver を作成して保存
                let output_rx = slot.subscribe_output();

                // PTYスロットを保存
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

                Ok(serde_json::json!({
                    "status": "ok",
                    "pane_id": pane_id,
                    "pid": pid,
                }))
            })
        });
    }

    // --- terminal.write ---
    {
        let state = state.clone();
        server.register_handler("terminal.write", move |payload| {
            let req: WriteRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid terminal.write payload: {}", e))
            })?;

            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                // base64 デコード
                use base64::Engine;
                let engine = base64::engine::general_purpose::STANDARD;
                let data = engine
                    .decode(&req.data)
                    .map_err(|e| NetworkError::Protocol(format!("base64 デコード失敗: {}", e)))?;

                let mut slots = state.pty_slots.lock().await;
                let key = (req.session_id.clone(), req.pane_id);
                let slot = slots.get_mut(&key).ok_or_else(|| {
                    NetworkError::Protocol(format!(
                        "ペインが見つかりません: session={}, pane_id={}",
                        req.session_id, req.pane_id
                    ))
                })?;

                slot.write(&data)
                    .map_err(|e| NetworkError::Protocol(format!("PTY書き込み失敗: {}", e)))?;

                Ok(serde_json::json!({"status": "ok"}))
            })
        });
    }

    // --- terminal.resize ---
    {
        let state = state.clone();
        server.register_handler("terminal.resize", move |payload| {
            let req: ResizeRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid terminal.resize payload: {}", e))
            })?;

            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                let slots = state.pty_slots.lock().await;
                let key = (req.session_id.clone(), req.pane_id);
                let slot = slots.get(&key).ok_or_else(|| {
                    NetworkError::Protocol(format!(
                        "ペインが見つかりません: session={}, pane_id={}",
                        req.session_id, req.pane_id
                    ))
                })?;

                slot.resize(req.cols, req.rows)
                    .map_err(|e| NetworkError::Protocol(format!("リサイズ失敗: {}", e)))?;

                tracing::debug!(
                    "ペインリサイズ: session={}, pane_id={}, {}x{}",
                    req.session_id,
                    req.pane_id,
                    req.cols,
                    req.rows
                );

                Ok(serde_json::json!({"status": "ok"}))
            })
        });
    }

    // --- terminal.read_output ---
    {
        let state = state.clone();
        server.register_handler("terminal.read_output", move |payload| {
            let req: ReadOutputRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid terminal.read_output payload: {}", e))
            })?;

            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                let key = (req.session_id.clone(), req.pane_id);

                // 1. receiver をマップから取り出す（ロックを短時間で解放）
                let mut receivers = state.output_receivers.lock().await;
                let rx = receivers.remove(&key);
                drop(receivers); // ロックを即座に解放

                let Some(mut rx) = rx else {
                    return Err(NetworkError::Protocol(format!(
                        "出力 receiver が見つかりません: session={}, pane_id={}",
                        req.session_id, req.pane_id
                    )));
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

                Ok(serde_json::json!({
                    "data": encoded,
                    "bytes_read": all_data.len(),
                }))
            })
        });
    }

    // --- terminal.kill_pane ---
    {
        let state = state.clone();
        server.register_handler("terminal.kill_pane", move |payload| {
            let req: KillPaneRequest = serde_json::from_value(payload).map_err(|e| {
                NetworkError::Protocol(format!("Invalid terminal.kill_pane payload: {}", e))
            })?;

            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
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
                    return Err(NetworkError::Protocol(format!(
                        "ペインが見つかりません: session={}, pane_id={}",
                        req.session_id, req.pane_id
                    )));
                }

                tracing::info!(
                    "ペイン終了: session={}, pane_id={}",
                    req.session_id,
                    req.pane_id
                );

                Ok(serde_json::json!({"status": "ok"}))
            })
        });
    }

    // =========================================================================
    // System Channel
    // =========================================================================

    // --- system.health ---
    {
        let state = state.clone();
        server.register_handler("system.health", move |_payload| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async {
                let registry = state.registry.read().await;
                let sessions_count = registry.list_sessions().len();
                let uptime_secs = state.started_at.elapsed().as_secs();

                Ok(serde_json::json!({
                    "status": "ok",
                    "sessions_count": sessions_count,
                    "uptime_secs": uptime_secs,
                }))
            })
        });
    }

    // --- system.shutdown ---
    {
        server.register_handler("system.shutdown", move |_payload| {
            tracing::info!("system.shutdown リクエスト受信");

            // シャットダウンはプロセス終了で実現
            // Daemon の tokio::select! がシグナルをキャッチしてクリーンアップする
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let pid = std::process::id();
                if let Ok(pid_i32) = i32::try_from(pid) {
                    unsafe {
                        let ret = libc::kill(pid_i32, libc::SIGTERM);
                        if ret != 0 {
                            tracing::warn!("system.shutdown: kill が失敗しました（errno）");
                            std::process::exit(1);
                        }
                    }
                } else {
                    tracing::error!("PIDがi32の範囲外: {}", pid);
                    std::process::exit(1);
                }
            });

            Ok(serde_json::json!({"status": "ok", "message": "shutting down"}))
        });
    }

    // サーバー起動
    tracing::info!("Daemon Unison QUIC サーバー起動: {}", addr);
    if let Err(e) = server.listen(&addr).await {
        tracing::error!("Daemon Unison サーバー起動失敗: {}", e);
    }
}
