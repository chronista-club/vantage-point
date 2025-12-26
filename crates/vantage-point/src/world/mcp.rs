//! The World MCP Server
//!
//! Claude CLI から The World を操作するための MCP ツールを提供。
//!
//! ## ツール
//! - `show`: ViewPoint にコンテンツを表示
//! - `clear`: ペインをクリア
//! - `parks`: 登録済み Paisley Park 一覧
//! - `status`: The World のステータス

use rmcp::{
    ErrorData as McpError, ServiceExt, handler::server::tool::ToolRouter, model::*,
    schemars::JsonSchema, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};

use super::WORLD_PORT;

/// show ツールのパラメータ
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// 表示するコンテンツ
    #[schemars(description = "表示するコンテンツ (markdown, html, テキスト)")]
    pub content: String,

    /// コンテンツタイプ
    #[schemars(description = "コンテンツタイプ: 'markdown' (デフォルト), 'html', 'log'")]
    pub content_type: Option<String>,

    /// ペイン ID
    #[schemars(description = "表示先ペイン ID (デフォルト: 'main')")]
    pub pane_id: Option<String>,

    /// 追記モード
    #[schemars(description = "既存コンテンツに追記 (デフォルト: false)")]
    pub append: Option<bool>,
}

/// clear ツールのパラメータ
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClearParams {
    /// ペイン ID
    #[schemars(description = "クリアするペイン ID (デフォルト: 'main')")]
    pub pane_id: Option<String>,
}

/// toggle_pane ツールのパラメータ
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TogglePaneParams {
    /// パネル ID ("left" or "right")
    #[schemars(description = "トグルするパネル: 'left' または 'right'")]
    pub pane_id: String,

    /// 明示的な表示状態
    #[schemars(description = "明示的な表示状態: true=表示, false=非表示 (省略時はトグル)")]
    pub visible: Option<bool>,
}

/// The World MCP ツールハンドラー
struct WorldTools {
    client: reqwest::Client,
    base_url: String,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WorldTools {
    fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            base_url: format!("http://localhost:{}", WORLD_PORT),
            tool_router: Self::tool_router(),
        }
    }

    /// ViewPoint にコンテンツを表示
    #[tool(description = "ViewPoint にマークダウン/HTML/ログを表示")]
    async fn show(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ShowParams>,
    ) -> Result<CallToolResult, McpError> {
        #[derive(Serialize)]
        struct Request {
            pane_id: String,
            content_type: String,
            content: String,
            append: bool,
        }

        let req = Request {
            pane_id: params.pane_id.unwrap_or_else(|| "main".to_string()),
            content_type: params
                .content_type
                .unwrap_or_else(|| "markdown".to_string()),
            content: params.content,
            append: params.append.unwrap_or(false),
        };

        let url = format!("{}/api/view/show", self.base_url);
        let result = self.client.post(&url).json(&req).send().await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "表示しました (pane: {})",
                    req.pane_id
                ))]))
            }
            Ok(resp) => {
                let status = resp.status();
                Err(McpError::internal_error(
                    format!("API エラー: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("HTTP エラー: {}. The World は稼働していますか？", e),
                None,
            )),
        }
    }

    /// ペインをクリア
    #[tool(description = "ViewPoint のペインをクリア")]
    async fn clear(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClearParams>,
    ) -> Result<CallToolResult, McpError> {
        #[derive(Serialize)]
        struct Request {
            pane_id: String,
        }

        let req = Request {
            pane_id: params.pane_id.unwrap_or_else(|| "main".to_string()),
        };

        let url = format!("{}/api/view/clear", self.base_url);
        let result = self.client.post(&url).json(&req).send().await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "クリアしました (pane: {})",
                    req.pane_id
                ))]))
            }
            Ok(resp) => {
                let status = resp.status();
                Err(McpError::internal_error(
                    format!("API エラー: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("HTTP エラー: {}", e),
                None,
            )),
        }
    }

    /// パネル表示をトグル
    #[tool(description = "左/右パネルの表示をトグル")]
    async fn toggle_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TogglePaneParams>,
    ) -> Result<CallToolResult, McpError> {
        // WebSocket 経由で送信が必要なので、現時点では未実装
        Ok(CallToolResult::success(vec![Content::text(format!(
            "toggle_pane: {} (visible: {:?}) - WebSocket 経由で実装予定",
            params.pane_id, params.visible
        ))]))
    }

    /// 登録済み Paisley Park 一覧
    #[tool(description = "The World に登録されている Paisley Park の一覧")]
    async fn parks(&self) -> Result<CallToolResult, McpError> {
        let url = format!("{}/api/parks", self.base_url);
        let result = self.client.get(&url).send().await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                Ok(CallToolResult::success(vec![Content::text(body)]))
            }
            Ok(resp) => {
                let status = resp.status();
                Err(McpError::internal_error(
                    format!("API エラー: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("HTTP エラー: {}", e),
                None,
            )),
        }
    }

    /// The World のステータス
    #[tool(description = "The World のステータスを取得")]
    async fn status(&self) -> Result<CallToolResult, McpError> {
        let url = format!("{}/api/status", self.base_url);
        let result = self.client.get(&url).send().await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                Ok(CallToolResult::success(vec![Content::text(body)]))
            }
            Ok(resp) => {
                let status = resp.status();
                Err(McpError::internal_error(
                    format!("API エラー: {}", status),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("HTTP エラー: {}", e),
                None,
            )),
        }
    }
}

#[tool_handler]
impl rmcp::ServerHandler for WorldTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "The World - ViewPoint にコンテンツを表示するための MCP Server。\
                 'show' でマークダウン/HTML/ログを表示、'clear' でペインをクリア、\
                 'parks' で登録済み Paisley Park 一覧、'status' でステータスを取得。"
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// MCP Server を起動（stdio）
pub async fn run_mcp_server() -> anyhow::Result<()> {
    let service = WorldTools::new()
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    service.waiting().await?;

    Ok(())
}
