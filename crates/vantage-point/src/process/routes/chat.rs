//! チャットハンドラー
//!
//! Claude CLIとの対話処理（Interactive mode / OneShot mode）

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::super::hub::Hub;
use super::super::session::SessionManager;
use crate::agent::{AgentConfig, AgentEvent, ClaudeAgent, InteractiveClaudeAgent};
use crate::agui::{AgUiEvent, AgUiEventBridge, MessageRole};
use crate::protocol::{ChatComponent, ChatMessage, ChatRole, DebugMode, ProcessMessage};

/// Handle incoming chat message using Interactive mode (stream-json)
/// Stream-JSONモードでは構造化されたJSON通信で対話
/// パーミッションは--permission-prompt-toolでMCPツール経由で処理
pub async fn handle_chat_message_interactive(
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
    hub.broadcast(ProcessMessage::AgUi {
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
                hub.broadcast(ProcessMessage::ChatChunk {
                    content: format!("Error: Failed to start Claude CLI: {}", e),
                    done: true,
                });
                hub.broadcast(ProcessMessage::AgUi {
                    event: AgUiEvent::run_error(&run_id, "INTERACTIVE_START_FAILED", e.to_string()),
                });
                return;
            }

            tracing::info!("Interactive Claude agent started successfully");
            if debug_mode != DebugMode::None {
                hub.broadcast(ProcessMessage::DebugInfo {
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
                hub.broadcast(ProcessMessage::ChatChunk {
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
                                hub_clone.broadcast(ProcessMessage::ChatChunk {
                                    content: text.clone(),
                                    done: false,
                                });

                                if first_chunk {
                                    hub_clone.broadcast(ProcessMessage::AgUi {
                                        event: AgUiEvent::text_message_start(
                                            &run_id_clone,
                                            &message_id_clone,
                                            MessageRole::Assistant,
                                        ),
                                    });
                                    first_chunk = false;
                                }

                                hub_clone.broadcast(ProcessMessage::AgUi {
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
                                    hub_clone.broadcast(ProcessMessage::DebugInfo {
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
                                hub_clone.broadcast(ProcessMessage::ChatComponent {
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
                                hub_clone.broadcast(ProcessMessage::ChatComponent {
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
                                hub_clone.broadcast(ProcessMessage::ChatChunk {
                                    content: String::new(),
                                    done: true,
                                });

                                // AG-UI: Emit message end and run finished
                                if !first_chunk {
                                    hub_clone.broadcast(ProcessMessage::AgUi {
                                        event: AgUiEvent::text_message_end(&run_id_clone, &message_id_clone),
                                    });
                                }

                                hub_clone.broadcast(ProcessMessage::AgUi {
                                    event: AgUiEvent::run_finished(&run_id_clone),
                                });

                                break;
                            }
                            Some(AgentEvent::Error(err)) => {
                                tracing::error!("Interactive agent error: {}", err);
                                hub_clone.broadcast(ProcessMessage::ChatChunk {
                                    content: format!("\n\nError: {}", err),
                                    done: true,
                                });
                                hub_clone.broadcast(ProcessMessage::AgUi {
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
                                hub_clone.broadcast(ProcessMessage::AgUi {
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
pub async fn handle_chat_message(
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
    hub.broadcast(ProcessMessage::AgUi {
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
            hub.broadcast(ProcessMessage::DebugInfo {
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
            hub.broadcast(ProcessMessage::DebugInfo {
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
            hub.broadcast(ProcessMessage::DebugInfo {
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
                    hub.broadcast(ProcessMessage::AgUi { event });
                }

                if debug_mode != DebugMode::None {
                    hub.broadcast(ProcessMessage::DebugInfo {
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
                        hub.broadcast(ProcessMessage::SessionList {
                            sessions: mgr.list(),
                            active_id: mgr.active_id.clone(),
                        });
                        drop(mgr);

                        if debug_mode != DebugMode::None {
                            hub.broadcast(ProcessMessage::DebugInfo {
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
                            hub.broadcast(ProcessMessage::AgUi { event });
                        }

                        if debug_mode != DebugMode::None {
                            hub.broadcast(ProcessMessage::DebugInfo {
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
                            hub.broadcast(ProcessMessage::AgUi { event });
                        }

                        if debug_mode == DebugMode::Detail {
                            hub.broadcast(ProcessMessage::DebugInfo {
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
                        hub.broadcast(ProcessMessage::ChatChunk {
                            content: chunk.clone(),
                            done: false,
                        });

                        // AG-UI: Emit TextMessageStart + TextMessageContent via bridge (REQ-AGUI-040)
                        for event in bridge.convert(AgentEvent::TextChunk(chunk.clone())) {
                            hub.broadcast(ProcessMessage::AgUi { event });
                        }

                        if is_first {
                            tracing::info!("Started receiving response from Claude CLI");

                            if debug_mode != DebugMode::None {
                                let elapsed = start_time.elapsed();
                                hub.broadcast(ProcessMessage::DebugInfo {
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
                            hub.broadcast(ProcessMessage::DebugInfo {
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
                            hub.broadcast(ProcessMessage::AgUi { event });
                        }

                        // Send final done signal (legacy WebSocket)
                        hub.broadcast(ProcessMessage::ChatChunk {
                            content: String::new(),
                            done: true,
                        });

                        if debug_mode != DebugMode::None {
                            let cost_str = cost
                                .map(|c| format!(" | ${:.4}", c))
                                .unwrap_or_default();
                            hub.broadcast(ProcessMessage::DebugInfo {
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
                            hub.broadcast(ProcessMessage::AgUi { event });
                        }

                        // Send error as a chat message
                        let error_msg = ChatMessage {
                            role: ChatRole::System,
                            content: format!("Error: {}", e),
                        };
                        hub.broadcast(ProcessMessage::ChatMessage { message: error_msg });

                        if debug_mode != DebugMode::None {
                            hub.broadcast(ProcessMessage::DebugInfo {
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
                            hub.broadcast(ProcessMessage::AgUi { event });
                        }

                        if debug_mode != DebugMode::None {
                            hub.broadcast(ProcessMessage::DebugInfo {
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
