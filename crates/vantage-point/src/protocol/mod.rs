//! Unified Protocol Module
//!
//! AG-UI + ACP準拠 + Vantage独自拡張の統一プロトコル。
//!
//! ## プロトコル構成
//!
//! - **AG-UI**: UI更新に特化したイベントベースプロトコル
//! - **ACP**: JSON-RPC 2.0ベースのEditor統合プロトコル
//! - **Vantage**: MIDI、Capability連携などの独自拡張
//!
//! ## 要件
//! - REQ-PROTO-001: AG-UI準拠
//! - REQ-PROTO-002: ACP準拠
//! - REQ-PROTO-003: Vantage拡張
//! - REQ-PROTO-004: EventBus連携

pub mod acp;
pub mod messages;
pub mod vantage;

// Re-export main types
pub use acp::{
    AcpMessage, AcpNotification, AcpRequest, AcpResponse, AgentCapabilities, AgentInfo,
    ClientCapabilities, ClientInfo, ContentBlock, InitializeParams, InitializeResult, Location,
    PermissionKind, PermissionOption, PermissionOutcome, PermissionRequest as AcpPermissionRequest,
    PermissionResponse as AcpPermissionResponse, PlanStep, RequestId, ResourceContent, RpcError,
    SessionNewParams, SessionNewResult, SessionPromptParams, SessionPromptResult, SessionUpdate,
    SessionUpdateKind, StopReason, ToolCall, ToolCallKind, ToolCallStatus,
};

pub use messages::{
    BrowserMessage, ChatComponent, ChatMessage, ChatRole, Choice, ComponentAction, Content,
    DebugMode, HistoryMessage, IpcMessage, ProgressStatus, SessionInfo, SplitDirection,
    StandMessage, TodoItem, TodoStatus,
};

pub use vantage::{
    CapabilityStateInfo, MidiControlChange, MidiEventType, MidiNote, SynergyTypeInfo, VantageEvent,
};

use crate::agui::AgUiEvent;
use serde::{Deserialize, Serialize};

// =============================================================================
// Unified Protocol Message
// =============================================================================

/// 統一プロトコルメッセージ
///
/// AG-UI、ACP、Vantage拡張を統一的に扱う。
/// EventBusを通じて配信され、各トランスポート（WebSocket、stdio）で送信。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum ProtocolMessage {
    /// AG-UI形式のイベント（UI更新用）
    AgUi {
        #[serde(flatten)]
        event: AgUiEvent,
    },

    /// ACP形式のJSON-RPCメッセージ（Editor統合用）
    Acp {
        #[serde(flatten)]
        message: AcpMessage,
    },

    /// Vantage Point独自拡張（MIDI、Capability等）
    Vantage {
        #[serde(flatten)]
        event: VantageEvent,
    },

    /// Stand内部メッセージ（WebSocket用、既存互換）
    Stand {
        #[serde(flatten)]
        message: StandMessage,
    },
}

impl ProtocolMessage {
    /// AG-UIイベントを作成
    pub fn agui(event: AgUiEvent) -> Self {
        Self::AgUi { event }
    }

    /// ACPメッセージを作成
    pub fn acp(message: AcpMessage) -> Self {
        Self::Acp { message }
    }

    /// Vantageイベントを作成
    pub fn vantage(event: VantageEvent) -> Self {
        Self::Vantage { event }
    }

    /// Standメッセージを作成
    pub fn stand(message: StandMessage) -> Self {
        Self::Stand { message }
    }
}

// =============================================================================
// Protocol Conversion Traits
// =============================================================================

/// AG-UIイベントへの変換
pub trait ToAgUi {
    fn to_agui(&self, run_id: &str) -> Option<AgUiEvent>;
}

/// ACPメッセージへの変換
pub trait ToAcp {
    fn to_acp(&self, session_id: &str) -> Option<AcpMessage>;
}

/// CapabilityEventからの変換
impl ToAgUi for crate::capability::CapabilityEvent {
    fn to_agui(&self, run_id: &str) -> Option<AgUiEvent> {
        // イベントタイプに基づいてAG-UIイベントに変換
        match self.event_type.as_str() {
            // ツール実行イベント
            t if t.starts_with("tool.") => {
                let tool_name = t.strip_prefix("tool.").unwrap_or(t);
                Some(AgUiEvent::tool_call_start(
                    run_id,
                    &format!("tool-{}", uuid::Uuid::new_v4()),
                    tool_name,
                ))
            }
            // テキストメッセージ
            "message.text" => self
                .payload
                .get("content")
                .and_then(|c| c.as_str())
                .map(|content| {
                    AgUiEvent::text_message_content(
                        run_id,
                        &format!("msg-{}", uuid::Uuid::new_v4()),
                        content,
                    )
                }),
            // その他は変換しない
            _ => None,
        }
    }
}

impl ToAcp for crate::capability::CapabilityEvent {
    fn to_acp(&self, session_id: &str) -> Option<AcpMessage> {
        // イベントタイプに基づいてACPメッセージに変換
        match self.event_type.as_str() {
            // ツール実行イベント → tool_call notification
            t if t.starts_with("tool.") => {
                let tool_name = t.strip_prefix("tool.").unwrap_or(t);
                Some(AcpMessage::notification(
                    acp::notifications::SESSION_UPDATE,
                    Some(serde_json::json!({
                        "sessionId": session_id,
                        "sessionUpdate": "tool_call",
                        "toolCallId": format!("tool-{}", uuid::Uuid::new_v4()),
                        "title": tool_name,
                        "kind": "other",
                        "status": "in_progress"
                    })),
                ))
            }
            // メッセージチャンク
            "message.chunk" => {
                self.payload
                    .get("content")
                    .and_then(|c| c.as_str())
                    .map(|content| {
                        AcpMessage::notification(
                            acp::notifications::SESSION_UPDATE,
                            Some(serde_json::json!({
                                "sessionId": session_id,
                                "sessionUpdate": "message_chunk",
                                "messageId": format!("msg-{}", uuid::Uuid::new_v4()),
                                "content": [{"type": "text", "text": content}]
                            })),
                        )
                    })
            }
            _ => None,
        }
    }
}

// =============================================================================
// Protocol Version
// =============================================================================

/// プロトコルバージョン情報
pub struct ProtocolVersion;

impl ProtocolVersion {
    /// AG-UIプロトコルバージョン
    pub const AGUI: &'static str = "1.0";

    /// ACPプロトコルバージョン
    pub const ACP: &'static str = "1.0";

    /// Vantage拡張バージョン
    pub const VANTAGE: &'static str = "0.3.0";
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_message_agui() {
        let event = AgUiEvent::run_started("run-123");
        let msg = ProtocolMessage::agui(event);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"protocol\":\"ag_ui\""));
    }

    #[test]
    fn test_protocol_message_acp() {
        let acp_msg = AcpMessage::notification(
            "session/update",
            Some(serde_json::json!({"sessionId": "sess-1"})),
        );
        let msg = ProtocolMessage::acp(acp_msg);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"protocol\":\"acp\""));
    }

    #[test]
    fn test_protocol_message_vantage() {
        let event = VantageEvent::midi_note_on(0, 60, 100);
        let msg = ProtocolMessage::vantage(event);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"protocol\":\"vantage\""));
    }

    #[test]
    fn test_protocol_message_stand() {
        let stand_msg = StandMessage::ChatChunk {
            content: "Hello".to_string(),
            done: false,
        };
        let msg = ProtocolMessage::stand(stand_msg);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"protocol\":\"stand\""));
    }
}
