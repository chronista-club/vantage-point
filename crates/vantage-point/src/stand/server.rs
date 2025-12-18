//! HTTP server with WebSocket support

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

use super::capabilities::{CapabilityConfig, StandCapabilities};
use super::hub::Hub;
use crate::agent::{AgentConfig, AgentEvent, ClaudeAgent};
use crate::capability::{ConductorCapability, ProjectInfo, RunningStand, StandStatus, UpdateCapability, UpdateCheckResult};
use crate::agui::{AgUiEvent, MessageRole};
use crate::mcp::PermissionResponse;
use crate::protocol::{
    BrowserMessage, ChatComponent, ChatMessage, ChatRole, ComponentAction, StandMessage,
    DebugMode, HistoryMessage, SessionInfo,
};
use std::collections::HashMap;

/// Chat message for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMessage {
    role: String,
    content: String,
    timestamp: u64,
}

/// Session entry with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionEntry {
    id: String,
    name: String,
    message_count: usize,
    model: Option<String>,
    /// Session creation timestamp (Unix millis)
    #[serde(default = "default_created_at")]
    created_at: u64,
    /// Chat history for this session
    #[serde(default)]
    messages: Vec<StoredMessage>,
}

fn default_created_at() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Persisted state for hot reload
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    /// Active session ID
    active_id: Option<String>,
    /// All known sessions
    sessions: HashMap<String, SessionEntry>,
    /// Counter for generating session names
    session_counter: usize,
    /// Project directory
    project_dir: String,
}

/// Session manager for multiple Claude sessions
#[derive(Debug, Default)]
struct SessionManager {
    /// Active session ID
    active_id: Option<String>,
    /// All known sessions: session_id -> SessionEntry
    sessions: HashMap<String, SessionEntry>,
    /// Counter for generating session names
    session_counter: usize,
    /// Port number for state file path
    port: u16,
    /// Project directory
    project_dir: String,
}

impl SessionManager {
    fn new() -> Self {
        Self::default()
    }

    /// Create with port and project_dir, attempting to restore from saved state
    fn with_config(port: u16, project_dir: String) -> Self {
        let state_path = Self::state_path(port);

        // Try to load existing state
        if let Ok(data) = std::fs::read_to_string(&state_path) {
            if let Ok(state) = serde_json::from_str::<PersistedState>(&data) {
                // Only restore if same project directory
                if state.project_dir == project_dir {
                    tracing::info!("Restored session state from {:?}", state_path);
                    return Self {
                        active_id: state.active_id,
                        sessions: state.sessions,
                        session_counter: state.session_counter,
                        port,
                        project_dir,
                    };
                } else {
                    tracing::info!("Project dir changed, starting fresh session");
                }
            }
        }

        Self {
            port,
            project_dir,
            ..Default::default()
        }
    }

    /// Get state file path for a port
    fn state_path(port: u16) -> PathBuf {
        crate::config::config_dir()
            .join("state")
            .join(format!("{}.json", port))
    }

    /// Save state to file
    fn save(&self) {
        let state = PersistedState {
            active_id: self.active_id.clone(),
            sessions: self.sessions.clone(),
            session_counter: self.session_counter,
            project_dir: self.project_dir.clone(),
        };

        let state_path = Self::state_path(self.port);

        // Ensure directory exists
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match serde_json::to_string_pretty(&state) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&state_path, json) {
                    tracing::warn!("Failed to save session state: {}", e);
                } else {
                    tracing::debug!("Saved session state to {:?}", state_path);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize session state: {}", e);
            }
        }
    }

    /// Get or create active session for chat
    /// Returns (session_id, is_continue) where is_continue=true means use --continue
    fn get_active_session(&self) -> (Option<String>, bool) {
        if let Some(ref id) = self.active_id {
            (Some(id.clone()), false) // Explicit --resume <id>
        } else {
            // No active session - use --continue for most recent
            (None, true)
        }
    }

    /// Register a session from Claude CLI init event
    fn register_session(&mut self, id: String, model: Option<String>) {
        if !self.sessions.contains_key(&id) {
            self.session_counter += 1;
            let name = format!("Session {}", self.session_counter);
            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            self.sessions.insert(
                id.clone(),
                SessionEntry {
                    id: id.clone(),
                    name,
                    message_count: 0,
                    model,
                    created_at,
                    messages: Vec::new(),
                },
            );
        }
        self.active_id = Some(id);
        self.save();
    }

    /// Add a message to the active session
    fn add_message(&mut self, role: &str, content: String) {
        if let Some(ref id) = self.active_id {
            if let Some(entry) = self.sessions.get_mut(id) {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                entry.messages.push(StoredMessage {
                    role: role.to_string(),
                    content,
                    timestamp,
                });
                self.save();
            }
        }
    }

    /// Get messages for a session
    fn get_messages(&self, session_id: &str) -> Vec<StoredMessage> {
        self.sessions
            .get(session_id)
            .map(|e| e.messages.clone())
            .unwrap_or_default()
    }

    /// Increment message count for active session
    fn increment_message_count(&mut self) {
        if let Some(ref id) = self.active_id
            && let Some(entry) = self.sessions.get_mut(id) {
                entry.message_count += 1;
                self.save();
            }
    }

    /// Switch to a different session
    fn switch_to(&mut self, session_id: &str) -> Option<&SessionEntry> {
        if self.sessions.contains_key(session_id) {
            self.active_id = Some(session_id.to_string());
            self.save();
            self.sessions.get(session_id)
        } else {
            None
        }
    }

    /// Create a new session (will be registered when Claude CLI responds)
    fn prepare_new_session(&mut self) {
        self.active_id = None;
        self.save();
    }

    /// Rename a session
    fn rename(&mut self, session_id: &str, new_name: String) -> bool {
        if let Some(entry) = self.sessions.get_mut(session_id) {
            entry.name = new_name;
            self.save();
            true
        } else {
            false
        }
    }

    /// Close/remove a session
    fn close(&mut self, session_id: &str) -> bool {
        if self.sessions.remove(session_id).is_some() {
            if self.active_id.as_deref() == Some(session_id) {
                // Switch to another session or none
                self.active_id = self.sessions.keys().next().cloned();
            }
            self.save();
            true
        } else {
            false
        }
    }

    /// Get all sessions as SessionInfo for UI
    fn list(&self) -> Vec<SessionInfo> {
        let mut sessions: Vec<_> = self
            .sessions
            .values()
            .map(|e| SessionInfo {
                id: e.id.clone(),
                name: e.name.clone(),
                is_active: self.active_id.as_deref() == Some(&e.id),
                message_count: e.message_count,
                model: e.model.clone(),
                created_at: e.created_at,
            })
            .collect();
        // Sort by created_at descending (newest first)
        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        sessions
    }
}

/// Pending permission request entry
struct PendingPermission {
    /// Response once user has responded (None = still waiting)
    response: Option<PermissionResponse>,
}

/// Application state
struct AppState {
    hub: Hub,
    /// Session manager for multiple Claude sessions
    sessions: Arc<RwLock<SessionManager>>,
    /// Cancellation token for current chat request
    cancel_token: Arc<RwLock<CancellationToken>>,
    /// Debug display mode
    debug_mode: DebugMode,
    /// Shutdown signal token
    shutdown_token: CancellationToken,
    /// Project directory for Claude agent
    project_dir: String,
    /// Pending permission requests: request_id -> response channel
    pending_permissions: Arc<RwLock<HashMap<String, PendingPermission>>>,
    /// Capability system (Agent, MIDI, Protocol)
    capabilities: Arc<StandCapabilities>,
    /// Conductor capability for managing multiple stands (optional, only for conductor mode)
    conductor: Option<Arc<RwLock<ConductorCapability>>>,
    /// Update capability for version checking (optional, only for conductor mode)
    update: Option<Arc<RwLock<UpdateCapability>>>,
}

impl AppState {
    /// Send debug info to connected clients
    fn send_debug(&self, category: &str, message: &str, data: Option<serde_json::Value>) {
        if self.debug_mode == DebugMode::None {
            return;
        }

        // For simple mode, skip detail-level messages
        if self.debug_mode == DebugMode::Simple && data.is_some() {
            // Still send but without detailed data
            self.hub.broadcast(StandMessage::DebugInfo {
                level: DebugMode::Simple,
                category: category.to_string(),
                message: message.to_string(),
                data: None,
            });
        } else {
            self.hub.broadcast(StandMessage::DebugInfo {
                level: self.debug_mode,
                category: category.to_string(),
                message: message.to_string(),
                data,
            });
        }
    }

    /// Send debug info only in detail mode
    fn send_debug_detail(&self, category: &str, message: &str, data: serde_json::Value) {
        if self.debug_mode == DebugMode::Detail {
            self.hub.broadcast(StandMessage::DebugInfo {
                level: DebugMode::Detail,
                category: category.to_string(),
                message: message.to_string(),
                data: Some(data),
            });
        }
    }

    /// Send AG-UI event to connected clients (REQ-AGUI-040)
    pub fn send_agui_event(&self, event: AgUiEvent) {
        self.hub.broadcast(StandMessage::AgUi { event });
    }
}

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
        sessions.sessions.len()
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

    let state = Arc::new(AppState {
        hub,
        sessions: Arc::new(RwLock::new(sessions)),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode,
        shutdown_token: shutdown_token.clone(),
        project_dir,
        pending_permissions: Arc::new(RwLock::new(HashMap::new())),
        capabilities,
        conductor: None, // Conductor mode is set via run_conductor()
        update: None,    // Update capability is only for conductor mode
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/api/show", post(show_handler))
        .route("/api/health", get(health_handler))
        .route("/api/shutdown", post(shutdown_handler))
        .route("/api/permission", post(permission_request_handler))
        .route("/api/permission/{request_id}", get(permission_poll_handler))
        // Conductor API routes
        .route("/api/conductor/projects", get(conductor_list_projects))
        .route("/api/conductor/stands", get(conductor_list_stands))
        .route("/api/conductor/stands/{project_name}/start", post(conductor_start_stand))
        .route("/api/conductor/stands/{project_name}/stop", post(conductor_stop_stand))
        .route("/api/conductor/stands/{project_name}/pointview", post(conductor_open_pointview))
        .route("/api/conductor/refresh", post(conductor_refresh))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("Starting vp on http://{}", addr);

    // Auto-open browser
    if auto_open_browser {
        let url = format!("http://localhost:{}", port);
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            if let Err(e) = open_browser(&url) {
                tracing::warn!("Failed to open browser: {}", e);
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Clone capabilities for shutdown
    let capabilities_for_shutdown = state.capabilities.clone();

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("Graceful shutdown initiated");
        })
        .await?;

    // Shutdown all capabilities
    tracing::info!("Shutting down capabilities...");
    if let Err(e) = capabilities_for_shutdown.shutdown().await {
        tracing::warn!("Error during capability shutdown: {}", e);
    }

    tracing::info!("Server stopped");
    Ok(())
}

/// Open browser (macOS)
fn open_browser(url: &str) -> Result<()> {
    std::process::Command::new("open").arg(url).spawn()?;
    Ok(())
}

/// Index page handler
async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../../../../web/index.html"))
}

/// Health check response
#[derive(serde::Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub pid: u32,
    pub project_dir: String,
}

async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        project_dir: state.project_dir.clone(),
    })
}

/// POST /api/show - Show content in browser
async fn show_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    state.hub.broadcast(msg);
    Json(serde_json::json!({"status": "ok"}))
}

/// POST /api/shutdown - Graceful shutdown
async fn shutdown_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    tracing::info!("Shutdown requested via API");
    state.shutdown_token.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}

/// POST /api/permission - Receive permission request from MCP tool
async fn permission_request_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    // Extract request_id from the ChatComponent
    let request_id = match &msg {
        StandMessage::ChatComponent {
            component: ChatComponent::PermissionRequest { request_id, .. },
            ..
        } => request_id.clone(),
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Expected ChatComponent with PermissionRequest"})),
            );
        }
    };

    tracing::info!("Permission request received: {}", request_id);

    // Store the pending request (no response yet)
    state.pending_permissions.write().await.insert(
        request_id.clone(),
        PendingPermission { response: None },
    );

    // Broadcast to WebSocket clients
    state.hub.broadcast(msg);

    state.send_debug(
        "permission",
        &format!("Permission request: {}", request_id),
        None,
    );

    // Return accepted and let MCP poll for response
    (
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({"status": "pending", "request_id": request_id})),
    )
}

/// GET /api/permission/{request_id} - Poll for permission response
async fn permission_poll_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let mut pending = state.pending_permissions.write().await;

    if let Some(entry) = pending.get(&request_id) {
        if let Some(ref response) = entry.response {
            // Response is ready - return it and remove from pending
            let response_clone = response.clone();
            pending.remove(&request_id);
            return (
                axum::http::StatusCode::OK,
                Json(serde_json::to_value(&response_clone).unwrap_or_default()),
            );
        } else {
            // Still waiting for user response
            return (
                axum::http::StatusCode::ACCEPTED,
                Json(serde_json::json!({"status": "pending"})),
            );
        }
    }

    // Request not found
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "Request not found"})),
    )
}

/// Handle a permission response from WebSocket (called from WebSocket handler)
async fn handle_permission_response(
    state: &Arc<AppState>,
    request_id: String,
    approved: bool,
    updated_input: Option<serde_json::Value>,
    message: Option<String>,
) {
    let mut pending = state.pending_permissions.write().await;

    if let Some(entry) = pending.get_mut(&request_id) {
        let response = if approved {
            PermissionResponse {
                behavior: "allow".to_string(),
                updated_input,
                message: None,
            }
        } else {
            PermissionResponse {
                behavior: "deny".to_string(),
                updated_input: None,
                message,
            }
        };

        // Store the response (will be retrieved by next poll)
        entry.response = Some(response);

        tracing::info!("Permission {} -> {}", request_id, if approved { "allow" } else { "deny" });

        // Broadcast component dismissed
        state.hub.broadcast(StandMessage::ComponentDismissed {
            request_id: request_id.clone(),
        });
    } else {
        tracing::warn!(
            "Permission response for unknown request: {}",
            request_id
        );
    }
}

/// WebSocket handler
async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle WebSocket connection
async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    state.hub.client_connected().await;

    // Send debug mode info on connection
    if state.debug_mode != DebugMode::None {
        let mode_msg = StandMessage::DebugModeChanged {
            mode: state.debug_mode,
        };
        let text = serde_json::to_string(&mode_msg).unwrap_or_default();
        let _ = sender.send(Message::Text(text.into())).await;
    }

    // Subscribe to broadcast messages
    let mut rx = state.hub.subscribe();

    // Task: Send broadcast messages to this client
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            let text = serde_json::to_string(&msg).unwrap_or_default();
            if sender.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // Clone state for chat handling
    let hub = state.hub.clone();
    let sessions = state.sessions.clone();
    let cancel_token = state.cancel_token.clone();
    let debug_mode = state.debug_mode;

    // Task: Receive messages from client
    let state_clone = state.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(browser_msg) = serde_json::from_str::<BrowserMessage>(&text) {
                        match browser_msg {
                            BrowserMessage::Ready => {
                                tracing::info!("Browser ready");
                                state_clone.send_debug("connection", "Browser connected", None);

                                // Send current session list on connect
                                let mgr = sessions.read().await;
                                hub.broadcast(StandMessage::SessionList {
                                    sessions: mgr.list(),
                                    active_id: mgr.active_id.clone(),
                                });
                            }
                            BrowserMessage::Pong => {
                                // Keepalive response
                            }
                            BrowserMessage::Action { pane_id, action } => {
                                tracing::info!("Action from pane {}: {}", pane_id, action);
                            }
                            BrowserMessage::Chat { message } => {
                                tracing::info!("Chat message received: {}", message);

                                // Create new cancellation token for this request
                                let new_token = CancellationToken::new();
                                *cancel_token.write().await = new_token.clone();

                                handle_chat_message(
                                    &hub,
                                    &sessions,
                                    &new_token,
                                    debug_mode,
                                    &state_clone.project_dir,
                                    message,
                                )
                                .await;
                            }
                            BrowserMessage::CancelChat => {
                                tracing::info!("Cancel chat requested");
                                let token = cancel_token.read().await;
                                token.cancel();
                                state_clone.send_debug("chat", "Request cancelled by user", None);

                                // Send done signal to clear typing indicator
                                hub.broadcast(StandMessage::ChatChunk {
                                    content: String::new(),
                                    done: true,
                                });
                            }
                            BrowserMessage::ResetSession => {
                                tracing::info!("New session requested");
                                sessions.write().await.prepare_new_session();
                                state_clone.send_debug(
                                    "session",
                                    "Starting new session (--continue)",
                                    None,
                                );

                                // Notify browser
                                hub.broadcast(StandMessage::ChatMessage {
                                    message: ChatMessage {
                                        role: ChatRole::System,
                                        content: "New session. Starting fresh conversation."
                                            .to_string(),
                                    },
                                });
                            }
                            BrowserMessage::ListSessions => {
                                let mgr = sessions.read().await;
                                hub.broadcast(StandMessage::SessionList {
                                    sessions: mgr.list(),
                                    active_id: mgr.active_id.clone(),
                                });
                            }
                            BrowserMessage::SwitchSession { session_id } => {
                                tracing::info!("Switch session to: {}", session_id);
                                let mut mgr = sessions.write().await;

                                // Get messages before switching (to avoid borrow issues)
                                let messages: Vec<HistoryMessage> = mgr
                                    .get_messages(&session_id)
                                    .into_iter()
                                    .map(|m| HistoryMessage {
                                        role: m.role,
                                        content: m.content,
                                        timestamp: m.timestamp,
                                    })
                                    .collect();

                                if let Some(entry) = mgr.switch_to(&session_id) {
                                    let name = entry.name.clone();

                                    // Send session switched notification
                                    hub.broadcast(StandMessage::SessionSwitched {
                                        session_id: session_id.clone(),
                                        name,
                                    });

                                    // Send session history for UI restoration
                                    hub.broadcast(StandMessage::SessionHistory {
                                        session_id: session_id.clone(),
                                        messages,
                                    });

                                    state_clone.send_debug(
                                        "session",
                                        &format!("Switched to {}", session_id),
                                        None,
                                    );
                                }
                            }
                            BrowserMessage::NewSession => {
                                tracing::info!("New session requested");
                                sessions.write().await.prepare_new_session();
                                hub.broadcast(StandMessage::ChatMessage {
                                    message: ChatMessage {
                                        role: ChatRole::System,
                                        content: "New session created.".to_string(),
                                    },
                                });
                            }
                            BrowserMessage::RenameSession { session_id, name } => {
                                tracing::info!("Rename session {} to {}", session_id, name);
                                let mut mgr = sessions.write().await;
                                if mgr.rename(&session_id, name.clone()) {
                                    // Send updated list
                                    hub.broadcast(StandMessage::SessionList {
                                        sessions: mgr.list(),
                                        active_id: mgr.active_id.clone(),
                                    });
                                }
                            }
                            BrowserMessage::CloseSession { session_id } => {
                                tracing::info!("Close session {}", session_id);
                                let mut mgr = sessions.write().await;
                                if mgr.close(&session_id) {
                                    hub.broadcast(StandMessage::SessionClosed { session_id });
                                    // Send updated list
                                    hub.broadcast(StandMessage::SessionList {
                                        sessions: mgr.list(),
                                        active_id: mgr.active_id.clone(),
                                    });
                                }
                            }
                            BrowserMessage::ComponentAction { action } => {
                                tracing::info!("Component action: {:?}", action);
                                state_clone.send_debug(
                                    "component",
                                    &format!("Received component action: {:?}", action),
                                    None,
                                );

                                // Handle permission responses
                                match action {
                                    ComponentAction::PermissionApprove {
                                        request_id,
                                        updated_input,
                                    } => {
                                        handle_permission_response(
                                            &state_clone,
                                            request_id,
                                            true,
                                            updated_input,
                                            None,
                                        )
                                        .await;
                                    }
                                    ComponentAction::PermissionDeny { request_id, message } => {
                                        handle_permission_response(
                                            &state_clone,
                                            request_id,
                                            false,
                                            None,
                                            message,
                                        )
                                        .await;
                                    }
                                    // TODO: Handle other component actions
                                    _ => {
                                        tracing::debug!("Unhandled component action: {:?}", action);
                                    }
                                }
                            }
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for either task to complete
    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    state.hub.client_disconnected().await;
}

/// Handle incoming chat message from browser
async fn handle_chat_message(
    hub: &Hub,
    sessions: &Arc<RwLock<SessionManager>>,
    cancel_token: &CancellationToken,
    debug_mode: DebugMode,
    project_dir: &str,
    message: String,
) {
    let start_time = Instant::now();

    // AG-UI: Generate run_id for this chat request (REQ-AGUI-040)
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let message_id = format!("msg-{}", uuid::Uuid::new_v4());

    // AG-UI: Emit RunStarted event
    hub.broadcast(StandMessage::AgUi {
        event: AgUiEvent::run_started(&run_id),
    });

    // Save user message to history
    sessions.write().await.add_message("user", message.clone());

    // Get session info from manager
    let (session_id, use_continue) = sessions.read().await.get_active_session();

    // Create agent config with project directory
    let mut config = AgentConfig {
        working_dir: Some(project_dir.to_string()),
        use_continue,
        ..Default::default()
    };

    // Create agent with session continuity
    if let Some(ref sid) = session_id {
        tracing::info!("Resuming session: {}", sid);
        if debug_mode != DebugMode::None {
            hub.broadcast(StandMessage::DebugInfo {
                level: debug_mode,
                category: "session".to_string(),
                message: format!("Resuming session: {}", sid),
                data: None,
            });
        }
        config.session_id = Some(sid.clone());
    } else if use_continue {
        tracing::info!("Using --continue (most recent session)");
        if debug_mode != DebugMode::None {
            hub.broadcast(StandMessage::DebugInfo {
                level: debug_mode,
                category: "session".to_string(),
                message: "Using --continue (most recent session)".to_string(),
                data: None,
            });
        }
    } else {
        tracing::info!("Starting new session");
        if debug_mode != DebugMode::None {
            hub.broadcast(StandMessage::DebugInfo {
                level: debug_mode,
                category: "session".to_string(),
                message: "Starting new session".to_string(),
                data: None,
            });
        }
    }

    let agent = ClaudeAgent::with_config(config);
    let mut rx = agent.chat(&message).await;

    let hub = hub.clone();
    let sessions = sessions.clone();
    let cancel_token = cancel_token.clone();
    let mut first_chunk = true;
    let mut chunk_count = 0;
    let mut response_buffer = String::new();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                tracing::info!("Chat request cancelled");

                // AG-UI: Emit RunError for cancellation (REQ-AGUI-040)
                hub.broadcast(StandMessage::AgUi {
                    event: AgUiEvent::run_error(&run_id, "CANCELLED", "Request cancelled by user"),
                });

                if debug_mode != DebugMode::None {
                    hub.broadcast(StandMessage::DebugInfo {
                        level: debug_mode,
                        category: "chat".to_string(),
                        message: "Request cancelled".to_string(),
                        data: None,
                    });
                }
                break;
            }
            event = rx.recv() => {
                match event {
                    Some(AgentEvent::SessionInit { session_id, model, tools, mcp_servers }) => {
                        tracing::info!(
                            "Session initialized: {}, model={:?}, tools={}, mcp={}",
                            session_id, model, tools.len(), mcp_servers.len()
                        );

                        // Register session with manager
                        let mut mgr = sessions.write().await;
                        mgr.register_session(session_id.clone(), model.clone());
                        mgr.increment_message_count();

                        // Send updated session list to browser
                        hub.broadcast(StandMessage::SessionList {
                            sessions: mgr.list(),
                            active_id: mgr.active_id.clone(),
                        });
                        drop(mgr);

                        if debug_mode != DebugMode::None {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: debug_mode,
                                category: "session".to_string(),
                                message: format!(
                                    "Session: {} | Model: {} | Tools: {} | MCP: {}",
                                    &session_id[..8.min(session_id.len())],
                                    model.as_deref().unwrap_or("unknown"),
                                    tools.len(),
                                    mcp_servers.len()
                                ),
                                data: if debug_mode == DebugMode::Detail {
                                    Some(serde_json::json!({
                                        "session_id": session_id,
                                        "model": model,
                                        "tools": tools,
                                        "mcp_servers": mcp_servers,
                                    }))
                                } else {
                                    None
                                },
                            });
                        }
                    }
                    Some(AgentEvent::ToolExecuting { name }) => {
                        tracing::info!("Tool executing: {}", name);

                        // AG-UI: Emit ToolCallStart (REQ-AGUI-040)
                        let tool_call_id = format!("tool-{}", uuid::Uuid::new_v4());
                        hub.broadcast(StandMessage::AgUi {
                            event: AgUiEvent::tool_call_start(&run_id, &tool_call_id, &name),
                        });

                        if debug_mode != DebugMode::None {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: debug_mode,
                                category: "tool".to_string(),
                                message: format!("🔧 {} を実行中...", name),
                                data: None,
                            });
                        }
                    }
                    Some(AgentEvent::ToolResult { name, preview }) => {
                        tracing::info!("Tool result: {} - {}", name, preview);

                        // AG-UI: Emit ToolCallEnd (simplified - no tool_call_id tracking yet)
                        // TODO: Proper tool_call_id tracking across ToolExecuting and ToolResult
                        hub.broadcast(StandMessage::AgUi {
                            event: AgUiEvent::ToolCallEnd {
                                run_id: run_id.clone(),
                                tool_call_id: format!("tool-{}", name), // Simplified ID
                                result: Some(serde_json::json!({ "preview": preview })),
                                error: None,
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                            },
                        });

                        if debug_mode == DebugMode::Detail {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: DebugMode::Detail,
                                category: "tool".to_string(),
                                message: format!("✓ {}: {}", name, preview),
                                data: None,
                            });
                        }
                    }
                    Some(AgentEvent::TextChunk(chunk)) => {
                        chunk_count += 1;

                        // Accumulate response for history
                        response_buffer.push_str(&chunk);

                        // Send streaming chunk
                        hub.broadcast(StandMessage::ChatChunk {
                            content: chunk.clone(),
                            done: false,
                        });

                        if first_chunk {
                            tracing::info!("Started receiving response from Claude CLI");

                            // AG-UI: Emit TextMessageStart on first chunk (REQ-AGUI-040)
                            hub.broadcast(StandMessage::AgUi {
                                event: AgUiEvent::text_message_start(&run_id, &message_id, MessageRole::Assistant),
                            });

                            if debug_mode != DebugMode::None {
                                let elapsed = start_time.elapsed();
                                hub.broadcast(StandMessage::DebugInfo {
                                    level: debug_mode,
                                    category: "timing".to_string(),
                                    message: format!("First chunk in {:?}", elapsed),
                                    data: None,
                                });
                            }
                            first_chunk = false;
                        }

                        // AG-UI: Emit TextMessageContent for each chunk (REQ-AGUI-040)
                        hub.broadcast(StandMessage::AgUi {
                            event: AgUiEvent::text_message_content(&run_id, &message_id, &chunk),
                        });

                        // Detailed debug: show each chunk
                        if debug_mode == DebugMode::Detail {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: DebugMode::Detail,
                                category: "chunk".to_string(),
                                message: format!("Chunk #{}", chunk_count),
                                data: Some(serde_json::json!({
                                    "length": chunk.len(),
                                    "content": if chunk.chars().count() > 100 {
                                        format!("{}...", chunk.chars().take(100).collect::<String>())
                                    } else {
                                        chunk
                                    }
                                })),
                            });
                        }
                    }
                    Some(AgentEvent::Done { result: _, cost }) => {
                        let elapsed = start_time.elapsed();
                        tracing::info!("Claude CLI response complete, cost: {:?}", cost);

                        // Save assistant response to history
                        if !response_buffer.is_empty() {
                            sessions
                                .write()
                                .await
                                .add_message("assistant", response_buffer.clone());
                        }

                        // AG-UI: Emit TextMessageEnd (REQ-AGUI-040)
                        if !first_chunk {
                            // Only emit if we actually started a message
                            hub.broadcast(StandMessage::AgUi {
                                event: AgUiEvent::text_message_end(&run_id, &message_id),
                            });
                        }

                        // AG-UI: Emit RunFinished (REQ-AGUI-040)
                        hub.broadcast(StandMessage::AgUi {
                            event: AgUiEvent::run_finished(&run_id),
                        });

                        // Send final done signal
                        hub.broadcast(StandMessage::ChatChunk {
                            content: String::new(),
                            done: true,
                        });

                        if debug_mode != DebugMode::None {
                            let cost_str = cost
                                .map(|c| format!(" | ${:.4}", c))
                                .unwrap_or_default();
                            hub.broadcast(StandMessage::DebugInfo {
                                level: debug_mode,
                                category: "timing".to_string(),
                                message: format!("Complete in {:?} ({} chunks){}", elapsed, chunk_count, cost_str),
                                data: None,
                            });
                        }
                        break;
                    }
                    Some(AgentEvent::Error(e)) => {
                        tracing::error!("Claude CLI error: {}", e);

                        // AG-UI: Emit RunError (REQ-AGUI-040)
                        hub.broadcast(StandMessage::AgUi {
                            event: AgUiEvent::run_error(&run_id, "AGENT_ERROR", &e),
                        });

                        // Send error as a chat message
                        let error_msg = ChatMessage {
                            role: ChatRole::System,
                            content: format!("Error: {}", e),
                        };
                        hub.broadcast(StandMessage::ChatMessage { message: error_msg });

                        if debug_mode != DebugMode::None {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: debug_mode,
                                category: "error".to_string(),
                                message: e.clone(),
                                data: None,
                            });
                        }
                        break;
                    }
                    None => break,
                }
            }
        }
    }
}

// =============================================================================
// Conductor API Handlers
// =============================================================================

/// Conductor projects response
#[derive(serde::Serialize)]
struct ConductorProjectsResponse {
    projects: Vec<ProjectInfo>,
}

/// Conductor stands response
#[derive(serde::Serialize)]
struct ConductorStandsResponse {
    stands: Vec<RunningStand>,
}

/// GET /api/conductor/projects - List all registered projects
async fn conductor_list_projects(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    let projects = conductor.list_projects().await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!(ConductorProjectsResponse { projects })),
    )
}

/// GET /api/conductor/stands - List all running stands
async fn conductor_list_stands(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    let stands = conductor.list_running_stands().await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!(ConductorStandsResponse { stands })),
    )
}

/// POST /api/conductor/stands/{project_name}/start - Start a stand for project
async fn conductor_start_stand(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.start_stand(&project_name).await {
        Ok(stand) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(&stand).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/conductor/stands/{project_name}/stop - Stop a stand for project
async fn conductor_stop_stand(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.stop_stand(&project_name).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "stopped", "project": project_name})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/conductor/stands/{project_name}/pointview - Open PointView for project
async fn conductor_open_pointview(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.open_pointview(&project_name).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "opened", "project": project_name})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/conductor/refresh - Refresh stand status
async fn conductor_refresh(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.refresh_stand_status().await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "refreshed"})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// ============================================================================
// Update API Handlers
// ============================================================================

/// GET /api/update/check - 更新をチェック
async fn update_check(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    let mut update = update.write().await;
    match update.check_update().await {
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(result).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/apply - 更新を適用（ダウンロード＆置換）
async fn update_apply(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // まず更新をチェック
    let mut update = update.write().await;
    let check_result = match update.check_update().await {
        Ok(result) => result,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Update check failed: {}", e)})),
            );
        }
    };

    // 更新がない場合
    if !check_result.update_available {
        return (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": false,
                "message": "No update available",
                "current_version": check_result.current_version,
                "latest_version": check_result.latest_version,
            })),
        );
    }

    // リリース情報を取得
    let Some(release) = check_result.release else {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Release info not available"})),
        );
    };

    // 更新を適用
    match update.apply_update(&release).await {
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(result).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/rollback - ロールバックを実行
async fn update_rollback(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // バックアップパスを取得
    let backup_path = match body.get("backup_path").and_then(|v| v.as_str()) {
        Some(path) => path,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "backup_path is required"})),
            );
        }
    };

    let update = update.read().await;
    match update.rollback(backup_path).await {
        Ok(_) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "Rollback completed. Restart required.",
                "restart_required": true,
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/restart - アプリケーションを再起動
///
/// リクエストボディ:
/// - `app_path`: 再起動するアプリのパス（省略時は現在のバイナリ）
/// - `delay`: 遅延秒数（デフォルト: 1秒）
async fn update_restart(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(_update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // パラメータを取得
    let app_path = body.get("app_path").and_then(|v| v.as_str());
    let delay = body
        .get("delay")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;

    // 再起動をスケジュール
    let result = if let Some(path) = app_path {
        UpdateCapability::restart_app(path, delay).await
    } else {
        UpdateCapability::restart_self(delay).await
    };

    match result {
        Ok(_) => {
            // 再起動スクリプトが起動されたので、このプロセスを終了する準備
            // クライアントにレスポンスを返してから終了
            let shutdown_token = state.shutdown_token.clone();

            // 少し遅延してからシャットダウン（レスポンスを返す時間を確保）
            tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                shutdown_token.cancel();
            });

            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "message": format!("Restart scheduled in {} seconds", delay),
                    "delay": delay,
                })),
            )
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// ============================================================================
// Mac App Update API Handlers
// ============================================================================

/// GET /api/update/mac/check - VantagePoint.app の更新をチェック
///
/// クエリパラメータ:
/// - `current_version`: 現在のアプリバージョン（必須）
async fn update_mac_check(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    let current_version = match params.get("current_version") {
        Some(v) => v,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "current_version query parameter is required"})),
            );
        }
    };

    let mut update = update.write().await;
    match update.check_mac_update(current_version).await {
        Ok(result) => (axum::http::StatusCode::OK, Json(serde_json::to_value(result).unwrap())),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/mac/apply - VantagePoint.app の更新を適用
///
/// リクエストボディ:
/// - `current_version`: 現在のバージョン（必須）
/// - `app_path`: アプリパス（省略時は自動検索）
async fn update_mac_apply(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // パラメータを取得
    let current_version = match body.get("current_version").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "current_version is required"})),
            );
        }
    };

    let app_path = body.get("app_path").and_then(|v| v.as_str());

    // まず最新リリースを取得
    let mut update_guard = update.write().await;
    let check_result = match update_guard.check_mac_update(&current_version).await {
        Ok(r) => r,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // 更新がなければ終了
    let Some(release) = check_result.release else {
        return (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": false,
                "message": "No update available",
                "current_version": current_version,
                "latest_version": check_result.latest_version,
            })),
        );
    };

    // 更新を適用
    match update_guard
        .apply_mac_update(&release, &current_version, app_path)
        .await
    {
        Ok(result) => (axum::http::StatusCode::OK, Json(serde_json::to_value(result).unwrap())),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/mac/rollback - VantagePoint.app をロールバック
async fn update_mac_rollback(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    let backup_path = match body.get("backup_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "backup_path is required"})),
            );
        }
    };

    let app_path = match body.get("app_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "app_path is required"})),
            );
        }
    };

    let update = update.read().await;
    match update.rollback_mac_app(backup_path, app_path).await {
        Ok(_) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "Rollback completed. Restart required.",
                "restart_required": true,
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// Conductorモードでスタンドサーバーを起動
/// 複数のProject Standを管理するための専用モード
pub async fn run_conductor(port: u16) -> Result<()> {
    use crate::capability::core::{Capability, CapabilityContext};

    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    // Initialize Conductor Capability
    let mut conductor = ConductorCapability::new();
    let ctx = CapabilityContext::new();

    if let Err(e) = conductor.initialize(&ctx).await {
        tracing::error!("Failed to initialize ConductorCapability: {}", e);
        return Err(anyhow::anyhow!("ConductorCapability initialization failed: {}", e));
    }

    // Initialize Update Capability
    let mut update = UpdateCapability::new();
    if let Err(e) = update.initialize(&ctx).await {
        tracing::warn!("Failed to initialize UpdateCapability: {}", e);
    }

    let conductor = Arc::new(RwLock::new(conductor));
    let update = Arc::new(RwLock::new(update));
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
        capabilities: Arc::new(StandCapabilities::new(CapabilityConfig {
            project_dir: String::new(),
            midi_config: None,
            bonjour_port: None,
        }).await),
        conductor: Some(conductor.clone()),
        update: Some(update.clone()),
    });

    let app = Router::new()
        .route("/api/health", get(health_handler))
        .route("/api/shutdown", post(shutdown_handler))
        // Conductor API routes
        .route("/api/conductor/projects", get(conductor_list_projects))
        .route("/api/conductor/stands", get(conductor_list_stands))
        .route("/api/conductor/stands/{project_name}/start", post(conductor_start_stand))
        .route("/api/conductor/stands/{project_name}/stop", post(conductor_stop_stand))
        .route("/api/conductor/stands/{project_name}/pointview", post(conductor_open_pointview))
        .route("/api/conductor/refresh", post(conductor_refresh))
        // Update API routes (vp CLI)
        .route("/api/update/check", get(update_check))
        .route("/api/update/apply", post(update_apply))
        .route("/api/update/rollback", post(update_rollback))
        .route("/api/update/restart", post(update_restart))
        // Update API routes (VantagePoint.app)
        .route("/api/update/mac/check", get(update_mac_check))
        .route("/api/update/mac/apply", post(update_mac_apply))
        .route("/api/update/mac/rollback", post(update_mac_rollback))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("Starting Conductor Stand on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Clone conductor for shutdown
    let conductor_for_shutdown = conductor.clone();

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
