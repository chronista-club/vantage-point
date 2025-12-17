//! HTTP server with WebSocket support

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, IntoResponse},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

use super::hub::Hub;
use crate::agent::{AgentConfig, AgentEvent, ClaudeAgent};
use crate::protocol::{
    BrowserMessage, ChatMessage, ChatRole, DaemonMessage, DebugMode, SessionInfo,
};
use std::collections::HashMap;

/// Session entry with metadata
#[derive(Debug, Clone)]
struct SessionEntry {
    id: String,
    name: String,
    message_count: usize,
    model: Option<String>,
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
}

impl SessionManager {
    fn new() -> Self {
        Self::default()
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
            self.sessions.insert(
                id.clone(),
                SessionEntry {
                    id: id.clone(),
                    name,
                    message_count: 0,
                    model,
                },
            );
        }
        self.active_id = Some(id);
    }

    /// Increment message count for active session
    fn increment_message_count(&mut self) {
        if let Some(ref id) = self.active_id
            && let Some(entry) = self.sessions.get_mut(id) {
                entry.message_count += 1;
            }
    }

    /// Switch to a different session
    fn switch_to(&mut self, session_id: &str) -> Option<&SessionEntry> {
        if self.sessions.contains_key(session_id) {
            self.active_id = Some(session_id.to_string());
            self.sessions.get(session_id)
        } else {
            None
        }
    }

    /// Create a new session (will be registered when Claude CLI responds)
    fn prepare_new_session(&mut self) {
        self.active_id = None;
    }

    /// Rename a session
    fn rename(&mut self, session_id: &str, new_name: String) -> bool {
        if let Some(entry) = self.sessions.get_mut(session_id) {
            entry.name = new_name;
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
            true
        } else {
            false
        }
    }

    /// Get all sessions as SessionInfo for UI
    fn list(&self) -> Vec<SessionInfo> {
        self.sessions
            .values()
            .map(|e| SessionInfo {
                id: e.id.clone(),
                name: e.name.clone(),
                is_active: self.active_id.as_deref() == Some(&e.id),
                message_count: e.message_count,
                model: e.model.clone(),
            })
            .collect()
    }
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
            self.hub.broadcast(DaemonMessage::DebugInfo {
                level: DebugMode::Simple,
                category: category.to_string(),
                message: message.to_string(),
                data: None,
            });
        } else {
            self.hub.broadcast(DaemonMessage::DebugInfo {
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
            self.hub.broadcast(DaemonMessage::DebugInfo {
                level: DebugMode::Detail,
                category: category.to_string(),
                message: message.to_string(),
                data: Some(data),
            });
        }
    }
}

/// Run the daemon server
pub async fn run(
    port: u16,
    auto_open_browser: bool,
    debug_mode: DebugMode,
    project_dir: String,
) -> Result<()> {
    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    let state = Arc::new(AppState {
        hub: Hub::new(),
        sessions: Arc::new(RwLock::new(SessionManager::new())),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode,
        shutdown_token: shutdown_token.clone(),
        project_dir,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(ws_handler))
        .route("/api/show", post(show_handler))
        .route("/api/health", get(health_handler))
        .route("/api/shutdown", post(shutdown_handler))
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

    // Serve with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token_clone.cancelled().await;
            tracing::info!("Graceful shutdown initiated");
        })
        .await?;

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
    Json(msg): Json<DaemonMessage>,
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
        let mode_msg = DaemonMessage::DebugModeChanged {
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
                                hub.broadcast(DaemonMessage::SessionList {
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
                                hub.broadcast(DaemonMessage::ChatChunk {
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
                                hub.broadcast(DaemonMessage::ChatMessage {
                                    message: ChatMessage {
                                        role: ChatRole::System,
                                        content: "New session. Starting fresh conversation."
                                            .to_string(),
                                    },
                                });
                            }
                            BrowserMessage::ListSessions => {
                                let mgr = sessions.read().await;
                                hub.broadcast(DaemonMessage::SessionList {
                                    sessions: mgr.list(),
                                    active_id: mgr.active_id.clone(),
                                });
                            }
                            BrowserMessage::SwitchSession { session_id } => {
                                tracing::info!("Switch session to: {}", session_id);
                                let mut mgr = sessions.write().await;
                                if let Some(entry) = mgr.switch_to(&session_id) {
                                    let name = entry.name.clone();
                                    hub.broadcast(DaemonMessage::SessionSwitched {
                                        session_id: session_id.clone(),
                                        name,
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
                                hub.broadcast(DaemonMessage::ChatMessage {
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
                                    hub.broadcast(DaemonMessage::SessionList {
                                        sessions: mgr.list(),
                                        active_id: mgr.active_id.clone(),
                                    });
                                }
                            }
                            BrowserMessage::CloseSession { session_id } => {
                                tracing::info!("Close session {}", session_id);
                                let mut mgr = sessions.write().await;
                                if mgr.close(&session_id) {
                                    hub.broadcast(DaemonMessage::SessionClosed { session_id });
                                    // Send updated list
                                    hub.broadcast(DaemonMessage::SessionList {
                                        sessions: mgr.list(),
                                        active_id: mgr.active_id.clone(),
                                    });
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
            hub.broadcast(DaemonMessage::DebugInfo {
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
            hub.broadcast(DaemonMessage::DebugInfo {
                level: debug_mode,
                category: "session".to_string(),
                message: "Using --continue (most recent session)".to_string(),
                data: None,
            });
        }
    } else {
        tracing::info!("Starting new session");
        if debug_mode != DebugMode::None {
            hub.broadcast(DaemonMessage::DebugInfo {
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

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => {
                tracing::info!("Chat request cancelled");
                if debug_mode != DebugMode::None {
                    hub.broadcast(DaemonMessage::DebugInfo {
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
                        hub.broadcast(DaemonMessage::SessionList {
                            sessions: mgr.list(),
                            active_id: mgr.active_id.clone(),
                        });
                        drop(mgr);

                        if debug_mode != DebugMode::None {
                            hub.broadcast(DaemonMessage::DebugInfo {
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
                        if debug_mode != DebugMode::None {
                            hub.broadcast(DaemonMessage::DebugInfo {
                                level: debug_mode,
                                category: "tool".to_string(),
                                message: format!("🔧 {} を実行中...", name),
                                data: None,
                            });
                        }
                    }
                    Some(AgentEvent::ToolResult { name, preview }) => {
                        tracing::info!("Tool result: {} - {}", name, preview);
                        if debug_mode == DebugMode::Detail {
                            hub.broadcast(DaemonMessage::DebugInfo {
                                level: DebugMode::Detail,
                                category: "tool".to_string(),
                                message: format!("✓ {}: {}", name, preview),
                                data: None,
                            });
                        }
                    }
                    Some(AgentEvent::TextChunk(chunk)) => {
                        chunk_count += 1;

                        // Send streaming chunk
                        hub.broadcast(DaemonMessage::ChatChunk {
                            content: chunk.clone(),
                            done: false,
                        });

                        if first_chunk {
                            tracing::info!("Started receiving response from Claude CLI");
                            if debug_mode != DebugMode::None {
                                let elapsed = start_time.elapsed();
                                hub.broadcast(DaemonMessage::DebugInfo {
                                    level: debug_mode,
                                    category: "timing".to_string(),
                                    message: format!("First chunk in {:?}", elapsed),
                                    data: None,
                                });
                            }
                            first_chunk = false;
                        }

                        // Detailed debug: show each chunk
                        if debug_mode == DebugMode::Detail {
                            hub.broadcast(DaemonMessage::DebugInfo {
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

                        // Send final done signal
                        hub.broadcast(DaemonMessage::ChatChunk {
                            content: String::new(),
                            done: true,
                        });

                        if debug_mode != DebugMode::None {
                            let cost_str = cost
                                .map(|c| format!(" | ${:.4}", c))
                                .unwrap_or_default();
                            hub.broadcast(DaemonMessage::DebugInfo {
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
                        // Send error as a chat message
                        let error_msg = ChatMessage {
                            role: ChatRole::System,
                            content: format!("Error: {}", e),
                        };
                        hub.broadcast(DaemonMessage::ChatMessage { message: error_msg });

                        if debug_mode != DebugMode::None {
                            hub.broadcast(DaemonMessage::DebugInfo {
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
