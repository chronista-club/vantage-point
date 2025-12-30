//! AG-UI Protocol Implementation
//!
//! Agent-User Interaction Protocol for connecting AI agents to user-facing applications.
//! Based on: https://docs.ag-ui.com
//!
//! ## Requirements Coverage
//! - REQ-AGUI-041: Event type definitions (this module)
//! - REQ-AGUI-040: WebSocket transport (integration with stand/server.rs)

use serde::{Deserialize, Serialize};

// =============================================================================
// Core Event Types
// =============================================================================

/// AG-UI Event - the main event type for agent-to-UI communication
///
/// All events are tagged with a `type` field for JSON serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgUiEvent {
    // -------------------------------------------------------------------------
    // Lifecycle Events (REQ-AGUI-001, REQ-AGUI-002, REQ-AGUI-003)
    // -------------------------------------------------------------------------
    /// Signals that an agent run has started
    RunStarted {
        /// Unique identifier for this run
        run_id: String,
        /// Optional thread/conversation ID
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        /// Timestamp (Unix millis)
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Signals that an agent run has finished successfully
    RunFinished {
        run_id: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Signals that an agent run has encountered an error
    RunError {
        run_id: String,
        error: AgUiError,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Signals that a step within a run has started
    StepStarted {
        run_id: String,
        step_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_name: Option<String>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Signals that a step within a run has finished
    StepFinished {
        run_id: String,
        step_id: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Text Message Events (REQ-AGUI-010)
    // -------------------------------------------------------------------------
    /// Start of a text message stream
    TextMessageStart {
        run_id: String,
        message_id: String,
        role: MessageRole,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Content chunk within a text message stream
    TextMessageContent {
        run_id: String,
        message_id: String,
        /// The text content delta
        delta: String,
    },

    /// End of a text message stream
    TextMessageEnd {
        run_id: String,
        message_id: String,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Tool Call Events (REQ-AGUI-020, REQ-AGUI-021)
    // -------------------------------------------------------------------------
    /// Start of a tool call
    ToolCallStart {
        run_id: String,
        tool_call_id: String,
        tool_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<String>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Streaming chunk of tool call arguments
    ToolCallArgsChunk {
        run_id: String,
        tool_call_id: String,
        /// JSON string delta for arguments
        delta: String,
    },

    /// End of a tool call (with result)
    ToolCallEnd {
        run_id: String,
        tool_call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Human-in-the-Loop Events (REQ-AGUI-021)
    // -------------------------------------------------------------------------
    /// Request user permission for an action
    PermissionRequest {
        run_id: String,
        request_id: String,
        tool_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Tool input parameters
        input: serde_json::Value,
        /// Timeout in seconds
        #[serde(default = "default_timeout")]
        timeout_seconds: u32,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// User's response to a permission request
    PermissionResponse {
        run_id: String,
        request_id: String,
        approved: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Modified input if user edited it
        #[serde(skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // User Prompt Events (REQ-PROMPT-001 to REQ-PROMPT-005)
    // -------------------------------------------------------------------------
    /// Request user input (confirm, text input, selection)
    UserPrompt {
        run_id: String,
        request_id: String,
        /// Prompt type: confirm, input, select, multi_select
        prompt_type: UserPromptType,
        /// Title displayed to user
        title: String,
        /// Optional detailed description
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Options for select/multi_select types
        #[serde(skip_serializing_if = "Option::is_none")]
        options: Option<Vec<PromptOption>>,
        /// Default value for input type
        #[serde(skip_serializing_if = "Option::is_none")]
        default_value: Option<String>,
        /// Timeout in seconds (default: 300)
        #[serde(default = "default_prompt_timeout")]
        timeout_seconds: u32,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// User's response to a prompt
    UserPromptResponse {
        run_id: String,
        request_id: String,
        /// Response outcome
        outcome: UserPromptOutcome,
        /// Text response (for input type or optional comment)
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// Selected option IDs (for select/multi_select)
        #[serde(skip_serializing_if = "Option::is_none")]
        selected_options: Option<Vec<String>>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // State Sync Events (REQ-AGUI-030, REQ-AGUI-031)
    // -------------------------------------------------------------------------
    /// Full state snapshot
    StateSnapshot {
        run_id: String,
        /// The complete state object
        state: serde_json::Value,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Incremental state update (JSON Patch format)
    StateDelta {
        run_id: String,
        /// JSON Patch operations
        delta: Vec<JsonPatchOp>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Activity Events
    // -------------------------------------------------------------------------
    /// Activity/progress snapshot
    ActivitySnapshot {
        run_id: String,
        activities: Vec<Activity>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Activity/progress delta
    ActivityDelta {
        run_id: String,
        /// Activities to add or update
        #[serde(default)]
        upsert: Vec<Activity>,
        /// Activity IDs to remove
        #[serde(default)]
        remove: Vec<String>,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    // -------------------------------------------------------------------------
    // Extension Events
    // -------------------------------------------------------------------------
    /// Raw event from external system (passthrough)
    Raw {
        run_id: String,
        source: String,
        payload: serde_json::Value,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },

    /// Custom application-specific event
    Custom {
        run_id: String,
        name: String,
        data: serde_json::Value,
        #[serde(default = "now_millis")]
        timestamp: u64,
    },
}

// =============================================================================
// Supporting Types
// =============================================================================

/// Message role
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

/// Error information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgUiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// JSON Patch operation (RFC 6902)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum JsonPatchOp {
    Add {
        path: String,
        value: serde_json::Value,
    },
    Remove {
        path: String,
    },
    Replace {
        path: String,
        value: serde_json::Value,
    },
    Move {
        from: String,
        path: String,
    },
    Copy {
        from: String,
        path: String,
    },
    Test {
        path: String,
        value: serde_json::Value,
    },
}

/// Activity/progress item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub id: String,
    pub label: String,
    pub status: ActivityStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// Activity status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    Pending,
    Running,
    Completed,
    Error,
    Cancelled,
}

/// Progress information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub current: u32,
    pub total: u32,
}

// =============================================================================
// User Prompt Types (REQ-PROMPT-001)
// =============================================================================

/// User prompt type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPromptType {
    /// Yes/No confirmation
    Confirm,
    /// Text input
    Input,
    /// Single selection
    Select,
    /// Multiple selection
    MultiSelect,
}

/// User prompt response outcome
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPromptOutcome {
    /// User approved/confirmed
    Approved,
    /// User rejected/declined
    Rejected,
    /// User cancelled
    Cancelled,
    /// Request timed out
    Timeout,
}

/// Prompt option for select/multi_select
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptOption {
    /// Unique option identifier
    pub id: String,
    /// Display label
    pub label: String,
    /// Optional description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Get current timestamp in milliseconds
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn default_timeout() -> u32 {
    30
}

/// Default timeout for user prompts (5 minutes)
pub fn default_prompt_timeout() -> u32 {
    300 // 5 minutes for user prompts
}

// =============================================================================
// Event Builder (convenience methods)
// =============================================================================

impl AgUiEvent {
    /// Create a RunStarted event
    pub fn run_started(run_id: impl Into<String>) -> Self {
        Self::RunStarted {
            run_id: run_id.into(),
            thread_id: None,
            timestamp: now_millis(),
        }
    }

    /// Create a RunFinished event
    pub fn run_finished(run_id: impl Into<String>) -> Self {
        Self::RunFinished {
            run_id: run_id.into(),
            timestamp: now_millis(),
        }
    }

    /// Create a RunError event
    pub fn run_error(
        run_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::RunError {
            run_id: run_id.into(),
            error: AgUiError {
                code: code.into(),
                message: message.into(),
                details: None,
            },
            timestamp: now_millis(),
        }
    }

    /// Create a TextMessageStart event
    pub fn text_message_start(
        run_id: impl Into<String>,
        message_id: impl Into<String>,
        role: MessageRole,
    ) -> Self {
        Self::TextMessageStart {
            run_id: run_id.into(),
            message_id: message_id.into(),
            role,
            timestamp: now_millis(),
        }
    }

    /// Create a TextMessageContent event
    pub fn text_message_content(
        run_id: impl Into<String>,
        message_id: impl Into<String>,
        delta: impl Into<String>,
    ) -> Self {
        Self::TextMessageContent {
            run_id: run_id.into(),
            message_id: message_id.into(),
            delta: delta.into(),
        }
    }

    /// Create a TextMessageEnd event
    pub fn text_message_end(run_id: impl Into<String>, message_id: impl Into<String>) -> Self {
        Self::TextMessageEnd {
            run_id: run_id.into(),
            message_id: message_id.into(),
            timestamp: now_millis(),
        }
    }

    /// Create a ToolCallStart event
    pub fn tool_call_start(
        run_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        Self::ToolCallStart {
            run_id: run_id.into(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            parent_message_id: None,
            timestamp: now_millis(),
        }
    }

    /// Create a PermissionRequest event
    pub fn permission_request(
        run_id: impl Into<String>,
        request_id: impl Into<String>,
        tool_name: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        Self::PermissionRequest {
            run_id: run_id.into(),
            request_id: request_id.into(),
            tool_name: tool_name.into(),
            description: None,
            input,
            timeout_seconds: default_timeout(),
            timestamp: now_millis(),
        }
    }

    /// Get the run_id from any event
    pub fn run_id(&self) -> &str {
        match self {
            Self::RunStarted { run_id, .. }
            | Self::RunFinished { run_id, .. }
            | Self::RunError { run_id, .. }
            | Self::StepStarted { run_id, .. }
            | Self::StepFinished { run_id, .. }
            | Self::TextMessageStart { run_id, .. }
            | Self::TextMessageContent { run_id, .. }
            | Self::TextMessageEnd { run_id, .. }
            | Self::ToolCallStart { run_id, .. }
            | Self::ToolCallArgsChunk { run_id, .. }
            | Self::ToolCallEnd { run_id, .. }
            | Self::PermissionRequest { run_id, .. }
            | Self::PermissionResponse { run_id, .. }
            | Self::UserPrompt { run_id, .. }
            | Self::UserPromptResponse { run_id, .. }
            | Self::StateSnapshot { run_id, .. }
            | Self::StateDelta { run_id, .. }
            | Self::ActivitySnapshot { run_id, .. }
            | Self::ActivityDelta { run_id, .. }
            | Self::Raw { run_id, .. }
            | Self::Custom { run_id, .. } => run_id,
        }
    }

    /// Create a UserPrompt event (REQ-PROMPT-001)
    pub fn user_prompt(
        run_id: impl Into<String>,
        request_id: impl Into<String>,
        prompt_type: UserPromptType,
        title: impl Into<String>,
    ) -> Self {
        Self::UserPrompt {
            run_id: run_id.into(),
            request_id: request_id.into(),
            prompt_type,
            title: title.into(),
            description: None,
            options: None,
            default_value: None,
            timeout_seconds: default_prompt_timeout(),
            timestamp: now_millis(),
        }
    }

    /// Create a confirm prompt
    pub fn confirm_prompt(
        run_id: impl Into<String>,
        request_id: impl Into<String>,
        title: impl Into<String>,
    ) -> Self {
        Self::user_prompt(run_id, request_id, UserPromptType::Confirm, title)
    }

    /// Create a text input prompt
    pub fn input_prompt(
        run_id: impl Into<String>,
        request_id: impl Into<String>,
        title: impl Into<String>,
        default_value: Option<String>,
    ) -> Self {
        Self::UserPrompt {
            run_id: run_id.into(),
            request_id: request_id.into(),
            prompt_type: UserPromptType::Input,
            title: title.into(),
            description: None,
            options: None,
            default_value,
            timeout_seconds: default_prompt_timeout(),
            timestamp: now_millis(),
        }
    }

    /// Create a select prompt
    pub fn select_prompt(
        run_id: impl Into<String>,
        request_id: impl Into<String>,
        title: impl Into<String>,
        options: Vec<PromptOption>,
    ) -> Self {
        Self::UserPrompt {
            run_id: run_id.into(),
            request_id: request_id.into(),
            prompt_type: UserPromptType::Select,
            title: title.into(),
            description: None,
            options: Some(options),
            default_value: None,
            timeout_seconds: default_prompt_timeout(),
            timestamp: now_millis(),
        }
    }

    /// Create a multi-select prompt
    pub fn multi_select_prompt(
        run_id: impl Into<String>,
        request_id: impl Into<String>,
        title: impl Into<String>,
        options: Vec<PromptOption>,
    ) -> Self {
        Self::UserPrompt {
            run_id: run_id.into(),
            request_id: request_id.into(),
            prompt_type: UserPromptType::MultiSelect,
            title: title.into(),
            description: None,
            options: Some(options),
            default_value: None,
            timeout_seconds: default_prompt_timeout(),
            timestamp: now_millis(),
        }
    }
}

// =============================================================================
// Event Bridge - AgentEvent → AgUiEvent 変換
// =============================================================================

use crate::agent::AgentEvent;
use std::collections::HashMap;

/// AgentEvent から AgUiEvent への変換ブリッジ
///
/// エージェントの内部イベントをAG-UIプロトコル準拠のイベントに変換する。
/// ツール呼び出しIDの追跡やメッセージ状態の管理も行う。
///
/// ## 使用例
/// ```ignore
/// let mut bridge = AgUiEventBridge::new("run-123");
/// for agent_event in agent_events {
///     let agui_events = bridge.convert(agent_event);
///     for event in agui_events {
///         hub.broadcast(StandMessage::AgUi { event });
///     }
/// }
/// ```
#[derive(Debug)]
pub struct AgUiEventBridge {
    /// 現在の run_id
    run_id: String,
    /// 現在のメッセージID
    message_id: String,
    /// ツール名 → tool_call_id のマッピング（進行中のツール呼び出し追跡）
    active_tool_calls: HashMap<String, String>,
    /// メッセージストリーム開始済みフラグ
    message_started: bool,
    /// テキストバッファ（累積レスポンス）
    text_buffer: String,
    /// tool_call_id カウンター
    tool_call_counter: u64,
}

impl AgUiEventBridge {
    /// 新しいブリッジを作成
    pub fn new(run_id: impl Into<String>) -> Self {
        let run_id = run_id.into();
        let message_id = format!("msg-{}", uuid::Uuid::new_v4());
        Self {
            run_id,
            message_id,
            active_tool_calls: HashMap::new(),
            message_started: false,
            text_buffer: String::new(),
            tool_call_counter: 0,
        }
    }

    /// run_id を取得
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// message_id を取得
    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    /// 累積テキストを取得
    pub fn text_buffer(&self) -> &str {
        &self.text_buffer
    }

    /// メッセージが開始されたかどうか
    pub fn is_message_started(&self) -> bool {
        self.message_started
    }

    /// RunStarted イベントを生成
    pub fn run_started(&self) -> AgUiEvent {
        AgUiEvent::run_started(&self.run_id)
    }

    /// AgentEvent を AgUiEvent のリストに変換
    ///
    /// 1つの AgentEvent が複数の AgUiEvent を生成する場合がある
    /// （例: 最初のTextChunkは TextMessageStart + TextMessageContent を生成）
    pub fn convert(&mut self, event: AgentEvent) -> Vec<AgUiEvent> {
        match event {
            AgentEvent::SessionInit { .. } => {
                // セッション初期化は AG-UI イベントを生成しない
                // （セッション管理は別レイヤーで処理）
                vec![]
            }

            AgentEvent::TextChunk(chunk) => {
                self.text_buffer.push_str(&chunk);
                let mut events = Vec::new();

                // 最初のチャンクでメッセージ開始イベントを発行
                if !self.message_started {
                    events.push(AgUiEvent::text_message_start(
                        &self.run_id,
                        &self.message_id,
                        MessageRole::Assistant,
                    ));
                    self.message_started = true;
                }

                // コンテンツイベント
                events.push(AgUiEvent::text_message_content(
                    &self.run_id,
                    &self.message_id,
                    &chunk,
                ));

                events
            }

            AgentEvent::ToolExecuting { name } => {
                // 新しいtool_call_idを生成
                self.tool_call_counter += 1;
                let tool_call_id = format!("tool-{}-{}", self.tool_call_counter, uuid::Uuid::new_v4());
                self.active_tool_calls.insert(name.clone(), tool_call_id.clone());

                vec![AgUiEvent::tool_call_start(
                    &self.run_id,
                    &tool_call_id,
                    &name,
                )]
            }

            AgentEvent::ToolResult { name, preview } => {
                // 対応するtool_call_idを取得（なければ生成）
                let tool_call_id = self
                    .active_tool_calls
                    .remove(&name)
                    .unwrap_or_else(|| format!("tool-orphan-{}", name));

                vec![AgUiEvent::ToolCallEnd {
                    run_id: self.run_id.clone(),
                    tool_call_id,
                    result: Some(serde_json::json!({ "preview": preview })),
                    error: None,
                    timestamp: now_millis(),
                }]
            }

            AgentEvent::Done { result: _, cost: _ } => {
                let mut events = Vec::new();

                // メッセージが開始されていれば終了イベントを発行
                if self.message_started {
                    events.push(AgUiEvent::text_message_end(
                        &self.run_id,
                        &self.message_id,
                    ));
                }

                // Run終了イベント
                events.push(AgUiEvent::run_finished(&self.run_id));

                events
            }

            AgentEvent::Error(error) => {
                let mut events = Vec::new();

                // メッセージが開始されていれば終了イベントを発行
                if self.message_started {
                    events.push(AgUiEvent::text_message_end(
                        &self.run_id,
                        &self.message_id,
                    ));
                }

                // エラーイベント
                events.push(AgUiEvent::run_error(&self.run_id, "AGENT_ERROR", &error));

                events
            }

            AgentEvent::UserInputRequest {
                request_id,
                request_type,
                prompt,
                options,
            } => {
                // UserInputRequest → UserPrompt 変換
                // prompt_typeをrequest_typeから決定
                let prompt_type = match request_type.as_deref() {
                    Some("confirmation") | Some("confirm") => UserPromptType::Confirm,
                    Some("select") | Some("choice") => UserPromptType::Select,
                    Some("multi_select") => UserPromptType::MultiSelect,
                    _ => UserPromptType::Input, // デフォルトは入力
                };

                // 選択肢を変換（value→id, label→label）
                let ui_options: Vec<PromptOption> = options
                    .into_iter()
                    .map(|o| PromptOption {
                        id: o.value,
                        label: o.label.unwrap_or_default(),
                        description: o.description,
                    })
                    .collect();

                vec![AgUiEvent::UserPrompt {
                    run_id: self.run_id.clone(),
                    request_id,
                    prompt_type,
                    title: prompt.unwrap_or_default(),
                    description: None,
                    options: if ui_options.is_empty() {
                        None
                    } else {
                        Some(ui_options)
                    },
                    default_value: None,
                    timeout_seconds: default_prompt_timeout(),
                    timestamp: now_millis(),
                }]
            }
        }
    }

    /// キャンセル時のイベントを生成
    pub fn cancelled(&mut self) -> Vec<AgUiEvent> {
        let mut events = Vec::new();

        // メッセージが開始されていれば終了イベントを発行
        if self.message_started {
            events.push(AgUiEvent::text_message_end(
                &self.run_id,
                &self.message_id,
            ));
            self.message_started = false;
        }

        // キャンセルエラーイベント
        events.push(AgUiEvent::run_error(
            &self.run_id,
            "CANCELLED",
            "Request cancelled by user",
        ));

        events
    }

    /// 新しいメッセージを開始（会話継続時）
    pub fn start_new_message(&mut self) {
        self.message_id = format!("msg-{}", uuid::Uuid::new_v4());
        self.message_started = false;
        self.text_buffer.clear();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// REQ-AGUI-041: Event type definitions
    #[test]
    fn test_run_started_serialization() {
        let event = AgUiEvent::run_started("run-123");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"RUN_STARTED""#));
        assert!(json.contains(r#""run_id":"run-123""#));
    }

    /// REQ-AGUI-041: Event type definitions
    #[test]
    fn test_text_message_events() {
        let start = AgUiEvent::text_message_start("run-1", "msg-1", MessageRole::Assistant);
        let content = AgUiEvent::text_message_content("run-1", "msg-1", "Hello");
        let end = AgUiEvent::text_message_end("run-1", "msg-1");

        assert!(matches!(start, AgUiEvent::TextMessageStart { .. }));
        assert!(matches!(content, AgUiEvent::TextMessageContent { delta, .. } if delta == "Hello"));
        assert!(matches!(end, AgUiEvent::TextMessageEnd { .. }));
    }

    /// REQ-AGUI-041: Event type definitions
    #[test]
    fn test_permission_request_serialization() {
        let event = AgUiEvent::permission_request(
            "run-1",
            "req-1",
            "file_write",
            serde_json::json!({"path": "/tmp/test.txt"}),
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"PERMISSION_REQUEST""#));
        assert!(json.contains(r#""tool_name":"file_write""#));
    }

    /// REQ-AGUI-041: Event type definitions
    #[test]
    fn test_run_id_extraction() {
        let event = AgUiEvent::run_started("my-run");
        assert_eq!(event.run_id(), "my-run");
    }

    /// REQ-AGUI-041: State delta with JSON Patch
    #[test]
    fn test_state_delta() {
        let event = AgUiEvent::StateDelta {
            run_id: "run-1".to_string(),
            delta: vec![JsonPatchOp::Add {
                path: "/foo".to_string(),
                value: serde_json::json!("bar"),
            }],
            timestamp: 0,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""op":"add""#));
        assert!(json.contains(r#""path":"/foo""#));
    }

    /// REQ-PROMPT-001: Confirm prompt
    #[test]
    fn test_confirm_prompt() {
        let event = AgUiEvent::confirm_prompt("run-1", "prompt-1", "Continue?");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"USER_PROMPT""#));
        assert!(json.contains(r#""prompt_type":"confirm""#));
        assert!(json.contains(r#""title":"Continue?""#));
    }

    /// REQ-PROMPT-001: Select prompt
    #[test]
    fn test_select_prompt() {
        let options = vec![
            PromptOption {
                id: "a".to_string(),
                label: "Option A".to_string(),
                description: None,
            },
            PromptOption {
                id: "b".to_string(),
                label: "Option B".to_string(),
                description: Some("Description B".to_string()),
            },
        ];
        let event = AgUiEvent::select_prompt("run-1", "prompt-2", "Choose:", options);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"USER_PROMPT""#));
        assert!(json.contains(r#""prompt_type":"select""#));
        assert!(json.contains(r#""id":"a""#));
        assert!(json.contains(r#""label":"Option B""#));
    }

    /// REQ-PROMPT-001: Input prompt
    #[test]
    fn test_input_prompt() {
        let event = AgUiEvent::input_prompt(
            "run-1",
            "prompt-3",
            "Enter API key:",
            Some("default".to_string()),
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"USER_PROMPT""#));
        assert!(json.contains(r#""prompt_type":"input""#));
        assert!(json.contains(r#""default_value":"default""#));
    }

    /// REQ-PROMPT-005: Response outcome
    #[test]
    fn test_user_prompt_response() {
        let event = AgUiEvent::UserPromptResponse {
            run_id: "run-1".to_string(),
            request_id: "prompt-1".to_string(),
            outcome: UserPromptOutcome::Approved,
            message: Some("OK".to_string()),
            selected_options: None,
            timestamp: 0,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"USER_PROMPT_RESPONSE""#));
        assert!(json.contains(r#""outcome":"approved""#));
    }

    /// REQ-PROMPT-001: Multi-select prompt
    #[test]
    fn test_multi_select_prompt() {
        let options = vec![
            PromptOption {
                id: "feature1".to_string(),
                label: "Feature 1".to_string(),
                description: None,
            },
            PromptOption {
                id: "feature2".to_string(),
                label: "Feature 2".to_string(),
                description: None,
            },
        ];
        let event =
            AgUiEvent::multi_select_prompt("run-1", "prompt-4", "Select features:", options);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""prompt_type":"multi_select""#));
    }

    /// REQ-PROMPT-004: Timeout setting
    #[test]
    fn test_prompt_timeout() {
        let event = AgUiEvent::confirm_prompt("run-1", "prompt-5", "Test");
        if let AgUiEvent::UserPrompt {
            timeout_seconds, ..
        } = event
        {
            assert_eq!(timeout_seconds, 300); // 5 minutes default
        } else {
            panic!("Expected UserPrompt event");
        }
    }
}
