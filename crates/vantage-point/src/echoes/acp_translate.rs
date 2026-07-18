//! ACP `session/update` → [`EchoesEvent`] 翻訳（doc 42 §3）
//!
//! [`super::acp_host::AcpAgentHost`]（grok / opencode の常駐 ACP host — ACP は engine 非依存な
//! ので本翻訳器も両 engine 共有、doc 43）の update 層翻訳。lifecycle
//! （SessionInit / TurnCompleted / Error / permission 自動応答）は host 側が扱い、本 module は
//! **`session/update` notification の update object だけ**を純翻訳する（data / calculations 分離）。
//!
//! ## 方針（doc 42 §1 の実測 wire + ACP 標準形で固定）
//!
//! | update.sessionUpdate | EchoesEvent |
//! |----------------------|-------------|
//! | `agent_message_chunk`（content.type=text） | `MessageChunk`（実測） |
//! | `agent_thought_chunk` | `ThoughtChunk`（実測） |
//! | `tool_call` | `ToolCall`（ACP 標準形 {toolCallId, title, kind, rawInput}） |
//! | `tool_call_update` | `ToolCallUpdate`（status=failed → is_error。started 未観測なら ToolCall 補完） |
//! | `plan` | `Plan`（{entries: [{content, status}]}） |
//! | その他（available_commands_update / x.ai 拡張等） | 無視（protocol drift 吸収 — doc 41 §5 と同じ寛容規律） |

use std::collections::HashSet;

use super::event::{EchoesEvent, PlanEntry};

/// content block（{type:"text", text}）から text を取り出す（それ以外の type は無視）。
fn content_text(content: &serde_json::Value) -> Option<String> {
    match content.get("type").and_then(|v| v.as_str()) {
        Some("text") => content
            .get("text")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// ACP `session/update` の update 層翻訳器（1 host = 1 個、turn を跨いで生存）。
#[derive(Debug, Default)]
pub struct AcpTranslator {
    /// `tool_call` で ToolCall を emit 済みの toolCallId（update 側の補完判定）。
    started_tools: HashSet<String>,
}

impl AcpTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// `session/update` の params.update 1 件を食わせ、0 個以上の [`EchoesEvent`] を得る。
    /// 未知 variant は無条件に空（寛容）。
    pub fn ingest(&mut self, update: &serde_json::Value) -> Vec<EchoesEvent> {
        let kind = update
            .get("sessionUpdate")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match kind {
            "agent_message_chunk" => update
                .get("content")
                .and_then(content_text)
                .map(|text| vec![EchoesEvent::MessageChunk { text }])
                .unwrap_or_default(),
            "agent_thought_chunk" => update
                .get("content")
                .and_then(content_text)
                .map(|text| vec![EchoesEvent::ThoughtChunk { text }])
                .unwrap_or_default(),
            "tool_call" => {
                let id = update
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.started_tools.insert(id.clone());
                vec![EchoesEvent::ToolCall {
                    id,
                    name: update
                        .get("title")
                        .and_then(|v| v.as_str())
                        .or_else(|| update.get("kind").and_then(|v| v.as_str()))
                        .unwrap_or("tool")
                        .to_string(),
                    input: update.get("rawInput").cloned().unwrap_or_default(),
                }]
            }
            "tool_call_update" => {
                let id = update
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let status = update.get("status").and_then(|v| v.as_str()).unwrap_or("");
                // in_progress 等の中間 update は流さない（終端だけ表示 — 頻度を抑える）。
                if status != "completed" && status != "failed" {
                    return Vec::new();
                }
                let mut out = Vec::new();
                if !self.started_tools.remove(id) {
                    out.push(EchoesEvent::ToolCall {
                        id: id.to_string(),
                        name: update
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string(),
                        input: update.get("rawInput").cloned().unwrap_or_default(),
                    });
                }
                let content = update
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter_map(|b| b.get("content").and_then(content_text))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                out.push(EchoesEvent::ToolCallUpdate {
                    tool_use_id: id.to_string(),
                    content: if content.is_empty() {
                        status.to_string()
                    } else {
                        content
                    },
                    is_error: status == "failed",
                });
                out
            }
            "plan" => {
                let entries = update
                    .get("entries")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                let content = e.get("content").and_then(|v| v.as_str())?;
                                Some(PlanEntry {
                                    content: content.to_string(),
                                    status: e
                                        .get("status")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("pending")
                                        .to_string(),
                                    active_form: None,
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if entries.is_empty() {
                    Vec::new()
                } else {
                    vec![EchoesEvent::Plan { entries }]
                }
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// doc 42 §1 の実測 wire（grok 0.2.64）そのままの update で主経路を固定。
    #[test]
    fn real_wire_message_and_thought_chunks() {
        let mut tr = AcpTranslator::new();
        let msg: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"gro"}}"#,
        )
        .unwrap();
        assert_eq!(
            tr.ingest(&msg),
            vec![EchoesEvent::MessageChunk { text: "gro".into() }]
        );
        let thought: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"考え中"}}"#,
        )
        .unwrap();
        assert_eq!(
            tr.ingest(&thought),
            vec![EchoesEvent::ThoughtChunk {
                text: "考え中".into()
            }]
        );
    }

    /// tool_call → tool_call_update（completed / failed / 中間 in_progress の抑止 / 補完）。
    #[test]
    fn tool_call_lifecycle_maps_and_backfills() {
        let mut tr = AcpTranslator::new();
        let call: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"shell","kind":"execute","rawInput":{"command":"ls"}}"#,
        )
        .unwrap();
        let ev = tr.ingest(&call);
        assert!(
            matches!(&ev[0], EchoesEvent::ToolCall { id, name, .. } if id == "t1" && name == "shell")
        );

        // 中間 update は無視
        let progress: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress"}"#,
        )
        .unwrap();
        assert_eq!(tr.ingest(&progress), Vec::new());

        let done: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"file1"}}]}"#,
        )
        .unwrap();
        assert_eq!(
            tr.ingest(&done),
            vec![EchoesEvent::ToolCallUpdate {
                tool_use_id: "t1".into(),
                content: "file1".into(),
                is_error: false,
            }]
        );

        // started 未観測の failed → ToolCall 補完 + is_error
        let failed: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t2","status":"failed","title":"shell"}"#,
        )
        .unwrap();
        let ev = tr.ingest(&failed);
        assert_eq!(ev.len(), 2);
        assert!(matches!(
            &ev[1],
            EchoesEvent::ToolCallUpdate { is_error: true, .. }
        ));
    }

    /// plan 変換 + 未知 variant の寛容性（実測の available_commands_update / x.ai 拡張形）。
    #[test]
    fn plan_maps_and_unknown_variants_ignored() {
        let mut tr = AcpTranslator::new();
        let plan: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"plan","entries":[{"content":"調査する","status":"in_progress"},{"content":"実装する","status":"pending"}]}"#,
        )
        .unwrap();
        let ev = tr.ingest(&plan);
        assert!(matches!(&ev[0], EchoesEvent::Plan { entries } if entries.len() == 2));

        let cmds: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"compact"}]}"#,
        )
        .unwrap();
        assert_eq!(tr.ingest(&cmds), Vec::new());
        let hook: serde_json::Value = serde_json::from_str(
            r#"{"sessionUpdate":"hook_execution","event_name":"stop","runs":[]}"#,
        )
        .unwrap();
        assert_eq!(tr.ingest(&hook), Vec::new());
    }
}
