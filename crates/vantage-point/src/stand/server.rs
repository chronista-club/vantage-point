//! HTTP server with WebSocket support
//!
//! Stand サーバーのエントリーポイント。`run()` と `run_conductor()` でサーバーを起動する。
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

use super::capabilities::{CapabilityConfig, StandCapabilities};
use super::hub::Hub;
use super::pty::PtyManager;
use super::routes::{conductor, health, permission, prompt, update, ws};
use super::session::SessionManager;
use super::state::AppState;
use super::tmux::TmuxManager;
use super::unison_server;
use crate::capability::{StandManagerCapability, UpdateCapability};
use crate::config::RunningStands;
use crate::protocol::DebugMode;

/// Run the Stand server
pub async fn run(
    port: u16,
    auto_open_browser: bool,
    debug_mode: DebugMode,
    cap_config: CapabilityConfig,
) -> Result<()> {
    let project_dir = cap_config.project_dir.clone();

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
    let capabilities = Arc::new(StandCapabilities::new(cap_config).await);

    // Initialize all capabilities
    if let Err(e) = capabilities.initialize().await {
        tracing::warn!("Failed to initialize capabilities: {}", e);
    }

    let hub = Hub::new();

    // Start event bridge: EventBus -> Hub
    let _event_bridge = capabilities.start_event_bridge(hub.sender());
    tracing::info!("Capability event bridge started");

    // tmux利用可能チェック
    let use_tmux = TmuxManager::is_available().await;
    if use_tmux {
        tracing::info!("tmux が利用可能 → tmux モードで起動");
    } else {
        tracing::info!("tmux が見つからない → portable-pty フォールバック");
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
        conductor: None, // Conductor mode is set via run_conductor()
        update: None,    // Update capability is only for conductor mode
        interactive_agent: Arc::new(RwLock::new(None)), // Interactive agent (stream-json mode)
        pty_manager: Arc::new(tokio::sync::Mutex::new(PtyManager::new())),
        tmux_manager: Arc::new(tokio::sync::Mutex::new(TmuxManager::new())),
        use_tmux,
        canvas_pid: Arc::new(tokio::sync::Mutex::new(None)),
        port,
    });

    let app = Router::new()
        .route("/", get(health::index_handler))
        .route("/ws", get(ws::ws_handler))
        .route("/api/show", post(health::show_handler))
        .route("/api/toggle-pane", post(health::toggle_pane_handler))
        .route("/api/split-pane", post(health::split_pane_handler))
        .route("/api/close-pane", post(health::close_pane_handler))
        .route("/api/canvas/open", post(health::canvas_open_handler))
        .route("/api/canvas/close", post(health::canvas_close_handler))
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
            "/api/conductor/stands",
            get(conductor::conductor_list_stands),
        )
        .route(
            "/api/conductor/stands/{project_name}/start",
            post(conductor::conductor_start_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/stop",
            post(conductor::conductor_stop_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/pointview",
            post(conductor::conductor_open_pointview),
        )
        .route("/api/conductor/refresh", post(conductor::conductor_refresh))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
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

    // Unison QUIC サーバーを並行起動
    let quic_port = port + unison_server::QUIC_PORT_OFFSET;
    {
        let state_for_quic = state.clone();
        tokio::spawn(async move {
            unison_server::start_unison_server(state_for_quic, port).await;
        });
    }

    // Register this Stand in running.json
    let pid = std::process::id();
    if let Err(e) = RunningStands::register(port, &state.project_dir, pid, Some(quic_port)) {
        tracing::warn!("Failed to register Stand in running.json: {}", e);
    } else {
        tracing::info!(
            "Registered Stand in running.json (port={}, quic={}, pid={})",
            port,
            quic_port,
            pid
        );
    }

    // Clone capabilities for shutdown
    let capabilities_for_shutdown = state.capabilities.clone();

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("Graceful shutdown initiated");
        })
        .await?;

    // Unregister from running.json
    if let Err(e) = RunningStands::unregister_by_port(port) {
        tracing::warn!("Failed to unregister Stand from running.json: {}", e);
    } else {
        tracing::info!("Unregistered Stand from running.json (port={})", port);
    }

    // Shutdown all capabilities
    tracing::info!("Shutting down capabilities...");
    if let Err(e) = capabilities_for_shutdown.shutdown().await {
        tracing::warn!("Error during capability shutdown: {}", e);
    }

    tracing::info!("Server stopped");
    Ok(())
}

/// Conductorモードでスタンドサーバーを起動
/// 複数のProject Standを管理するための専用モード
pub async fn run_conductor(port: u16) -> Result<()> {
    use crate::capability::core::{Capability, CapabilityContext};

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Initialize Conductor Capability
    let mut conductor_cap = StandManagerCapability::new();
    let ctx = CapabilityContext::new();

    if let Err(e) = conductor_cap.initialize(&ctx).await {
        tracing::error!("Failed to initialize StandManagerCapability: {}", e);
        return Err(anyhow::anyhow!(
            "StandManagerCapability initialization failed: {}",
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
            StandCapabilities::new(CapabilityConfig {
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
        tmux_manager: Arc::new(tokio::sync::Mutex::new(TmuxManager::new())),
        use_tmux: false, // Conductor モードでは tmux 不要
        canvas_pid: Arc::new(tokio::sync::Mutex::new(None)),
        port,
    });

    let app = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/api/shutdown", post(health::shutdown_handler))
        // Conductor API routes
        .route(
            "/api/conductor/projects",
            get(conductor::conductor_list_projects),
        )
        .route(
            "/api/conductor/stands",
            get(conductor::conductor_list_stands),
        )
        .route(
            "/api/conductor/stands/{project_name}/start",
            post(conductor::conductor_start_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/stop",
            post(conductor::conductor_stop_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/pointview",
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

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Starting Conductor Stand on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Clone conductor for shutdown
    let conductor_for_shutdown = conductor_cap.clone();

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("Conductor graceful shutdown initiated");
        })
        .await?;

    // Shutdown conductor capability
    tracing::info!("Shutting down Conductor...");
    if let Err(e) = conductor_for_shutdown.write().await.shutdown().await {
        tracing::warn!("Error during conductor shutdown: {}", e);
    }

    tracing::info!("Conductor stopped");
    Ok(())
}
