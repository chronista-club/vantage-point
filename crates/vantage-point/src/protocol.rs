//! Protocol definitions for communication between components

use serde::{Deserialize, Serialize};

use crate::agui::AgUiEvent;

/// Debug display mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugMode {
    /// No debug information
    #[default]
    None,
    /// Simple debug info (session ID, timing)
    Simple,
    /// Detailed debug info (full JSON, all events)
    Detail,
}

/// Content types that can be displayed in the viewer
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// Plain text log
    Log(String),
    /// Markdown content
    Markdown(String),
    /// Base64-encoded image
    ImageBase64 { data: String, mime_type: String },
    /// Raw HTML
    Html(String),
}

/// Stored chat message for history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    pub timestamp: u64,
}

/// Message from Stand to browser (WebSocket)
///
/// Stand: AI Agent server that wields capabilities on behalf of the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StandMessage {
    /// Show content in a pane
    Show {
        pane_id: String,
        content: Content,
        append: bool,
    },
    /// Clear a pane
    Clear { pane_id: String },
    /// Split a pane
    Split {
        pane_id: String,
        direction: SplitDirection,
        new_pane_id: String,
    },
    /// Close a pane
    Close { pane_id: String },
    /// Ping for keepalive
    Ping,
    /// Chat message to display
    ChatMessage { message: ChatMessage },
    /// Chat streaming chunk (for real-time display)
    ChatChunk { content: String, done: bool },
    /// Debug information
    DebugInfo {
        level: DebugMode,
        category: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },
    /// Notify debug mode change
    DebugModeChanged { mode: DebugMode },
    /// Session list response
    SessionList {
        sessions: Vec<SessionInfo>,
        active_id: Option<String>,
    },
    /// Session switched notification
    SessionSwitched { session_id: String, name: String },
    /// Session created notification
    SessionCreated { session: SessionInfo },
    /// Session closed notification
    SessionClosed { session_id: String },
    /// Session history (for restoring chat on session switch)
    SessionHistory {
        session_id: String,
        messages: Vec<HistoryMessage>,
    },
    /// Interactive component (AG-UI style)
    ChatComponent {
        component: ChatComponent,
        /// If true, this component requires user interaction
        #[serde(default)]
        interactive: bool,
    },
    /// Component dismissed/resolved
    ComponentDismissed { request_id: String },
    /// AG-UI protocol event (REQ-AGUI-040)
    AgUi { event: AgUiEvent },
}

/// Session information for UI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Claude CLI session ID
    pub id: String,
    /// Display name (user-defined or auto-generated)
    pub name: String,
    /// Whether this is the active session
    pub is_active: bool,
    /// Number of messages in session (approximate)
    pub message_count: usize,
    /// Model used in this session
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Session creation timestamp (Unix millis)
    #[serde(default)]
    pub created_at: u64,
}

/// Split direction
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// Message from browser to Stand (WebSocket)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserMessage {
    /// Browser is ready
    Ready,
    /// Pong response
    Pong,
    /// User action (future)
    Action { pane_id: String, action: String },
    /// Chat message from user
    Chat { message: String },
    /// Cancel current chat request
    CancelChat,
    /// Reset session (start new conversation)
    ResetSession,
    /// List all sessions
    ListSessions,
    /// Switch to a different session
    SwitchSession { session_id: String },
    /// Create a new session
    NewSession,
    /// Rename a session
    RenameSession { session_id: String, name: String },
    /// Close/delete a session
    CloseSession { session_id: String },
    /// Response to an interactive component
    ComponentAction { action: ComponentAction },
}

/// Chat message for display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

/// Chat message role
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

/// Internal message for IPC (Unix Socket or internal channel)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMessage {
    pub id: Option<String>,
    pub payload: StandMessage,
}

// =============================================================================
// Chat Components (AG-UI inspired Generative UI)
// =============================================================================

/// Interactive chat component types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "component", rename_all = "snake_case")]
pub enum ChatComponent {
    /// Permission request dialog (for --permission-prompt-tool)
    PermissionRequest {
        request_id: String,
        tool_name: String,
        #[serde(default)]
        description: Option<String>,
        /// Tool input parameters (JSON)
        input: serde_json::Value,
        /// Timeout in seconds (default: 30)
        #[serde(default = "default_timeout")]
        timeout_seconds: u32,
    },
    /// Todo list display
    TodoList {
        items: Vec<TodoItem>,
        #[serde(default)]
        title: Option<String>,
    },
    /// Progress indicator
    Progress {
        label: String,
        #[serde(default)]
        current: Option<u32>,
        #[serde(default)]
        total: Option<u32>,
        status: ProgressStatus,
    },
    /// Choice buttons for user selection
    ChoiceButtons {
        request_id: String,
        prompt: String,
        choices: Vec<Choice>,
        #[serde(default)]
        allow_multiple: bool,
    },
    /// Code diff preview
    CodeDiff {
        request_id: String,
        file_path: String,
        before: String,
        after: String,
        #[serde(default)]
        language: Option<String>,
    },
}

fn default_timeout() -> u32 {
    30
}

/// Todo item for TodoList component
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub active_form: Option<String>,
}

/// Todo item status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Progress status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Running,
    Completed,
    Error,
}

/// Choice option for ChoiceButtons
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Response to a component interaction
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ComponentAction {
    /// Permission approved
    PermissionApprove {
        request_id: String,
        #[serde(default)]
        updated_input: Option<serde_json::Value>,
    },
    /// Permission denied
    PermissionDeny {
        request_id: String,
        #[serde(default)]
        message: Option<String>,
    },
    /// Choice selected
    ChoiceSelect {
        request_id: String,
        selected_ids: Vec<String>,
    },
    /// Code diff approved
    DiffApprove { request_id: String },
    /// Code diff rejected
    DiffReject {
        request_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_mode_default() {
        assert_eq!(DebugMode::default(), DebugMode::None);
    }

    #[test]
    fn test_debug_mode_from_str() {
        let simple: DebugMode = serde_json::from_str(r#""simple""#).unwrap();
        assert_eq!(simple, DebugMode::Simple);

        let detail: DebugMode = serde_json::from_str(r#""detail""#).unwrap();
        assert_eq!(detail, DebugMode::Detail);
    }

    #[test]
    fn test_stand_message_serialization() {
        let msg = StandMessage::ChatChunk {
            content: "Hello".to_string(),
            done: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"chat_chunk""#));
        assert!(json.contains(r#""content":"Hello""#));
    }

    #[test]
    fn test_browser_message_deserialization() {
        let json = r#"{"type":"chat","message":"Hello, Claude!"}"#;
        let msg: BrowserMessage = serde_json::from_str(json).unwrap();
        match msg {
            BrowserMessage::Chat { message } => {
                assert_eq!(message, "Hello, Claude!");
            }
            _ => panic!("Expected Chat message"),
        }
    }

    #[test]
    fn test_session_info() {
        let session = SessionInfo {
            id: "abc123".to_string(),
            name: "Test Session".to_string(),
            is_active: true,
            message_count: 5,
            model: Some("claude-opus-4-5-20251101".to_string()),
            created_at: 0,
        };
        let json = serde_json::to_string(&session).unwrap();
        let parsed: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "abc123");
        assert_eq!(parsed.name, "Test Session");
        assert!(parsed.is_active);
    }
}
