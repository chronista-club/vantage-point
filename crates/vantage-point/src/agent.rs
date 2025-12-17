//! Agent module - Claude CLI integration for chat functionality
//!
//! Uses `claude -p` with `--output-format stream-json` for structured responses
//! and `--resume` for session continuity.

use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Message types for agent communication
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Session initialized with full info
    SessionInit {
        session_id: String,
        model: Option<String>,
        tools: Vec<String>,
        mcp_servers: Vec<String>,
    },
    /// A chunk of text content
    TextChunk(String),
    /// Tool execution started
    ToolExecuting { name: String },
    /// Tool execution completed
    ToolResult { name: String, preview: String },
    /// Stream completed with final result
    Done { result: String, cost: Option<f64> },
    /// Error occurred
    Error(String),
}

/// Configuration for Claude agent
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    /// Working directory for the agent
    pub working_dir: Option<String>,
    /// Session ID to resume (if continuing conversation)
    pub session_id: Option<String>,
    /// Use --continue flag (resume most recent session)
    pub use_continue: bool,
    /// Model to use (e.g., "sonnet", "opus", "haiku")
    pub model: Option<String>,
    /// System prompt
    pub system_prompt: Option<String>,
    /// Allowed tools (empty = default tools)
    pub allowed_tools: Vec<String>,
    /// MCP config file path (uses Claude's default if not set)
    pub mcp_config: Option<String>,
}

/// Agent that communicates with Claude CLI
#[derive(Clone)]
pub struct ClaudeAgent {
    config: AgentConfig,
}

impl ClaudeAgent {
    pub fn new() -> Self {
        Self {
            config: AgentConfig::default(),
        }
    }

    pub fn with_config(config: AgentConfig) -> Self {
        Self { config }
    }

    /// Set session ID for conversation continuity
    pub fn with_session(mut self, session_id: String) -> Self {
        self.config.session_id = Some(session_id);
        self
    }

    /// Set working directory
    pub fn with_working_dir(mut self, dir: String) -> Self {
        self.config.working_dir = Some(dir);
        self
    }

    /// Set model
    pub fn with_model(mut self, model: String) -> Self {
        self.config.model = Some(model);
        self
    }

    /// Send a message to Claude CLI and stream the response
    pub async fn chat(&self, message: &str) -> mpsc::Receiver<AgentEvent> {
        let (tx, rx) = mpsc::channel(100);
        let message = message.to_string();
        let config = self.config.clone();

        tokio::spawn(async move {
            let result = run_claude_cli(&message, &config, tx.clone()).await;
            if let Err(e) = result {
                let _ = tx.send(AgentEvent::Error(e.to_string())).await;
            }
        });

        rx
    }
}

impl Default for ClaudeAgent {
    fn default() -> Self {
        Self::new()
    }
}

/// Claude CLI JSON message types
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ClaudeMessage {
    System {
        session_id: String,
        #[serde(default)]
        subtype: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        tools: Option<Vec<String>>,
        #[serde(default)]
        mcp_servers: Option<Vec<McpServerInfo>>,
    },
    Assistant {
        message: AssistantMessage,
        session_id: String,
    },
    Result {
        #[serde(default)]
        result: Option<String>,
        session_id: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default)]
        total_cost_usd: Option<f64>,
    },
}

/// MCP server info from Claude CLI
#[derive(Debug, serde::Deserialize)]
struct McpServerInfo {
    name: String,
    #[serde(default)]
    status: String,
}

#[derive(Debug, serde::Deserialize)]
struct AssistantMessage {
    content: Vec<ContentBlock>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
    ToolResult {
        tool_use_id: String,
    },
    #[serde(other)]
    Other,
}

/// Run claude CLI with stream-json output and parse responses
async fn run_claude_cli(
    prompt: &str,
    config: &AgentConfig,
    tx: mpsc::Sender<AgentEvent>,
) -> anyhow::Result<()> {
    let mut cmd = Command::new("claude");

    // Use print mode with stream-json output
    cmd.arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");

    // Resume session: --continue for most recent, --resume <id> for specific
    if config.use_continue {
        cmd.arg("--continue");
    } else if let Some(ref session_id) = config.session_id {
        cmd.arg("--resume").arg(session_id);
    }

    // Set model if specified
    if let Some(ref model) = config.model {
        cmd.arg("--model").arg(model);
    }

    // Set system prompt if specified
    if let Some(ref system_prompt) = config.system_prompt {
        cmd.arg("--system-prompt").arg(system_prompt);
    }

    // Set allowed tools if specified
    if !config.allowed_tools.is_empty() {
        cmd.arg("--allowedTools")
            .arg(config.allowed_tools.join(","));
    }

    // Set MCP config file if specified
    if let Some(ref mcp_config) = config.mcp_config {
        cmd.arg("--mcp-config").arg(mcp_config);
    }

    // Add the prompt
    cmd.arg(prompt);

    // Set working directory if specified
    if let Some(ref dir) = config.working_dir {
        cmd.current_dir(dir);
    }

    // Configure stdio
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    tracing::info!(
        "Starting Claude CLI (session: {:?}, mcp_config: {:?})",
        config.session_id.as_deref().unwrap_or("new"),
        config.mcp_config.as_deref().unwrap_or("default")
    );

    let mut child = cmd.spawn().map_err(|e| {
        anyhow::anyhow!(
            "Failed to spawn Claude CLI. Is 'claude' installed and in PATH? Error: {}",
            e
        )
    })?;

    let stdout = child.stdout.take().expect("stdout not captured");
    let stderr = child.stderr.take().expect("stderr not captured");

    // Track the last text we sent to avoid duplicates
    let mut last_text = String::new();

    // Read stdout line by line (each line is a JSON message)
    let tx_stdout = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut session_id_sent = false;

        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<ClaudeMessage>(&line) {
                Ok(msg) => match msg {
                    ClaudeMessage::System {
                        session_id,
                        subtype,
                        model,
                        tools,
                        mcp_servers,
                    } => {
                        // Only send init event once, for the "init" subtype
                        if !session_id_sent && subtype.as_deref() == Some("init") {
                            let mcp_names: Vec<String> = mcp_servers
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|s| s.status == "connected")
                                .map(|s| s.name)
                                .collect();

                            let tools_list = tools.unwrap_or_default();
                            tracing::info!(
                                "Claude CLI init: session={}, model={:?}, tools={}, mcp_servers={}",
                                session_id,
                                model,
                                tools_list.len(),
                                mcp_names.len()
                            );

                            let _ = tx_stdout
                                .send(AgentEvent::SessionInit {
                                    session_id: session_id.clone(),
                                    model,
                                    tools: tools_list,
                                    mcp_servers: mcp_names,
                                })
                                .await;
                            session_id_sent = true;
                        }
                    }
                    ClaudeMessage::Assistant { message, .. } => {
                        // Extract text and tool events from content blocks
                        for block in message.content {
                            match block {
                                ContentBlock::Text { text } => {
                                    // Send incremental text (new content only)
                                    if text.len() > last_text.len() && text.starts_with(&last_text)
                                    {
                                        let new_text = &text[last_text.len()..];
                                        let _ = tx_stdout
                                            .send(AgentEvent::TextChunk(new_text.to_string()))
                                            .await;
                                    } else if text != last_text {
                                        // Text changed completely, send all
                                        let _ = tx_stdout
                                            .send(AgentEvent::TextChunk(text.clone()))
                                            .await;
                                    }
                                    last_text = text;
                                }
                                ContentBlock::ToolUse { name, .. } => {
                                    let _ =
                                        tx_stdout.send(AgentEvent::ToolExecuting { name }).await;
                                }
                                ContentBlock::ToolResult { .. } | ContentBlock::Other => {}
                            }
                        }
                    }
                    ClaudeMessage::Result {
                        result,
                        is_error,
                        total_cost_usd,
                        ..
                    } => {
                        if is_error {
                            let _ = tx_stdout
                                .send(AgentEvent::Error(result.unwrap_or_default()))
                                .await;
                        } else {
                            let _ = tx_stdout
                                .send(AgentEvent::Done {
                                    result: result.unwrap_or_default(),
                                    cost: total_cost_usd,
                                })
                                .await;
                        }
                    }
                },
                Err(e) => {
                    tracing::debug!("Failed to parse Claude message: {} - line: {}", e, line);
                }
            }
        }
    });

    // Read stderr (for debugging)
    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!("Claude CLI stderr: {}", line);
        }
    });

    // Wait for process to complete
    let status = child.wait().await?;

    // Wait for output tasks
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if !status.success() {
        let _ = tx
            .send(AgentEvent::Error(format!(
                "Claude CLI exited with status: {}",
                status
            )))
            .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires claude CLI to be installed
    async fn test_claude_agent_with_session() {
        let agent = ClaudeAgent::new();
        let mut rx = agent.chat("Say hello").await;

        let mut session_id = None;
        let mut output = String::new();
        let mut tools_count = 0;
        let mut mcp_count = 0;

        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::SessionInit {
                    session_id: sid,
                    model,
                    tools,
                    mcp_servers,
                } => {
                    println!("Session ID: {}", sid);
                    println!("Model: {:?}", model);
                    println!("Tools: {} ({:?})", tools.len(), tools);
                    println!("MCP Servers: {} ({:?})", mcp_servers.len(), mcp_servers);
                    session_id = Some(sid);
                    tools_count = tools.len();
                    mcp_count = mcp_servers.len();
                }
                AgentEvent::TextChunk(chunk) => {
                    output.push_str(&chunk);
                }
                AgentEvent::ToolExecuting { name } => {
                    println!("Tool executing: {}", name);
                }
                AgentEvent::ToolResult { name, preview } => {
                    println!("Tool result: {} - {}", name, preview);
                }
                AgentEvent::Done { result, cost } => {
                    println!("Done! Result: {}, Cost: {:?}", result, cost);
                    break;
                }
                AgentEvent::Error(e) => {
                    panic!("Error: {}", e);
                }
            }
        }

        assert!(session_id.is_some());
        println!("Output: {}", output);
        println!("Tools: {}, MCP: {}", tools_count, mcp_count);
    }
}
