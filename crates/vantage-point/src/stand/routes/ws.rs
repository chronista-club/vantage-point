//! WebSocketハンドラー

use std::sync::Arc;

use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::super::state::AppState;
use super::chat::handle_chat_message_interactive;
use super::permission::handle_permission_response;
use super::prompt::handle_user_prompt_response;
use crate::protocol::{
    BrowserMessage, ChatMessage, ChatRole, ComponentAction, DebugMode, HistoryMessage, StandMessage,
};

/// WebSocket handler
pub async fn ws_handler(
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
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    let text = serde_json::to_string(&msg).unwrap_or_default();
                    if sender.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    // バッファオーバーフローでメッセージがスキップされた
                    tracing::warn!("WebSocket broadcast lagged: {} messages dropped", count);
                    // 継続して受信を試みる
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
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

                                // PTY起動はTerminalResizeメッセージ受信時に遅延
                                // ブラウザが正しいターミナルサイズを送信してから起動することで
                                // 初期出力のサイズ不整合を防ぐ
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
                            BrowserMessage::TerminalInput { data } => {
                                // base64デコードしてPTYに書き込み
                                use base64::Engine;
                                let engine = base64::engine::general_purpose::STANDARD;
                                match engine.decode(&data) {
                                    Ok(bytes) => {
                                        let mut pty_mgr = state_clone.pty_manager.lock().await;
                                        if let Err(e) = pty_mgr.write(&bytes) {
                                            tracing::warn!("PTY write error: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Terminal input base64 decode error: {}", e);
                                    }
                                }
                            }
                            BrowserMessage::TerminalResize { cols, rows } => {
                                let mut pty_mgr = state_clone.pty_manager.lock().await;
                                if !pty_mgr.is_active() {
                                    // 初回TerminalResize: ブラウザの実サイズでPTYを起動
                                    if let Err(e) = pty_mgr.start(
                                        &state_clone.project_dir,
                                        cols,
                                        rows,
                                        state_clone.hub.sender(),
                                    ) {
                                        tracing::warn!("Failed to start PTY session: {}", e);
                                        state_clone.send_debug(
                                            "terminal",
                                            &format!("PTY起動失敗: {}", e),
                                            None,
                                        );
                                    } else {
                                        state_clone.send_debug(
                                            "terminal",
                                            &format!("PTYセッション開始 ({}x{})", cols, rows),
                                            None,
                                        );
                                    }
                                } else {
                                    // 既にアクティブ: リサイズのみ
                                    if let Err(e) = pty_mgr.resize(cols, rows) {
                                        tracing::warn!("PTY resize error: {}", e);
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
