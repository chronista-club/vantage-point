//! Protocol definitions for communication between components

use serde::{Deserialize, Serialize};

use crate::agui::AgUiEvent;
use crate::repo::lanes_state::LaneInfo;

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

/// board（board Canvas の scope 別永続リスト）の 1 item（board モデル 2026-07-15）。
///
/// repo が id を一元発行し（webview は自前生成しない）、 [`RepoMessage::BoardUpdated`] の
/// snapshot で配信される。 JSON は webview の BoardItem と揃える（camelCase:
/// id / content / contentType / title / createdAt / updatedAt）ので DB stack にもそのまま載る。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardItem {
    pub id: String,
    pub content: String,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: String,
    /// 最終更新時刻（RFC3339。show=createdAt 同値 / update で stamp、doc 52 §5 計器盤の鮮度）。
    /// 旧 item（wave 3 以前に貼られた）は欠くので Option — 表示側は createdAt に fallback する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Message from Process to browser (WebSocket)
///
/// Process: AI Agent server that wields capabilities on behalf of the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RepoMessage {
    /// doc 48 Phase 2: Editor Mode bridge — MCP から GUI (vp-app webview) への editor 操作。
    ///
    /// daemon の `editor_fields` / `editor_values` / `editor_set` handler が request_id を
    /// 発行して broadcast し、GUI が webview で評価した結果を `editor_result` request で
    /// 返す (request-response)。topic は category=event (非 retained) — 再購読時に stale な
    /// command が replay されてはならない。
    EditorCommand {
        /// 応答相関 id。GUI は `editor_result` にこの id を載せて返す。
        request_id: String,
        /// 操作: editor 系 "fields" | "values" | "set"、layout 系（doc 49 LE-P2）
        /// "layout_get" | "layout_set" | "layout_history"。GUI 評価 RPC の共用配管
        op: String,
        /// op="set" の対象 field id
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field_id: Option<String>,
        /// op="set" の新しい値
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
    /// Show content in a pane
    Show {
        pane_id: String,
        content: Content,
        append: bool,
        /// ペインのタイトル（タブ表示用）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// このメッセージが属する Lane（per-lane board scope、root/performer 語彙）。
        /// `None` = conductor（lead）。topic の lane segment になり、retained を lane 別に分離する。
        /// wire 後方互換のため `skip_serializing_if`（旧 consumer は field 欠落を conductor 扱い）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
        /// board scope: `"lane"` のみ（'proj' は 2026-07-23 撤去。wire 上は文字列のまま = 旧値との互換）。
        /// show した item をどの board に貼るか。 `None` = lane で後方互換。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    /// Clear a pane
    Clear {
        pane_id: String,
        /// 属する Lane（`None` = conductor）。[`RepoMessage::Show`] の lane と同義。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
        /// clear 対象の board scope（`None` = lane）。[`RepoMessage::Show`] の scope と同義。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    /// board（board Canvas の scope 別永続リスト）の snapshot（board モデル 2026-07-15）。
    ///
    /// repo が唯一の truth を持ち、 item 追加/削除/clear のたびに更新後 snapshot を broadcast する。
    /// topic `process/board/state/board/{scope}/{lane}`（category=state で retained）に載り、
    /// 再接続 / board 切替時の初期配信を retained が兼ねる。 webview はこれを受けて board を置換する view。
    BoardUpdated {
        /// board scope: `"lane"` のみ（'proj' は 2026-07-23 撤去）。
        scope: String,
        /// lane board のときの lane（root/performer）。 proj board は `None`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
        /// items（新→古）。
        items: Vec<BoardItem>,
        /// cursor（現在 main に出す item の id）。 view の初期 cursor に使う。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
    },
    /// Split a pane
    Split {
        pane_id: String,
        direction: SplitDirection,
        new_pane_id: String,
        /// 属する Lane（`None` = conductor）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
    },
    /// Close a pane
    Close {
        pane_id: String,
        /// 属する Lane（`None` = conductor）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
    },
    /// Toggle side panel visibility
    TogglePane {
        pane_id: String,
        /// Optional explicit state: true = show, false = hide, None = toggle
        #[serde(default)]
        visible: Option<bool>,
        /// 属する Lane（`None` = conductor）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lane: Option<String>,
    },
    /// Ping for keepalive
    Ping,
    /// Chat message to display
    ChatMessage { message: ChatMessage },
    /// Chat streaming chunk (for real-time display)
    ChatChunk { content: String, done: bool },
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
    /// Lane PTY 出力（base64、 per-lane）。 doc 27 §4.1 S1: repo Lane PtySlot の出力を
    /// `process/terminal/{lane}/data/out` topic に乗せ、 daemon 経由で WebView に届ける
    /// (raw WebSocket `/ws/terminal` 退役の置換)。 session 系 `TerminalOutput` とは別系統
    /// (こちらは LanePool スコープ、 lane address を持つ)。
    LaneTerminalOutput {
        lane: String,
        /// doc 50 §4.6 A6: 発生元 session の VP 採番 key（1 Lane = N term session）。
        /// `EchoesEvent.session` と対称の additive field — topic key は lane のまま、session は
        /// 本 field で運び、World A の xterm が session 別に振り分ける（doc 38 落とし穴① =
        /// 「session を lane 名に埋めない」を Act I 側でも踏襲）。旧 sender 由来は default の 1。
        #[serde(default = "default_session_key")]
        session: u32,
        data: String,
    },
    /// Echoes Act II（構造化会話 GUI）の翻訳済みイベント（per-lane）。doc 32。
    /// `EchoesAgentHost` が headless claude の stream-json を [`crate::echoes::EchoesEvent`]
    /// へ翻訳し、`process/echoes/data/{lane}/event` topic に乗せて vp-app へ届ける。
    /// LaneTerminalOutput（Act I の生 PTY）とは別系統の per-lane ephemeral stream。
    EchoesEvent {
        lane: String,
        /// doc 38: 発生元 session の VP 採番 key（1 Lane = N session）。additive field —
        /// 旧 sender 由来の message は default の 1（= N=1 特殊ケースの唯一 session）に解決。
        /// ⚠️ session を lane 名に埋めない（doc 38 落とし穴① — topic key は lane のまま、
        /// session は本 field で運ぶ）。
        #[serde(default = "default_session_key")]
        session: u32,
        event: crate::echoes::EchoesEvent,
    },
    /// Canvas Lane 切り替え指示
    SwitchLane {
        /// active 化する lane token: "root"（lead）or performer 名（例: "feat-api"）。
        /// 現 repo 内の lane-within-repo 切替（B1 で repo 切替意味論から変更）。
        lane: String,
    },
    /// wiremsg: repo の Lane 一覧 snapshot（retained state topic）。
    ///
    /// LanePool 変化のたび全 list を publish し、`process/star-platinum/state/lanes`
    /// に retain される（category=state → RetainedStore が最新値を保持）。
    /// subscriber は subscribe 即値 + 変化で push を受ける。
    /// 設計: creo-memories `mem_1CbA198fsHJsoKpu2jDUCv`（wiremsg restructure）。
    LanesSnapshot {
        lanes: Vec<LaneInfo>,
        /// doc 44 D4: この repo の**開発起点 lane 名**（Host の帳簿が解決した値）。
        ///
        /// lane の属性ではなく **repo 側の指定**なので、`LaneInfo` には持たせず
        /// snapshot に 1 本添える（descriptor に入れると `lane.descriptor` へ永続され、
        /// 帳簿と二重の真実源になる）。
        ///
        /// `None` は「まだ判らない」= 受け手は前回値を保つ（既定値に落とさない）。
        /// 解決できた publisher は必ず `Some` を入れる（未指定なら予約名が入る）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
    },
}

/// [`RepoMessage::EchoesEvent::session`] の serde default（doc 38 の N=1 特殊ケース =
/// 唯一 session の key 1。session field を持たない旧 sender との後方互換）。
fn default_session_key() -> u32 {
    1
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
    pub payload: RepoMessage,
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
    fn test_process_message_serialization() {
        let msg = RepoMessage::ChatChunk {
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
        let msg = RepoMessage::Show {
            pane_id: "design".to_string(),
            content: Content::Markdown("# Hello".to_string()),
            append: false,
            title: Some("設計書".to_string()),
            lane: None,
            scope: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"show""#));
        assert!(json.contains(r#""title":"設計書""#));
        assert!(json.contains(r#""pane_id":"design""#));
    }

    #[test]
    fn test_show_without_title_omits_field() {
        let msg = RepoMessage::Show {
            pane_id: "main".to_string(),
            content: Content::Markdown("# Hello".to_string()),
            append: false,
            title: None,
            lane: None,
            scope: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("title"));
    }

    #[test]
    fn test_split_message_serialization() {
        let msg = RepoMessage::Split {
            pane_id: "main".to_string(),
            direction: SplitDirection::Horizontal,
            new_pane_id: "pane-1".to_string(),
            lane: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"split""#));
        assert!(json.contains(r#""direction":"horizontal""#));
        assert!(json.contains(r#""new_pane_id":"pane-1""#));
    }

    #[test]
    fn test_close_message_serialization() {
        let msg = RepoMessage::Close {
            pane_id: "pane-1".to_string(),
            lane: None,
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
