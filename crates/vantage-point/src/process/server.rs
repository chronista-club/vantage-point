//! HTTP server with WebSocket support
//!
//! Process サーバーのエントリーポイント。`run()` と `run_conductor()` でサーバーを起動する。
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
use super::routes::{conductor, health, lanes, permission, prompt, update, ws};
use super::session::SessionManager;
use super::state::AppState;
use super::unison_server;
use crate::capability::{ProcessManagerCapability, UpdateCapability};
use crate::config::RunningProcesses;
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
    let terminal_token = crate::config::RunningProcesses::generate_terminal_token();

    // tmux Actor 起動（tmux 環境下でのみ有効）
    let project_name = crate::resolve::project_name_from_path(
        &project_dir,
        &crate::config::Config::load().unwrap_or_default(),
    )
    .to_string();
    let tmux_handle = super::tmux_actor::spawn(&crate::tmux::session_name(&project_name));

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
        conductor: None,
        update: None,
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        canvas_pid: Arc::new(tokio::sync::Mutex::new(None)),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: terminal_token.clone(),
        tmux: tmux_handle,
        ruby_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::ruby_vm::RubyRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        pane_contents: Arc::new(std::sync::Mutex::new(HashMap::new())),
    });

    // ペイン状態をディスクから復元（前回 Process 終了時の状態）
    state.restore_pane_contents();

    let app = Router::new()
        .route("/", get(health::index_handler))
        .route("/canvas", get(health::canvas_handler))
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
        .route("/api/canvas/capture", post(health::canvas_capture_handler))
        .route(
            "/api/canvas/layout",
            get(health::canvas_layout_get_handler).post(health::canvas_layout_save_handler),
        )
        .route("/api/ruby/eval", post(health::ruby_eval_handler))
        .route("/api/ruby/run", post(health::ruby_run_handler))
        .route("/api/ruby/stop", post(health::ruby_stop_handler))
        .route("/api/ruby/list", get(health::ruby_list_handler))
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
        // Conductor API routes
        .route(
            "/api/conductor/projects",
            get(conductor::conductor_list_projects),
        )
        .route(
            "/api/conductor/processes",
            get(conductor::conductor_list_processes),
        )
        .route(
            "/api/conductor/processes/{project_name}/start",
            post(conductor::conductor_start_process),
        )
        .route(
            "/api/conductor/processes/{project_name}/stop",
            post(conductor::conductor_stop_process),
        )
        .route(
            "/api/conductor/processes/{project_name}/pointview",
            post(conductor::conductor_open_pointview),
        )
        .route("/api/conductor/refresh", post(conductor::conductor_refresh))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let addr: SocketAddr = format!("[::1]:{}", port).parse().unwrap();
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

    // running.json の pid と quic_port を更新
    // （start.rs で仮登録済み。ここでサーバー起動後の正確な情報に更新する）
    let pid = std::process::id();
    if let Err(e) = RunningProcesses::update_pid_and_quic(port, pid, quic_port, &terminal_token) {
        tracing::warn!("Failed to update Process in running.json: {}", e);
    } else {
        tracing::info!(
            "Updated Process in running.json (port={}, quic={}, pid={})",
            port,
            quic_port,
            pid
        );
    }

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

    // Unregister from running.json
    if let Err(e) = RunningProcesses::unregister_by_port(port) {
        tracing::warn!("Failed to unregister Process from running.json: {}", e);
    } else {
        tracing::info!("Unregistered Process from running.json (port={})", port);
    }

    // ペイン状態をディスクに保存（次回起動時に復元）
    state_for_shutdown.persist_pane_contents();

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

/// ConductorモードでProcessサーバーを起動
/// 複数のProject Processを管理するための専用モード
pub async fn run_conductor(port: u16) -> Result<()> {
    use crate::capability::core::{Capability, CapabilityContext};

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Initialize Conductor Capability
    let mut conductor_cap = ProcessManagerCapability::new();
    let ctx = CapabilityContext::new();

    if let Err(e) = conductor_cap.initialize(&ctx).await {
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

    let conductor_cap = Arc::new(RwLock::new(conductor_cap));
    let update_cap = Arc::new(RwLock::new(update_cap));
    let hub = Hub::new();

    // Create minimal state for conductor mode
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
        conductor: Some(conductor_cap.clone()),
        update: Some(update_cap.clone()),
        interactive_agent: Arc::new(RwLock::new(None)),
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        canvas_pid: Arc::new(tokio::sync::Mutex::new(None)),
        port,
        file_watchers: Arc::new(tokio::sync::Mutex::new(FileWatcherManager::new())),
        terminal_token: "CONDUCTOR_DISABLED".to_string(),
        tmux: None,
        ruby_registry: Arc::new(tokio::sync::Mutex::new(
            crate::process::ruby_vm::RubyRegistry::new(),
        )),
        screenshot_waiters: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        pane_contents: Arc::new(std::sync::Mutex::new(HashMap::new())),
    });

    let app = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        // Canvas Lane 集約 WebSocket
        .route("/ws/lanes", get(lanes::lanes_ws_handler))
        // Conductor API routes
        .route(
            "/api/conductor/projects",
            get(conductor::conductor_list_projects),
        )
        .route(
            "/api/conductor/processes",
            get(conductor::conductor_list_processes),
        )
        .route(
            "/api/conductor/processes/{project_name}/start",
            post(conductor::conductor_start_process),
        )
        .route(
            "/api/conductor/processes/{project_name}/stop",
            post(conductor::conductor_stop_process),
        )
        .route(
            "/api/conductor/processes/{project_name}/pointview",
            post(conductor::conductor_open_pointview),
        )
        .route("/api/conductor/refresh", post(conductor::conductor_refresh))
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

    let addr: SocketAddr = format!("[::1]:{}", port).parse().unwrap();
    tracing::info!("Starting Conductor Process on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Clone conductor for shutdown
    let conductor_for_shutdown = conductor_cap.clone();

    // ヘルスモニター起動（30秒間隔で Process 監視 + ゴースト除去 + クラッシュ復旧）
    let health_monitor = tokio::spawn(ProcessManagerCapability::run_health_monitor(
        conductor_cap.clone(),
        shutdown_token.clone(),
    ));

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("Conductor graceful shutdown initiated");
        })
        .await?;

    // ヘルスモニター停止
    health_monitor.abort();

    // Shutdown conductor capability
    tracing::info!("Shutting down Conductor...");
    if let Err(e) = conductor_for_shutdown.write().await.shutdown().await {
        tracing::warn!("Error during conductor shutdown: {}", e);
    }

    tracing::info!("Conductor stopped");
    Ok(())
}
