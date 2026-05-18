//! MCP (Model Context Protocol) server implementation
//!
//! Provides tools for Claude Code to display content in browser:
//! - show: Display markdown/html/log content
//! - clear: Clear a pane
//! - permission: Handle permission requests for tool execution
//!
//! ## 通信レイヤー
//! process チャネルは Unison QUIC で通信。
//! Ruby VM / capture / permission 等の一部 API は HTTP フォールバック。

// running.json 不使用 — discovery モジュール経由
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

/// Parameters for the close_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClosePaneParams {
    /// Pane ID to close
    #[schemars(description = "ID of the pane to close")]
    pub pane_id: String,
}

/// Parameters for msg_send tool (VP-24)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MsgSendParams {
    /// 宛先アドレス
    #[schemars(description = "Destination msgbox address (e.g. 'agent', 'protocol', 'notify')")]
    pub to: String,

    /// メッセージ本文（JSON）
    #[schemars(description = "Message payload as JSON value")]
    pub payload: serde_json::Value,

    /// メッセージ種別
    #[schemars(
        description = "Message kind: 'direct' (default), 'notification', 'request', 'response'"
    )]
    pub kind: Option<String>,

    /// 返信先メッセージID（スレッド用）
    #[schemars(description = "Reply-to message ID for threaded conversations")]
    pub reply_to: Option<String>,

    /// TTL（秒）— msg の有効期限（VP-158: 全 msg 永続化が default）
    #[schemars(
        description = "TTL in seconds for the message. Default: 172800 (48h). All messages are persisted across Process restarts (VP-158, no opt-in needed)."
    )]
    pub ttl_secs: Option<u64>,

    /// 明示 ack モード
    #[schemars(
        description = "If true, the message is NOT auto-acked on recv; receiver must call ack() explicitly. Useful for at-least-once delivery with crash resilience during message processing."
    )]
    pub manual_ack: Option<bool>,
}

/// Parameters for msg_ack tool (明示 ack)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MsgAckParams {
    /// ack 対象のメッセージID
    #[schemars(description = "Message ID to acknowledge (received from msg_recv)")]
    pub id: String,
}

/// Parameters for msg_recv tool (VP-24, VP-157 で lane 追加)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MsgRecvParams {
    /// 受信タイムアウト（秒）
    #[schemars(
        description = "Timeout in seconds to wait for a message (default: 5, max: 30). Returns immediately if a message is already queued."
    )]
    pub timeout: Option<u64>,

    /// 送信元フィルタ
    #[schemars(description = "Only receive messages from this address (optional filter)")]
    pub from: Option<String>,

    /// VP-166: 受信先 lane (default "lead"、flat 名: "lead" or "<worker-name>")
    #[schemars(
        description = "Receive from this lane (default: 'lead', or a worker name like 'chore'). Reads the lane's `<stand>#<lane>` box."
    )]
    pub lane: Option<String>,

    /// VP-166: 受信先 stand (default "agent" = coding-assistant inbox)
    #[schemars(
        description = "Which inbox of the lane to receive from: 'agent' (default, = the lane's Claude session inbox) or 'canvas' (= the lane's Paisley Park / Canvas inbox; canvas box wiring is planned)."
    )]
    pub stand: Option<String>,
}

/// Parameters for msg_directory tool (VP-65: cross-process actor discovery)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MsgDirectoryParams {
    /// project_name フィルタ（省略時は全プロジェクト）
    #[schemars(
        description = "Filter by project name (e.g. 'creo-memories'). If omitted, returns all registered actors across all projects."
    )]
    pub project_name: Option<String>,
}

/// Parameters for msg_thread tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MsgThreadParams {
    /// スレッドを辿る起点のメッセージID
    #[schemars(
        description = "Message ID to trace the reply_to chain from. Returns all messages in the thread (root + all descendants), sorted by timestamp."
    )]
    pub id: String,
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

/// Parameters for the switch_lane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SwitchLaneParams {
    /// Lane name (project name) to switch to
    #[schemars(
        description = "Lane name (project name) to switch the Canvas to. e.g. 'vantage-point', 'creo-memories'"
    )]
    pub lane: String,
}

/// Parameters for the add_worker tool (R5: lane clone + Worker Lane spawn).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AddWorkerParams {
    /// Worker name. Used as the `name` field of the Lane address (`<project>/worker/<name>`).
    #[schemars(
        description = "Worker name (人間可読の短い slug、 例: 'feat-api', 'sub'). Lane address の `<project>/worker/<name>` 部分になる。"
    )]
    pub name: String,
    /// Optional branch. If omitted, server auto-derives `<git-user>/<sanitized-name>`.
    #[schemars(
        description = "Lane clone する branch 名 (省略可)。 省略時は server が `git config user.name` から `<user>/<name>` を auto-derive。"
    )]
    pub branch: Option<String>,
    /// Optional Lane Stand. Defaults to "echoes".
    #[schemars(
        description = "Lane Stand 種類: 'echoes' (default、 Claude CLI) or 'shell' (shell)。"
    )]
    pub stand: Option<String>,
}

/// Parameters for the delete_worker tool (VP-124 Phase 1).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteWorkerParams {
    /// Worker name to delete.
    #[schemars(
        description = "Worker name to delete (例: 'keystage', 'feat-api')。 Lane address の `<project>/worker/<name>` の `<name>` 部分。"
    )]
    pub name: String,

    /// Whether to also remove the lane workspace dir.
    #[schemars(
        description = "Lane workspace dir も削除するか (default: true)。 false で SP pool + tmux session のみ kill、 dir 残置 (debug / forensic 用途)。"
    )]
    #[serde(default)]
    pub cleanup: Option<bool>,
}

/// Parameters for the list_lanes tool (VP-124 Phase 1).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListLanesParams {
    /// Lane kind filter.
    #[schemars(description = "Lane kind フィルタ: 'lead' or 'worker'。 省略時は両方含む。")]
    #[serde(default)]
    pub kind: Option<String>,

    /// Lane state filter.
    #[schemars(
        description = "Lane state フィルタ: 'running' / 'spawning' / 'exiting' / 'dead'。 省略時は全状態。"
    )]
    #[serde(default)]
    pub state: Option<String>,
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

/// Parameters for the port_roles tool (no args、 placeholder for schemars→rmcp 1.6 compat).
///
/// schemars 1.x は `Parameters<()>` を `{const: null}` schema として generate するが、
/// rmcp 1.6 + MCP spec は inputSchema に `{type: "object"}` を必須化。 空 struct で
/// `{type: "object", properties: {}}` を生成させて MCP client validation を通過させる。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortRolesParams {}

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

/// capture_terminal ツールのパラメータ
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptureTerminalParams {
    /// 保存先パス
    #[schemars(
        description = "Save path for the PNG screenshot (default: /tmp/vp-terminal-{timestamp}.png)"
    )]
    pub path: Option<String>,
}

/// tmux ペイン分割のパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxSplitParams {
    /// 水平分割 (true) or 垂直分割 (false)。デフォルト: true
    #[schemars(description = "Horizontal split (true, default) or vertical split (false)")]
    pub horizontal: Option<bool>,
    /// 新しいペインで実行するコマンド（省略するとデフォルトシェル）
    #[schemars(
        description = "Command to run in the new pane (e.g. 'claude --dangerously-skip-permissions'). Defaults to shell."
    )]
    pub command: Option<String>,
    /// コンテンツ種別: "shell" (The Hand ✋), "agent"/"echoes" (Echoes 💬、 旧 HD 📖), "canvas"/"pp" (Paisley Park 🧭)
    #[schemars(
        description = "Content type for the new pane: 'shell' (The Hand, default shell), 'agent'/'echoes' (Echoes 💬, Claude CLI; 'hd' legacy alias), 'canvas'/'pp' (Paisley Park). Overridden by 'command' if both specified."
    )]
    pub content_type: Option<String>,
}

/// tmux ペインキャプチャのパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxCaptureParams {
    /// ペイン ID（例: %0）またはエージェント label。省略すると全ペインをキャプチャ。
    #[schemars(
        description = "Pane ID (e.g. %0) or agent label (e.g. 'Moody Blues'). If omitted, captures all panes."
    )]
    pub pane_id: Option<String>,
}

/// エージェントデプロイのパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxAgentDeployParams {
    /// エージェント名（"Moody Blues", "Sticky Fingers" 等）
    #[schemars(description = "Agent label (e.g. 'Moody Blues', 'Sticky Fingers')")]
    pub label: String,
    /// 新しいペインで実行するコマンド
    #[schemars(
        description = "Command to run in the new pane (e.g. 'claude --dangerously-skip-permissions')"
    )]
    pub command: Option<String>,
    /// 実行中タスクの説明
    #[schemars(description = "Description of the task this agent is performing")]
    pub task: Option<String>,
    /// 水平分割 (true) or 垂直分割 (false)。デフォルト: true
    #[schemars(description = "Horizontal split (true, default) or vertical split (false)")]
    pub horizontal: Option<bool>,
}

/// エージェントステータス更新のパラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxAgentStatusParams {
    /// ペイン ID またはエージェント label
    #[schemars(description = "Pane ID (e.g. %3) or agent label (e.g. 'Moody Blues')")]
    pub pane_id: String,
    /// ステータス（"running", "waiting", "done", "error"）
    #[schemars(description = "Agent status: 'running', 'waiting', 'done', or 'error'")]
    pub status: String,
    /// タスク説明（更新する場合）
    #[schemars(description = "Updated task description (optional)")]
    pub task: Option<String>,
}

/// エージェントへのテキスト送信パラメータ
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TmuxAgentSendParams {
    /// ペイン ID またはエージェント label
    #[schemars(description = "Pane ID (e.g. %3) or agent label (e.g. 'Moody Blues')")]
    pub pane_id: String,
    /// 送信するテキスト（改行はそのまま送信される）
    #[schemars(description = "Text to send to the pane (newlines are sent as-is)")]
    pub text: String,
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
/// この `vp mcp` プロセスが属する Lane (VP-166 PR-4)。
///
/// cwd から判定する: cwd が `vp_data_dir()/lanes/<parent>-<name>` なら worker `<name>`、
/// それ以外（= repo path）なら lead。`msg_recv` の default lane / `msg_send` の `from` /
/// `list_lanes` の `is_self` 付与に使う。
#[derive(Debug, Clone)]
pub struct SelfLane {
    /// `"lead"` or `"<worker-name>"`（flat 名）
    pub lane_name: String,
    /// worker context のとき `Some(parent project 名)`、lead context のとき `None`
    pub worker_parent: Option<String>,
}

impl SelfLane {
    /// cwd から SelfLane を判定（VP-166 PR-4）。失敗時（cwd 取れない / config 読めない 等）は lead 扱い。
    pub fn detect() -> Self {
        let lead = || SelfLane {
            lane_name: "lead".to_string(),
            worker_parent: None,
        };
        let Ok(cwd) = std::env::current_dir() else {
            return lead();
        };
        let Ok(lanes_root) = crate::lane::config::wings_dir() else {
            return lead();
        };
        if !cwd.starts_with(&lanes_root) {
            return lead(); // 通常 project の cwd = lead
        }
        let Some(dirname) = cwd.file_name().and_then(|n| n.to_str()) else {
            return lead();
        };
        // config から parent project を最長一致で resolve (`<parent>-<name>` の <parent> 部分)
        let Ok(config) = crate::config::Config::load() else {
            return lead();
        };
        let Some(parent) = config
            .projects
            .iter()
            .filter(|p| dirname.starts_with(&format!("{}-", p.name)))
            .max_by_key(|p| p.name.len())
        else {
            return lead();
        };
        match dirname.strip_prefix(&format!("{}-", parent.name)) {
            Some(name) if !name.is_empty() => SelfLane {
                lane_name: name.to_string(),
                worker_parent: Some(parent.name.clone()),
            },
            _ => lead(),
        }
    }

    /// `msg_send` の `from` フィールド値: lead は `"agent"`（bare、cross-process forward 時に
    /// `normalize_from` で `agent@<project>` になる）、worker は `"agent@<parent>/<name>"`。
    pub fn from_address(&self) -> String {
        match &self.worker_parent {
            Some(parent) => format!("agent@{}/{}", parent, self.lane_name),
            None => "agent".to_string(),
        }
    }
}

/// Unison QUIC で Process と通信する。
/// process チャネルは lazy 接続し、persistent に保持。
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
    /// この MCP プロセスが属する Lane（cwd 由来、VP-166 PR-4）
    self_lane: SelfLane,
    tool_router: ToolRouter<Self>,
}

impl Clone for VantageMcp {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            process_url: self.process_url.clone(),
            process_port: self.process_port.clone(),
            process_channel: self.process_channel.clone(),
            self_lane: self.self_lane.clone(),
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
            self_lane: SelfLane::detect(),
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
                            "Process 通信失敗 ({}): {}. Auto-start failed. Run `vp sp start` manually.",
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

        // VP-184: Builder API 移行。 dev default (= SkipVerification) を明示的に渡す。
        // PR-3 で trust_anchors を InternalMeshKeypair の client 半分に差し替える。
        let transport = unison::network::quic::QuicClient::builder()
            .trust_anchors(unison::network::TrustAnchors::SkipVerification)
            .build()
            .map_err(|e| McpError::internal_error(format!("Unison client error: {}", e), None))?;
        let client = unison::ProtocolClient::new(transport);
        client.connect(&addr).await.map_err(|e| {
            McpError::internal_error(format!("Unison connect error ({}): {}", addr, e), None)
        })?;
        // unison 内部の request timeout は default 30s。 `quic_call_with_timeout` の outer
        // timeout (msg_recv で server_timeout + buffer = 最大 35s) より長く取らないと
        // unison 側が先に発火してしまうので、 余裕を持って 60s に引き上げる (VP-163)。
        let channel = Arc::new(
            client
                .open_channel(channel_name)
                .await
                .map_err(|e| {
                    McpError::internal_error(
                        format!("Unison {} channel error: {}", channel_name, e),
                        None,
                    )
                })?
                .with_request_timeout(std::time::Duration::from_secs(60)),
        );

        *guard = Some(Arc::clone(&channel));
        Ok(channel)
    }

    /// Unison QUIC の "process" チャネルでメソッドを呼び出す（outer timeout = 5s）
    ///
    /// 接続失敗 / タイムアウト時はチャネルをリセットして1回リトライする。
    async fn quic_call(
        &self,
        method: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        self.quic_call_with_timeout(method, payload, std::time::Duration::from_secs(5))
            .await
    }

    /// Unison QUIC の "process" チャネル呼び出し（outer timeout 指定可）
    ///
    /// `msg_recv` のように **server 側が長時間ブロックする** method では、 outer timeout を
    /// server 側 timeout より長く取らないと client が先に諦め、 チャネルを reset → 空振り
    /// リトライ → 同一 box への recv 二重発火 → msg ロスにつながる (VP-163)。 そういう
    /// method は呼び出し側で `server_timeout + buffer` を渡すこと。
    ///
    /// また、 server ハンドラが `Err(e)` を返した場合、 unison は専用 error frame を持たない
    /// ため `unison_server.rs` の dispatch loop が **成功フレームに `{"error": "<msg>"}` を詰めて**
    /// 返してくる。 これをそのまま素通しすると `msg_send` 等が「成功」と誤報するので (VP-163)、
    /// その形のレスポンスは `McpError` に変換する。
    async fn quic_call_with_timeout(
        &self,
        method: &str,
        payload: serde_json::Value,
        timeout: std::time::Duration,
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
            timeout,
            channel.request::<serde_json::Value, serde_json::Value>(method, &payload),
        )
        .await;

        let resp = match result {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                // チャネルエラー: リセットしてリトライ
                tracing::warn!("QUIC process.{} failed, retrying: {}", method, e);
                *self.process_channel.lock().await = None;

                let channel = self
                    .get_quic_channel(&self.process_channel, "process")
                    .await?;
                tokio::time::timeout(
                    timeout,
                    channel.request::<serde_json::Value, serde_json::Value>(method, &payload),
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
                })?
            }
            Err(_) => {
                // タイムアウト: 古い接続をリセットしてリトライ
                tracing::warn!("QUIC process.{} timed out, resetting channel", method);
                *self.process_channel.lock().await = None;

                let channel = self
                    .get_quic_channel(&self.process_channel, "process")
                    .await?;
                tokio::time::timeout(
                    timeout,
                    channel.request::<serde_json::Value, serde_json::Value>(method, &payload),
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
                })?
            }
        };

        // server ハンドラの Err は `{"error": "<msg>"}` の単一キー object で返ってくる (VP-163)
        if let Some(err_msg) = rpc_response_error(&resp) {
            write_trace(
                &TraceEntry::new(
                    "quic",
                    &tid,
                    "response",
                    "ERROR",
                    format!("process.{} error: {}", method, err_msg),
                )
                .with_elapsed(start.elapsed().as_millis() as u64),
            );
            return Err(McpError::internal_error(
                format!("process.{}: {}", method, err_msg),
                None,
            ));
        }

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

    /// Process ポートを再解決し、変わっていれば URL を更新してリトライ用 URL を返す
    ///
    /// 接続失敗時に呼ばれる。discovery で cwd に一致する
    /// Process を検索し、現在の URL と異なる場合のみリトライ URL を返す。
    async fn try_reconnect(&self, endpoint: &str) -> Option<String> {
        let process_info = crate::discovery::find_for_cwd().await?;
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

    /// label または pane_id を受け取り、(pane_id, 表示名) を返す
    ///
    /// `%` で始まる場合もそうでない場合も `tmux_resolve_pane` を1回呼ぶ。
    /// サーバー側で pane_id → 即返却 + meta 取得、label → 逆引き + meta 取得を統一処理。
    async fn resolve_pane(&self, query: &str) -> Result<(String, String), McpError> {
        if query.starts_with('%') {
            // pane_id → meta を取得して表示名を生成
            let display = match self
                .quic_call("tmux_resolve_pane", serde_json::json!({"query": query}))
                .await
            {
                Ok(resp) => {
                    if let Some(label) = resp.pointer("/meta/label").and_then(|v| v.as_str()) {
                        format!("{} ({})", label, query)
                    } else {
                        query.to_string()
                    }
                }
                Err(_) => query.to_string(),
            };
            return Ok((query.to_string(), display));
        }
        let resp = self
            .quic_call("tmux_resolve_pane", serde_json::json!({"query": query}))
            .await?;
        let pane_id = resp
            .get("pane_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                McpError::invalid_params(format!("ペインが見つかりません: {}", query), None)
            })?;
        let label = resp.pointer("/meta/label").and_then(|v| v.as_str());
        let display = match label {
            Some(l) => format!("{} ({})", l, pane_id),
            None => pane_id.clone(),
        };
        Ok((pane_id, display))
    }

    /// Process が見つからない場合に自動起動する
    ///
    /// `vp sp start` をバックグラウンドで spawn し、
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

        // vp sp start をデタッチ実行
        let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());
        let spawn_result = std::process::Command::new(&vp_bin)
            .args(["sp", "start", "-C", &cwd_str])
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
                format!("vp sp start spawn 失敗: {}", e),
            ));
            return None;
        }

        // running.json からポートを取得し、health check で起動完了を確認
        // 最大 5 秒（200ms × 25回）
        let poll_interval = Duration::from_millis(200);
        let max_attempts = 25;

        for _ in 0..max_attempts {
            tokio::time::sleep(poll_interval).await;

            // 稼働中 Process を検索（TheWorld API → HTTP スキャンフォールバック）
            let process_info = match crate::discovery::find_for_cwd().await {
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

    /// Process に QUIC で ProcessMessage を送信（show/clear/toggle_pane/close_pane）
    async fn process_call(
        &self,
        method: &str,
        msg: &ProcessMessage,
    ) -> Result<serde_json::Value, McpError> {
        let payload = serde_json::to_value(msg)
            .map_err(|e| McpError::internal_error(format!("Serialize error: {}", e), None))?;
        self.quic_call(method, payload).await
    }

    /// Canvas の表示 Lane を切り替える
    #[tool(
        description = "Switch the active lane (project) in the PP Canvas window. The lane name is the project name shown in the lane bar."
    )]
    async fn switch_lane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<SwitchLaneParams>,
    ) -> Result<CallToolResult, McpError> {
        // TheWorld の HTTP API 経由で Canvas に switch_lane を送信
        let world_port = crate::cli::WORLD_PORT;
        let url = format!("http://[::1]:{}/api/canvas/switch_lane", world_port);
        let body = serde_json::json!({ "lane": params.lane });

        let client = reqwest::Client::new();
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                    format!("Switched Canvas lane to '{}'", params.lane),
                )]))
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                Err(McpError::internal_error(
                    format!("TheWorld API error: {} {}", status, text),
                    None,
                ))
            }
            Err(e) => Err(McpError::internal_error(
                format!("Failed to reach TheWorld: {}", e),
                None,
            )),
        }
    }

    /// R5: 現 project の SP に Worker Lane を新規作成 (lane clone + PtySlot spawn)。
    ///
    /// - cwd ベースで自動的に local SP を解決 (`self.process_url`)。
    /// - branch 省略時は server 側で `<git-user>/<sanitized-name>` を auto-derive。
    /// - 名前重複は HTTP 409 CONFLICT、 lane clone 失敗は 500 で返ってくる。
    #[tool(
        description = "Create a new Worker Lane in the current project (lane clone + spawn). Resolves the local SP via cwd. If `branch` is omitted, the server auto-derives `<git-user>/<sanitized-name>`. Returns the Lane address `<project>/worker/<name>` on success. Use this to spawn isolated parallel work (e.g. feature branches, exploratory experiments)."
    )]
    async fn add_worker(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<AddWorkerParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "name は必須です (空文字不可)".to_string(),
                None,
            ));
        }
        let mut body = serde_json::json!({
            "kind": "worker",
            "name": params.name,
        });
        if let Some(b) = params.branch.as_ref().filter(|s| !s.trim().is_empty()) {
            body["branch"] = serde_json::Value::String(b.clone());
        }
        if let Some(s) = params.stand.as_ref().filter(|s| !s.trim().is_empty()) {
            body["stand"] = serde_json::Value::String(s.clone());
        }
        let url = format!("{}/api/lanes", self.process_url.lock().await);
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(60)) // lane clone は 数 sec ~ 数 10 sec かかる
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            // server からのエラー文を素直に伝える (CONFLICT = 名前重複等)
            return Err(McpError::internal_error(
                format!("SP /api/lanes {}: {}", status, text),
                None,
            ));
        }
        // 成功 body は LaneInfo JSON。 address だけ抽出して短い human 向け text を返す。
        let parsed: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let addr = parsed
            .get("address")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                parsed.get("address").and_then(|a| {
                    let proj = a.get("project")?.as_str()?;
                    let nm = a.get("name")?.as_str()?;
                    Some(format!("{}/worker/{}", proj, nm))
                })
            })
            .unwrap_or_else(|| format!("worker/{}", params.name));
        let cwd = parsed.get("cwd").and_then(|v| v.as_str()).unwrap_or("?");
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Worker Lane created: {}\n  cwd: {}", addr, cwd),
        )]))
    }

    /// Delete a Worker Lane in the current project (VP-124 Phase 1).
    ///
    /// 3-step orchestration を 1 call で完結: SP pool removal + child PTY kill + tmux session kill +
    /// (optional) lane workspace dir cleanup。 server-side `delete_lane_orchestrated` への薄い HTTP
    /// wrapper、 cwd ベースで自動的に local SP と project を解決。
    #[tool(
        description = "Delete a Worker Lane in the current project. SP pool removal + child PTY kill + tmux session kill + lane workspace dir cleanup を 1 call で完結 (= 旧来の手動 3 step `vp lane rm` + `tmux kill-session` + `curl -X DELETE` を置換)。 cwd ベースで local SP を自動解決、 cleanup=false で dir 残置 (debug 用途)。 Lead Lane は削除不可 (architecture rule、 SP shutdown が path)。"
    )]
    async fn delete_worker(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<DeleteWorkerParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "name は必須です (空文字不可)".to_string(),
                None,
            ));
        }

        // SP の project name を /api/health から取得 (project_dir basename = project name)。
        // address 構築のため必要、 add_worker と異なり POST body に name 1 つだけ渡せば SP 側で
        // project 補完される pattern が使えない (DELETE は full address を query で受ける design)。
        let process_url = self.process_url.lock().await.clone();
        let health = self
            .client
            .get(format!("{}/api/health", process_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("/api/health parse 失敗: {}", e), None)
            })?;
        let project_dir = health
            .get("project_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::internal_error("/api/health に project_dir なし".to_string(), None)
            })?;
        let project_name = std::path::Path::new(project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("project_dir の basename 取得失敗: {}", project_dir),
                    None,
                )
            })?;

        let address = format!("{}/worker/{}", project_name, params.name);
        let cleanup = params.cleanup.unwrap_or(true);

        // reqwest 0.12 の RequestBuilder.query は &[(impl Serialize, impl Serialize)] を取るが、
        // `&str` の tuple slice の type 推論が出ないので manual percent-encoding で URL 構築。
        // address 内の `/` は `%2F` にする以外は ASCII safe (project name / worker name は git
        // branch 互換 slug のため英数 + dash + underscore のみ)。
        let address_enc = address.replace('/', "%2F");
        let url = format!(
            "{}/api/lanes?address={}&cleanup={}",
            process_url, address_enc, cleanup
        );
        let resp = self
            .client
            .delete(&url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("DELETE /api/lanes 失敗: {}", e), None)
            })?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(McpError::internal_error(
                format!("SP DELETE /api/lanes {}: {}", status, text),
                None,
            ));
        }

        // 成功 body は DeletedLaneInfo JSON。 human 向けに要点だけ要約。
        let parsed: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        let pid = parsed
            .get("pid")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "(no pid)".to_string());
        let tmux_killed = parsed
            .get("tmux_killed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cleanup_status = parsed
            .get("cleanup")
            .and_then(|v| v.as_str())
            .unwrap_or("(skipped)");

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Worker Lane deleted: {}\n  pid: {} (killed)\n  tmux_killed: {}\n  cleanup: {}",
                address, pid, tmux_killed, cleanup_status
            ),
        )]))
    }

    /// List Lanes in the current project with comprehensive routing info (VP-124 Phase 1).
    ///
    /// Lead Lane Echoes が「lane を operate するすべての座標」 を 1 call で取得するための tool。
    /// GET /api/lanes wrapper、 各 Lane に mailbox_addresses (per-Lane Stands の wire address)、
    /// top-level に project_addresses + world_addresses を synthesize。
    #[tool(
        description = "List all Lanes (Lead + Workers) in the current project with comprehensive routing info. Each Lane returns: address, kind, state, stand, pid, cwd, tmux session, worker_status, AND mailbox_addresses (= wire-ready addresses for `msg_send`)。 Each lane's mailbox_addresses has two entries: `agent` (= the lane's Claude session inbox, e.g. `agent@vantage-point` for lead or `agent@vantage-point/chore` for worker 'chore') and `canvas` (= the lane's Canvas / Paisley Park inbox, e.g. `canvas@vantage-point/chore`)。 Top-level also returns project_addresses (e.g. `gold_experience@<project>`) and world_addresses (e.g. `hermit_purple@world`)。 Use this to discover Workers, decide deletion targets, pick mailbox routes for msg_send。 Replaces multi-step `vp ps` + `curl /api/lanes`。"
    )]
    async fn list_lanes(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ListLanesParams>,
    ) -> Result<CallToolResult, McpError> {
        let process_url = self.process_url.lock().await.clone();

        // project name は /api/health から (= delete_worker と同型 pattern)。
        // 旧実装 (`lanes_in.first().get("project")`) は SP 起動直後 lanes 空の race
        // window で `"unknown"` fallback → `gold_experience@unknown` 等の偽 address を
        // 返す bug があり、 PR-β-4 review feedback (`feedback_jsonschema_field_scope`)
        // 同様 contract 強度のため authoritative source `/api/health.project_dir` に統一。
        let health = self
            .client
            .get(format!("{}/api/health", process_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| {
                McpError::internal_error(format!("/api/health parse 失敗: {}", e), None)
            })?;
        let project_dir = health
            .get("project_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                McpError::internal_error("/api/health に project_dir なし".to_string(), None)
            })?;
        let project = std::path::Path::new(project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("project_dir basename 取得失敗: {}", project_dir),
                    None,
                )
            })?
            .to_string();

        let resp = self
            .client
            .get(format!("{}/api/lanes", process_url))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("SP に到達できません: {}", e), None))?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| McpError::internal_error(format!("/api/lanes parse 失敗: {}", e), None))?;

        let lanes_in = resp
            .get("lanes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // フィルタ + mailbox_addresses 注入
        let mut lanes_out: Vec<serde_json::Value> = Vec::new();
        for mut lane in lanes_in.into_iter() {
            // kind / state filter
            if let Some(k) = &params.kind
                && lane.get("kind").and_then(|v| v.as_str()) != Some(k.as_str())
            {
                continue;
            }
            if let Some(s) = &params.state
                && lane.get("state").and_then(|v| v.as_str()) != Some(s.as_str())
            {
                continue;
            }

            // mailbox_addresses 計算 (= per-Lane の wire address。VP-166 設計 doc 16)。
            //
            // 各 Lane (lead / worker) は 2 つの box を持つ:
            //   - `agent#<lane>`  = その lane の Claude session 宛 (= coding-assistant inbox)
            //   - `canvas#<lane>` = その lane の Canvas / PP 宛 (PR-5 で配線)
            // actor 名は `stands.rs` の `id` 体系 (`ECHOES.id = "agent"` / `PAISLEY_PARK.id = "canvas"`)。
            // JoJo 愛称 (`echoes` / `paisley_park`) は表示専用なので wire には出さない。
            // wire syntax は `<stand-id>@<project>/<lane>` (lead は `/lane` 省略可)。
            // 旧実装の `<JoJo名>.<lane>@<project>` (`.` 区切り) は `parse_address` で弾かれる不正形だった。
            let lane_label = match lane.get("kind").and_then(|v| v.as_str()) {
                Some("lead") => "lead".to_string(),
                Some("worker") => lane
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unnamed")
                    .to_string(),
                _ => "unknown".to_string(),
            };
            // lead は `agent@<project>` (lane 省略 = lead)、worker は `agent@<project>/<name>`
            let lane_suffix = if lane_label == "lead" {
                String::new()
            } else {
                format!("/{}", lane_label)
            };
            let mailbox = serde_json::json!({
                "agent": format!("agent@{}{}", project, lane_suffix),
                "canvas": format!("canvas@{}{}", project, lane_suffix),
            });
            // VP-166 PR-4: この MCP プロセスの lane と一致する entry に `is_self` を付与
            // (SP は caller を知らないので MCP 側で post-process)。agent は自分の entry を
            // 見つけて mailbox_addresses["agent"] = 自分の正規アドレス、を読める。
            let is_self = lane_label == self.self_lane.lane_name;

            if let Some(obj) = lane.as_object_mut() {
                obj.insert("mailbox_addresses".to_string(), mailbox);
                obj.insert("is_self".to_string(), serde_json::Value::Bool(is_self));
            }
            lanes_out.push(lane);
        }

        // top-level に project / world Stand addresses を synthesize
        let result = serde_json::json!({
            "project": project,
            "lanes": lanes_out,
            "project_addresses": {
                "gold_experience": format!("gold_experience@{}", project),
            },
            "world_addresses": {
                "hermit_purple": "hermit_purple@world",
            },
        });

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&result).unwrap_or_default(),
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

        // VP-83 Phase 2.2: Native App に Canvas pane auto-open 通知を送信。
        // 既に Canvas pane があれば Swift 側で no-op、無ければ自動生成。
        #[cfg(target_os = "macos")]
        {
            let port = *self.process_port.lock().await;
            crate::notify::post_canvas_open(port);
        }

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

    /// Split the current tmux window to create a new pane.
    ///
    /// tmux split-window で新しいペインを作成する。
    /// worker 起動や並列 CC セッション作成に使う。
    #[tool(
        description = "Split the current tmux window to create a new pane. Use this to spawn parallel workers (e.g. Claude Code sessions, shell commands). Returns the new pane ID."
    )]
    async fn tmux_split(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxSplitParams>,
    ) -> Result<CallToolResult, McpError> {
        let horizontal = params.horizontal.unwrap_or(true);
        let mut payload = serde_json::json!({"horizontal": horizontal});
        if let Some(cmd) = &params.command {
            payload["command"] = serde_json::Value::String(cmd.clone());
        }
        if let Some(ct) = &params.content_type {
            payload["content_type"] = serde_json::Value::String(ct.clone());
        }
        let resp = self.quic_call("tmux_split", payload).await?;
        let pane_id = resp
            .pointer("/pane/id")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let cmd_display = resp
            .pointer("/pane/command")
            .and_then(|v| v.as_str())
            .unwrap_or("shell");
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("New pane created: {} ({})", pane_id, cmd_display),
        )]))
    }

    /// Capture tmux pane content as text.
    ///
    /// tmux capture-pane で指定ペイン（または全ペイン）のターミナル出力をテキストとして取得する。
    /// AI エージェントが他のペインの状態を把握するのに使う。
    #[tool(
        description = "Capture tmux pane content as text. If pane_id is omitted, captures all panes in the session. Useful for monitoring worker progress or reading terminal output from other panes."
    )]
    async fn tmux_capture(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxCaptureParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(query) = &params.pane_id {
            // 単一ペインキャプチャ（label でも指定可能）
            let (pane_id, display) = self.resolve_pane(query).await?;
            let resp = self
                .quic_call("tmux_capture", serde_json::json!({"pane_id": pane_id}))
                .await?;
            let content = resp.get("content").and_then(|v| v.as_str()).unwrap_or("");
            Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                format!("=== Pane {} ===\n{}", display, content),
            )]))
        } else {
            // 全ペインキャプチャ
            let resp = self
                .quic_call("tmux_capture_all", serde_json::json!({}))
                .await?;
            let captures = resp
                .get("captures")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut output = String::new();
            for cap in &captures {
                let pane_id = cap
                    .pointer("/pane/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let cmd = cap
                    .pointer("/pane/command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let content = cap.get("content").and_then(|v| v.as_str()).unwrap_or("");
                // エージェントメタデータがあればラベルを併記
                let label = cap.pointer("/agent/label").and_then(|v| v.as_str());
                let header = match label {
                    Some(l) => format!("=== {} ({}) [{}] ===", l, pane_id, cmd),
                    None => format!("=== Pane {} ({}) ===", pane_id, cmd),
                };
                output.push_str(&format!("{}\n{}\n\n", header, content));
            }

            if output.is_empty() {
                output = "No tmux panes found (tmux not active?)".to_string();
            }

            Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                output.trim_end().to_string(),
            )]))
        }
    }

    /// Show tmux pane dashboard on Canvas.
    ///
    /// 全 tmux ペインをキャプチャして Canvas に markdown ダッシュボードとして表示する。
    #[tool(
        description = "Show a tmux pane dashboard on Canvas. Captures all panes in the current tmux session and displays them as a markdown dashboard. Great for monitoring parallel workers."
    )]
    async fn tmux_dashboard(&self) -> Result<CallToolResult, McpError> {
        // 全ペインキャプチャ
        let resp = self
            .quic_call("tmux_capture_all", serde_json::json!({}))
            .await?;
        let captures = resp
            .get("captures")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if captures.is_empty() {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                "No tmux panes found (tmux not active?)".to_string(),
            )]));
        }

        // エージェント情報を持つペインと通常ペインを分類
        let mut agent_panes = Vec::new();
        let mut normal_panes = Vec::new();
        for cap in &captures {
            if cap.get("agent").is_some() && !cap["agent"].is_null() {
                agent_panes.push(cap);
            } else {
                normal_panes.push(cap);
            }
        }

        // markdown ダッシュボードを構築
        let mut md = String::from("# tmux Dashboard\n\n");

        // エージェントパイプライン表示
        if !agent_panes.is_empty() {
            md.push_str("## Agent Pipeline\n\n");
            md.push_str("| Pane | Agent | Status | Task |\n");
            md.push_str("|------|-------|--------|------|\n");
            for cap in &agent_panes {
                let pane_id = cap
                    .pointer("/pane/id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let label = cap
                    .pointer("/agent/label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let status = cap
                    .pointer("/agent/status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                let task = cap
                    .pointer("/agent/task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                let status_icon = match status {
                    "running" => "🟢",
                    "waiting" => "⏳",
                    "done" => "✅",
                    "error" => "🔴",
                    _ => "⚪",
                };
                md.push_str(&format!(
                    "| {} | {} | {} {} | {} |\n",
                    pane_id, label, status_icon, status, task
                ));
            }
            md.push('\n');
        }

        // 全ペインの出力表示
        for cap in &captures {
            let pane_id = cap
                .pointer("/pane/id")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let cmd = cap
                .pointer("/pane/command")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let width = cap
                .pointer("/pane/width")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let height = cap
                .pointer("/pane/height")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let active = cap
                .pointer("/pane/active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let content = cap.get("content").and_then(|v| v.as_str()).unwrap_or("");

            // エージェント名があれば表示
            let agent_label = cap.pointer("/agent/label").and_then(|v| v.as_str());
            let agent_status = cap.pointer("/agent/status").and_then(|v| v.as_str());

            // 最後の数行だけ表示（ダッシュボード向け）
            let tail_lines: Vec<&str> = content.lines().rev().take(15).collect();
            let tail: String = tail_lines.into_iter().rev().collect::<Vec<_>>().join("\n");

            let active_marker = if active { " *" } else { "" };
            let agent_info = match (agent_label, agent_status) {
                (Some(label), Some(status)) => format!(" — {} ({})", label, status),
                _ => String::new(),
            };
            md.push_str(&format!(
                "## {} `{}` ({}x{}){}{}\n\n```\n{}\n```\n\n",
                pane_id, cmd, width, height, active_marker, agent_info, tail
            ));
        }

        // Canvas に表示
        self.quic_call(
            "show",
            serde_json::json!({
                "pane_id": "tmux-dashboard",
                "content": {"Markdown": md},
                "title": "tmux Dashboard",
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Dashboard shown on Canvas ({} panes)", captures.len()),
        )]))
    }

    /// Deploy an agent to a new tmux pane.
    ///
    /// tmux split + エージェントメタデータ設定を1コールで実行。
    /// team-bucciarati 等のパイプラインから呼ばれる想定。
    #[tool(
        description = "Deploy an agent to a new tmux pane. Creates a split pane, runs the command, and tags it with agent metadata (label, status, task). Returns the pane ID for subsequent status updates."
    )]
    async fn tmux_agent_deploy(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxAgentDeployParams>,
    ) -> Result<CallToolResult, McpError> {
        // 1. ペイン分割
        let horizontal = params.horizontal.unwrap_or(true);
        let mut split_payload = serde_json::json!({"horizontal": horizontal});
        if let Some(cmd) = &params.command {
            split_payload["command"] = serde_json::Value::String(cmd.clone());
        }
        let resp = self.quic_call("tmux_split", split_payload).await?;
        let pane_id = resp
            .pointer("/pane/id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        // 2. エージェントメタデータ設定
        self.quic_call(
            "tmux_set_agent_meta",
            serde_json::json!({
                "pane_id": pane_id,
                "label": params.label,
                "status": "running",
                "task": params.task,
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Agent '{}' deployed to pane {} (status: running, task: {})",
                params.label,
                pane_id,
                params.task.as_deref().unwrap_or("-")
            ),
        )]))
    }

    /// Update agent status on a tmux pane.
    ///
    /// エージェントの実行ステータスを更新する。ダッシュボードに反映される。
    #[tool(
        description = "Update the status of an agent running in a tmux pane. Status values: 'running', 'waiting', 'done', 'error'. Optionally update the task description."
    )]
    async fn tmux_agent_status(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxAgentStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let (pane_id, display) = self.resolve_pane(&params.pane_id).await?;
        let mut payload = serde_json::json!({
            "pane_id": pane_id,
            "status": params.status,
        });
        if let Some(task) = &params.task {
            payload["task"] = serde_json::Value::String(task.clone());
        }
        self.quic_call("tmux_update_agent_status", payload).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Agent status updated: pane={}, status={}",
                display, params.status
            ),
        )]))
    }

    /// Send text input to a tmux pane (agent intervention).
    ///
    /// tmux send-keys でペインにテキストを送信する。
    /// エージェントへの介入やユーザー入力の自動化に使う。
    #[tool(
        description = "Send text input to a tmux pane via send-keys. Use this to intervene in an agent's execution or provide input. Text is sent as-is (include '\\n' for Enter)."
    )]
    async fn tmux_agent_send(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<TmuxAgentSendParams>,
    ) -> Result<CallToolResult, McpError> {
        let (pane_id, display) = self.resolve_pane(&params.pane_id).await?;
        self.quic_call(
            "tmux_send_keys",
            serde_json::json!({
                "pane_id": pane_id,
                "keys": params.text,
            }),
        )
        .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Sent to pane {}: {:?}", display, params.text),
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

        // TheWorld 経由で Canvas にキャプチャリクエスト（Canvas は常に TheWorld の WS に接続）
        let world_port = crate::cli::WORLD_PORT;
        let url = format!("http://[::1]:{}/api/canvas/capture", world_port);
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

    /// VantagePoint.app のターミナルウィンドウを PNG スクリーンショットとしてキャプチャ
    ///
    /// macOS の screencapture コマンドで VantagePoint ウィンドウをキャプチャする。
    /// 保存されたファイルは Claude の Read ツールで画像として確認可能。
    #[tool(
        description = "Capture the VantagePoint.app terminal window as a PNG screenshot. The saved file can be viewed with the Read tool. Use this to inspect rendering issues, verify UI changes, or debug visual problems."
    )]
    async fn capture_terminal(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<CaptureTerminalParams>,
    ) -> Result<CallToolResult, McpError> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let save_path = params
            .path
            .unwrap_or_else(|| format!("/tmp/vp-terminal-{}.png", ts));

        // VantagePoint ウィンドウの CGWindowID を取得
        // NOTE: kCGWindowOwnerName はアプリの表示名（"Vantage Point"）であり
        //       バイナリ名（"VantagePoint"）とは異なる。
        //       JXA の ObjC bridge は権限制限で動作しないことがあるため swift -e を使用。
        //       Layer=0 かつ Name 非空のウィンドウがメインウィンドウ。
        let swift_script = r#"
import CoreGraphics
let windows = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as? [[String: Any]] ?? []
// layer=0 で最も大きいウィンドウをメインウィンドウとみなす
// （ウィンドウタイトルが空の場合があるため name ではなくサイズで判定）
var bestId = 0
var bestArea = 0
for w in windows {
    let owner = w["kCGWindowOwnerName"] as? String ?? ""
    if owner.contains("Vantage") || owner == "VantagePoint" {
        let layer = w["kCGWindowLayer"] as? Int ?? -1
        if layer == 0 {
            let bounds = w["kCGWindowBounds"] as? [String: Any] ?? [:]
            let width = bounds["Width"] as? Int ?? 0
            let height = bounds["Height"] as? Int ?? 0
            let area = width * height
            if area > bestArea {
                bestArea = area
                bestId = w["kCGWindowNumber"] as? Int ?? 0
            }
        }
    }
}
if bestId > 0 { print(bestId) }
"#;
        let wid_output = tokio::process::Command::new("swift")
            .args(["-e", swift_script])
            .output()
            .await
            .map_err(|e| McpError::internal_error(format!("swift 実行失敗: {}", e), None))?;

        let window_id = if wid_output.status.success() {
            let id = String::from_utf8_lossy(&wid_output.stdout)
                .trim()
                .to_string();
            if id.is_empty() {
                return Err(McpError::internal_error(
                    "VantagePoint ウィンドウが見つかりません。VantagePoint.app が起動しているか確認してください。".to_string(),
                    None,
                ));
            }
            id
        } else {
            let stderr = String::from_utf8_lossy(&wid_output.stderr);
            return Err(McpError::internal_error(
                format!(
                    "VantagePoint ウィンドウ ID 取得失敗（VantagePoint.app が未起動の可能性）: {}",
                    stderr.trim()
                ),
                None,
            ));
        };

        let mut cmd = tokio::process::Command::new("screencapture");
        cmd.args(["-x", "-o"]); // -x: サウンドなし, -o: 影なし
        cmd.args(["-l", &window_id]); // -l: ウィンドウ ID 指定
        cmd.arg(&save_path);

        let output = cmd.output().await.map_err(|e| {
            McpError::internal_error(format!("screencapture 実行失敗: {}", e), None)
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(McpError::internal_error(
                format!("screencapture 失敗: {}", stderr),
                None,
            ));
        }

        // ファイルサイズ取得
        let metadata = tokio::fs::metadata(&save_path)
            .await
            .map_err(|e| McpError::internal_error(format!("保存ファイル確認失敗: {}", e), None))?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Terminal screenshot saved: {}\nSize: {} bytes\nUse the Read tool to view this image.",
                save_path,
                metadata.len()
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
        if let Some(code) = exit_code
            && code != 0
        {
            result.push_str(&format!(" (exit code: {})", code));
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

        // URL からポートを抽出（失敗時は VP_PROCESS_PORT → 33000 の順でフォールバック）
        let port: u16 = base_url
            .split(':')
            .next_back()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| {
                std::env::var("VP_PROCESS_PORT")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(33000)
            });

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
        let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());
        let mut cmd = std::process::Command::new(&vp_bin);
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

    // =========================================================================
    // Msgbox ツール (VP-24)
    // =========================================================================

    /// Send a message to a msgbox address
    #[tool(
        description = "Send a message to a VP Msgbox address. Use this for inter-agent communication (replaces ccwire). Local actors: 'agent' (Echoes lead), 'protocol', 'notify'. World actor: 'hermit_purple@world' (MIDI / external control). Cross-process: 'agent@<project>' — call msg_directory first to discover available addresses across all VP processes. All messages are persisted (VP-158, no opt-in needed)."
    )]
    async fn msg_send(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<MsgSendParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::capability::msgbox::MessageKind;

        let kind = match params.kind.as_deref() {
            Some("notification") => MessageKind::Notification,
            Some("request") => MessageKind::Request,
            Some("response") => MessageKind::Response,
            _ => MessageKind::Direct,
        };

        // VP-157: from = "agent"（= lead の正規 address、 旧 "mcp" は廃止）。
        // VP-166 PR-4: worker context なら from = "agent@<parent>/<name>"（= 自 lane の address。
        // bare "agent" だと cross-process forward で送信元 SP の `normalize_from` が間違った
        // project でスタンプしうる — VP-165 の from 汚染の一因。最初から正しい from を付ける）。
        let from = self.self_lane.from_address();
        let mut msg = crate::capability::msgbox::Message::new(&from, &params.to, kind)
            .with_payload(&params.payload);

        if let Some(reply_to) = params.reply_to {
            msg = msg.with_reply_to(reply_to);
        }

        // VP-158: 全 msg 永続化が default のため persistent flag 廃止 (= 「on-memory」 概念排除)

        // opt-in TTL
        if let Some(secs) = params.ttl_secs {
            msg = msg.with_ttl_secs(secs);
        }

        // opt-in 明示 ack モード
        if params.manual_ack.unwrap_or(false) {
            msg = msg.manual_ack();
        }

        let msg_id = msg.id.clone();

        // QUIC 経由で Process の Msgbox に送信
        let payload = serde_json::to_value(&msg)
            .map_err(|e| McpError::internal_error(format!("Serialize error: {}", e), None))?;
        self.quic_call("msg_send", payload).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Message sent to '{}' (id: {})", params.to, msg_id),
        )]))
    }

    /// List registered msgbox addresses
    #[tool(
        description = "List all registered Msgbox addresses in the current Process. Shows which capabilities and agents have active msgboxes."
    )]
    async fn msg_peers(&self) -> Result<CallToolResult, McpError> {
        let resp = self.quic_call("msg_peers", serde_json::json!({})).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "[]".to_string()),
        )]))
    }

    /// Cross-process actor directory (VP-65)
    #[tool(
        description = "List all registered actors across all VP processes via TheWorld registry. Returns project_name → [actors] mapping. Use this to discover available addresses (e.g. 'agent@creo-memories') before calling msg_send. Optionally filter by project_name."
    )]
    async fn msg_directory(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<MsgDirectoryParams>,
    ) -> Result<CallToolResult, McpError> {
        // TheWorld の HTTP API 経由で actor 一覧を取得
        let world_port = crate::cli::WORLD_PORT;
        let url = match params.project_name.as_deref() {
            Some(p) => format!(
                "http://[::1]:{}/api/world/msgbox/list?project_name={}",
                world_port,
                urlencoding::encode(p)
            ),
            None => format!("http://[::1]:{}/api/world/msgbox/list", world_port),
        };

        let client = reqwest::Client::new();
        let resp = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                return Err(McpError::internal_error(
                    format!("TheWorld API error: {} {}", status, text),
                    None,
                ));
            }
            Err(e) => {
                return Err(McpError::internal_error(
                    format!("Failed to reach TheWorld: {}", e),
                    None,
                ));
            }
        };

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                return Err(McpError::internal_error(
                    format!("Failed to parse TheWorld response: {}", e),
                    None,
                ));
            }
        };

        // entries を project_name → [actors] にグループ化
        let entries = body
            .get("entries")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut by_project: std::collections::BTreeMap<String, (u16, Vec<String>)> =
            std::collections::BTreeMap::new();
        for entry in &entries {
            let project = entry
                .get("project_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let actor = entry
                .get("actor")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let port = entry.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            if project.is_empty() || actor.is_empty() {
                continue;
            }
            let entry = by_project.entry(project).or_insert((port, Vec::new()));
            entry.0 = port;
            entry.1.push(actor);
        }

        let directory: serde_json::Value = by_project
            .into_iter()
            .map(|(project, (port, actors))| {
                (
                    project,
                    serde_json::json!({
                        "port": port,
                        "actors": actors,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into();

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&serde_json::json!({
                "count": entries.len(),
                "directory": directory,
            }))
            .unwrap_or_else(|_| "{}".to_string()),
        )]))
    }

    /// Receive a message from a lane's mailbox (VP-166: (lane, stand)-aware)
    #[tool(
        description = "Receive a message from a lane's mailbox in this project. Params: `lane` (default 'lead', or a worker name like 'chore') and `stand` (default 'agent' = the lane's Claude session inbox; 'canvas' = the lane's Paisley Park / Canvas inbox). Reads box `<stand>#<lane>` (e.g. `agent#lead` for the lead's inbox, `agent#chore` for worker 'chore'). Waits up to timeout seconds; returns immediately if a message is queued. Use this for inter-lane communication. Senders write to `agent@<project>` (lead) / `agent@<project>/<name>` (worker)."
    )]
    async fn msg_recv(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<MsgRecvParams>,
    ) -> Result<CallToolResult, McpError> {
        let timeout = params.timeout.unwrap_or(5).min(30);
        // VP-166 PR-4: lane 省略時は自 lane を default に（worker context なら自分の worker 名、
        // lead context なら "lead"）。これで worker の Echoes が `msg_recv`（lane 省略）を叩くと
        // 自分の `agent#<name>` box を読む（lead の box を盗まない）。
        let lane = params
            .lane
            .unwrap_or_else(|| self.self_lane.lane_name.clone());
        let stand = params.stand.unwrap_or_else(|| "agent".to_string());

        let payload = serde_json::json!({
            "timeout": timeout,
            "from": params.from,
            "lane": lane,
            "stand": stand,
        });

        // VP-163: server 側は最大 `timeout` 秒ブロックするので、 outer timeout は +5s 余裕を持たせる。
        // (旧: quic_call 固定 5s で timeout>=5 が必ず空振りリトライ + msg ロスしていた)
        let resp = self
            .quic_call_with_timeout(
                "msg_recv",
                payload,
                std::time::Duration::from_secs(timeout + 5),
            )
            .await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "null".to_string()),
        )]))
    }

    /// Acknowledge a message explicitly (for manual_ack mode)
    #[tool(
        description = "Explicitly acknowledge a message received with manual_ack=true. Removes the message from the persistent store. No-op for auto-ack messages (they are acked automatically on recv)."
    )]
    async fn msg_ack(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<MsgAckParams>,
    ) -> Result<CallToolResult, McpError> {
        let payload = serde_json::json!({
            "id": params.id,
        });
        self.quic_call("msg_ack", payload).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Acked message {}", params.id),
        )]))
    }

    /// Get the full thread (reply_to chain) for a given message
    #[tool(
        description = "Trace the reply_to chain from a given message and return all messages in the thread (root + all descendants), sorted by timestamp."
    )]
    async fn msg_thread(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<MsgThreadParams>,
    ) -> Result<CallToolResult, McpError> {
        let payload = serde_json::json!({ "id": params.id });
        let resp = self.quic_call("msg_thread", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "thread retrieved".to_string()),
        )]))
    }

    // ========================================================================
    // VP Port Management (VP-83 refinement, memory: mem_1CaKCbNE24KTQDuf9x4Eim)
    //
    // Deterministic port layout に基づき、slot × lane × role から port / URL
    // を透過的に計算して返す。HD agent は msg_send 結果の URL を WebFetch で
    // 読み込む等、自律 workflow で port を問い合わせできる。
    // ========================================================================

    /// Port 計算: slot × lane × role → port
    #[tool(
        description = "Compute the deterministic port for a given project slot + lane index + role. Returns the port number. Use port_url to get the full URL."
    )]
    async fn port_show(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<PortShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let port = match params.role.as_deref() {
            Some(role) => layout.port(params.slot, params.lane, role),
            None => layout.lane_base(params.slot, params.lane),
        };
        let text = match port {
            Some(p) => format!("{}", p),
            None => format!(
                "no port: slot={}, lane={}, role={:?}",
                params.slot, params.lane, params.role
            ),
        };
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            text,
        )]))
    }

    /// URL 生成: `http://localhost:{port}`
    #[tool(
        description = "Generate a localhost URL (http://localhost:{port}) for a project slot + lane + role. Convenience wrapper over port_show."
    )]
    async fn port_url(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<PortUrlParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let text = match layout.url(params.slot, params.lane, &params.role) {
            Some(u) => u,
            None => format!(
                "no URL: slot={}, lane={}, role={}",
                params.slot, params.lane, params.role
            ),
        };
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            text,
        )]))
    }

    /// Role → offset table
    #[tool(
        description = "List all registered port roles (agent/dev_server/db_admin/canvas/preview) with their offsets within a Lane."
    )]
    async fn port_roles(
        &self,
        // VP-XXX (rmcp 1.6 follow-up): `Parameters<()>` は schemars 1.x で `{const: null}` schema を
        // 生成 → rmcp 1.6 が MCP spec 厳格 check で reject (`tools[18].inputSchema.type` expected
        // `"object"`)。 空 struct (`PortRolesParams`) で `{type: "object", properties: {}}` 形式を
        // 生成させて MCP client の zod validation を通過させる。
        _params: rmcp::handler::server::wrapper::Parameters<PortRolesParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let mut out = format!("Role offsets (lane_size={}):\n", layout.lane_size);
        for (name, offset) in layout.valid_roles() {
            out.push_str(&format!("  +{:>2}  {}\n", offset, name));
        }
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            out,
        )]))
    }

    /// 1 Project slot の全割当一覧 (Markdown)
    #[tool(
        description = "Show the full port layout for one project slot: SP HTTP, SP Unison, and all Lane × role combinations. Returns Markdown-formatted text."
    )]
    async fn port_layout(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<PortLayoutParams>,
    ) -> Result<CallToolResult, McpError> {
        let layout = crate::port_layout::PortLayout::default();
        let Some(base) = layout.project_base(params.slot) else {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                format!(
                    "slot {} is out of range (max_projects={})",
                    params.slot, layout.max_projects
                ),
            )]));
        };
        let mut md = format!("# Project slot {} (base {})\n\n", params.slot, base);
        md.push_str(&format!(
            "- SP HTTP: `{}`\n- SP Unison: `{}`\n\n",
            layout.sp_port(params.slot).unwrap(),
            layout.unison_port(params.slot).unwrap()
        ));
        for lane in 0..layout.max_lanes_per_project() {
            let Some(lb) = layout.lane_base(params.slot, lane) else {
                continue;
            };
            let label = if lane == 0 { "Lead" } else { "Worker" };
            md.push_str(&format!("## Lane {} ({}) — base `{}`\n\n", lane, label, lb));
            for (role, offset) in layout.valid_roles() {
                if let Some(p) = layout.port(params.slot, lane, &role) {
                    md.push_str(&format!("- +{} `{}`: `{}`\n", offset, role, p));
                }
            }
            md.push('\n');
        }
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            md,
        )]))
    }
}

// --- Port Management tool params ---

/// Parameters for `port_show`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortShowParams {
    /// Project slot (0-based, see port_slot_list for mapping)
    #[schemars(description = "Project slot index (0-19)")]
    pub slot: u16,
    /// Lane index (0 = Lead, 1+ = Worker)
    #[schemars(description = "Lane index within project slot (0 = Lead, 1+ = Worker)")]
    #[serde(default)]
    pub lane: u16,
    /// Role (agent / dev_server / db_admin / canvas / preview)
    #[schemars(
        description = "Role within Lane (agent/dev_server/db_admin/canvas/preview). Omit to get the Lane base port."
    )]
    pub role: Option<String>,
}

/// Parameters for `port_url`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortUrlParams {
    pub slot: u16,
    #[serde(default)]
    pub lane: u16,
    pub role: String,
}

/// Parameters for `port_layout`
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortLayoutParams {
    /// Project slot to show
    pub slot: u16,
}

#[tool_handler]
impl rmcp::ServerHandler for VantageMcp {
    fn get_info(&self) -> ServerInfo {
        // rmcp 1.6 で ServerInfo は #[non_exhaustive] になり struct expression (= `ServerInfo { ... ..Default::default() }`)
        // が外部 crate から使えなくなった。 `Default::default()` で base instance を作ってから pub field を mutate
        // する pattern で API contract を満たす (= 公式が future-compatible として用意してる upgrade path)。
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Vantage Point Process - Display rich content (markdown, HTML, images) in a browser viewer. \
             Use 'capture_canvas' to take a PNG screenshot of the Canvas (viewable with Read tool), \
             'show' to display content, 'clear' to clear panes, \
             'close_pane' to close a pane, 'toggle_pane' to toggle panel visibility, \
             'permission' to request user approval, 'restart' to restart the Process, \
             'watch_file' to monitor a log file in real-time, and 'unwatch_file' to stop monitoring.\n\n\
             When using 'show', prefer content_type='markdown' as the default format. \
             Markdown renders well in the Canvas and is easy to read. \
             Use content_type='html' only when you need precise visual layout (dashboards, diagrams with colors, interactive elements).".into()
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

/// worker context のとき parent project の path を config から引く（VP-165 (A)）。
///
/// lead context（`worker_parent = None`）or parent が config に無い なら `None`。
fn worker_parent_path(self_lane: &SelfLane, config: &crate::config::Config) -> Option<String> {
    let parent_name = self_lane.worker_parent.as_ref()?;
    config
        .projects
        .iter()
        .find(|p| &p.name == parent_name)
        .map(|p| p.path.clone())
}

/// Process ポートを解決（MCP 通信先の決定）
///
/// VP-165 (doc 17 決定A): **discovery（= TheWorld、reconciliation の真実源）で live port を
/// 引くのを最優先**にする。`VP_PROCESS_PORT` env は tmux セッション作成時の snapshot で、
/// port reshuffle（config の project リスト変更）に追従しない → stale を踏むと別 project の
/// SP に msg を投げ、その SP の `local_project` で `from` が汚染される（VP-165 dogfood 症状 (1)）。
/// env は discovery 一時障害時の fallback に格下げ。
///
/// 優先度:
/// 1. 明示的なポート引数（Some で指定された場合）
/// 2. discovery:
///    - worker context（cwd = `vp_data_dir()/lanes/<parent>-<name>`）→ parent project の path を
///      config から引いて `find_by_project`。worker の cwd は登録 project path 配下でないので
///      `find_for_cwd` は効かない
///    - lead context → `find_for_cwd`（cwd 一致 or 配下の running SP）
/// 3. `VP_PROCESS_PORT` env（discovery 障害 / parent SP 未起動 時の fast path、reshuffle 後は古い可能性）
/// 4. デフォルトポート 33000
async fn resolve_process_port(explicit_port: Option<u16>) -> u16 {
    // 1. 明示的なポート指定
    if let Some(port) = explicit_port {
        return port;
    }

    // 2. discovery で live port を引く
    let self_lane = SelfLane::detect();
    match &self_lane.worker_parent {
        Some(_) => {
            // worker: parent project の SP を discovery で解決
            if let Some(parent_path) = crate::config::Config::load()
                .ok()
                .as_ref()
                .and_then(|c| worker_parent_path(&self_lane, c))
                && let Some(info) = crate::discovery::find_by_project(&parent_path).await
            {
                return info.port;
            }
        }
        None => {
            // lead: cwd 一致（or 配下）の running SP
            if let Some(info) = crate::discovery::find_for_cwd().await {
                return info.port;
            }
        }
    }

    // 3. VP_PROCESS_PORT env（fallback、reshuffle 後は古い可能性あり）
    if let Ok(env_port) = std::env::var("VP_PROCESS_PORT")
        && let Ok(port) = env_port.parse::<u16>()
    {
        return port;
    }

    // 4. フォールバック
    33000
}

/// Run the MCP server over stdio
pub async fn run_mcp_server(process_port: Option<u16>) -> anyhow::Result<()> {
    // rustls 0.23+ は CryptoProvider の明示的な設定が必要（QUIC 接続用）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // トレースログファイルを早期初期化
    crate::trace_log::init_log_file();

    // Resolve the actual port to use
    let resolved_port = resolve_process_port(process_port).await;

    // VP-83: Worker self-register — cwd が lane Worker dir なら、起動時に
    // TheWorld msgbox registry に worker-{name}@{project} を登録する。
    // Worker の Claude CLI (MCP server) が自分を名乗る = Worker 責務の体現。
    self_register_if_worker().await;

    // Note: In MCP mode, we should not use tracing to stdout
    // as it interferes with JSON-RPC communication
    let service = VantageMcp::new(resolved_port)
        .serve(stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {}", e))?;

    service.waiting().await?;

    Ok(())
}

/// Worker の自己登録 — cwd が `vp_data_dir()/lanes/{project}-{worker}` パターンなら、
/// parent project を config の最長一致で決定、TheWorld に worker-{name}@{project} 登録。
///
/// Worker 側の責務 (user 提案 2026-04-25): Worker が自分で名乗る = push model。
/// SP や TheWorld が Worker を代理登録する pull model は設計違反。
async fn self_register_if_worker() {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return,
    };
    // lane dir prefix か確認
    let lanes_root = match crate::lane::config::wings_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    if !cwd.starts_with(&lanes_root) {
        return; // Worker でない (通常 project の cwd)
    }
    let dirname = match cwd.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return,
    };
    // config から parent project を最長一致で resolve
    let config = match crate::config::Config::load() {
        Ok(c) => c,
        Err(_) => return,
    };
    let parent = match config
        .projects
        .iter()
        .filter(|p| dirname.starts_with(&format!("{}-", p.name)))
        .max_by_key(|p| p.name.len())
    {
        Some(p) => p,
        None => return,
    };
    let worker_name = match dirname.strip_prefix(&format!("{}-", parent.name)) {
        Some(w) => w.to_string(),
        None => return,
    };
    // parent SP の port を discovery で取得
    let Some(process) = crate::discovery::find_by_project_blocking(&parent.path) else {
        // parent SP 未起動 — 今は skip。SP 起動後に user が手動 register or ws resync
        return;
    };
    let actor = format!("worker-{}", worker_name);
    let world_port = crate::cli::WORLD_PORT;
    let _ = crate::capability::msgbox_remote::register_actor_to_world(
        world_port,
        &parent.name,
        process.port,
        &actor,
    )
    .await;
    // stderr に出して MCP stdio (stdout) に干渉しないように
    eprintln!(
        "vp mcp self-register: {actor}@{} → port {}",
        parent.name, process.port
    );
}

/// QUIC "process" チャネルのレスポンスが server ハンドラの Err かどうか判定する (VP-163)。
///
/// unison は専用の error frame を持たないため、 `unison_server.rs` の dispatch loop は
/// ハンドラの `Err(e)` を **成功フレームに `{"error": "<msg>"}` を詰めて** 返す。
/// その形 (= 単一キー `"error"` で値が string) なら err message を返す。 それ以外は `None`。
/// (`{"error": ..., "to": ...}` のような複数キーや非 string は対象外 — 通常の payload)
fn rpc_response_error(resp: &serde_json::Value) -> Option<&str> {
    let obj = resp.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    obj.get("error")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_self_lane_from_address() {
        // VP-166 PR-4: lead は bare "agent"、worker は "agent@<parent>/<name>"
        let lead = SelfLane {
            lane_name: "lead".to_string(),
            worker_parent: None,
        };
        assert_eq!(lead.from_address(), "agent");

        let worker = SelfLane {
            lane_name: "chore".to_string(),
            worker_parent: Some("vantage-point".to_string()),
        };
        assert_eq!(worker.from_address(), "agent@vantage-point/chore");
    }

    #[test]
    fn test_worker_parent_path_resolution() {
        use crate::config::{Config, ProjectConfig};
        let mut cfg = Config::default();
        cfg.projects.push(ProjectConfig {
            name: "vantage-point".to_string(),
            path: "/Users/x/repos/vantage-point".to_string(),
            port: None,
            enabled: true,
            slot: None,
        });

        // worker → parent の path
        let worker = SelfLane {
            lane_name: "chore".to_string(),
            worker_parent: Some("vantage-point".to_string()),
        };
        assert_eq!(
            worker_parent_path(&worker, &cfg).as_deref(),
            Some("/Users/x/repos/vantage-point")
        );

        // lead context → None（worker_parent が無い）
        let lead = SelfLane {
            lane_name: "lead".to_string(),
            worker_parent: None,
        };
        assert_eq!(worker_parent_path(&lead, &cfg), None);

        // config に無い parent → None
        let unknown = SelfLane {
            lane_name: "x".to_string(),
            worker_parent: Some("not-in-config".to_string()),
        };
        assert_eq!(worker_parent_path(&unknown, &cfg), None);
    }

    #[test]
    fn test_rpc_response_error_detection() {
        // server ハンドラの Err 形式 → err message を取り出す
        assert_eq!(
            rpc_response_error(
                &serde_json::json!({"error": "自分自身('agent')への送信はできません"})
            ),
            Some("自分自身('agent')への送信はできません")
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!({"error": "不明なメソッド: process.foo"})),
            Some("不明なメソッド: process.foo")
        );

        // 通常の成功 payload は対象外
        assert_eq!(
            rpc_response_error(&serde_json::json!({"status": "ok", "id": "x"})),
            None
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!({"message": null, "reason": "timeout"})),
            None
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!({"message": {"id": "x"}})),
            None
        );

        // 複数キーで "error" を含んでも、 これは err frame ではない (= HTTP handler 等の形)
        assert_eq!(
            rpc_response_error(&serde_json::json!({"error": "x", "to": "agent"})),
            None
        );

        // "error" が非 string、 object 以外
        assert_eq!(rpc_response_error(&serde_json::json!({"error": 42})), None);
        assert_eq!(
            rpc_response_error(&serde_json::json!({"error": {"nested": true}})),
            None
        );
        assert_eq!(
            rpc_response_error(&serde_json::json!("just a string")),
            None
        );
        assert_eq!(rpc_response_error(&serde_json::json!(null)), None);
        assert_eq!(rpc_response_error(&serde_json::json!([1, 2, 3])), None);
        assert_eq!(rpc_response_error(&serde_json::json!({})), None);
    }
}
