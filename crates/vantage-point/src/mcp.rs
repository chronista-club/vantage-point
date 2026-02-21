//! MCP (Model Context Protocol) server implementation
//!
//! Provides tools for Claude Code to display content in browser:
//! - show: Display markdown/html/log content
//! - clear: Clear a pane
//! - permission: Handle permission requests for tool execution
//!
//! ## 通信レイヤー
//! - **Unison QUIC**: QUIC 通信（port + 1000）で Stand と接続
//! - **HTTP**: `permission` / `restart` のみ（ポーリング・プロセス管理用）

use crate::config::RunningStands;
use crate::stand::unison_server::QUIC_PORT_OFFSET;
use rmcp::{
    ErrorData as McpError, ServiceExt, handler::server::tool::ToolRouter, model::*,
    schemars::JsonSchema, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use unison::network::channel::UnisonChannel;
use unison::{ProtocolClient, UnisonClient};

use crate::protocol::{ChatComponent, StandMessage};

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

    /// Pane title (for tab display)
    #[schemars(description = "Title for the pane tab. If not provided, the pane_id is used.")]
    pub title: Option<String>,
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

/// Parameters for the toggle_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TogglePaneParams {
    /// Pane ID to toggle ("left" or "right")
    #[schemars(description = "Pane ID to toggle: 'left' for left panel, 'right' for right panel")]
    pub pane_id: String,

    /// Explicit visibility state
    #[schemars(
        description = "Set explicit visibility: true = show, false = hide. If not provided, toggles current state."
    )]
    pub visible: Option<bool>,
}

/// Parameters for the split_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SplitPaneParams {
    /// Split direction
    #[schemars(description = "Split direction: 'horizontal' or 'vertical'")]
    pub direction: String,

    /// Source pane ID to split from
    #[schemars(description = "Pane ID to split from. If not provided, splits the 'main' pane.")]
    pub source_pane_id: Option<String>,
}

/// Parameters for the close_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClosePaneParams {
    /// Pane ID to close
    #[schemars(description = "ID of the pane to close")]
    pub pane_id: String,
}

/// Response format for permission tool
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// MCP → Stand 通信クライアント
///
/// QUIC 専用ツール（show, clear, split_pane 等）は Unison Channel で通信。
/// "stand" チャネル（show/clear/toggle_pane/split_pane/close_pane）と
/// "canvas" チャネル（open/close）を遅延接続で管理する。
/// `permission` / `restart` のみ HTTP を使用（ポーリング・プロセス管理用）。
pub struct VantageMcp {
    /// HTTP クライアント（permission / restart 用）
    client: reqwest::Client,
    /// Stand の HTTP URL（permission / restart 用）
    stand_url: Arc<Mutex<String>>,
    /// Stand チャネル（show/clear/toggle_pane/split_pane/close_pane）
    stand_ch: Arc<Mutex<Option<UnisonChannel>>>,
    /// Canvas チャネル（open/close）
    canvas_ch: Arc<Mutex<Option<UnisonChannel>>>,
    /// QUIC サーバーアドレス（[::1]:port）
    quic_addr: String,
    tool_router: ToolRouter<Self>,
}

impl Clone for VantageMcp {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            stand_url: self.stand_url.clone(),
            stand_ch: self.stand_ch.clone(),
            canvas_ch: self.canvas_ch.clone(),
            quic_addr: self.quic_addr.clone(),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl VantageMcp {
    pub fn new(stand_port: u16) -> Self {
        let quic_port = stand_port + QUIC_PORT_OFFSET;
        Self {
            client: reqwest::Client::new(),
            stand_url: Arc::new(Mutex::new(format!("http://localhost:{}", stand_port))),
            stand_ch: Arc::new(Mutex::new(None)),
            canvas_ch: Arc::new(Mutex::new(None)),
            quic_addr: format!("[::1]:{}", quic_port),
            tool_router: Self::tool_router(),
        }
    }

    /// Stand / Canvas チャネルの遅延接続
    ///
    /// 初回呼び出し時に QUIC 接続を確立し、"stand" と "canvas" の
    /// 2チャネルをオープンする。2回目以降は何もしない。
    async fn ensure_channels(&self) -> Result<(), McpError> {
        let mut stand_guard = self.stand_ch.lock().await;
        if stand_guard.is_some() {
            return Ok(());
        }

        // 接続してチャネルをオープン
        match ProtocolClient::new_default() {
            Ok(mut client) => {
                if let Err(e) = UnisonClient::connect(&mut client, &self.quic_addr).await {
                    return Err(McpError::internal_error(
                        format!(
                            "Stand QUIC 接続失敗 ({}): {}. Is vp running?",
                            self.quic_addr, e
                        ),
                        None,
                    ));
                }

                let stand = client.open_channel("stand").await.map_err(|e| {
                    McpError::internal_error(format!("stand チャネルオープン失敗: {}", e), None)
                })?;
                let canvas = client.open_channel("canvas").await.map_err(|e| {
                    McpError::internal_error(format!("canvas チャネルオープン失敗: {}", e), None)
                })?;

                *stand_guard = Some(stand);
                drop(stand_guard);

                let mut canvas_guard = self.canvas_ch.lock().await;
                *canvas_guard = Some(canvas);

                Ok(())
            }
            Err(e) => Err(McpError::internal_error(
                format!("QUIC クライアント作成失敗: {}. Is vp running?", e),
                None,
            )),
        }
    }

    /// Stand チャネル経由で RPC 呼び出し
    ///
    /// UnisonChannel::request() が内部で message_id 生成・Response 待機を行う。
    /// 失敗時はチャネルをリセットして次回再接続を試みる。
    async fn stand_call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        self.ensure_channels().await?;

        let guard = self.stand_ch.lock().await;
        let ch = guard
            .as_ref()
            .ok_or_else(|| McpError::internal_error("Stand チャネル未接続", None))?;

        match ch.request(method, payload).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                // 接続断 → リセットして次回再接続
                tracing::debug!("stand チャネル '{}' 失敗: {}", method, e);
                drop(guard);
                *self.stand_ch.lock().await = None;
                *self.canvas_ch.lock().await = None;
                Err(McpError::internal_error(
                    format!(
                        "stand チャネル '{}' 失敗: {}. Stand may have restarted.",
                        method, e
                    ),
                    None,
                ))
            }
        }
    }

    /// Canvas チャネル経由で RPC 呼び出し
    ///
    /// UnisonChannel::request() が内部で message_id 生成・Response 待機を行う。
    /// 失敗時はチャネルをリセットして次回再接続を試みる。
    async fn canvas_call(&self, method: &str) -> Result<serde_json::Value, McpError> {
        self.ensure_channels().await?;

        let guard = self.canvas_ch.lock().await;
        let ch = guard
            .as_ref()
            .ok_or_else(|| McpError::internal_error("Canvas チャネル未接続", None))?;

        match ch.request(method, serde_json::json!({})).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                tracing::debug!("canvas チャネル '{}' 失敗: {}", method, e);
                drop(guard);
                *self.stand_ch.lock().await = None;
                *self.canvas_ch.lock().await = None;
                Err(McpError::internal_error(
                    format!(
                        "canvas チャネル '{}' 失敗: {}. Stand may have restarted.",
                        method, e
                    ),
                    None,
                ))
            }
        }
    }

    /// Open the Canvas window (native WebView)
    #[tool(
        description = "Open the Vantage Point Canvas window. The canvas displays the same content as the browser viewer in a native window."
    )]
    async fn open_canvas(&self) -> Result<CallToolResult, McpError> {
        let resp = self.canvas_call("open").await?;
        let status = resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("opened");
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Canvas {}",
            status
        ))]))
    }

    /// Close the Canvas window
    #[tool(description = "Close the Vantage Point Canvas window.")]
    async fn close_canvas(&self) -> Result<CallToolResult, McpError> {
        let resp = self.canvas_call("close").await?;
        let status = resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("closed");
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Canvas {}",
            status
        ))]))
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

        let payload = serde_json::json!({
            "content": &params.content,
            "content_type": &content_type,
            "pane_id": &pane_id,
            "append": append,
            "title": &params.title,
        });

        self.stand_call("show", payload).await?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Content displayed in pane '{}'",
            pane_id
        ))]))
    }

    /// Clear content in a pane
    #[tool(description = "Clear content in a specific pane of the browser viewer")]
    async fn clear(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClearParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        self.stand_call("clear", serde_json::json!({"pane_id": &pane_id}))
            .await?;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Pane '{}' cleared",
            pane_id
        ))]))
    }

    /// Toggle side panel visibility
    #[tool(
        description = "Toggle side panel visibility in the Vantage Point browser viewer. Use pane_id 'left' or 'right'."
    )]
    async fn toggle_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TogglePaneParams>,
    ) -> Result<CallToolResult, McpError> {
        let state_desc = match params.visible {
            Some(true) => "shown",
            Some(false) => "hidden",
            None => "toggled",
        };

        self.stand_call(
            "toggle_pane",
            serde_json::json!({
                "pane_id": &params.pane_id,
                "visible": params.visible,
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Pane '{}' {}",
            params.pane_id, state_desc
        ))]))
    }

    /// Split a pane into two
    #[tool(
        description = "Split a pane in the Vantage Point browser viewer. Creates a new pane by splitting an existing one horizontally or vertically."
    )]
    async fn split_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<SplitPaneParams>,
    ) -> Result<CallToolResult, McpError> {
        let source_pane_id = params.source_pane_id.unwrap_or_else(|| "main".to_string());

        // direction の検証
        match params.direction.to_lowercase().as_str() {
            "horizontal" | "h" | "vertical" | "v" => {}
            _ => {
                return Err(McpError::invalid_params(
                    "direction must be 'horizontal' or 'vertical'",
                    None,
                ));
            }
        };

        // UUID の先頭セグメントでペインIDを生成
        let new_pane_id = uuid::Uuid::new_v4().to_string();
        let new_pane_id = new_pane_id.split('-').next().unwrap_or(&new_pane_id);
        let new_pane_id = format!("pane-{}", new_pane_id);

        self.stand_call(
            "split_pane",
            serde_json::json!({
                "direction": &params.direction,
                "source_pane_id": &source_pane_id,
                "new_pane_id": &new_pane_id,
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Pane '{}' split. New pane ID: '{}'",
            source_pane_id, new_pane_id
        ))]))
    }

    /// Close a pane
    #[tool(description = "Close a pane in the Vantage Point browser viewer.")]
    async fn close_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClosePaneParams>,
    ) -> Result<CallToolResult, McpError> {
        self.stand_call(
            "close_pane",
            serde_json::json!({"pane_id": &params.pane_id}),
        )
        .await?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Pane '{}' closed",
            params.pane_id
        ))]))
    }

    /// Request permission for tool execution from user
    ///
    /// This tool is called by Claude CLI via --permission-prompt-tool flag.
    /// It sends a permission request to the WebUI and waits for user response.
    /// HTTP ポーリングベースのため QUIC 化は別タスク。
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
                format!(
                    "Failed to send permission request to Stand: {}. Is vp running?",
                    e
                ),
                None,
            ));
        }

        let send_resp = send_result.unwrap();
        if !send_resp.status().is_success() {
            return Err(McpError::internal_error(
                format!(
                    "Stand returned error on permission request: {}",
                    send_resp.status()
                ),
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
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
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
    /// HTTP ベースのプロセス管理のため QUIC は使わない。
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
            .next_back()
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
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
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
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
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
                 Use 'open_canvas' to open the native Canvas window, 'close_canvas' to close it, \
                 'show' to display content, 'clear' to clear panes, 'split_pane' to split a pane \
                 horizontally or vertically, 'close_pane' to close a pane, 'toggle_pane' to toggle panel visibility, \
                 'permission' to request user approval, and 'restart' to restart the Stand.".into()
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
