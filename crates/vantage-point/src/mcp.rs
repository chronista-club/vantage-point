//! MCP (Model Context Protocol) server implementation
//!
//! Provides tools for Claude Code to display content in browser:
//! - show: Display markdown/html/log content
//! - clear: Clear a pane
//! - permission: Handle permission requests for tool execution
//!
//! ## 通信レイヤー
//! process / canvas チャネルは Unison QUIC で通信。
//! Ruby VM / capture / permission 等の一部 API は HTTP フォールバック。

use crate::config::RunningProcesses;
use rmcp::{
    ErrorData as McpError, ServiceExt, handler::server::tool::ToolRouter, model::*,
    schemars::JsonSchema, tool, tool_handler, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::protocol::{ChatComponent, ProcessMessage};

/// Parameters for the show tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// Content to display
    #[schemars(description = "Content to display (markdown, html, or plain text)")]
    pub content: String,

    /// Content type (markdown, html, log, url)
    #[schemars(
        description = "Content type: 'markdown' (default), 'html', 'log', or 'url' (display a web page in an iframe)"
    )]
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

/// Parameters for the watch_file tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WatchFileParams {
    /// File path to watch
    #[schemars(description = "Absolute path to the log file to watch")]
    pub path: String,

    /// Pane ID to display logs in
    #[schemars(description = "Pane ID to display watched logs in")]
    pub pane_id: String,

    /// Log format
    #[schemars(description = "Log format: 'json_lines' (default) or 'plain'")]
    pub format: Option<String>,

    /// Level filter regex
    #[schemars(description = "Regex to filter log levels, e.g. 'INFO|WARN|ERROR'")]
    pub filter: Option<String>,

    /// Targets to exclude
    #[schemars(description = "List of target names to exclude from display")]
    pub exclude_targets: Option<Vec<String>>,

    /// Pane title
    #[schemars(description = "Title for the pane tab")]
    pub title: Option<String>,

    /// Display style
    #[schemars(description = "Display style: 'terminal' (default) or 'plain'")]
    pub style: Option<String>,
}

/// Parameters for the unwatch_file tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UnwatchFileParams {
    /// Pane ID to stop watching
    #[schemars(description = "Pane ID to stop file watching for")]
    pub pane_id: String,
}

/// Parameters for the eval_ruby tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EvalRubyParams {
    /// Ruby code to execute
    #[schemars(description = "Ruby code to execute (mutually exclusive with 'file')")]
    pub code: Option<String>,

    /// Ruby file path to execute (relative to project dir)
    #[schemars(
        description = "Ruby file path to execute, relative to project directory (mutually exclusive with 'code')"
    )]
    pub file: Option<String>,

    /// Pane ID to display results in
    #[schemars(description = "Pane ID to display results in (default: 'main')")]
    pub pane_id: Option<String>,
}

/// Parameters for the run_ruby tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RunRubyParams {
    /// Ruby code to run as daemon
    #[schemars(
        description = "Ruby code to run as a long-running daemon process (mutually exclusive with 'file')"
    )]
    pub code: Option<String>,

    /// Ruby file path to run as daemon (relative to project dir)
    #[schemars(
        description = "Ruby file path to run as daemon, relative to project directory (mutually exclusive with 'code')"
    )]
    pub file: Option<String>,

    /// Process display name
    #[schemars(description = "Display name for the process (default: filename or 'daemon')")]
    pub name: Option<String>,

    /// Pane ID to stream output to
    #[schemars(description = "Pane ID to stream output to (default: 'main')")]
    pub pane_id: Option<String>,
}

/// Parameters for the stop_ruby tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StopRubyParams {
    /// Process ID to stop
    #[schemars(
        description = "Ruby process ID to stop (e.g. 'rb-0001'). Use list_ruby to see running processes."
    )]
    pub process_id: String,
}

/// Parameters for the capture_canvas tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptureCanvasParams {
    /// Save path
    #[schemars(
        description = "Save path for the PNG screenshot (default: /tmp/vp-canvas-{timestamp}.png)"
    )]
    pub path: Option<String>,

    /// Capture specific pane only
    #[schemars(description = "Capture only a specific pane by its pane_id")]
    pub pane_id: Option<String>,
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

/// Permission request sent to Process
#[derive(Debug, Serialize, Deserialize)]
pub struct PermissionRequestPayload {
    pub request_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub timeout_seconds: u32,
}

/// MCP → Process 通信クライアント
///
/// Unison QUIC で Process と通信する。
/// process / canvas チャネルは lazy 接続し、persistent に保持。
/// Ruby / capture / permission 等の未対応メソッドは HTTP フォールバック。
pub struct VantageMcp {
    /// HTTP クライアント（QUIC 未対応の API 用フォールバック）
    client: reqwest::Client,
    /// Process の HTTP ベース URL
    process_url: Arc<Mutex<String>>,
    /// Process の HTTP ポート番号（QUIC = port + QUIC_PORT_OFFSET）
    process_port: Arc<Mutex<u16>>,
    /// Unison "process" チャネル（lazy 接続、canvas 操作も含む）
    process_channel: Arc<Mutex<Option<Arc<unison::network::channel::UnisonChannel>>>>,
    tool_router: ToolRouter<Self>,
}

impl Clone for VantageMcp {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            process_url: self.process_url.clone(),
            process_port: self.process_port.clone(),
            process_channel: self.process_channel.clone(),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl VantageMcp {
    pub fn new(process_port: u16) -> Self {
        Self {
            client: reqwest::Client::new(),
            process_url: Arc::new(Mutex::new(format!("http://[::1]:{}", process_port))),
            process_port: Arc::new(Mutex::new(process_port)),
            process_channel: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    /// Process に HTTP POST でメッセージを送信
    ///
    /// `endpoint` は `/api/show` 等の API パス。
    /// `body` は JSON シリアライズ可能なペイロード。
    ///
    /// 接続失敗時は Process ポートを再解決してリトライする（lazy reconnect）。
    async fn http_post(
        &self,
        endpoint: &str,
        body: &impl Serialize,
    ) -> Result<serde_json::Value, McpError> {
        use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

        let tid = new_trace_id();
        let start = std::time::Instant::now();
        let url = format!("{}{}", self.process_url.lock().await, endpoint);

        write_trace(
            &TraceEntry::new("mcp", &tid, "request", "INFO", format!("POST {}", endpoint))
                .with_data(serde_json::to_value(body).unwrap_or_default()),
        );

        let resp = match self
            .client
            .post(&url)
            .json(body)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_connect() => {
                // 接続失敗 → ポートを再解決してリトライ
                let new_url = self.try_reconnect(endpoint).await;
                if let Some(retry_url) = new_url {
                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "reconnect",
                        "INFO",
                        format!("Process 再検出: {}", retry_url),
                    ));
                    self.client
                        .post(&retry_url)
                        .json(body)
                        .timeout(Duration::from_secs(10))
                        .send()
                        .await
                        .map_err(|e2| {
                            McpError::internal_error(
                                format!("Process 通信失敗 ({}): {}. Is vp running?", endpoint, e2),
                                None,
                            )
                        })?
                } else if let Some(auto_url) = self.auto_start_process(endpoint).await {
                    // running.json にも見つからない → Process を自動起動
                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "auto_start",
                        "INFO",
                        format!("Process 自動起動後リトライ: {}", auto_url),
                    ));
                    self.client
                        .post(&auto_url)
                        .json(body)
                        .timeout(Duration::from_secs(10))
                        .send()
                        .await
                        .map_err(|e2| {
                            McpError::internal_error(
                                format!("Process 通信失敗 ({}): {}. Process auto-start succeeded but request failed.", endpoint, e2),
                                None,
                            )
                        })?
                } else {
                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "error",
                        "ERROR",
                        format!("POST {} 失敗（自動起動も失敗）: {}", endpoint, e),
                    ));
                    return Err(McpError::internal_error(
                        format!(
                            "Process 通信失敗 ({}): {}. Auto-start failed. Run `vp start` manually.",
                            endpoint, e
                        ),
                        None,
                    ));
                }
            }
            Err(e) => {
                write_trace(&TraceEntry::new(
                    "mcp",
                    &tid,
                    "error",
                    "ERROR",
                    format!("POST {} 失敗: {}", endpoint, e),
                ));
                return Err(McpError::internal_error(
                    format!("Process 通信失敗 ({}): {}. Is vp running?", endpoint, e),
                    None,
                ));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            write_trace(&TraceEntry::new(
                "mcp",
                &tid,
                "error",
                "ERROR",
                format!("POST {} HTTP {}", endpoint, status),
            ));
            return Err(McpError::internal_error(
                format!("Process returned HTTP {}: {}", status, endpoint),
                None,
            ));
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            McpError::internal_error(format!("レスポンスのパースに失敗: {}", e), None)
        })?;

        write_trace(
            &TraceEntry::new(
                "mcp",
                &tid,
                "response",
                "INFO",
                format!("POST {} OK", endpoint),
            )
            .with_elapsed(start.elapsed().as_millis() as u64),
        );

        Ok(json)
    }

    /// Unison QUIC チャネルを取得（lazy 接続）
    ///
    /// チャネルが未接続または切断済みの場合、新規接続して返す。
    async fn get_quic_channel(
        &self,
        channel_slot: &Arc<Mutex<Option<Arc<unison::network::channel::UnisonChannel>>>>,
        channel_name: &str,
    ) -> Result<Arc<unison::network::channel::UnisonChannel>, McpError> {
        let mut guard = channel_slot.lock().await;

        // 既存チャネルがあれば再利用
        if let Some(ch) = guard.as_ref() {
            return Ok(Arc::clone(ch));
        }

        // 新規接続
        let port = *self.process_port.lock().await;
        let quic_port = port + crate::process::unison_server::QUIC_PORT_OFFSET;
        let addr = format!("[::1]:{}", quic_port);

        let client = unison::ProtocolClient::new_default()
            .map_err(|e| McpError::internal_error(format!("Unison client error: {}", e), None))?;
        client.connect(&addr).await.map_err(|e| {
            McpError::internal_error(format!("Unison connect error ({}): {}", addr, e), None)
        })?;
        let channel = Arc::new(client.open_channel(channel_name).await.map_err(|e| {
            McpError::internal_error(
                format!("Unison {} channel error: {}", channel_name, e),
                None,
            )
        })?);

        *guard = Some(Arc::clone(&channel));
        Ok(channel)
    }

    /// Unison QUIC の "process" チャネルでメソッドを呼び出す
    ///
    /// 接続失敗時はチャネルをリセットして1回リトライする。
    async fn quic_call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

        let tid = new_trace_id();
        let start = std::time::Instant::now();
        write_trace(
            &TraceEntry::new(
                "quic",
                &tid,
                "request",
                "INFO",
                format!("process.{}", method),
            )
            .with_data(payload.clone()),
        );

        let channel = self
            .get_quic_channel(&self.process_channel, "process")
            .await?;

        // タイムアウト付きリクエスト（Process 再起動時のハング防止）
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            channel.request(method, payload.clone()),
        )
        .await;

        match result {
            Ok(Ok(resp)) => {
                write_trace(
                    &TraceEntry::new(
                        "quic",
                        &tid,
                        "response",
                        "INFO",
                        format!("process.{} OK", method),
                    )
                    .with_elapsed(start.elapsed().as_millis() as u64),
                );
                Ok(resp)
            }
            Ok(Err(e)) => {
                // チャネルエラー: リセットしてリトライ
                tracing::warn!("QUIC process.{} failed, retrying: {}", method, e);
                *self.process_channel.lock().await = None;

                let channel = self
                    .get_quic_channel(&self.process_channel, "process")
                    .await?;
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.request(method, payload),
                )
                .await
                .map_err(|_| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry timed out", method),
                        None,
                    )
                })?
                .map_err(|e| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry failed: {}", method, e),
                        None,
                    )
                })
            }
            Err(_) => {
                // タイムアウト: 古い接続をリセットしてリトライ
                tracing::warn!("QUIC process.{} timed out, resetting channel", method);
                *self.process_channel.lock().await = None;

                let channel = self
                    .get_quic_channel(&self.process_channel, "process")
                    .await?;
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    channel.request(method, payload),
                )
                .await
                .map_err(|_| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry timed out", method),
                        None,
                    )
                })?
                .map_err(|e| {
                    McpError::internal_error(
                        format!("QUIC process.{} retry failed: {}", method, e),
                        None,
                    )
                })
            }
        }
    }

    /// Process ポートを再解決し、変わっていれば URL を更新してリトライ用 URL を返す
    ///
    /// 接続失敗時に呼ばれる。`running.json` から cwd に一致する
    /// Process を検索し、現在の URL と異なる場合のみリトライ URL を返す。
    async fn try_reconnect(&self, endpoint: &str) -> Option<String> {
        let process_info = RunningProcesses::find_for_cwd()?;
        let new_base = format!("http://[::1]:{}", process_info.port);

        let mut current = self.process_url.lock().await;
        if *current != new_base {
            *current = new_base.clone();
            *self.process_port.lock().await = process_info.port;
            // ポートが変わったので QUIC チャネルもリセット
            *self.process_channel.lock().await = None;
            Some(format!("{}{}", new_base, endpoint))
        } else {
            None
        }
    }

    /// Process が見つからない場合に自動起動する
    ///
    /// `vp start --headless` をバックグラウンドで spawn し、
    /// health check ポーリングで起動完了を待つ。
    /// 成功したら `process_url` を更新し、新しい URL を返す。
    async fn auto_start_process(&self, endpoint: &str) -> Option<String> {
        use crate::trace_log::{TraceEntry, new_trace_id, write_trace};

        let tid = new_trace_id();
        let cwd = std::env::current_dir().ok()?;
        let cwd_str = cwd.display().to_string();

        write_trace(&TraceEntry::new(
            "mcp",
            &tid,
            "auto_start",
            "INFO",
            format!("Process 自動起動: project_dir={}", cwd_str),
        ));

        // vp start --headless をデタッチ実行
        let spawn_result = std::process::Command::new("vp")
            .arg("start")
            .arg("--headless")
            .arg("-C")
            .arg(&cwd_str)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        if let Err(e) = spawn_result {
            write_trace(&TraceEntry::new(
                "mcp",
                &tid,
                "auto_start",
                "ERROR",
                format!("vp start spawn 失敗: {}", e),
            ));
            return None;
        }

        // running.json からポートを取得し、health check で起動完了を確認
        // 最大 5 秒（200ms × 25回）
        let poll_interval = Duration::from_millis(200);
        let max_attempts = 25;

        for _ in 0..max_attempts {
            tokio::time::sleep(poll_interval).await;

            // running.json から新しい Process を検索
            let process_info = match RunningProcesses::find_for_cwd() {
                Some(info) => info,
                None => continue,
            };

            let new_base = format!("http://[::1]:{}", process_info.port);
            let health_url = format!("{}/api/health", new_base);

            // health check
            match self
                .client
                .get(&health_url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    // 起動完了 — process_url / process_port を更新、QUIC チャネルもリセット
                    let mut current = self.process_url.lock().await;
                    *current = new_base.clone();
                    *self.process_port.lock().await = process_info.port;
                    *self.process_channel.lock().await = None;

                    write_trace(&TraceEntry::new(
                        "mcp",
                        &tid,
                        "auto_start",
                        "INFO",
                        format!("Process 自動起動成功: port={}", process_info.port),
                    ));

                    return Some(format!("{}{}", new_base, endpoint));
                }
                _ => continue,
            }
        }

        write_trace(&TraceEntry::new(
            "mcp",
            &tid,
            "auto_start",
            "ERROR",
            "Process 自動起動タイムアウト（5秒）".to_string(),
        ));

        None
    }

    /// Process に QUIC で ProcessMessage を送信（show/clear/toggle_pane/split_pane/close_pane）
    async fn process_call(
        &self,
        method: &str,
        msg: &ProcessMessage,
    ) -> Result<serde_json::Value, McpError> {
        let payload = serde_json::to_value(msg)
            .map_err(|e| McpError::internal_error(format!("Serialize error: {}", e), None))?;
        self.quic_call(method, payload).await
    }

    /// Canvas API を呼び出す（QUIC 優先、失敗時は HTTP フォールバック）
    async fn canvas_call(&self, action: &str) -> Result<serde_json::Value, McpError> {
        let method = format!("{}_canvas", action);
        match self.quic_call(&method, serde_json::json!({})).await {
            Ok(resp) => Ok(resp),
            Err(_) => {
                // HTTP フォールバック
                let url = self.process_url.lock().await;
                let api_url = format!("{}/api/canvas/{}", url, action);
                drop(url);
                let resp = self.client.post(&api_url).send().await.map_err(|e| {
                    McpError::internal_error(
                        format!("Canvas {} failed (HTTP fallback): {}", action, e),
                        None,
                    )
                })?;
                resp.json().await.map_err(|e| {
                    McpError::internal_error(
                        format!("Canvas {} response parse error: {}", action, e),
                        None,
                    )
                })
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
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Canvas {}", status),
        )]))
    }

    /// Close the Canvas window
    #[tool(description = "Close the Vantage Point Canvas window.")]
    async fn close_canvas(&self) -> Result<CallToolResult, McpError> {
        let resp = self.canvas_call("close").await?;
        let status = resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("closed");
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Canvas {}", status),
        )]))
    }

    /// Show content in the browser viewer
    #[tool(
        description = "Display content in the Vantage Point browser viewer. Supports markdown, html, log, and url formats. Use content_type='url' to embed a web page in an iframe."
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

        // content_type → protocol::Content enum 変換
        let content = match content_type.as_str() {
            "html" => crate::protocol::Content::Html(params.content),
            "log" => crate::protocol::Content::Log(params.content),
            "url" => crate::protocol::Content::Url(params.content),
            _ => crate::protocol::Content::Markdown(params.content),
        };

        let msg = ProcessMessage::Show {
            pane_id: pane_id.clone(),
            content,
            append,
            title: params.title,
        };

        self.process_call("show", &msg).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Content displayed in pane '{}'", pane_id),
        )]))
    }

    /// Clear content in a pane
    #[tool(description = "Clear content in a specific pane of the browser viewer")]
    async fn clear(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClearParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let msg = ProcessMessage::Clear {
            pane_id: pane_id.clone(),
        };
        self.process_call("clear", &msg).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Pane '{}' cleared", pane_id),
        )]))
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

        let msg = ProcessMessage::TogglePane {
            pane_id: params.pane_id.clone(),
            visible: params.visible,
        };
        self.process_call("toggle_pane", &msg).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Pane '{}' {}", params.pane_id, state_desc),
        )]))
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
        let direction = match params.direction.to_lowercase().as_str() {
            "horizontal" | "h" => crate::protocol::SplitDirection::Horizontal,
            "vertical" | "v" => crate::protocol::SplitDirection::Vertical,
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

        let msg = ProcessMessage::Split {
            pane_id: source_pane_id.clone(),
            direction,
            new_pane_id: new_pane_id.clone(),
        };
        self.process_call("split_pane", &msg).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Pane '{}' split. New pane ID: '{}'",
                source_pane_id, new_pane_id
            ),
        )]))
    }

    /// Close a pane
    #[tool(description = "Close a pane in the Vantage Point browser viewer.")]
    async fn close_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClosePaneParams>,
    ) -> Result<CallToolResult, McpError> {
        let msg = ProcessMessage::Close {
            pane_id: params.pane_id.clone(),
        };
        self.process_call("close_pane", &msg).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Pane '{}' closed", params.pane_id),
        )]))
    }

    /// Watch a log file and display it in real-time in a pane
    #[tool(
        description = "Watch a log file and display new lines in real-time in a Vantage Point pane. Supports JSON Lines and plain text formats with level filtering and target exclusion."
    )]
    async fn watch_file(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<WatchFileParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::file_watcher::{WatchConfig, WatchFormat, WatchStyle};

        let format = match params.format.as_deref() {
            Some("plain") => WatchFormat::Plain,
            _ => WatchFormat::JsonLines,
        };

        let style = match params.style.as_deref() {
            Some("plain") => WatchStyle::Plain,
            _ => WatchStyle::Terminal,
        };

        let config = WatchConfig {
            path: params.path.clone(),
            pane_id: params.pane_id.clone(),
            format,
            filter: params.filter,
            exclude_targets: params.exclude_targets.unwrap_or_default(),
            title: params.title,
            style,
        };

        let payload = serde_json::to_value(&config)
            .map_err(|e| McpError::internal_error(format!("Serialize error: {}", e), None))?;
        self.quic_call("watch_file", payload).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Now watching '{}' → pane '{}'", params.path, params.pane_id),
        )]))
    }

    /// Stop watching a file
    #[tool(description = "Stop watching a file for a specific pane.")]
    async fn unwatch_file(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<UnwatchFileParams>,
    ) -> Result<CallToolResult, McpError> {
        self.quic_call(
            "unwatch_file",
            serde_json::json!({"pane_id": params.pane_id}),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Stopped watching pane '{}'", params.pane_id),
        )]))
    }

    /// Capture the Canvas window as a PNG screenshot
    ///
    /// html2canvas で Canvas の DOM をキャプチャし、PNG ファイルとして保存する。
    /// 保存されたファイルは Claude の Read ツールで画像として確認可能。
    #[tool(
        description = "Capture the Canvas window as a PNG screenshot. The saved file can be viewed with the Read tool."
    )]
    async fn capture_canvas(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<CaptureCanvasParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = serde_json::json!({
            "path": params.path,
            "pane_id": params.pane_id,
        });

        // タイムアウトを長めに設定（Canvas 自動起動 + キャプチャ待ち）
        let url = format!("{}/api/canvas/capture", self.process_url.lock().await);
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(
                    format!("Canvas capture 通信失敗: {}. Is vp running?", e),
                    None,
                )
            })?;

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            McpError::internal_error(format!("Canvas capture レスポンスパース失敗: {}", e), None)
        })?;

        let status = json
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        if status != "ok" {
            let msg = json
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            return Err(McpError::internal_error(
                format!("Canvas capture 失敗: {}", msg),
                None,
            ));
        }

        let saved_path = json
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let width = json.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
        let height = json.get("height").and_then(|v| v.as_u64()).unwrap_or(0);
        let size_bytes = json.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Screenshot saved: {}\nSize: {}x{} ({} bytes)\nUse the Read tool to view this image.",
                saved_path, width, height, size_bytes
            ),
        )]))
    }

    /// Execute Ruby code and display results in a pane
    #[tool(
        description = "Execute Ruby code or a Ruby file and display the results in a Canvas pane. For short-lived execution (scripts, data processing). Use run_ruby for long-running daemon processes."
    )]
    async fn eval_ruby(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<EvalRubyParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let body = serde_json::json!({
            "code": params.code,
            "file": params.file,
            "pane_id": pane_id,
        });

        let resp = self.http_post("/api/ruby/eval", &body).await?;

        let stdout = resp.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
        let stderr = resp.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
        let exit_code = resp.get("exit_code").and_then(|v| v.as_i64());
        let elapsed = resp.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);

        let mut result = format!("Ruby eval completed in {}ms", elapsed);
        if let Some(code) = exit_code {
            if code != 0 {
                result.push_str(&format!(" (exit code: {})", code));
            }
        }
        if !stdout.is_empty() {
            result.push_str(&format!("\n\nstdout:\n{}", stdout));
        }
        if !stderr.is_empty() {
            result.push_str(&format!("\n\nstderr:\n{}", stderr));
        }

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            result,
        )]))
    }

    /// Run Ruby code as a long-running daemon process
    #[tool(
        description = "Run Ruby code or a Ruby file as a long-running daemon process. Output is streamed to a Canvas pane in real-time. Use stop_ruby to gracefully stop the process. Use list_ruby to see running processes."
    )]
    async fn run_ruby(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<RunRubyParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let body = serde_json::json!({
            "code": params.code,
            "file": params.file,
            "name": params.name,
            "pane_id": pane_id,
        });

        let resp = self.http_post("/api/ruby/run", &body).await?;

        let process_id = resp
            .get("process_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Ruby daemon started: {} (streaming to pane '{}'). Use stop_ruby with process_id='{}' to stop.",
                process_id, pane_id, process_id
            ),
        )]))
    }

    /// Stop a running Ruby daemon process
    #[tool(
        description = "Gracefully stop a running Ruby daemon process. Sends a shutdown signal and waits for the process to exit."
    )]
    async fn stop_ruby(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<StopRubyParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = serde_json::json!({
            "process_id": params.process_id,
        });

        self.http_post("/api/ruby/stop", &body).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Ruby process '{}' stop signal sent", params.process_id),
        )]))
    }

    /// List running Ruby daemon processes
    #[tool(
        description = "List all running Ruby daemon processes with their IDs, names, pane IDs, and status."
    )]
    async fn list_ruby(&self) -> Result<CallToolResult, McpError> {
        let url = format!("{}/api/ruby/list", self.process_url.lock().await);
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Ruby list 通信失敗: {}. Is vp running?", e), None)
            })?;

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            McpError::internal_error(format!("Ruby list レスポンスパース失敗: {}", e), None)
        })?;

        let processes = json.get("processes").and_then(|v| v.as_array());
        let result = match processes {
            Some(procs) if procs.is_empty() => "No running Ruby processes.".to_string(),
            Some(procs) => {
                let mut lines = vec!["Running Ruby processes:".to_string()];
                for p in procs {
                    let id = p.get("process_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let pane = p.get("pane_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = p
                        .get("status")
                        .map(|v| format!("{}", v))
                        .unwrap_or_else(|| "?".to_string());
                    lines.push(format!(
                        "  {} - {} (pane: {}, status: {})",
                        id, name, pane, status
                    ));
                }
                lines.join("\n")
            }
            None => "No running Ruby processes.".to_string(),
        };

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            result,
        )]))
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

        // Create the ProcessMessage
        let message = ProcessMessage::ChatComponent {
            component,
            interactive: true,
        };

        let url = self.process_url.lock().await;

        // First, send the permission request to the Process
        let send_result = self
            .client
            .post(format!("{}/api/permission", *url))
            .json(&message)
            .send()
            .await;

        if let Err(e) = send_result {
            return Err(McpError::internal_error(
                format!(
                    "Failed to send permission request to Process: {}. Is vp running?",
                    e
                ),
                None,
            ));
        }

        let send_resp = send_result.unwrap();
        if !send_resp.status().is_success() {
            return Err(McpError::internal_error(
                format!(
                    "Process returned error on permission request: {}",
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
                return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
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
                            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
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
                        format!("Unexpected response from Process: {}", resp.status()),
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

    /// Restart the Vantage Point Process
    ///
    /// This tool restarts the Process process while preserving session state.
    /// Useful after rebuilding the binary.
    /// HTTP ベースのプロセス管理のため QUIC は使わない。
    #[tool(
        description = "Restart the Vantage Point Process. Session state is preserved. Returns when Process is ready."
    )]
    async fn restart(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<RestartParams>,
    ) -> Result<CallToolResult, McpError> {
        let url = self.process_url.lock().await;
        let base_url = url.clone();
        drop(url);

        // Extract port from URL
        let port: u16 = base_url
            .split(':')
            .next_back()
            .and_then(|s| s.parse().ok())
            .unwrap_or(33000);

        // 1. Get current Process info (project_dir)
        let health_url = format!("{}/api/health", base_url);
        let health_resp = self.client.get(&health_url).send().await.map_err(|e| {
            McpError::internal_error(format!("Failed to get Process health: {}", e), None)
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

        // 3. Wait for Process to stop (poll health endpoint)
        let stop_timeout = Duration::from_secs(10);
        let poll_interval = Duration::from_millis(200);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > stop_timeout {
                return Err(McpError::internal_error(
                    "Timeout waiting for Process to stop".to_string(),
                    None,
                ));
            }

            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
                    // Still running, wait
                    tokio::time::sleep(poll_interval).await;
                }
                _ => {
                    // Process is down
                    break;
                }
            }
        }

        // 4. Start new Process process
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
            McpError::internal_error(format!("Failed to spawn new Process: {}", e), None)
        })?;

        // 5. Wait for new Process to be ready
        let start_timeout = Duration::from_secs(15);
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > start_timeout {
                return Err(McpError::internal_error(
                    "Timeout waiting for Process to start".to_string(),
                    None,
                ));
            }

            tokio::time::sleep(poll_interval).await;

            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
                    // Process is up — QUIC チャネルをリセットして再接続を強制
                    *self.process_channel.lock().await = None;

                    return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                        format!(
                            "Process restarted successfully on port {}. Project: {}",
                            port, project_dir
                        ),
                    )]));
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
                "Vantage Point Process - Display rich content (markdown, HTML, images) in a browser viewer. \
                 Use 'open_canvas' to open the native Canvas window, 'close_canvas' to close it, \
                 'capture_canvas' to take a PNG screenshot of the Canvas (viewable with Read tool), \
                 'show' to display content, 'clear' to clear panes, 'split_pane' to split a pane \
                 horizontally or vertically, 'close_pane' to close a pane, 'toggle_pane' to toggle panel visibility, \
                 'permission' to request user approval, 'restart' to restart the Process, \
                 'watch_file' to monitor a log file in real-time, and 'unwatch_file' to stop monitoring.\n\n\
                 When using 'show', prefer content_type='markdown' as the default format. \
                 Markdown renders well in the Canvas and is easy to read. \
                 Use content_type='html' only when you need precise visual layout (dashboards, diagrams with colors, interactive elements).".into()
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

/// Resolve Process port for MCP communication
///
/// Priority:
/// 1. Explicit port argument (if provided and != 33000)
/// 2. running.json lookup by current working directory
/// 3. Default port (33000)
fn resolve_process_port(explicit_port: u16) -> u16 {
    // If an explicit port was provided (not the default), use it
    if explicit_port != 33000 {
        return explicit_port;
    }

    // Try to find a running Process for the current directory
    if let Some(process_info) = RunningProcesses::find_for_cwd() {
        return process_info.port;
    }

    // Fall back to default
    33000
}

/// Run the MCP server over stdio
pub async fn run_mcp_server(process_port: u16) -> anyhow::Result<()> {
    // トレースログファイルを早期初期化
    crate::trace_log::init_log_file();

    // Resolve the actual port to use
    let resolved_port = resolve_process_port(process_port);

    // Note: In MCP mode, we should not use tracing to stdout
    // as it interferes with JSON-RPC communication
    let service = VantageMcp::new(resolved_port)
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    service.waiting().await?;

    Ok(())
}
