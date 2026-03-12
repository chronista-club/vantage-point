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

    // Start event bridge: EventBus -> Hub
    let _event_bridge = capabilities.start_event_bridge(hub.sender());
    tracing::info!("Capability event bridge started");

    // Terminal チャネル認証トークンを生成
    let terminal_token = crate::discovery::generate_terminal_token();

    // tmux Actor 起動（tmux 環境下でのみ有効）
    let project_name = crate::resolve::project_name_from_path(
        &project_dir,
        &crate::config::Config::load().unwrap_or_default(),
    )
    .to_string();
    let tmux_handle = super::tmux_actor::spawn(&crate::tmux::session_name(&project_name));

    // TopicRouter 初期化 + Hub → TopicRouter ブリッジ
    let topic_router = Arc::new(TopicRouter::new());
    {
        let router_clone = topic_router.clone();
        let mut hub_rx = hub.subscribe();
        tokio::spawn(async move {
            loop {
                match hub_rx.recv().await {
                    Ok(msg) => router_clone.route(msg).await,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("TopicRouter lagged: {} messages dropped", n);
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
        canvas_pid: Arc::new(tokio::sync::Mutex::new(None)),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: terminal_token.clone(),
        tmux: tmux_handle,
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
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
        .route("/api/canvas/open", post(health::canvas_open_handler))
        .route("/api/canvas/close", post(health::canvas_close_handler))
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
        .route("/api/world/projects", get(world::world_list_projects))
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
            "/api/world/processes/register",
            post(world::world_register_process),
        )
        .route(
            "/api/world/processes/unregister",
            post(world::world_unregister_process),
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

    // TheWorld に Process を登録（ベストエフォート — 失敗してもスキャンで発見される）
    let pid = std::process::id();
    crate::discovery::register(port, &state.project_dir, pid, &terminal_token).await;

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

    // TheWorld から登録解除
    crate::discovery::unregister(port).await;

    // ペイン状態をディスクに保存（次回起動時に復元、RetainedStore から取得）
    state_for_shutdown.persist_pane_contents().await;

    // メニューバーアプリに停止を通知
    crate::notify::post_process_changed(port, "stopped");

    // ファイル監視を全停止
    file_watchers_for_shutdown.lock().await.stop_all();

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

    // PID ファイル書き出し（Daemon 統合）
    process::write_pid_file()?;

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
                bonjour_port: None,
            })
            .await,
        ),
        world: Some(world_cap.clone()),
        update: Some(update_cap.clone()),
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        canvas_pid: Arc::new(tokio::sync::Mutex::new(None)),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "WORLD_DISABLED".to_string(),
        tmux: None,
        process_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::process_runner::ProcessRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        topic_router,
        canvas_senders: Arc::new(tokio::sync::Mutex::new(Vec::new())),
    });

    let app = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
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
        .route("/api/world/projects", get(world::world_list_projects))
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

    // Clone world for shutdown
    let world_for_shutdown = world_cap.clone();

    // Daemon QUIC サーバー起動（PTY セッション管理、同一ポートで UDP/QUIC）
    let daemon_state = std::sync::Arc::new(crate::daemon::server::DaemonState::new());
    let daemon_handle = tokio::spawn(crate::daemon::server::start_daemon_server(
        daemon_state,
        port,
    ));
    tracing::info!("Daemon QUIC サーバー統合起動 (port: {})", port);

    // ヘルスモニター起動（30秒間隔で Process 監視 + ゴースト除去 + クラッシュ復旧）
    let health_monitor = tokio::spawn(ProcessManagerCapability::run_health_monitor(
        world_cap.clone(),
        shutdown_token.clone(),
    ));

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

    // Shutdown world capability
    tracing::info!("Shutting down World...");
    if let Err(e) = world_for_shutdown.write().await.shutdown().await {
        tracing::warn!("Error during world shutdown: {}", e);
    }

    // PID ファイル削除
    process::remove_pid_file();
    tracing::info!("World stopped");
    Ok(())
}
