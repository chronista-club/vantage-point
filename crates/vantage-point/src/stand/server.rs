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
use crate::agent::{AgentConfig, AgentEvent, ClaudeAgent, InteractiveClaudeAgent};
use crate::agui::{AgUiEvent, AgUiEventBridge, MessageRole};
use crate::capability::{ConductorCapability, ProjectInfo, RunningStand, UpdateCapability};
use crate::config::RunningStands;
use crate::mcp::PermissionResponse;
use crate::protocol::{
    BrowserMessage, ChatComponent, ChatMessage, ChatRole, ComponentAction, DebugMode,
    HistoryMessage, SessionInfo, StandMessage,
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
        if let Ok(data) = std::fs::read_to_string(&state_path)
            && let Ok(state) = serde_json::from_str::<PersistedState>(&data)
        {
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

    /// Set the active session ID
    fn set_active_session(&mut self, id: String) {
        self.active_id = Some(id);
        self.save();
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
        if let Some(ref id) = self.active_id
            && let Some(entry) = self.sessions.get_mut(id)
        {
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
            && let Some(entry) = self.sessions.get_mut(id)
        {
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
    /// Original input from the permission request (needed for "allow" response)
    original_input: serde_json::Value,
    /// Response once user has responded (None = still waiting)
    response: Option<PermissionResponse>,
}

/// Pending user prompt request entry (REQ-PROMPT-001 to REQ-PROMPT-005)
#[derive(Debug, Clone, Serialize)]
struct PendingPrompt {
    /// The prompt request data
    request: PendingPromptRequest,
    /// Response once user has responded (None = still waiting)
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<UserPromptResponseData>,
}

/// User prompt request data stored in pending prompts
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingPromptRequest {
    request_id: String,
    prompt_type: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<PromptOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_value: Option<String>,
    timeout_seconds: u32,
    created_at: u64,
}

/// Prompt option for select/multi_select
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptOption {
    id: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// User prompt response data
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserPromptResponseData {
    /// Response outcome: approved, rejected, cancelled, timeout
    outcome: String,
    /// Text response (for input type or optional comment)
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// Selected option IDs (for select/multi_select)
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_options: Option<Vec<String>>,
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
    /// Pending user prompts: request_id -> response (REQ-PROMPT-001)
    pending_prompts: Arc<RwLock<HashMap<String, PendingPrompt>>>,
    /// Capability system (Agent, MIDI, Protocol)
    capabilities: Arc<StandCapabilities>,
    /// Conductor capability for managing multiple stands (optional, only for conductor mode)
    conductor: Option<Arc<RwLock<ConductorCapability>>>,
    /// Update capability for version checking (optional, only for conductor mode)
    update: Option<Arc<RwLock<UpdateCapability>>>,
    /// Interactive Claude agent (stream-json mode for structured communication)
    interactive_agent: Arc<RwLock<Option<InteractiveClaudeAgent>>>,
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
                tags: vec![],
            });
        } else {
            self.hub.broadcast(StandMessage::DebugInfo {
                level: self.debug_mode,
                category: category.to_string(),
                message: message.to_string(),
                data,
                tags: vec![],
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
                tags: vec![],
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
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities,
        conductor: None, // Conductor mode is set via run_conductor()
        update: None,    // Update capability is only for conductor mode
        interactive_agent: Arc::new(RwLock::new(None)), // Interactive agent (stream-json mode)
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/api/show", post(show_handler))
        .route("/api/toggle-pane", post(toggle_pane_handler))
        .route("/api/health", get(health_handler))
        .route("/api/shutdown", post(shutdown_handler))
        .route("/api/permission", post(permission_request_handler))
        .route("/api/permission/{request_id}", get(permission_poll_handler))
        // User prompt API routes (REQ-PROMPT-001)
        .route("/api/prompt", post(prompt_request_handler))
        .route(
            "/api/prompt/{request_id}",
            get(prompt_poll_handler).post(prompt_respond_handler),
        )
        .route("/api/prompts/pending", get(prompts_list_pending_handler))
        // Conductor API routes
        .route("/api/conductor/projects", get(conductor_list_projects))
        .route("/api/conductor/stands", get(conductor_list_stands))
        .route(
            "/api/conductor/stands/{project_name}/start",
            post(conductor_start_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/stop",
            post(conductor_stop_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/pointview",
            post(conductor_open_pointview),
        )
        .route("/api/conductor/refresh", post(conductor_refresh))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
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

    // Register this Stand in running.json
    let pid = std::process::id();
    if let Err(e) = RunningStands::register(port, &state.project_dir, pid) {
        tracing::warn!("Failed to register Stand in running.json: {}", e);
    } else {
        tracing::info!(
            "Registered Stand in running.json (port={}, pid={})",
            port,
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

/// POST /api/toggle-pane - Toggle side panel visibility
async fn toggle_pane_handler(
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
    // Extract request_id and input from the ChatComponent
    let (request_id, original_input) = match &msg {
        StandMessage::ChatComponent {
            component:
                ChatComponent::PermissionRequest {
                    request_id, input, ..
                },
            ..
        } => (request_id.clone(), input.clone()),
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Expected ChatComponent with PermissionRequest"})),
            );
        }
    };

    tracing::info!("Permission request received: {}", request_id);

    // Store the pending request with original input (needed for "allow" response)
    state.pending_permissions.write().await.insert(
        request_id.clone(),
        PendingPermission {
            original_input,
            response: None,
        },
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
    tracing::info!(
        ">>> handle_permission_response called: request_id={}, approved={}",
        request_id,
        approved
    );

    let mut pending = state.pending_permissions.write().await;
    tracing::debug!(
        "Pending permissions count: {}, keys: {:?}",
        pending.len(),
        pending.keys().collect::<Vec<_>>()
    );

    if let Some(entry) = pending.get_mut(&request_id) {
        let response = if approved {
            // For "allow", use updated_input if provided, otherwise use the original input
            // Claude Code expects updatedInput to be present for "allow" responses
            let final_input = updated_input.or_else(|| Some(entry.original_input.clone()));
            tracing::debug!(
                "Creating allow response with updatedInput: {:?}",
                final_input
            );
            PermissionResponse {
                behavior: "allow".to_string(),
                updated_input: final_input,
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

        tracing::info!(
            "Permission {} -> {}",
            request_id,
            if approved { "allow" } else { "deny" }
        );

        // Broadcast component dismissed
        state.hub.broadcast(StandMessage::ComponentDismissed {
            request_id: request_id.clone(),
        });
    } else {
        tracing::warn!("Permission response for unknown request: {}", request_id);
    }
}

// =============================================================================
// User Prompt Handlers (REQ-PROMPT-001 to REQ-PROMPT-005)
// =============================================================================

/// Request body for prompt creation
#[derive(Debug, Deserialize)]
struct PromptRequest {
    run_id: String,
    request_id: String,
    prompt_type: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    options: Option<Vec<crate::agui::PromptOption>>,
    #[serde(default)]
    default_value: Option<String>,
    #[serde(default = "default_prompt_timeout_secs")]
    timeout_seconds: u32,
}

fn default_prompt_timeout_secs() -> u32 {
    300
}

/// POST /api/prompt - Create a user prompt and wait for response
async fn prompt_request_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let request_id = req.request_id.clone();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    tracing::info!(
        "User prompt request received: {} (type: {})",
        request_id,
        req.prompt_type
    );

    // Convert agui::PromptOption to local PromptOption
    let options = req.options.as_ref().map(|opts| {
        opts.iter()
            .map(|o| PromptOption {
                id: o.id.clone(),
                label: o.label.clone(),
                description: o.description.clone(),
            })
            .collect::<Vec<_>>()
    });

    // Store the pending request with full data
    state.pending_prompts.write().await.insert(
        request_id.clone(),
        PendingPrompt {
            request: PendingPromptRequest {
                request_id: request_id.clone(),
                prompt_type: req.prompt_type.clone(),
                title: req.title.clone(),
                description: req.description.clone(),
                options,
                default_value: req.default_value.clone(),
                timeout_seconds: req.timeout_seconds,
                created_at,
            },
            response: None,
        },
    );

    // Convert prompt_type string to enum
    let prompt_type = match req.prompt_type.as_str() {
        "confirm" => crate::agui::UserPromptType::Confirm,
        "input" => crate::agui::UserPromptType::Input,
        "select" => crate::agui::UserPromptType::Select,
        "multi_select" => crate::agui::UserPromptType::MultiSelect,
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid prompt_type"})),
            );
        }
    };

    // Create and broadcast the AG-UI event
    let event = AgUiEvent::UserPrompt {
        run_id: req.run_id,
        request_id: request_id.clone(),
        prompt_type,
        title: req.title,
        description: req.description,
        options: req.options,
        default_value: req.default_value,
        timeout_seconds: req.timeout_seconds,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    };

    // Broadcast to WebSocket clients
    state.hub.broadcast(StandMessage::AgUi { event });

    state.send_debug(
        "prompt",
        &format!("User prompt created: {}", request_id),
        None,
    );

    // Return accepted and let caller poll for response
    (
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({"status": "pending", "request_id": request_id})),
    )
}

/// GET /api/prompt/{request_id} - Poll for prompt response
async fn prompt_poll_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let mut pending = state.pending_prompts.write().await;

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

/// POST /api/prompt/{request_id} - Submit a prompt response via HTTP
async fn prompt_respond_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
    Json(response): Json<UserPromptResponseData>,
) -> impl IntoResponse {
    // Use the existing WebSocket handler logic
    handle_user_prompt_response(
        &state,
        request_id.clone(),
        response.outcome.clone(),
        response.message,
        response.selected_options,
    )
    .await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"status": "accepted", "request_id": request_id})),
    )
}

/// Handle a user prompt response from WebSocket
async fn handle_user_prompt_response(
    state: &Arc<AppState>,
    request_id: String,
    outcome: String,
    message: Option<String>,
    selected_options: Option<Vec<String>>,
) {
    let mut pending = state.pending_prompts.write().await;

    if let Some(entry) = pending.get_mut(&request_id) {
        let response = UserPromptResponseData {
            outcome: outcome.clone(),
            message,
            selected_options,
        };

        // Store the response (will be retrieved by next poll)
        entry.response = Some(response);

        tracing::info!("User prompt {} -> {}", request_id, outcome);

        // Broadcast component dismissed
        state.hub.broadcast(StandMessage::ComponentDismissed {
            request_id: request_id.clone(),
        });

        // Also send AG-UI event
        let agui_outcome = match outcome.as_str() {
            "approved" => crate::agui::UserPromptOutcome::Approved,
            "rejected" => crate::agui::UserPromptOutcome::Rejected,
            "cancelled" => crate::agui::UserPromptOutcome::Cancelled,
            _ => crate::agui::UserPromptOutcome::Timeout,
        };

        // Clone request_id before moving it
        let request_id_for_agent = request_id.clone();

        state.hub.broadcast(StandMessage::AgUi {
            event: AgUiEvent::UserPromptResponse {
                run_id: String::new(), // Will be set by the client
                request_id,
                outcome: agui_outcome,
                message: entry.response.as_ref().and_then(|r| r.message.clone()),
                selected_options: entry
                    .response
                    .as_ref()
                    .and_then(|r| r.selected_options.clone()),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            },
        });

        // Release pending lock before sending to agent
        drop(pending);

        // Send user_input_result to Interactive Claude agent
        let confirmed = matches!(agui_outcome, crate::agui::UserPromptOutcome::Approved);
        let agent_guard = state.interactive_agent.read().await;
        if let Some(ref agent) = *agent_guard {
            if let Err(e) = agent
                .send_user_input_result(&request_id_for_agent, confirmed)
                .await
            {
                tracing::error!("Failed to send user_input_result to agent: {}", e);
            } else {
                tracing::info!(
                    "user_input_result sent: {} -> {}",
                    request_id_for_agent,
                    if confirmed { "approved" } else { "rejected" }
                );
            }
        } else {
            tracing::warn!("No Interactive agent running, cannot send user_input_result");
        }
    } else {
        tracing::warn!("User prompt response for unknown request: {}", request_id);
    }
}

/// List pending prompts (for external polling, e.g., VantagePoint.app)
async fn prompts_list_pending_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pending = state.pending_prompts.read().await;

    // Filter prompts that don't have a response yet and return full request data
    let prompts_without_response: Vec<&PendingPromptRequest> = pending
        .values()
        .filter(|entry| entry.response.is_none())
        .map(|entry| &entry.request)
        .collect();

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"prompts": prompts_without_response})),
    )
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

                                // Use Interactive mode (stream-json) for bidirectional communication
                                // これにより user_input_result を送信可能
                                handle_chat_message_interactive(
                                    &hub,
                                    &sessions,
                                    &new_token,
                                    debug_mode,
                                    &state_clone.project_dir,
                                    &state_clone.interactive_agent,
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
                                    ComponentAction::PermissionDeny {
                                        request_id,
                                        message,
                                    } => {
                                        handle_permission_response(
                                            &state_clone,
                                            request_id,
                                            false,
                                            None,
                                            message,
                                        )
                                        .await;
                                    }
                                    // User prompt response (REQ-PROMPT-005)
                                    ComponentAction::UserPromptSubmit {
                                        request_id,
                                        outcome,
                                        message,
                                        selected_options,
                                    } => {
                                        handle_user_prompt_response(
                                            &state_clone,
                                            request_id,
                                            outcome,
                                            message,
                                            selected_options,
                                        )
                                        .await;
                                    }
                                    // Handle other component actions
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

/// Handle incoming chat message using Interactive mode (stream-json)
/// Stream-JSONモードでは構造化されたJSON通信で対話
/// パーミッションは--permission-prompt-toolでMCPツール経由で処理
async fn handle_chat_message_interactive(
    hub: &Hub,
    sessions: &Arc<RwLock<SessionManager>>,
    cancel_token: &CancellationToken,
    debug_mode: DebugMode,
    project_dir: &str,
    interactive_agent: &Arc<RwLock<Option<InteractiveClaudeAgent>>>,
    message: String,
) {
    let start_time = Instant::now();

    // AG-UI: Generate run_id for this chat request
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

    // Initialize Interactive agent if not already running
    {
        let mut agent_guard = interactive_agent.write().await;
        if agent_guard.is_none() {
            tracing::info!("Initializing Interactive Claude agent...");

            // ホームディレクトリを取得
            let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/Users/makoto".to_string());
            let repos_path = format!("{}/repos", home_dir);

            let mut config = AgentConfig {
                working_dir: Some(project_dir.to_string()),
                use_continue,
                // パーミッションプロンプトをMCPツール経由で処理
                permission_prompt_tool: Some("mcp__vantage-point__permission".to_string()),
                // ~/repos/ 以下のみアクセス可能に制限
                allowed_tools: vec![
                    // ファイル操作は ~/repos/ 以下に制限
                    format!("Edit(path:{}/**)", repos_path),
                    format!("Read(path:{}/**)", repos_path),
                    format!("Write(path:{}/**)", repos_path),
                    // Bash は git, cargo, bun 等の開発コマンドを許可
                    "Bash(git:*)".to_string(),
                    "Bash(cargo:*)".to_string(),
                    "Bash(bun:*)".to_string(),
                    "Bash(bunx:*)".to_string(),
                    "Bash(ls:*)".to_string(),
                    "Bash(cat:*)".to_string(),
                    "Bash(mkdir:*)".to_string(),
                    // MCP ツールを許可 (vantage-point, creo-memories)
                    "mcp__vantage-point__*".to_string(),
                    "mcp__creo-memories__*".to_string(),
                ],
                ..Default::default()
            };

            if let Some(ref sid) = session_id {
                config.session_id = Some(sid.clone());
            }

            let agent = InteractiveClaudeAgent::new(config);
            if let Err(e) = agent.start().await {
                tracing::error!("Failed to start Interactive agent: {}", e);
                hub.broadcast(StandMessage::ChatChunk {
                    content: format!("Error: Failed to start Claude CLI: {}", e),
                    done: true,
                });
                hub.broadcast(StandMessage::AgUi {
                    event: AgUiEvent::run_error(&run_id, "INTERACTIVE_START_FAILED", e.to_string()),
                });
                return;
            }

            tracing::info!("Interactive Claude agent started successfully");
            if debug_mode != DebugMode::None {
                hub.broadcast(StandMessage::DebugInfo {
                    level: debug_mode,
                    category: "agent".to_string(),
                    message: "Interactive Claude agent started (stream-json mode)".to_string(),
                    data: None,
                    tags: vec![],
                });
            }

            *agent_guard = Some(agent);
        }
    }

    // Send message to Interactive agent
    {
        let agent_guard = interactive_agent.read().await;
        if let Some(ref agent) = *agent_guard {
            if let Err(e) = agent.send(&message).await {
                tracing::error!("Failed to send message to Interactive agent: {}", e);
                hub.broadcast(StandMessage::ChatChunk {
                    content: format!("Error: {}", e),
                    done: true,
                });
                return;
            }
            tracing::info!("Message sent to Interactive agent");
        }
    }

    // Start Interactive output listener task
    let hub_clone = hub.clone();
    let interactive_agent_clone = interactive_agent.clone();
    let sessions_clone = sessions.clone();
    let run_id_clone = run_id.clone();
    let message_id_clone = message_id.clone();
    let cancel_token_clone = cancel_token.clone();
    let debug_mode_clone = debug_mode;

    tokio::spawn(async move {
        let agent_guard = interactive_agent_clone.read().await;
        if let Some(ref agent) = *agent_guard {
            let events_rx = agent.events();
            let mut events = events_rx.lock().await;
            let mut response_buffer = String::new();
            let mut first_chunk = true;

            loop {
                tokio::select! {
                    _ = cancel_token_clone.cancelled() => {
                        tracing::info!("Interactive chat cancelled");
                        break;
                    }
                    event = events.recv() => {
                        match event {
                            Some(AgentEvent::TextChunk(text)) => {
                                // Accumulate response
                                response_buffer.push_str(&text);

                                // Send to WebSocket
                                hub_clone.broadcast(StandMessage::ChatChunk {
                                    content: text.clone(),
                                    done: false,
                                });

                                if first_chunk {
                                    hub_clone.broadcast(StandMessage::AgUi {
                                        event: AgUiEvent::text_message_start(
                                            &run_id_clone,
                                            &message_id_clone,
                                            MessageRole::Assistant,
                                        ),
                                    });
                                    first_chunk = false;
                                }

                                hub_clone.broadcast(StandMessage::AgUi {
                                    event: AgUiEvent::text_message_content(
                                        &run_id_clone,
                                        &message_id_clone,
                                        &text,
                                    ),
                                });
                            }
                            Some(AgentEvent::SessionInit { session_id, model, tools, mcp_servers }) => {
                                tracing::info!(
                                    "Session initialized: id={}, model={:?}, tools={}, mcp={}",
                                    session_id, model, tools.len(), mcp_servers.len()
                                );
                                // Update session manager with the new session ID
                                sessions_clone.write().await.set_active_session(session_id.clone());

                                if debug_mode_clone != DebugMode::None {
                                    hub_clone.broadcast(StandMessage::DebugInfo {
                                        level: debug_mode_clone,
                                        category: "session".to_string(),
                                        message: format!("Session: {}", session_id),
                                        data: Some(serde_json::json!({
                                            "model": model,
                                            "tools_count": tools.len(),
                                            "mcp_servers": mcp_servers
                                        })),
                                        tags: vec!["interactive".to_string(), "session".to_string()],
                                    });
                                }
                            }
                            Some(AgentEvent::ToolExecuting { name }) => {
                                tracing::info!("Tool executing: {}", name);
                                hub_clone.broadcast(StandMessage::ChatComponent {
                                    component: ChatComponent::ToolExecution {
                                        tool_name: name.clone(),
                                        status: "running".to_string(),
                                        result: None,
                                    },
                                    interactive: false,
                                });
                            }
                            Some(AgentEvent::ToolResult { name, preview }) => {
                                tracing::info!("Tool result: {} -> {}", name, preview);
                                hub_clone.broadcast(StandMessage::ChatComponent {
                                    component: ChatComponent::ToolExecution {
                                        tool_name: name.clone(),
                                        status: "completed".to_string(),
                                        result: Some(preview),
                                    },
                                    interactive: false,
                                });
                            }
                            Some(AgentEvent::Done { result: _result, cost }) => {
                                tracing::info!("Interactive response complete (cost: {:?})", cost);

                                // Save assistant response to history
                                if !response_buffer.is_empty() {
                                    sessions_clone.write().await.add_message("assistant", response_buffer.clone());
                                }

                                // Send done signal
                                hub_clone.broadcast(StandMessage::ChatChunk {
                                    content: String::new(),
                                    done: true,
                                });

                                // AG-UI: Emit message end and run finished
                                if !first_chunk {
                                    hub_clone.broadcast(StandMessage::AgUi {
                                        event: AgUiEvent::text_message_end(&run_id_clone, &message_id_clone),
                                    });
                                }

                                hub_clone.broadcast(StandMessage::AgUi {
                                    event: AgUiEvent::run_finished(&run_id_clone),
                                });

                                break;
                            }
                            Some(AgentEvent::Error(err)) => {
                                tracing::error!("Interactive agent error: {}", err);
                                hub_clone.broadcast(StandMessage::ChatChunk {
                                    content: format!("\n\nError: {}", err),
                                    done: true,
                                });
                                hub_clone.broadcast(StandMessage::AgUi {
                                    event: AgUiEvent::run_error(&run_id_clone, "AGENT_ERROR", &err),
                                });
                                break;
                            }
                            Some(AgentEvent::UserInputRequest {
                                request_id,
                                request_type,
                                prompt,
                                options,
                            }) => {
                                tracing::info!("User input request: id={}, type={:?}", request_id, request_type);

                                // Determine prompt type from request_type
                                let prompt_type = match request_type.as_deref() {
                                    Some("confirmation") | Some("confirm") => crate::agui::UserPromptType::Confirm,
                                    Some("select") | Some("choice") => crate::agui::UserPromptType::Select,
                                    Some("multi_select") => crate::agui::UserPromptType::MultiSelect,
                                    _ => crate::agui::UserPromptType::Input,
                                };

                                // Convert options
                                let ui_options: Vec<crate::agui::PromptOption> = options
                                    .iter()
                                    .map(|o| crate::agui::PromptOption {
                                        id: o.value.clone(),
                                        label: o.label.clone().unwrap_or_default(),
                                        description: o.description.clone(),
                                    })
                                    .collect();

                                // AG-UI: Emit UserPrompt event
                                hub_clone.broadcast(StandMessage::AgUi {
                                    event: AgUiEvent::UserPrompt {
                                        run_id: run_id_clone.clone(),
                                        request_id,
                                        prompt_type,
                                        title: prompt.unwrap_or_else(|| "確認してください".to_string()),
                                        description: None,
                                        options: if ui_options.is_empty() { None } else { Some(ui_options) },
                                        default_value: None,
                                        timeout_seconds: crate::agui::default_prompt_timeout(),
                                        timestamp: crate::agui::now_millis(),
                                    },
                                });
                            }
                            None => {
                                // Channel closed
                                tracing::warn!("Interactive agent event channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        }
    });

    let elapsed = start_time.elapsed();
    tracing::info!("Interactive message handling initiated in {:?}", elapsed);
}

/// Handle incoming chat message from browser (OneShot mode - legacy)
#[allow(dead_code)]
async fn handle_chat_message(
    hub: &Hub,
    sessions: &Arc<RwLock<SessionManager>>,
    cancel_token: &CancellationToken,
    debug_mode: DebugMode,
    project_dir: &str,
    message: String,
) {
    let start_time = Instant::now();

    // AG-UI: Create event bridge for this run (REQ-AGUI-040)
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let mut bridge = AgUiEventBridge::new(&run_id);

    // AG-UI: Emit RunStarted event
    hub.broadcast(StandMessage::AgUi {
        event: bridge.run_started(),
    });

    // Save user message to history
    sessions.write().await.add_message("user", message.clone());

    // Get session info from manager
    let (session_id, use_continue) = sessions.read().await.get_active_session();

    // Create agent config with project directory
    // input_format: stream-json で双方向通信を有効化し、user_input_result を送信可能に
    let mut config = AgentConfig {
        working_dir: Some(project_dir.to_string()),
        use_continue,
        input_format: Some("stream-json".to_string()),
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
                tags: vec![],
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
                tags: vec![],
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
                tags: vec![],
            });
        }
    }

    let agent = ClaudeAgent::with_config(config);
    let mut rx = agent.chat(&message).await;

    let hub = hub.clone();
    let sessions = sessions.clone();
    let cancel_token = cancel_token.clone();
    let mut chunk_count = 0;

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                tracing::info!("Chat request cancelled");

                // AG-UI: Emit cancellation events via bridge (REQ-AGUI-040)
                for event in bridge.cancelled() {
                    hub.broadcast(StandMessage::AgUi { event });
                }

                if debug_mode != DebugMode::None {
                    hub.broadcast(StandMessage::DebugInfo {
                        level: debug_mode,
                        category: "chat".to_string(),
                        message: "Request cancelled".to_string(),
                        data: None,
                        tags: vec![],
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
                                tags: vec!["interactive".to_string(), "session".to_string()],
                            });
                        }
                    }
                    Some(AgentEvent::ToolExecuting { ref name }) => {
                        tracing::info!("Tool executing: {}", name);

                        // AG-UI: Emit ToolCallStart via bridge (REQ-AGUI-040)
                        for event in bridge.convert(AgentEvent::ToolExecuting { name: name.clone() }) {
                            hub.broadcast(StandMessage::AgUi { event });
                        }

                        if debug_mode != DebugMode::None {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: debug_mode,
                                category: "tool".to_string(),
                                message: format!("🔧 {} を実行中...", name),
                                data: None,
                                tags: vec![],
                            });
                        }
                    }
                    Some(AgentEvent::ToolResult { ref name, ref preview }) => {
                        tracing::info!("Tool result: {} - {}", name, preview);

                        // AG-UI: Emit ToolCallEnd via bridge (proper tool_call_id tracking)
                        for event in bridge.convert(AgentEvent::ToolResult {
                            name: name.clone(),
                            preview: preview.clone(),
                        }) {
                            hub.broadcast(StandMessage::AgUi { event });
                        }

                        if debug_mode == DebugMode::Detail {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: DebugMode::Detail,
                                category: "tool".to_string(),
                                message: format!("✓ {}: {}", name, preview),
                                data: None,
                                tags: vec![],
                            });
                        }
                    }
                    Some(AgentEvent::TextChunk(ref chunk)) => {
                        chunk_count += 1;
                        let is_first = !bridge.is_message_started();

                        // Send streaming chunk (legacy WebSocket)
                        hub.broadcast(StandMessage::ChatChunk {
                            content: chunk.clone(),
                            done: false,
                        });

                        // AG-UI: Emit TextMessageStart + TextMessageContent via bridge (REQ-AGUI-040)
                        for event in bridge.convert(AgentEvent::TextChunk(chunk.clone())) {
                            hub.broadcast(StandMessage::AgUi { event });
                        }

                        if is_first {
                            tracing::info!("Started receiving response from Claude CLI");

                            if debug_mode != DebugMode::None {
                                let elapsed = start_time.elapsed();
                                hub.broadcast(StandMessage::DebugInfo {
                                    level: debug_mode,
                                    category: "timing".to_string(),
                                    message: format!("First chunk in {:?}", elapsed),
                                    data: None,
                                    tags: vec![],
                                });
                            }
                        }

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
                                        chunk.clone()
                                    }
                                })),
                                tags: vec!["interactive".to_string(), "chunk".to_string()],
                            });
                        }
                    }
                    Some(AgentEvent::Done { result, cost }) => {
                        let elapsed = start_time.elapsed();
                        tracing::info!("Claude CLI response complete, cost: {:?}", cost);

                        // Save assistant response to history (using bridge's buffer)
                        let response_text = bridge.text_buffer().to_string();
                        if !response_text.is_empty() {
                            sessions
                                .write()
                                .await
                                .add_message("assistant", response_text);
                        }

                        // AG-UI: Emit TextMessageEnd + RunFinished via bridge (REQ-AGUI-040)
                        for event in bridge.convert(AgentEvent::Done { result, cost }) {
                            hub.broadcast(StandMessage::AgUi { event });
                        }

                        // Send final done signal (legacy WebSocket)
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
                                tags: vec![],
                            });
                        }
                        break;
                    }
                    Some(AgentEvent::Error(ref e)) => {
                        tracing::error!("Claude CLI error: {}", e);

                        // AG-UI: Emit TextMessageEnd (if started) + RunError via bridge (REQ-AGUI-040)
                        for event in bridge.convert(AgentEvent::Error(e.clone())) {
                            hub.broadcast(StandMessage::AgUi { event });
                        }

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
                                tags: vec![],
                            });
                        }
                        break;
                    }
                    Some(AgentEvent::UserInputRequest {
                        ref request_id,
                        ref request_type,
                        ref prompt,
                        ref options,
                    }) => {
                        tracing::info!("User input request: id={}, type={:?}", request_id, request_type);

                        // AG-UI: Emit UserPrompt via bridge
                        for event in bridge.convert(AgentEvent::UserInputRequest {
                            request_id: request_id.clone(),
                            request_type: request_type.clone(),
                            prompt: prompt.clone(),
                            options: options.clone(),
                        }) {
                            hub.broadcast(StandMessage::AgUi { event });
                        }

                        if debug_mode != DebugMode::None {
                            hub.broadcast(StandMessage::DebugInfo {
                                level: debug_mode,
                                category: "permission".to_string(),
                                message: format!(
                                    "⏳ ユーザー入力待ち: {}",
                                    prompt.as_deref().unwrap_or("確認してください")
                                ),
                                data: if debug_mode == DebugMode::Detail {
                                    Some(serde_json::json!({
                                        "request_id": request_id,
                                        "request_type": request_type,
                                        "options_count": options.len(),
                                    }))
                                } else {
                                    None
                                },
                                tags: vec!["interactive".to_string(), "permission".to_string()],
                            });
                        }
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
async fn conductor_list_projects(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
async fn conductor_list_stands(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
async fn conductor_refresh(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
async fn update_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
async fn update_apply(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
    let delay = body.get("delay").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

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
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(result).unwrap()),
        ),
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
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(result).unwrap()),
        ),
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
        return Err(anyhow::anyhow!(
            "ConductorCapability initialization failed: {}",
            e
        ));
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
        pending_prompts: Arc::new(RwLock::new(HashMap::new())),
        capabilities: Arc::new(
            StandCapabilities::new(CapabilityConfig {
                project_dir: String::new(),
                midi_config: None,
                bonjour_port: None,
            })
            .await,
        ),
        conductor: Some(conductor.clone()),
        update: Some(update.clone()),
        interactive_agent: Arc::new(RwLock::new(None)),
    });

    let app = Router::new()
        .route("/api/health", get(health_handler))
        .route("/api/shutdown", post(shutdown_handler))
        // Conductor API routes
        .route("/api/conductor/projects", get(conductor_list_projects))
        .route("/api/conductor/stands", get(conductor_list_stands))
        .route(
            "/api/conductor/stands/{project_name}/start",
            post(conductor_start_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/stop",
            post(conductor_stop_stand),
        )
        .route(
            "/api/conductor/stands/{project_name}/pointview",
            post(conductor_open_pointview),
        )
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

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
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
