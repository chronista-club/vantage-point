//! HTTP server with WebSocket support

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

use super::hub::Hub;
use crate::agent::{AgentEvent, ClaudeAgent};
use crate::protocol::{BrowserMessage, ChatMessage, ChatRole, DaemonMessage, DebugMode};

/// Application state
struct AppState {
    hub: Hub,
    /// Current Claude session ID for conversation continuity
    session_id: Arc<RwLock<Option<String>>>,
    /// Cancellation token for current chat request
    cancel_token: Arc<RwLock<CancellationToken>>,
    /// Debug display mode
    debug_mode: DebugMode,
    /// Shutdown signal token
    shutdown_token: CancellationToken,
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
pub async fn run(port: u16, auto_open_browser: bool, debug_mode: DebugMode) -> Result<()> {
    // Shutdown signal
    let shutdown_token = CancellationToken::new();
    let shutdown_token_clone = shutdown_token.clone();

    let state = Arc::new(AppState {
        hub: Hub::new(),
        session_id: Arc::new(RwLock::new(None)),
        cancel_token: Arc::new(RwLock::new(CancellationToken::new())),
        debug_mode,
        shutdown_token: shutdown_token.clone(),
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
    tracing::info!("Starting vantaged on http://{}", addr);

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
    std::process::Command::new("open")
        .arg(url)
        .spawn()?;
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
}

async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
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
async fn shutdown_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    tracing::info!("Shutdown requested via API");
    state.shutdown_token.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}

/// WebSocket handler
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle WebSocket connection
async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();

    state.hub.client_connected().await;

    // Send debug mode info on connection
    if state.debug_mode != DebugMode::None {
        let mode_msg = DaemonMessage::DebugModeChanged { mode: state.debug_mode };
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
    let session_id = state.session_id.clone();
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
                                    &session_id,
                                    &new_token,
                                    debug_mode,
                                    message,
                                ).await;
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
                                tracing::info!("Session reset requested");
                                let old_session = session_id.write().await.take();

                                if let Some(sid) = old_session {
                                    state_clone.send_debug(
                                        "session",
                                        &format!("Session cleared: {}", sid),
                                        None,
                                    );
                                }

                                // Notify browser
                                hub.broadcast(DaemonMessage::ChatMessage {
                                    message: ChatMessage {
                                        role: ChatRole::System,
                                        content: "Session reset. Starting new conversation.".to_string(),
                                    },
                                });
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
    session_id_state: &Arc<RwLock<Option<String>>>,
    cancel_token: &CancellationToken,
    debug_mode: DebugMode,
    message: String,
) {
    let start_time = Instant::now();

    // Get current session ID if any
    let current_session = session_id_state.read().await.clone();

    // Create agent with session continuity
    let agent = if let Some(ref sid) = current_session {
        tracing::info!("Continuing session: {}", sid);
        if debug_mode != DebugMode::None {
            hub.broadcast(DaemonMessage::DebugInfo {
                level: debug_mode,
                category: "session".to_string(),
                message: format!("Continuing session: {}", sid),
                data: None,
            });
        }
        ClaudeAgent::new().with_session(sid.clone())
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
        ClaudeAgent::new()
    };

    let mut rx = agent.chat(&message).await;

    let hub = hub.clone();
    let session_id_state = session_id_state.clone();
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
                    Some(AgentEvent::SessionInit { session_id }) => {
                        tracing::info!("Session initialized: {}", session_id);
                        // Store session ID for future messages
                        *session_id_state.write().await = Some(session_id.clone());

                        if debug_mode != DebugMode::None {
                            hub.broadcast(DaemonMessage::DebugInfo {
                                level: debug_mode,
                                category: "session".to_string(),
                                message: format!("Session ID: {}", session_id),
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
                    Some(AgentEvent::Done { result: _ }) => {
                        let elapsed = start_time.elapsed();
                        tracing::info!("Claude CLI response complete");

                        // Send final done signal
                        hub.broadcast(DaemonMessage::ChatChunk {
                            content: String::new(),
                            done: true,
                        });

                        if debug_mode != DebugMode::None {
                            hub.broadcast(DaemonMessage::DebugInfo {
                                level: debug_mode,
                                category: "timing".to_string(),
                                message: format!("Complete in {:?} ({} chunks)", elapsed, chunk_count),
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
