//! MCP (Model Context Protocol) server implementation
//!
//! Provides tools for Claude Code to display content in browser:
//! - show: Display markdown/html/log content
//! - clear: Clear a pane
//! - permission: Handle permission requests for tool execution

use crate::config::RunningStands;
use rmcp::{
    ErrorData as McpError, ServiceExt, handler::server::tool::ToolRouter, model::*,
    schemars::JsonSchema, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::protocol::{ChatComponent, Content as StandContent, StandMessage};

/// Parameters for the show tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// Content to display
    #[schemars(description = "Content to display (markdown, html, or plain text)")]
    pub content: String,

    /// Content type (markdown, html, log)
    #[schemars(description = "Content type: 'markdown' (default), 'html', or 'log'")]
    pub content_type: Option<String>,

    /// Pane ID
    #[schemars(description = "Pane ID to display content in (default: 'main')")]
    pub pane_id: Option<String>,

    /// Append mode
    #[schemars(description = "Append to existing content instead of replacing")]
    pub append: Option<bool>,
}

/// Parameters for the clear tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClearParams {
    /// Pane ID to clear
    #[schemars(description = "Pane ID to clear (default: 'main')")]
    pub pane_id: Option<String>,
}

/// Parameters for the permission tool (Claude CLI --permission-prompt-tool)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PermissionParams {
    /// Tool name that Claude wants to execute
    #[schemars(description = "Name of the tool Claude wants to execute")]
    pub tool_name: String,

    /// Tool input parameters (passed through from Claude CLI)
    #[schemars(description = "Input parameters for the tool (JSON object)")]
    pub input: serde_json::Value,
}

/// Parameters for the restart tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RestartParams {
    /// Whether to open WebView after restart (default: false for headless)
    #[schemars(description = "Open WebView window after restart (default: false)")]
    pub open_viewer: Option<bool>,
}

/// Response format for permission tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    /// Behavior: "allow" or "deny"
    pub behavior: String,
    /// Updated input parameters (optional, for "allow" response)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<serde_json::Value>,
    /// Message (optional, for "deny" response)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Permission request sent to Stand
#[derive(Debug, Serialize, Deserialize)]
pub struct PermissionRequestPayload {
    pub request_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub timeout_seconds: u32,
}

/// HTTP client for communicating with the Stand's HTTP server
#[derive(Clone)]
pub struct VantageMcp {
    client: reqwest::Client,
    stand_url: Arc<Mutex<String>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl VantageMcp {
    pub fn new(stand_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            stand_url: Arc::new(Mutex::new(format!("http://localhost:{}", stand_port))),
            tool_router: Self::tool_router(),
        }
    }

    /// Show content in the browser viewer
    #[tool(
        description = "Display content in the Vantage Point browser viewer. Supports markdown, html, and log formats."
    )]
    async fn show(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());
        let content_type = params
            .content_type
            .unwrap_or_else(|| "markdown".to_string());
        let append = params.append.unwrap_or(false);

        let content_enum = match content_type.as_str() {
            "html" => StandContent::Html(params.content),
            "log" => StandContent::Log(params.content),
            _ => StandContent::Markdown(params.content),
        };

        let message = StandMessage::Show {
            pane_id: pane_id.clone(),
            content: content_enum,
            append,
        };

        let url = self.stand_url.lock().await;
        let result = self
            .client
            .post(format!("{}/api/show", *url))
            .json(&message)
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Content displayed in pane '{}'",
                    pane_id
                ))]))
            }
            Ok(resp) => {
                let status = resp.status();
                Err(McpError::internal_error(
                    format!("Stand returned error: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to connect to Stand: {}. Is vp running?", e),
                None,
            )),
        }
    }

    /// Clear content in a pane
    #[tool(description = "Clear content in a specific pane of the browser viewer")]
    async fn clear(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClearParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let message = StandMessage::Clear {
            pane_id: pane_id.clone(),
        };

        let url = self.stand_url.lock().await;
        let result = self
            .client
            .post(format!("{}/api/show", *url))
            .json(&message)
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Pane '{}' cleared",
                    pane_id
                ))]))
            }
            Ok(resp) => {
                let status = resp.status();
                Err(McpError::internal_error(
                    format!("Stand returned error: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to connect to Stand: {}", e),
                None,
            )),
        }
    }

    /// Request permission for tool execution from user
    ///
    /// This tool is called by Claude CLI via --permission-prompt-tool flag.
    /// It sends a permission request to the WebUI and waits for user response.
    #[tool(
        description = "Request permission for tool execution from user. Returns JSON with 'behavior' (allow/deny) and optional 'updatedInput' or 'message'."
    )]
    async fn permission(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<PermissionParams>,
    ) -> Result<CallToolResult, McpError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let timeout_seconds: u32 = 60; // 60 seconds timeout

        // Create the ChatComponent for permission request
        let component = ChatComponent::PermissionRequest {
            request_id: request_id.clone(),
            tool_name: params.tool_name.clone(),
            description: None,
            input: params.input.clone(),
            timeout_seconds,
        };

        // Create the StandMessage
        let message = StandMessage::ChatComponent {
            component,
            interactive: true,
        };

        let url = self.stand_url.lock().await;

        // First, send the permission request to the Stand
        let send_result = self
            .client
            .post(format!("{}/api/permission", *url))
            .json(&message)
            .send()
            .await;

        if let Err(e) = send_result {
            return Err(McpError::internal_error(
                format!("Failed to send permission request to Stand: {}. Is vp running?", e),
                None,
            ));
        }

        let send_resp = send_result.unwrap();
        if !send_resp.status().is_success() {
            return Err(McpError::internal_error(
                format!("Stand returned error on permission request: {}", send_resp.status()),
                None,
            ));
        }

        // Now poll for the response with timeout
        let poll_url = format!("{}/api/permission/{}", *url, request_id);
        let timeout = Duration::from_secs(timeout_seconds as u64);
        let poll_interval = Duration::from_millis(500);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                // Timeout - deny by default
                let response = PermissionResponse {
                    behavior: "deny".to_string(),
                    updated_input: None,
                    message: Some("Permission request timed out".to_string()),
                };
                return Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&response).unwrap(),
                )]));
            }

            // Poll for response
            let poll_result = self.client.get(&poll_url).send().await;

            match poll_result {
                Ok(resp) if resp.status().is_success() => {
                    // Got a response
                    match resp.json::<PermissionResponse>().await {
                        Ok(permission_resp) => {
                            return Ok(CallToolResult::success(vec![Content::text(
                                serde_json::to_string(&permission_resp).unwrap(),
                            )]));
                        }
                        Err(e) => {
                            return Err(McpError::internal_error(
                                format!("Failed to parse permission response: {}", e),
                                None,
                            ));
                        }
                    }
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                    // Response not ready yet, continue polling
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::ACCEPTED => {
                    // Still waiting for user response
                }
                Ok(resp) => {
                    return Err(McpError::internal_error(
                        format!("Unexpected response from Stand: {}", resp.status()),
                        None,
                    ));
                }
                Err(e) => {
                    return Err(McpError::internal_error(
                        format!("Failed to poll permission response: {}", e),
                        None,
                    ));
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Restart the Vantage Point Stand
    ///
    /// This tool restarts the Stand process while preserving session state.
    /// Useful after rebuilding the binary.
    #[tool(
        description = "Restart the Vantage Point Stand. Session state is preserved. Returns when Stand is ready."
    )]
    async fn restart(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<RestartParams>,
    ) -> Result<CallToolResult, McpError> {
        let url = self.stand_url.lock().await;
        let base_url = url.clone();
        drop(url);

        // Extract port from URL
        let port: u16 = base_url
            .split(':')
            .last()
            .and_then(|s| s.parse().ok())
            .unwrap_or(33000);

        // 1. Get current Stand info (project_dir)
        let health_url = format!("{}/api/health", base_url);
        let health_resp = self.client.get(&health_url).send().await.map_err(|e| {
            McpError::internal_error(format!("Failed to get Stand health: {}", e), None)
        })?;

        let health: serde_json::Value = health_resp.json().await.map_err(|e| {
            McpError::internal_error(format!("Failed to parse health response: {}", e), None)
        })?;

        let project_dir = health
            .get("project_dir")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();

        // 2. Send shutdown request
        let shutdown_url = format!("{}/api/shutdown", base_url);
        let _ = self.client.post(&shutdown_url).send().await;

        // 3. Wait for Stand to stop (poll health endpoint)
        let stop_timeout = Duration::from_secs(10);
        let poll_interval = Duration::from_millis(200);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > stop_timeout {
                return Err(McpError::internal_error(
                    "Timeout waiting for Stand to stop".to_string(),
                    None,
                ));
            }

            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    // Still running, wait
                    tokio::time::sleep(poll_interval).await;
                }
                _ => {
                    // Stand is down
                    break;
                }
            }
        }

        // 4. Start new Stand process
        let open_viewer = params.open_viewer.unwrap_or(false);
        let mut cmd = std::process::Command::new("vp");
        cmd.arg("start")
            .arg("-C")
            .arg(&project_dir)
            .arg("-p")
            .arg(port.to_string());

        if !open_viewer {
            cmd.arg("--headless");
        }

        // Spawn detached process
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        cmd.spawn().map_err(|e| {
            McpError::internal_error(format!("Failed to spawn new Stand: {}", e), None)
        })?;

        // 5. Wait for new Stand to be ready
        let start_timeout = Duration::from_secs(15);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > start_timeout {
                return Err(McpError::internal_error(
                    "Timeout waiting for Stand to start".to_string(),
                    None,
                ));
            }

            tokio::time::sleep(poll_interval).await;

            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    // Stand is up
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "Stand restarted successfully on port {}. Project: {}",
                        port, project_dir
                    ))]));
                }
                _ => {
                    // Not ready yet, continue polling
                }
            }
        }
    }
}

#[tool_handler]
impl rmcp::ServerHandler for VantageMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Vantage Point Stand - Display rich content (markdown, HTML, images) in a browser viewer. \
                 Use 'show' to display content, 'clear' to clear panes, 'permission' to request user approval, \
                 and 'restart' to restart the Stand (useful after rebuilding the binary).".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Resolve Stand port for MCP communication
///
/// Priority:
/// 1. Explicit port argument (if provided and != 33000)
/// 2. running.json lookup by current working directory
/// 3. Default port (33000)
fn resolve_stand_port(explicit_port: u16) -> u16 {
    // If an explicit port was provided (not the default), use it
    if explicit_port != 33000 {
        return explicit_port;
    }

    // Try to find a running Stand for the current directory
    if let Some(stand_info) = RunningStands::find_for_cwd() {
        return stand_info.port;
    }

    // Fall back to default
    33000
}

/// Run the MCP server over stdio
pub async fn run_mcp_server(stand_port: u16) -> anyhow::Result<()> {
    // Resolve the actual port to use
    let resolved_port = resolve_stand_port(stand_port);

    // Note: In MCP mode, we should not use tracing to stdout
    // as it interferes with JSON-RPC communication
    let service = VantageMcp::new(resolved_port)
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    service.waiting().await?;

    Ok(())
}
