//! codex app-server notification → [`ConversationEvent`] 翻訳（doc 41 §2-3）
//!
//! [`super::codex_host::CodexAgentHost`]（常駐 RpcHost）の item 層翻訳。turn / thread の
//! lifecycle（SessionInit / TurnCompleted / Error）は host 側が扱い、本 module は
//! **item 系 notification だけ**を純翻訳する（data / calculations 分離）。
//!
//! ## 方針（doc 41 §1 の実測 wire + `generate-json-schema` 0.144.5 で固定）
//!
//! | notification | ConversationEvent |
//! |--------------|-------------|
//! | `item/agentMessage/delta` | `MessageChunk`（主 stream） |
//! | `item/reasoning/textDelta` / `summaryTextDelta` | `ThoughtChunk` |
//! | `item/completed` agentMessage / reasoning | delta を一度も見ていない時だけ全文 fallback（重複防止 — claude 翻訳と同じ規律） |
//! | `item/started` tool 系 | `ToolCall` |
//! | `item/completed` tool 系 | `ToolCallUpdate`（started 未観測なら ToolCall を補完 — exec 版 codex_translate の規律を踏襲） |
//! | その他（userMessage / mcpServer 状態 / tokenUsage 等） | 無視（未知 method に寛容 = protocol drift 吸収、doc 41 §5） |

use std::collections::HashSet;

use super::event::ConversationEvent;

/// tool 系 item の表示写像（exec 版 `codex_translate::tool_name` と同じ語彙に揃える —
/// GUI の見た目が TurnHost 時代と変わらないこと）。None = tool として扱わない type。
fn tool_name(item_type: &str) -> Option<&'static str> {
    match item_type {
        "commandExecution" => Some("shell"),
        "fileChange" => Some("apply_patch"),
        "mcpToolCall" => Some("mcp"),
        "dynamicToolCall" => Some("tool"),
        "webSearch" => Some("web_search"),
        "imageGeneration" => Some("image_generation"),
        _ => None,
    }
}

/// ToolCall の input 表示: 既知 field を優先し、無ければ item 全体（exec 版と同じ方針）。
fn tool_input(item: &serde_json::Value) -> serde_json::Value {
    if let Some(cmd) = item.get("command") {
        return serde_json::json!({ "command": cmd, "cwd": item.get("cwd") });
    }
    if let Some(args) = item.get("arguments") {
        return serde_json::json!({
            "server": item.get("server"),
            "tool": item.get("tool"),
            "arguments": args,
        });
    }
    if let Some(q) = item.get("query") {
        return serde_json::json!({ "query": q });
    }
    if let Some(changes) = item.get("changes") {
        return serde_json::json!({ "changes": changes });
    }
    item.clone()
}

/// ToolCallUpdate の content / is_error（tool 種別ごとの結果表現差を吸収）。
fn tool_result(item: &serde_json::Value) -> (String, bool) {
    let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let failed = status == "failed"
        || item.get("error").is_some_and(|e| !e.is_null())
        || item
            .get("exitCode")
            .and_then(|v| v.as_i64())
            .is_some_and(|c| c != 0);
    let content = item
        .get("aggregatedOutput")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            item.get("result")
                .filter(|v| !v.is_null())
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| status.to_string());
    (content, failed)
}

/// reasoning item の全文（`summary` 優先、無ければ `content`。どちらも文字列 or 配列を許容）。
fn reasoning_text(item: &serde_json::Value) -> String {
    for key in ["summary", "content"] {
        match item.get(key) {
            Some(serde_json::Value::String(s)) if !s.is_empty() => return s.clone(),
            Some(serde_json::Value::Array(a)) => {
                let joined: String = a
                    .iter()
                    .filter_map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .or_else(|| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !joined.is_empty() {
                    return joined;
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// app-server notification の item 層翻訳器（1 host = 1 個、turn を跨いで生存）。
#[derive(Debug, Default)]
pub struct CodexRpcTranslator {
    /// delta を観測した item id（completed の全文 fallback を抑止 — 重複防止）。
    delta_seen: HashSet<String>,
    /// `item/started` で ToolCall を emit 済みの item id（completed 側の補完判定）。
    started_tools: HashSet<String>,
}

impl CodexRpcTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// notification 1 件を食わせ、0 個以上の [`ConversationEvent`] を得る（純翻訳）。
    /// lifecycle 系（thread/turn/error）は host が先に刈るため、ここに来るのは item 系のみ
    /// という前提は**置かない** — 未知 method は無条件に空を返す（寛容）。
    pub fn ingest(&mut self, method: &str, params: &serde_json::Value) -> Vec<ConversationEvent> {
        match method {
            "item/agentMessage/delta" => {
                self.mark_delta(params);
                match params.get("delta").and_then(|v| v.as_str()) {
                    Some(d) if !d.is_empty() => {
                        vec![ConversationEvent::MessageChunk { text: d.into() }]
                    }
                    _ => Vec::new(),
                }
            }
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                self.mark_delta(params);
                match params.get("delta").and_then(|v| v.as_str()) {
                    Some(d) if !d.is_empty() => {
                        vec![ConversationEvent::ThoughtChunk { text: d.into() }]
                    }
                    _ => Vec::new(),
                }
            }
            "item/started" => {
                let Some(item) = params.get("item") else {
                    return Vec::new();
                };
                self.tool_started(item)
            }
            "item/completed" => {
                let Some(item) = params.get("item") else {
                    return Vec::new();
                };
                self.item_completed(item)
            }
            _ => Vec::new(),
        }
    }

    fn mark_delta(&mut self, params: &serde_json::Value) {
        if let Some(id) = params.get("itemId").and_then(|v| v.as_str()) {
            self.delta_seen.insert(id.to_string());
        }
    }

    fn tool_started(&mut self, item: &serde_json::Value) -> Vec<ConversationEvent> {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let Some(name) = tool_name(item_type) else {
            return Vec::new(); // agentMessage / reasoning / userMessage 等は started で何も出さない
        };
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        self.started_tools.insert(id.clone());
        vec![ConversationEvent::ToolCall {
            id,
            name: name.into(),
            input: tool_input(item),
        }]
    }

    fn item_completed(&mut self, item: &serde_json::Value) -> Vec<ConversationEvent> {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        match item_type {
            "agentMessage" => {
                if self.delta_seen.remove(id) {
                    return Vec::new(); // delta で全文流し済み
                }
                match item.get("text").and_then(|v| v.as_str()) {
                    Some(t) if !t.is_empty() => {
                        vec![ConversationEvent::MessageChunk { text: t.into() }]
                    }
                    _ => Vec::new(),
                }
            }
            "reasoning" => {
                if self.delta_seen.remove(id) {
                    return Vec::new();
                }
                let text = reasoning_text(item);
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![ConversationEvent::ThoughtChunk { text }]
                }
            }
            t => {
                let Some(name) = tool_name(t) else {
                    return Vec::new(); // userMessage / plan / review 等
                };
                let mut out = Vec::new();
                if !self.started_tools.remove(id) {
                    // started を観測していない completed は ToolCall を補完（行が浮かない）。
                    out.push(ConversationEvent::ToolCall {
                        id: id.to_string(),
                        name: name.into(),
                        input: tool_input(item),
                    });
                }
                let (content, is_error) = tool_result(item);
                out.push(ConversationEvent::ToolCallUpdate {
                    tool_use_id: id.to_string(),
                    content,
                    is_error,
                });
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// doc 41 §1 の実測 wire（2026-07-18、codex-cli 0.144.5）そのままの行で
    /// delta → completed 抑止の主経路を固定する。
    #[test]
    fn real_wire_agent_message_deltas_then_completed_suppressed() {
        let mut tr = CodexRpcTranslator::new();
        let d1: serde_json::Value = serde_json::from_str(
            r#"{"threadId":"019f7207-7392-7a41-b2a2-0d6adcfc405e","turnId":"019f7207-9b54-7a42-a926-9ef7edfdeeb4","itemId":"msg_04f764","delta":"pong"}"#,
        )
        .unwrap();
        let d2: serde_json::Value =
            serde_json::from_str(r#"{"itemId":"msg_04f764","delta":"-alpha"}"#).unwrap();
        assert_eq!(
            tr.ingest("item/agentMessage/delta", &d1),
            vec![ConversationEvent::MessageChunk {
                text: "pong".into()
            }]
        );
        assert_eq!(
            tr.ingest("item/agentMessage/delta", &d2),
            vec![ConversationEvent::MessageChunk {
                text: "-alpha".into()
            }]
        );
        // completed（実測形: text に全文）— delta 済みなので何も出さない
        let done: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"agentMessage","id":"msg_04f764","text":"pong-alpha","phase":"final_answer","memoryCitation":null}}"#,
        )
        .unwrap();
        assert_eq!(tr.ingest("item/completed", &done), Vec::new());
        // 別 item の completed（delta 無し）は全文 fallback
        let done2: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"agentMessage","id":"msg_other","text":"full text"}}"#,
        )
        .unwrap();
        assert_eq!(
            tr.ingest("item/completed", &done2),
            vec![ConversationEvent::MessageChunk {
                text: "full text".into()
            }]
        );
    }

    /// userMessage（実測 wire）は echo しない（ChatView の optimistic bubble と二重になる）。
    #[test]
    fn user_message_items_are_ignored() {
        let mut tr = CodexRpcTranslator::new();
        let p: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"userMessage","id":"u1","clientId":null,"content":[{"type":"text","text":"hi"}]}}"#,
        )
        .unwrap();
        assert_eq!(tr.ingest("item/started", &p), Vec::new());
        assert_eq!(tr.ingest("item/completed", &p), Vec::new());
    }

    /// commandExecution: started=ToolCall（name=shell、exec 版と同語彙）/ completed=Update、
    /// exitCode≠0 は is_error。
    #[test]
    fn command_execution_maps_to_shell_tool_pair() {
        let mut tr = CodexRpcTranslator::new();
        let started: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"commandExecution","id":"c1","command":"ls -la","cwd":"/w","status":"inProgress"}}"#,
        )
        .unwrap();
        let ev = tr.ingest("item/started", &started);
        assert_eq!(ev.len(), 1);
        assert!(matches!(
            &ev[0],
            ConversationEvent::ToolCall { id, name, .. } if id == "c1" && name == "shell"
        ));

        let completed: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"commandExecution","id":"c1","command":"ls -la","aggregatedOutput":"file1\nfile2","exitCode":0,"status":"completed"}}"#,
        )
        .unwrap();
        let ev = tr.ingest("item/completed", &completed);
        assert_eq!(
            ev,
            vec![ConversationEvent::ToolCallUpdate {
                tool_use_id: "c1".into(),
                content: "file1\nfile2".into(),
                is_error: false,
            }],
            "started 済みなので Update のみ"
        );

        // started 未観測 + exitCode≠0 → ToolCall 補完 + is_error
        let failed: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"commandExecution","id":"c2","command":"false","aggregatedOutput":"","exitCode":1,"status":"failed"}}"#,
        )
        .unwrap();
        let ev = tr.ingest("item/completed", &failed);
        assert_eq!(ev.len(), 2, "ToolCall 補完 + Update");
        assert!(matches!(&ev[0], ConversationEvent::ToolCall { id, .. } if id == "c2"));
        assert!(matches!(
            &ev[1],
            ConversationEvent::ToolCallUpdate { is_error: true, .. }
        ));
    }

    /// reasoning: delta 優先、delta 無し completed は summary/content から全文 fallback。
    #[test]
    fn reasoning_delta_then_completed_suppressed_and_fallback() {
        let mut tr = CodexRpcTranslator::new();
        let d: serde_json::Value =
            serde_json::from_str(r#"{"itemId":"r1","delta":"考え中"}"#).unwrap();
        assert_eq!(
            tr.ingest("item/reasoning/textDelta", &d),
            vec![ConversationEvent::ThoughtChunk {
                text: "考え中".into()
            }]
        );
        let done: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"reasoning","id":"r1","summary":["結論"],"content":null}}"#,
        )
        .unwrap();
        assert_eq!(
            tr.ingest("item/completed", &done),
            Vec::new(),
            "delta 済みは抑止"
        );

        let done2: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"reasoning","id":"r2","summary":[{"text":"要約 A"},{"text":"要約 B"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            tr.ingest("item/completed", &done2),
            vec![ConversationEvent::ThoughtChunk {
                text: "要約 A\n要約 B".into()
            }]
        );
    }

    /// mcpToolCall / webSearch / 未知 method の寛容性。
    #[test]
    fn mcp_web_and_unknown_methods() {
        let mut tr = CodexRpcTranslator::new();
        let mcp: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"mcpToolCall","id":"m1","server":"creo","tool":"remember","arguments":{"k":1},"status":"completed","result":{"ok":true},"error":null}}"#,
        )
        .unwrap();
        let ev = tr.ingest("item/completed", &mcp);
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[0], ConversationEvent::ToolCall { name, .. } if name == "mcp"));
        assert!(
            matches!(&ev[1], ConversationEvent::ToolCallUpdate { content, is_error: false, .. } if content.contains("ok"))
        );

        let ws: serde_json::Value = serde_json::from_str(
            r#"{"item":{"type":"webSearch","id":"w1","query":"rust jsonl","action":null}}"#,
        )
        .unwrap();
        let ev = tr.ingest("item/started", &ws);
        assert!(matches!(&ev[0], ConversationEvent::ToolCall { name, .. } if name == "web_search"));

        // 未知 method / 未知 item type は静かに無視（protocol drift 吸収）
        assert_eq!(
            tr.ingest("thread/tokenUsage/updated", &serde_json::json!({})),
            Vec::new()
        );
        let unknown: serde_json::Value =
            serde_json::from_str(r#"{"item":{"type":"sleep","id":"s1","durationMs":10}}"#).unwrap();
        assert_eq!(tr.ingest("item/completed", &unknown), Vec::new());
    }
}
