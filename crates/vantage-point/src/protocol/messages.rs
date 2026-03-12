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
    /// 外部URLをiframeで表示
    Url(String),
}

impl Content {
    /// 既存コンテンツに新しいコンテンツを追記
    pub fn append_with(&self, other: &Content) -> Content {
        match (self, other) {
            (Content::Log(a), Content::Log(b)) => Content::Log(format!("{}{}", a, b)),
            (Content::Html(a), Content::Html(b)) => Content::Html(format!("{}{}", a, b)),
            (Content::Markdown(a), Content::Markdown(b)) => {
                Content::Markdown(format!("{}{}", a, b))
            }
            // 型が異なる場合は新しいコンテンツで上書き
            (_, other) => other.clone(),
        }
    }
}

/// Stored chat message for history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    pub timestamp: u64,
}

/// Message from Process to browser (WebSocket)
///
/// Process: AI Agent server that wields capabilities on behalf of the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessMessage {
    /// Show content in a pane
    Show {
        pane_id: String,
        content: Content,
        append: bool,
        /// ペインのタイトル（タブ表示用）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
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
    /// Toggle side panel visibility
    TogglePane {
        pane_id: String,
        /// Optional explicit state: true = show, false = hide, None = toggle
        #[serde(default)]
        visible: Option<bool>,
    },
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
        /// 複数タグによるフィルタリング用（例: ["pty", "permission", "broadcast"]）
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
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
    /// ターミナルPTY出力（base64エンコード）
    TerminalOutput { data: String },
    /// ターミナルPTYセッション開始通知
    TerminalReady,
    /// ターミナルPTYセッション終了通知（子プロセス EOF）
    TerminalExited,
    /// トレースログエントリ（debug.log ファイルからの配信）
    TraceLog {
        ts: String,
        process: String,
        trace_id: String,
        step: String,
        level: String,
        msg: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
    },
    /// Canvas スクリーンショット要求
    ScreenshotRequest {
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
    },
    /// Canvas Lane 切り替え指示
    SwitchLane {
        /// 切り替え先の Lane 名（プロジェクト名）
        lane: String,
    },
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

/// Message from browser to Process (WebSocket)
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
    /// ターミナル入力（base64エンコード）
    TerminalInput { data: String },
    /// ターミナルリサイズ
    TerminalResize { cols: u16, rows: u16 },
    /// Canvas スクリーンショット応答（base64 PNG）
    ScreenshotResponse {
        request_id: String,
        data: String,
        width: u32,
        height: u32,
    },
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
    pub payload: ProcessMessage,
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
    /// Tool execution status indicator
    ToolExecution {
        tool_name: String,
        status: String, // "running", "completed", "failed"
        #[serde(default)]
        result: Option<String>,
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
    /// User prompt response (REQ-PROMPT-005)
    UserPromptSubmit {
        request_id: String,
        /// Response outcome: "approved", "rejected", "cancelled"
        outcome: String,
        /// Text response (for input type or optional comment)
        #[serde(default)]
        message: Option<String>,
        /// Selected option IDs (for select/multi_select)
        #[serde(default)]
        selected_options: Option<Vec<String>>,
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
    fn test_process_message_serialization() {
        let msg = ProcessMessage::ChatChunk {
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
    fn test_show_with_title_serialization() {
        let msg = ProcessMessage::Show {
            pane_id: "design".to_string(),
            content: Content::Markdown("# Hello".to_string()),
            append: false,
            title: Some("設計書".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"show""#));
        assert!(json.contains(r#""title":"設計書""#));
        assert!(json.contains(r#""pane_id":"design""#));
    }

    #[test]
    fn test_show_without_title_omits_field() {
        let msg = ProcessMessage::Show {
            pane_id: "main".to_string(),
            content: Content::Markdown("# Hello".to_string()),
            append: false,
            title: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("title"));
    }

    #[test]
    fn test_split_message_serialization() {
        let msg = ProcessMessage::Split {
            pane_id: "main".to_string(),
            direction: SplitDirection::Horizontal,
            new_pane_id: "pane-1".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"split""#));
        assert!(json.contains(r#""direction":"horizontal""#));
        assert!(json.contains(r#""new_pane_id":"pane-1""#));
    }

    #[test]
    fn test_close_message_serialization() {
        let msg = ProcessMessage::Close {
            pane_id: "pane-1".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"close""#));
        assert!(json.contains(r#""pane_id":"pane-1""#));
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
