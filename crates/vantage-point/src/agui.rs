//! AG-UI Protocol Implementation
//!
//! Agent-User Interaction Protocol for connecting AI agents to user-facing applications.
//! Based on: https://docs.ag-ui.com
//!
//! ## Requirements Coverage
//! - REQ-AGUI-041: Event type definitions (this module)
//! - REQ-AGUI-040: WebSocket transport (integration with daemon/server.rs)

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
    Add { path: String, value: serde_json::Value },
    Remove { path: String },
    Replace { path: String, value: serde_json::Value },
    Move { from: String, path: String },
    Copy { from: String, path: String },
    Test { path: String, value: serde_json::Value },
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
// Helper Functions
// =============================================================================

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn default_timeout() -> u32 {
    30
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
    pub fn run_error(run_id: impl Into<String>, code: impl Into<String>, message: impl Into<String>) -> Self {
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
    pub fn text_message_start(run_id: impl Into<String>, message_id: impl Into<String>, role: MessageRole) -> Self {
        Self::TextMessageStart {
            run_id: run_id.into(),
            message_id: message_id.into(),
            role,
            timestamp: now_millis(),
        }
    }

    /// Create a TextMessageContent event
    pub fn text_message_content(run_id: impl Into<String>, message_id: impl Into<String>, delta: impl Into<String>) -> Self {
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
    pub fn tool_call_start(run_id: impl Into<String>, tool_call_id: impl Into<String>, tool_name: impl Into<String>) -> Self {
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
            | Self::StateSnapshot { run_id, .. }
            | Self::StateDelta { run_id, .. }
            | Self::ActivitySnapshot { run_id, .. }
            | Self::ActivityDelta { run_id, .. }
            | Self::Raw { run_id, .. }
            | Self::Custom { run_id, .. } => run_id,
        }
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
            delta: vec![
                JsonPatchOp::Add {
                    path: "/foo".to_string(),
                    value: serde_json::json!("bar"),
                },
            ],
            timestamp: 0,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""op":"add""#));
        assert!(json.contains(r#""path":"/foo""#));
    }
}
