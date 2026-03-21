//! HTTP server with WebSocket support
//!
//! Process サーバーのエントリーポイント。`run()` と `run_world()` でサーバーを起動する。
//! ルートハンドラーは `routes/` モジュールに分離されている。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::routing::{get, post};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

use super::capabilities::{CapabilityConfig, ProcessCapabilities};
use super::hub::Hub;
use super::pty::PtyManager;
use super::routes::{health, lanes, permission, prompt, update, world, ws};
use super::session::SessionManager;
use super::state::AppState;
use super::topic_router::TopicRouter;
use super::unison_server;
use crate::capability::{ProcessManagerCapability, UpdateCapability};
use crate::file_watcher::FileWatcherManager;
use crate::protocol::DebugMode;

/// Run the Process server
pub async fn run(
    port: u16,
    auto_open_browser: bool,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    let project_dir = cap_config.project_dir.clone();

    // rustls 0.23+ は CryptoProvider の明示的な設定が必要
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // トレースログファイルを早期初期化
    crate::trace_log::init_log_file();

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Create session manager with state restoration
    let sessions = SessionManager::with_config(port, project_dir.clone());
    tracing::info!(
        "Session manager initialized with {} sessions",
        sessions.session_count()
    );

    // Initialize Capability system
    let capabilities = Arc::new(ProcessCapabilities::new(cap_config).await);

    // Initialize all capabilities
    if let Err(e) = capabilities.initialize().await {
        tracing::warn!("Failed to initialize capabilities: {}", e);
    }

    let hub = Hub::new();

    // Start event bridge: EventBus -> Hub（shutdown token で停止可能）
    let _event_bridge = capabilities.start_event_bridge(hub.sender(), shutdown_token.clone());
    tracing::info!("Capability event bridge started");

    // Terminal チャネル認証トークンを生成
    let terminal_token = crate::discovery::generate_terminal_token();

    // tmux / ccwire はvp sp コマンドで独立管理（server.rs では触らない）
    // TmuxActor は SP がペイン操作（tmux_split 等）に使うため、既存セッションがあれば起動
    let project_name = crate::resolve::project_name_from_path(
        &project_dir,
        &crate::config::Config::load().unwrap_or_default(),
    )
    .to_string();
    let tmux_session = crate::tmux::session_name(&project_name);

    let tmux_handle =
        if crate::tmux::is_tmux_available() && crate::tmux::session_exists(&tmux_session) {
            super::tmux_actor::spawn_for_session(&tmux_session)
        } else {
            None
        };
    let tmux_session_name = tmux_session.clone();

    // TopicRouter 初期化 + Hub → TopicRouter ブリッジ（shutdown token で停止可能）
    let topic_router = Arc::new(TopicRouter::new());
    {
        let router_clone = topic_router.clone();
        let mut hub_rx = hub.subscribe();
        let shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!("TopicRouter bridge: shutdown");
                        break;
                    }
                    result = hub_rx.recv() => {
                        match result {
                            Ok(msg) => router_clone.route(msg).await,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("TopicRouter lagged: {} messages dropped", n);
                            }
                        }
                    }
                }
            }
        });
    }

    // SurrealDB に接続（VP-21: SP ローカルテーブル移行）
    // 接続失敗時は warn して DB なしで継続（フォールバック）
    // スキーマ定義も行う（TheWorld 未起動の SP 単独起動時でも正常動作するよう冪等に実行）
    let vpdb: Option<crate::db::SharedVpDb> = {
        let password = crate::db::ensure_db_password();
        match crate::db::VpDb::connect(crate::db::SURREAL_PORT, &password, 10).await {
            Ok(db) => {
                if let Err(e) = db.define_schema().await {
                    tracing::warn!("SP: SurrealDB スキーマ定義失敗（DB なしで継続）: {}", e);
                    None
                } else {
                    tracing::info!("SP: SurrealDB 接続成功 (port={})", crate::db::SURREAL_PORT);
                    Some(std::sync::Arc::new(db))
                }
            }
            Err(e) => {
                tracing::warn!("SP: SurrealDB 未接続、DB なしで継続: {}", e);
                None
            }
        }
    };

    // MCP 用 Mailbox ハンドルを登録（VP-24）
    let mcp_mailbox = capabilities.mailbox_router.register("mcp").await;

    // Notification ブリッジ: Mailbox "notify" → DistributedNotification（VP-24）
    // Mailbox に送られた Notification メッセージを macOS DistributedNotification に変換
    // shutdown token で停止可能
    {
        let notify_handle = capabilities.mailbox_router.register("notify").await;
        let project_dir_clone = project_dir.clone();
        let shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!("Notification bridge: shutdown");
                        break;
                    }
                    msg = notify_handle.recv() => {
                        match msg {
                            Some(msg) if msg.kind == crate::capability::mailbox::MessageKind::Notification => {
                                let project = msg
                                    .payload
                                    .get("project")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| {
                                        project_dir_clone
                                            .rsplit('/')
                                            .find(|s| !s.is_empty())
                                            .unwrap_or("unknown")
                                    })
                                    .to_string();
                                let message = msg
                                    .payload
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("完了")
                                    .to_string();
                                crate::notify::post_cc_notification(&project, &message);
                            }
                            Some(_) => {} // 非 Notification メッセージは無視
                            None => break, // チャネル閉鎖
                        }
                    }
                }
            }
        });
    }

    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(sessions)),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode,
        shutdown_token: shutdown_token.clone(),
        project_dir,
        pending_permissions: Arc::new(RwLock::new(HashMap::new())),
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities,
        world: None,
        update: None,
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: terminal_token.clone(),
        tmux: Arc::new(tokio::sync::Mutex::new(tmux_handle)),
        tmux_session_name: tmux_session_name,
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        mcp_mailbox: Some(mcp_mailbox),
        vpdb,
    });

    // ペイン状態をディスクから復元（前回 Process 終了時の状態 → RetainedStore）
    state.restore_pane_contents().await;

    let app = Router::new()
        .route("/", get(health::index_handler))
        .route("/canvas", get(health::canvas_handler))
        .route("/vendor/{filename}", get(health::vendor_handler))
        .route("/wasm/{filename}", get(health::wasm_handler))
        .route("/ws", get(ws::ws_handler))
        // Canvas Lane 集約 WebSocket（全 Process のメッセージを Lane でラップして中継）
        .route("/ws/lanes", get(lanes::lanes_ws_handler))
        .route("/api/show", post(health::show_handler))
        .route("/api/toggle-pane", post(health::toggle_pane_handler))
        .route("/api/split-pane", post(health::split_pane_handler))
        .route("/api/close-pane", post(health::close_pane_handler))
        .route("/api/watch-file", post(health::watch_file_handler))
        .route("/api/unwatch-file", post(health::unwatch_file_handler))
        // tmux ペイン操作（Native App の Cmd+D / Cmd+Shift+D から呼ばれる）
        .route("/api/tmux/split", post(health::tmux_split_handler))
        .route("/api/tmux/close", post(health::tmux_close_handler))
        .route("/api/ruby/eval", post(health::ruby_eval_handler))
        .route("/api/ruby/run", post(health::ruby_run_handler))
        .route("/api/ruby/stop", post(health::ruby_stop_handler))
        .route("/api/ruby/list", get(health::ruby_list_handler))
        // ProcessRunner 汎用 API
        .route("/api/process/run", post(health::process_run_handler))
        .route("/api/process/eval", post(health::process_run_eval_handler))
        .route("/api/process/stop", post(health::process_stop_handler))
        .route("/api/process/inject", post(health::process_inject_handler))
        .route("/api/process/list", get(health::process_list_handler))
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        .route("/api/panes", axum::routing::delete(health::clear_panes_handler))
        .route(
            "/api/permission",
            post(permission::permission_request_handler),
        )
        .route(
            "/api/permission/{request_id}",
            get(permission::permission_poll_handler),
        )
        // User prompt API routes (REQ-PROMPT-001)
        .route("/api/prompt", post(prompt::prompt_request_handler))
        .route(
            "/api/prompt/{request_id}",
            get(prompt::prompt_poll_handler).post(prompt::prompt_respond_handler),
        )
        .route(
            "/api/prompts/pending",
            get(prompt::prompts_list_pending_handler),
        )
        // World API routes
        .route(
            "/api/world/projects",
            get(world::world_list_projects).post(world::world_add_project),
        )
        .route(
            "/api/world/projects/reorder",
            post(world::world_reorder_projects),
        )
        .route(
            "/api/world/projects/update",
            post(world::world_update_project),
        )
        .route(
            "/api/world/projects/remove",
            post(world::world_remove_project),
        )
        .route("/api/world/processes", get(world::world_list_processes))
        .route(
            "/api/world/processes/{project_name}/start",
            post(world::world_start_process),
        )
        .route(
            "/api/world/processes/{project_name}/stop",
            post(world::world_stop_process),
        )
        .route(
            "/api/world/processes/{project_name}/pointview",
            post(world::world_open_pointview),
        )
        .route("/api/world/refresh", post(world::world_refresh))
        .route(
            "/api/world/ccwire/sessions",
            get(world::world_ccwire_sessions),
        )
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let addr: SocketAddr = format!("[::1]:{}", port).parse()?;
    tracing::info!("Starting vp on http://{}", addr);

    // Auto-open browser
    if auto_open_browser {
        let url = format!("http://localhost:{}", port);
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            if let Err(e) = health::open_browser(&url) {
                tracing::warn!("Failed to open browser: {}", e);
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Unison QUIC サーバーを並行起動（readiness signal 付き）
    let quic_port = port + unison_server::QUIC_PORT_OFFSET;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    {
        let state_for_quic = state.clone();
        tokio::spawn(async move {
            unison_server::start_unison_server(state_for_quic, port, ready_tx).await;
        });
    }

    // QUIC サーバーのバインド完了を待つ
    let _ = ready_rx.await;
    tracing::info!("QUIC server ready on port {}", quic_port);

    // デバッグモード時のみトレースログ監視を起動
    if debug_mode != DebugMode::None {
        let hub_for_log = state.hub.clone();
        tokio::spawn(async move {
            crate::trace_log::watch_and_broadcast(hub_for_log).await;
        });
    }

    // TheWorld に QUIC Registry 登録（永続接続 + heartbeat）
    // 切断時に TheWorld が即時除去するため、HTTP 登録は不要
    let pid = std::process::id();
    crate::discovery::spawn_registry_keepalive(
        port,
        &state.project_dir,
        pid,
        &terminal_token,
        shutdown_token.clone(),
    );

    // メニューバーアプリに起動完了を通知
    crate::notify::post_process_changed(port, "started");

    // Clone for shutdown
    let capabilities_for_shutdown = state.capabilities.clone();
    let file_watchers_for_shutdown = state.file_watchers.clone();
    let state_for_shutdown = state.clone();

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("Graceful shutdown initiated");
        })
        .await?;

    // QUIC Registry 切断で TheWorld が即時除去するため、明示的 unregister は不要
    // （spawn_registry_keepalive の shutdown handler が unregister を送信済み）

    // ペイン状態をディスクに保存（次回起動時に復元、RetainedStore から取得）
    state_for_shutdown.persist_pane_contents().await;

    // メニューバーアプリに停止を通知
    crate::notify::post_process_changed(port, "stopped");

    // ファイル監視を全停止
    file_watchers_for_shutdown.lock().await.stop_all();

    // tmux / ccwire は vp sp stop で管理（SP 停止時には触らない）

    // Shutdown all capabilities
    tracing::info!("Shutting down capabilities...");
    if let Err(e) = capabilities_for_shutdown.shutdown().await {
        tracing::warn!("Error during capability shutdown: {}", e);
    }

    tracing::info!("Server stopped");
    Ok(())
}

/// WorldモードでProcessサーバーを起動
/// 複数のProject Processを管理するための専用モード
/// Daemon（PTY管理 QUIC サーバー）も統合して起動する
pub async fn run_world(port: u16) -> Result<()> {
    use crate::capability::core::{Capability, CapabilityContext};
    use crate::daemon::process;

    // PID ファイルはポートバインド成功後に書き出す（下記参照）

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Initialize World Capability
    let mut world_cap = ProcessManagerCapability::new();
    let ctx = CapabilityContext::new();

    if let Err(e) = world_cap.initialize(&ctx).await {
        tracing::error!("Failed to initialize ProcessManagerCapability: {}", e);
        return Err(anyhow::anyhow!(
            "ProcessManagerCapability initialization failed: {}",
            e
        ));
    }

    // Initialize Update Capability
    let mut update_cap = UpdateCapability::new();
    if let Err(e) = update_cap.initialize(&ctx).await {
        tracing::warn!("Failed to initialize UpdateCapability: {}", e);
    }

    // SurrealDB 認証パスワードを取得（なければ生成）
    let db_password = crate::db::ensure_db_password();

    // SurrealDB デーモンを自動起動（未起動なら起動、起動済みならスキップ）
    // TheWorld 終了時には SurrealDB は止めない（独立デーモン）
    match crate::db::ensure_surreal_running(crate::db::SURREAL_PORT, &db_password) {
        Ok(pid) => {
            tracing::info!(
                "SurrealDB 起動済み (pid={}, port={})",
                pid,
                crate::db::SURREAL_PORT
            );
        }
        Err(e) => {
            tracing::warn!("SurrealDB 起動失敗（DB なしで継続）: {}", e);
        }
    }

    // SurrealDB に接続してスキーマ定義
    let vpdb: Option<crate::db::SharedVpDb> =
        match crate::db::VpDb::connect(crate::db::SURREAL_PORT, &db_password, 100).await {
            Ok(db) => {
                if let Err(e) = db.define_schema().await {
                    tracing::warn!("SurrealDB スキーマ定義失敗: {}", e);
                }
                Some(std::sync::Arc::new(db))
            }
            Err(e) => {
                tracing::warn!("SurrealDB 接続失敗（DB なしで継続）: {}", e);
                None
            }
        };

    // VpDb を ProcessManagerCapability に注入し、DB からプロジェクトを再読み込み
    // （initialize 時点では vpdb 未設定のため config.toml から読み込まれている。
    //   ここで DB マイグレーション + DB → HashMap 同期を実行する）
    if let Some(ref db) = vpdb {
        world_cap.set_vpdb(db.clone());
        if let Err(e) = world_cap.load_config().await {
            tracing::warn!("DB 付き config 再読み込み失敗: {}", e);
        }
    }

    let world_cap = Arc::new(RwLock::new(world_cap));
    let update_cap = Arc::new(RwLock::new(update_cap));
    let hub = Hub::new();

    // TopicRouter（World モードでは Hub ブリッジ不要だが、AppState の必須フィールド）
    let topic_router = Arc::new(TopicRouter::new());

    // Create minimal state for world mode
    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(SessionManager::new())),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode: DebugMode::None,
        shutdown_token: shutdown_token.clone(),
        project_dir: String::new(),
        pending_permissions: Arc::new(RwLock::new(HashMap::new())),
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities: Arc::new(
            ProcessCapabilities::new(CapabilityConfig {
                project_dir: String::new(),
                midi_config: None,
            })
            .await,
        ),
        world: Some(world_cap.clone()),
        update: Some(update_cap.clone()),
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "WORLD_DISABLED".to_string(),
        tmux: Arc::new(tokio::sync::Mutex::new(None)),
        tmux_session_name: String::new(),
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        started_at: chrono::Utc::now().to_rfc3339(),
        mcp_mailbox: None,  // World モードでは MCP Mailbox 不要
        vpdb: vpdb.clone(), // World モードでも DB 参照あり
    });

    let app = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        .route("/api/panes", axum::routing::delete(health::clear_panes_handler))
        // Canvas HTML（PP window が TheWorld ポートから直接ロードするため必要）
        .route("/canvas", get(health::canvas_handler))
        .route("/vendor/{filename}", get(health::vendor_handler))
        // Canvas Lane 集約 WebSocket
        .route("/ws/lanes", get(lanes::lanes_ws_handler))
        // Canvas API（TheWorld 経由で Canvas WS に到達 — 一元管理）
        .route("/api/canvas/capture", post(health::canvas_capture_handler))
        .route(
            "/api/canvas/switch_lane",
            post(health::canvas_switch_lane_handler),
        )
        .route(
            "/api/canvas/layout",
            get(health::canvas_layout_get_handler).post(health::canvas_layout_save_handler),
        )
        // World API routes
        .route(
            "/api/world/projects",
            get(world::world_list_projects).post(world::world_add_project),
        )
        .route(
            "/api/world/projects/reorder",
            post(world::world_reorder_projects),
        )
        .route(
            "/api/world/projects/update",
            post(world::world_update_project),
        )
        .route(
            "/api/world/projects/remove",
            post(world::world_remove_project),
        )
        .route("/api/world/processes", get(world::world_list_processes))
        .route(
            "/api/world/processes/{project_name}/start",
            post(world::world_start_process),
        )
        .route(
            "/api/world/processes/{project_name}/stop",
            post(world::world_stop_process),
        )
        .route(
            "/api/world/processes/{project_name}/pointview",
            post(world::world_open_pointview),
        )
        .route("/api/world/refresh", post(world::world_refresh))
        .route(
            "/api/world/ccwire/sessions",
            get(world::world_ccwire_sessions),
        )
        // HTTP register/unregister: Swift メニューバーアプリの移行完了まで残す（後方互換）
        // SP は QUIC registry チャネルで自己登録するため、これらは外部ツール用
        .route(
            "/api/world/processes/register",
            post(world::world_register_process),
        )
        .route(
            "/api/world/processes/unregister",
            post(world::world_unregister_process),
        )
        // Update API routes (vp CLI)
        .route("/api/update/check", get(update::update_check))
        .route("/api/update/apply", post(update::update_apply))
        .route("/api/update/rollback", post(update::update_rollback))
        .route("/api/update/restart", post(update::update_restart))
        // Update API routes (VantagePoint.app)
        .route("/api/update/mac/check", get(update::update_mac_check))
        .route("/api/update/mac/apply", post(update::update_mac_apply))
        .route(
            "/api/update/mac/rollback",
            post(update::update_mac_rollback),
        )
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr: SocketAddr = format!("[::1]:{}", port).parse()?;
    tracing::info!("{} 起動 http://{}", crate::stands::WORLD.display(), addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // ポートバインド成功後に PID ファイルを書き出す
    // （バインド前に書くと、失敗時に既存デーモンの PID が上書きされ制御不能になる）
    process::write_pid_file()?;

    // Clone for shutdown
    let world_for_shutdown = world_cap.clone();

    // Daemon QUIC サーバー起動（PTY セッション管理 + Registry チャネル、同一ポートで UDP/QUIC）
    // ProcessManagerCapability の running_processes を DaemonState と共有
    let running_processes_ref = world_cap.read().await.running_processes_ref();
    let projects_ref = world_cap.read().await.projects_ref();
    let daemon_state = std::sync::Arc::new(
        crate::daemon::server::DaemonState::new()
            .with_running_processes(running_processes_ref, projects_ref),
    );
    let daemon_handle = tokio::spawn(crate::daemon::server::start_daemon_server(
        daemon_state,
        port,
    ));
    tracing::info!(
        "Daemon QUIC サーバー統合起動 (port: {}, registry チャネル有効)",
        port
    );

    // ヘルスモニター起動（30秒間隔で Process 監視 + ゴースト除去 + クラッシュ復旧）
    let health_monitor = tokio::spawn(ProcessManagerCapability::run_health_monitor(
        world_cap.clone(),
        shutdown_token.clone(),
    ));

    // LIVE SELECT → 通知ブリッジ（VP-21 Phase 4）
    // processes テーブルの変更を検知して DistributedNotification に変換
    // DB 切断でストリームが終了した場合は再接続ループで自律復帰する
    if let Some(db) = vpdb.clone() {
        let shutdown = shutdown_token.clone();
        tokio::spawn(async move {
            use futures::StreamExt;
            tracing::info!("LIVE SELECT processes ブリッジ起動");
            // 再接続ループ: ストリームが切断されたら 5秒待って再サブスクライブ
            'reconnect: loop {
                if shutdown.is_cancelled() {
                    break 'reconnect;
                }

                let stream = match db.live_processes().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("LIVE SELECT 起動失敗（5秒後に再試行）: {}", e);
                        tokio::select! {
                            _ = shutdown.cancelled() => break 'reconnect,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                        }
                        continue 'reconnect;
                    }
                };

                let mut stream = std::pin::pin!(stream);
                let mut error_count: u32 = 0;
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => {
                            tracing::info!("LIVE SELECT ブリッジ: shutdown");
                            break 'reconnect;
                        }
                        item = stream.next() => {
                            match item {
                                Some(Ok(notification)) => {
                                    error_count = 0; // 成功時にリセット
                                    let action = notification.action;
                                    let data = &notification.data;
                                    let port_val = data["port"].as_u64().unwrap_or(0) as u16;
                                    let project_name = data["project_name"]
                                        .as_str()
                                        .unwrap_or("unknown");

                                    let event = match action {
                                        surrealdb::types::Action::Create => "started",
                                        surrealdb::types::Action::Update => "updated",
                                        surrealdb::types::Action::Delete => "stopped",
                                        _ => "changed",
                                    };

                                    tracing::info!(
                                        "LIVE SELECT: {} '{}' (port={})",
                                        event,
                                        project_name,
                                        port_val
                                    );

                                    if port_val > 0 {
                                        crate::notify::post_process_changed(port_val, event);
                                    }
                                }
                                Some(Err(e)) => {
                                    error_count += 1;
                                    tracing::warn!("LIVE SELECT エラー ({}/5): {}", error_count, e);
                                    // 連続 5 回エラーで再接続ループに移行
                                    if error_count >= 5 {
                                        tracing::warn!("LIVE SELECT 連続エラー → 再接続...");
                                        tokio::select! {
                                            _ = shutdown.cancelled() => break 'reconnect,
                                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                                        }
                                        continue 'reconnect;
                                    }
                                }
                                None => {
                                    // ストリーム終了（DB 再起動など）— 再接続を試みる
                                    tracing::warn!(
                                        "LIVE SELECT ストリーム切断、5秒後に再接続..."
                                    );
                                    tokio::select! {
                                        _ = shutdown.cancelled() => break 'reconnect,
                                        _ = tokio::time::sleep(
                                            std::time::Duration::from_secs(5)
                                        ) => {}
                                    }
                                    continue 'reconnect;
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    // シグナルハンドラ: SIGTERM でグレースフルシャットダウン
    let shutdown_for_signal = shutdown_token.clone();
    tokio::spawn(async move {
        let sigterm_result =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT 受信、シャットダウン開始");
            }
            _ = async {
                match sigterm_result {
                    Ok(mut sigterm) => { sigterm.recv().await; }
                    Err(e) => {
                        tracing::warn!("SIGTERM ハンドラ登録失敗: {}, SIGINT のみで停止", e);
                        std::future::pending::<()>().await;
                    }
                }
            } => {
                tracing::info!("SIGTERM 受信、シャットダウン開始");
            }
        }
        shutdown_for_signal.cancel();
    });

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("World graceful shutdown initiated");
        })
        .await?;

    // クリーンアップ
    health_monitor.abort();
    daemon_handle.abort();

    // Shutdown capabilities
    tracing::info!("Shutting down World...");
    if let Err(e) = world_for_shutdown.write().await.shutdown().await {
        tracing::warn!("Error during world shutdown: {}", e);
    }
    {
        let mut update = update_cap.write().await;
        if let Err(e) = update.shutdown().await {
            tracing::warn!("Error during update shutdown: {}", e);
        }
    }

    // SurrealDB は独立デーモンなので TheWorld 終了時には止めない
    // 再起動が必要な場合は `vp db restart` を使用

    // PID ファイル削除
    process::remove_pid_file();
    tracing::info!("World stopped");
    Ok(())
}
