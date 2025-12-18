//! ACP (Agent Client Protocol) Types
//!
//! JSON-RPC 2.0ベースのAgent-Editor通信プロトコル。
//! Zed Editor主導で策定されたオープン標準。
//!
//! ## 参照
//! - https://agentclientprotocol.com
//!
//! ## 要件
//! - REQ-PROTO-002: ACP準拠

use serde::{Deserialize, Serialize};
use serde_json::Value;

// =============================================================================
// JSON-RPC 2.0 Base Types
// =============================================================================

/// JSON-RPC リクエスト
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC レスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// JSON-RPC Notification（応答なし）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// リクエストID（数値 or 文字列）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// JSON-RPC エラー
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// =============================================================================
// ACP Message Wrapper
// =============================================================================

/// ACPメッセージ（Request/Response/Notification）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AcpMessage {
    Request(AcpRequest),
    Response(AcpResponse),
    Notification(AcpNotification),
}

impl AcpMessage {
    /// JSON-RPC 2.0 バージョン文字列
    pub const VERSION: &'static str = "2.0";

    /// リクエストを作成
    pub fn request(id: impl Into<RequestId>, method: &str, params: Option<Value>) -> Self {
        Self::Request(AcpRequest {
            jsonrpc: Self::VERSION.to_string(),
            id: id.into(),
            method: method.to_string(),
            params,
        })
    }

    /// レスポンスを作成
    pub fn response(id: RequestId, result: Value) -> Self {
        Self::Response(AcpResponse {
            jsonrpc: Self::VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        })
    }

    /// エラーレスポンスを作成
    pub fn error_response(id: RequestId, code: i32, message: &str) -> Self {
        Self::Response(AcpResponse {
            jsonrpc: Self::VERSION.to_string(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        })
    }

    /// Notificationを作成
    pub fn notification(method: &str, params: Option<Value>) -> Self {
        Self::Notification(AcpNotification {
            jsonrpc: Self::VERSION.to_string(),
            method: method.to_string(),
            params,
        })
    }
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        Self::Number(n)
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

// =============================================================================
// ACP Methods
// =============================================================================

/// ACPメソッド名
pub mod methods {
    // Agent → Client (Agent exposes)
    pub const INITIALIZE: &str = "initialize";
    pub const AUTHENTICATE: &str = "authenticate";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_LOAD: &str = "session/load";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";

    // Client → Agent (Client exposes)
    pub const FILE_READ: &str = "file/read";
    pub const FILE_WRITE: &str = "file/write";
    pub const FILE_LIST: &str = "file/list";
    pub const TERMINAL_CREATE: &str = "terminal/create";
    pub const TERMINAL_EXECUTE: &str = "terminal/execute";
}

/// ACP Notification名
pub mod notifications {
    pub const SESSION_UPDATE: &str = "session/update";
    pub const TOOL_CALL_UPDATE: &str = "tool_call/update";
}

// =============================================================================
// Initialize
// =============================================================================

/// initialize リクエストパラメータ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// プロトコルバージョン
    pub protocol_version: String,
    /// クライアント情報
    pub client_info: ClientInfo,
    /// クライアントCapabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ClientCapabilities>,
}

/// initialize レスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// プロトコルバージョン
    pub protocol_version: String,
    /// エージェント情報
    pub agent_info: AgentInfo,
    /// エージェントCapabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AgentCapabilities>,
}

/// クライアント情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// エージェント情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// クライアントCapabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedded_context: Option<bool>,
}

/// エージェントCapabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_load: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode_switch: Option<bool>,
}

// =============================================================================
// Session Types
// =============================================================================

/// session/new リクエストパラメータ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewParams {
    /// ワーキングディレクトリ
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// 初期モード
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

/// session/new レスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNewResult {
    pub session_id: String,
}

/// session/prompt リクエストパラメータ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptParams {
    pub session_id: String,
    pub content: Vec<ContentBlock>,
}

/// session/prompt レスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptResult {
    pub stop_reason: StopReason,
}

// =============================================================================
// Content Types (MCP compatible)
// =============================================================================

/// コンテンツブロック
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Audio {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "embedded_resource")]
    EmbeddedResource {
        resource: ResourceContent,
    },
    #[serde(rename = "resource_link")]
    ResourceLink {
        uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

/// リソースコンテンツ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

// =============================================================================
// Tool Call Types
// =============================================================================

/// ツールコール状態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl Default for ToolCallStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// ツールコール種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

impl Default for ToolCallKind {
    fn default() -> Self {
        Self::Other
    }
}

/// ツールコール情報
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: String,
    pub title: String,
    #[serde(default)]
    pub kind: ToolCallKind,
    #[serde(default)]
    pub status: ToolCallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<Location>>,
}

/// ファイル位置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

// =============================================================================
// Permission Types
// =============================================================================

/// Permission要求
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_call_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub options: Vec<PermissionOption>,
}

/// Permissionオプション
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionKind,
}

/// Permission種別（ACP標準 + Vantage拡張）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    // ACP標準
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,

    // Vantage拡張 (REQ-PROTO-003)
    #[serde(rename = "vantage:allow_with_edit")]
    AllowWithEdit,
    #[serde(rename = "vantage:delegate")]
    Delegate,
    #[serde(rename = "vantage:require_confirm")]
    RequireConfirm,
}

/// Permissionレスポンス
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionResponse {
    pub request_id: String,
    pub outcome: PermissionOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_option_id: Option<String>,
}

/// Permissionの結果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOutcome {
    Selected,
    Cancelled,
}

// =============================================================================
// Session Update Types
// =============================================================================

/// session/update Notification パラメータ
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub session_id: String,
    #[serde(flatten)]
    pub update: SessionUpdateKind,
}

/// 更新の種類
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdateKind {
    /// メッセージチャンク
    MessageChunk {
        message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        content: Vec<ContentBlock>,
    },
    /// ツールコール開始
    ToolCall(ToolCall),
    /// ツールコール更新
    ToolCallUpdate {
        tool_call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<ToolCallStatus>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ContentBlock>>,
    },
    /// 実行計画
    Plan {
        plan_id: String,
        steps: Vec<PlanStep>,
    },
}

/// 実行計画のステップ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

// =============================================================================
// Stop Reason
// =============================================================================

/// 終了理由（ACP標準）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// モデルが正常終了
    EndTurn,
    /// トークン上限到達
    MaxTokens,
    /// ターンリクエスト上限到達
    MaxTurnRequests,
    /// 拒否
    Refusal,
    /// ユーザーキャンセル
    Cancelled,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = AcpMessage::request(
            1,
            methods::INITIALIZE,
            Some(serde_json::json!({
                "protocolVersion": "1.0",
                "clientInfo": { "name": "VantagePoint", "version": "0.3.0" }
            })),
        );

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn test_notification_serialization() {
        let notif = AcpMessage::notification(
            notifications::SESSION_UPDATE,
            Some(serde_json::json!({
                "sessionId": "sess-123",
                "sessionUpdate": "message_chunk",
                "messageId": "msg-1",
                "content": [{"type": "text", "text": "Hello"}]
            })),
        );

        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"session/update\""));
    }

    #[test]
    fn test_tool_call_status() {
        let status = ToolCallStatus::InProgress;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"in_progress\"");
    }

    #[test]
    fn test_permission_kind_with_vantage_extension() {
        // ACP標準
        let allow = PermissionKind::AllowOnce;
        assert_eq!(serde_json::to_string(&allow).unwrap(), "\"allow_once\"");

        // Vantage拡張
        let delegate = PermissionKind::Delegate;
        assert_eq!(
            serde_json::to_string(&delegate).unwrap(),
            "\"vantage:delegate\""
        );
    }

    #[test]
    fn test_stop_reason() {
        let reason = StopReason::EndTurn;
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(json, "\"end_turn\"");
    }

    #[test]
    fn test_content_block() {
        let text = ContentBlock::Text {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&text).unwrap();
        assert!(json.contains("\"type\":\"text\""));

        let link = ContentBlock::ResourceLink {
            uri: "file:///path/to/file.rs".to_string(),
            name: Some("file.rs".to_string()),
        };
        let json = serde_json::to_string(&link).unwrap();
        assert!(json.contains("\"type\":\"resource_link\""));
    }
}
