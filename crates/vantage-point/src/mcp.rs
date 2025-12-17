//! MCP (Model Context Protocol) server implementation
//!
//! Provides tools for Claude Code to display content in browser:
//! - show: Display markdown/html/log content
//! - clear: Clear a pane

use rmcp::{
    ErrorData as McpError, ServiceExt, handler::server::tool::ToolRouter, model::*,
    schemars::JsonSchema, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::protocol::{Content as DaemonContent, DaemonMessage};

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

/// HTTP client for communicating with the daemon's HTTP server
#[derive(Clone)]
pub struct VantageMcp {
    client: reqwest::Client,
    daemon_url: Arc<Mutex<String>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl VantageMcp {
    pub fn new(daemon_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            daemon_url: Arc::new(Mutex::new(format!("http://localhost:{}", daemon_port))),
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
            "html" => DaemonContent::Html(params.content),
            "log" => DaemonContent::Log(params.content),
            _ => DaemonContent::Markdown(params.content),
        };

        let message = DaemonMessage::Show {
            pane_id: pane_id.clone(),
            content: content_enum,
            append,
        };

        let url = self.daemon_url.lock().await;
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
                    format!("Daemon returned error: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to connect to daemon: {}. Is vp running?", e),
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

        let message = DaemonMessage::Clear {
            pane_id: pane_id.clone(),
        };

        let url = self.daemon_url.lock().await;
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
                    format!("Daemon returned error: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to connect to daemon: {}", e),
                None,
            )),
        }
    }
}

#[tool_handler]
impl rmcp::ServerHandler for VantageMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Vantage Point daemon - Display rich content (markdown, HTML, images) in a browser viewer. \
                 Use 'show' to display content and 'clear' to clear panes.".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Run the MCP server over stdio
pub async fn run_mcp_server(daemon_port: u16) -> anyhow::Result<()> {
    // Note: In MCP mode, we should not use tracing to stdout
    // as it interferes with JSON-RPC communication
    let service = VantageMcp::new(daemon_port)
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    service.waiting().await?;

    Ok(())
}
