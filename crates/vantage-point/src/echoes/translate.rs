//! stream-json → [`EchoesEvent`] 翻訳層（claude engine 用、PR1 で凍結）
//!
//! claude 2.1.197 の `--output-format stream-json --include-partial-messages`
//! が吐く JSONL を、GUI 語彙 [`EchoesEvent`] の列へ変換する状態機械。
//! 実スキーマの根拠は design doc 32 §10（Step 0 スパイク）。
//!
//! ## 状態機械の要点
//! - streaming 本文/thinking は `stream_event` の `content_block_delta` から取る
//!   （`assistant` 全文 message は delta の累積スナップショットなので使わない）。
//! - `content_block_delta` は `event.index` を持つ。`content_block_start` で
//!   index → block 種別を記録し、delta を種別に応じて振り分ける。
//! - tool の完全 input は `input_json_delta.partial_json` を index ごとに蓄積し、
//!   `content_block_stop` で 1 度だけ parse して [`EchoesEvent::ToolCall`] を発火。
//! - tool 結果は `user` message の `tool_result` から [`EchoesEvent::ToolCallUpdate`]。
//! - `TodoWrite` の ToolCall は [`EchoesEvent::Plan`] へ導出する。

use std::collections::HashMap;

use serde::Deserialize;

use super::event::{EchoesEvent, PlanEntry};

/// 1 engine プロセスの stream を通す翻訳器（プロセスごとに 1 個、可変状態を持つ）。
#[derive(Debug, Default)]
pub struct EchoesTranslator {
    /// content block index → 進行中の block 状態。
    blocks: HashMap<u64, BlockState>,
}

/// content block の進行中状態（`content_block_start`〜`stop` の間だけ生きる）。
#[derive(Debug, Clone)]
enum BlockState {
    /// text / thinking は delta を都度流すので蓄積不要（種別だけ保持）。
    Text,
    Thinking,
    /// tool_use は id/name と partial_json バッファを保持し、stop でまとめて発火。
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
    /// 未知 block（signature 等）。破棄する。
    Other,
}

impl EchoesTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// stream-json の 1 行を食わせ、0 個以上の [`EchoesEvent`] を得る。
    ///
    /// parse 不能な行・未知 type は握り潰して空を返す（stream を止めない）。
    pub fn ingest(&mut self, line: &str) -> Vec<EchoesEvent> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }
        match serde_json::from_str::<RawLine>(line) {
            Ok(raw) => self.translate(raw),
            Err(e) => {
                tracing::debug!("echoes: stream-json parse 失敗（無視）: {e} - line: {line}");
                Vec::new()
            }
        }
    }

    fn translate(&mut self, raw: RawLine) -> Vec<EchoesEvent> {
        match raw {
            RawLine::System(sys) => self.on_system(sys),
            RawLine::StreamEvent { event } => self.on_stream_event(event),
            RawLine::User { message } => on_user(message),
            RawLine::Result(res) => vec![on_result(res)],
            // assistant（累積スナップショット）/ rate_limit_event 等は破棄。
            RawLine::Other => Vec::new(),
        }
    }

    fn on_system(&mut self, sys: RawSystem) -> Vec<EchoesEvent> {
        // init 以外の system subtype（hook_started / status / thinking_tokens …）は破棄。
        if sys.subtype.as_deref() != Some("init") {
            return Vec::new();
        }
        let Some(session_id) = sys.session_id else {
            return Vec::new();
        };
        let mcp_servers = sys
            .mcp_servers
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.name)
            .collect();
        vec![EchoesEvent::SessionInit {
            session_id,
            model: sys.model,
            permission_mode: sys.permission_mode,
            cwd: sys.cwd,
            tools: sys.tools.unwrap_or_default(),
            mcp_servers,
            slash_commands: sys.slash_commands.unwrap_or_default(),
        }]
    }

    fn on_stream_event(&mut self, event: StreamEventBody) -> Vec<EchoesEvent> {
        match event {
            StreamEventBody::ContentBlockStart {
                index,
                content_block,
            } => {
                let state = match content_block {
                    RawContentBlock::Text => BlockState::Text,
                    RawContentBlock::Thinking => BlockState::Thinking,
                    RawContentBlock::ToolUse { id, name } => BlockState::ToolUse {
                        id,
                        name,
                        json_buf: String::new(),
                    },
                    RawContentBlock::Other => BlockState::Other,
                };
                self.blocks.insert(index, state);
                Vec::new()
            }
            StreamEventBody::ContentBlockDelta { index, delta } => self.on_delta(index, delta),
            StreamEventBody::ContentBlockStop { index } => self.on_block_stop(index),
            // message_start / message_delta / message_stop / 未知は破棄。
            StreamEventBody::Other => Vec::new(),
        }
    }

    fn on_delta(&mut self, index: u64, delta: RawDelta) -> Vec<EchoesEvent> {
        match delta {
            RawDelta::TextDelta { text } => vec![EchoesEvent::MessageChunk { text }],
            RawDelta::ThinkingDelta { thinking } => {
                vec![EchoesEvent::ThoughtChunk { text: thinking }]
            }
            RawDelta::InputJsonDelta { partial_json } => {
                if let Some(BlockState::ToolUse { json_buf, .. }) = self.blocks.get_mut(&index) {
                    json_buf.push_str(&partial_json);
                }
                Vec::new()
            }
            // signature_delta / 未知は破棄。
            RawDelta::Other => Vec::new(),
        }
    }

    fn on_block_stop(&mut self, index: u64) -> Vec<EchoesEvent> {
        // tool_use block の終端でだけ ToolCall / Plan を発火する。
        match self.blocks.remove(&index) {
            Some(BlockState::ToolUse { id, name, json_buf }) => {
                let input: serde_json::Value = if json_buf.trim().is_empty() {
                    serde_json::Value::Object(Default::default())
                } else {
                    serde_json::from_str(&json_buf).unwrap_or_else(|e| {
                        tracing::debug!("echoes: tool input JSON parse 失敗 {name}: {e}");
                        serde_json::Value::Object(Default::default())
                    })
                };
                // TodoWrite は plan ウィジェット用に Plan へ導出。
                if name == "TodoWrite"
                    && let Some(plan) = plan_from_todowrite(&input)
                {
                    return vec![plan];
                }
                vec![EchoesEvent::ToolCall { id, name, input }]
            }
            _ => Vec::new(),
        }
    }
}

// =============================================================================
// user message（tool_result）/ result の変換（状態不要 = 自由関数）
// =============================================================================

fn on_user(message: RawUserMessage) -> Vec<EchoesEvent> {
    message
        .content
        .into_iter()
        .filter_map(|block| match block {
            RawUserContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(EchoesEvent::ToolCallUpdate {
                tool_use_id,
                content: tool_result_text(&content),
                is_error,
            }),
            RawUserContent::Other => None,
        })
        .collect()
}

fn on_result(res: RawResult) -> EchoesEvent {
    if res.is_error {
        EchoesEvent::Error {
            message: res
                .result
                .unwrap_or_else(|| "engine turn error".to_string()),
        }
    } else {
        EchoesEvent::TurnCompleted {
            session_id: res.session_id,
            cost_usd: res.total_cost_usd,
        }
    }
}

/// tool_result の `content`（string または block 配列）を表示用 text へ潰す。
/// transcript replay（[`super::transcript`]）も同じ潰し方を使うため crate 内公開。
pub(super) fn tool_result_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// TodoWrite の input（`{"todos":[{content,status,activeForm}]}`）を Plan へ。
/// transcript replay（[`super::transcript`]）も同じ導出を使うため crate 内公開。
pub(super) fn plan_from_todowrite(input: &serde_json::Value) -> Option<EchoesEvent> {
    let todos = input.get("todos")?.as_array()?;
    let entries = todos
        .iter()
        .filter_map(|t| {
            Some(PlanEntry {
                content: t.get("content")?.as_str()?.to_string(),
                status: t
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("pending")
                    .to_string(),
                active_form: t
                    .get("activeForm")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string()),
            })
        })
        .collect();
    Some(EchoesEvent::Plan { entries })
}

// =============================================================================
// 生 stream-json スキーマ（Step 0 実測、doc 32 §10）
// =============================================================================

/// stream-json の 1 行（外側 tag = "type"）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawLine {
    System(RawSystem),
    StreamEvent {
        event: StreamEventBody,
    },
    User {
        message: RawUserMessage,
    },
    Result(RawResult),
    /// assistant（累積スナップショット）/ rate_limit_event / 未知 type を全て吸収。
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawSystem {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// ⚠️ init は camelCase（`permissionMode`）。他フィールドは snake_case 混在。
    #[serde(default, rename = "permissionMode")]
    permission_mode: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    mcp_servers: Option<Vec<RawMcpServer>>,
    #[serde(default)]
    slash_commands: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RawMcpServer {
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEventBody {
    ContentBlockStart {
        index: u64,
        content_block: RawContentBlock,
    },
    ContentBlockDelta {
        index: u64,
        delta: RawDelta,
    },
    ContentBlockStop {
        index: u64,
    },
    /// message_start / message_delta / message_stop / 未知。
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawContentBlock {
    Text,
    Thinking,
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    /// signature_delta 等。
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawUserMessage {
    #[serde(default)]
    content: Vec<RawUserContent>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawUserContent {
    ToolResult {
        tool_use_id: String,
        /// string または block 配列。
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    total_cost_usd: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実測の init 行から SessionInit を取り出せる。
    #[test]
    fn parses_session_init() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp/x","session_id":"sid-1","model":"claude-haiku-4-5","permissionMode":"acceptEdits","tools":["Bash","Edit"],"mcp_servers":[{"name":"vantage-point","status":"connected"}],"slash_commands":["a","b"]}"#;
        let mut t = EchoesTranslator::new();
        let evs = t.ingest(line);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            EchoesEvent::SessionInit {
                session_id,
                model,
                permission_mode,
                tools,
                mcp_servers,
                slash_commands,
                ..
            } => {
                assert_eq!(session_id, "sid-1");
                assert_eq!(model.as_deref(), Some("claude-haiku-4-5"));
                assert_eq!(permission_mode.as_deref(), Some("acceptEdits"));
                assert_eq!(tools, &["Bash", "Edit"]);
                assert_eq!(mcp_servers, &["vantage-point"]);
                assert_eq!(slash_commands, &["a", "b"]);
            }
            other => panic!("expected SessionInit, got {other:?}"),
        }
    }

    /// text_delta / thinking_delta が chunk に化ける。
    #[test]
    fn streams_text_and_thinking_deltas() {
        let mut t = EchoesTranslator::new();
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}}"#);
        let think = t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"考え"}}}"#);
        assert_eq!(
            think,
            vec![EchoesEvent::ThoughtChunk {
                text: "考え".into()
            }]
        );

        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#);
        let text = t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"hello"}}}"#);
        assert_eq!(
            text,
            vec![EchoesEvent::MessageChunk {
                text: "hello".into()
            }]
        );
    }

    /// tool_use は partial_json を蓄積し、stop で完全 input 付き ToolCall を出す。
    #[test]
    fn assembles_tool_call_from_partial_json() {
        let mut t = EchoesTranslator::new();
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu-1","name":"Bash","input":{}}}}"#);
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"echo "}}}"#);
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"hi\"}"}}}"#);
        let evs =
            t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            EchoesEvent::ToolCall { id, name, input } => {
                assert_eq!(id, "tu-1");
                assert_eq!(name, "Bash");
                assert_eq!(input.get("command").unwrap(), "echo hi");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// TodoWrite は Plan に化ける。
    #[test]
    fn todowrite_becomes_plan() {
        let mut t = EchoesTranslator::new();
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu-2","name":"TodoWrite","input":{}}}}"#);
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"todos\":[{\"content\":\"step1\",\"status\":\"in_progress\",\"activeForm\":\"doing step1\"}]}"}}}"#);
        let evs =
            t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#);
        assert_eq!(
            evs,
            vec![EchoesEvent::Plan {
                entries: vec![PlanEntry {
                    content: "step1".into(),
                    status: "in_progress".into(),
                    active_form: Some("doing step1".into()),
                }]
            }]
        );
    }

    /// user tool_result は ToolCallUpdate になる。
    #[test]
    fn user_tool_result_becomes_update() {
        let mut t = EchoesTranslator::new();
        let evs = t.ingest(r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-1","type":"tool_result","content":"PERMISSION_TEST_OK","is_error":false}]}}"#);
        assert_eq!(
            evs,
            vec![EchoesEvent::ToolCallUpdate {
                tool_use_id: "tu-1".into(),
                content: "PERMISSION_TEST_OK".into(),
                is_error: false,
            }]
        );
    }

    /// result/success は TurnCompleted。
    #[test]
    fn result_becomes_turn_completed() {
        let mut t = EchoesTranslator::new();
        let evs = t.ingest(r#"{"type":"result","subtype":"success","session_id":"sid-1","is_error":false,"total_cost_usd":0.012}"#);
        assert_eq!(
            evs,
            vec![EchoesEvent::TurnCompleted {
                session_id: "sid-1".into(),
                cost_usd: Some(0.012),
            }]
        );
    }

    /// ノイズ行（hook / status / rate_limit / assistant スナップショット）は無視。
    #[test]
    fn ignores_noise_lines() {
        let mut t = EchoesTranslator::new();
        for line in [
            r#"{"type":"system","subtype":"hook_started"}"#,
            r#"{"type":"system","subtype":"status"}"#,
            r#"{"type":"system","subtype":"thinking_tokens"}"#,
            r#"{"type":"rate_limit_event"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"snapshot"}]},"session_id":"s"}"#,
            r#"not even json"#,
        ] {
            assert!(t.ingest(line).is_empty(), "should ignore: {line}");
        }
    }

    /// ゴールデン: 実測の Read→Edit→text 完全ターン（Step 0 spike B）を通す。
    #[test]
    fn golden_full_turn_read_edit() {
        let raw = include_str!("testdata/turn_read_edit.jsonl");
        let mut t = EchoesTranslator::new();
        let mut evs = Vec::new();
        for line in raw.lines() {
            evs.extend(t.ingest(line));
        }

        let session_inits = evs
            .iter()
            .filter(|e| matches!(e, EchoesEvent::SessionInit { .. }))
            .count();
        let tool_calls: Vec<&str> = evs
            .iter()
            .filter_map(|e| match e {
                EchoesEvent::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let has_thought = evs
            .iter()
            .any(|e| matches!(e, EchoesEvent::ThoughtChunk { .. }));
        let text: String = evs
            .iter()
            .filter_map(|e| match e {
                EchoesEvent::MessageChunk { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let turn_completed = evs
            .iter()
            .filter(|e| matches!(e, EchoesEvent::TurnCompleted { .. }))
            .count();
        let updates = evs
            .iter()
            .filter(|e| matches!(e, EchoesEvent::ToolCallUpdate { .. }))
            .count();

        assert_eq!(session_inits, 1, "init は 1 回");
        assert_eq!(
            tool_calls,
            vec!["Read", "Edit"],
            "Read→Edit の順で tool_call"
        );
        assert!(has_thought, "thinking chunk がある");
        assert!(updates >= 2, "tool_result が 2 件以上");
        assert!(
            text.to_lowercase().contains("done"),
            "最終 text に done を含む: {text:?}"
        );
        assert_eq!(turn_completed, 1, "turn_completed は 1 回");

        // Edit tool_call の完全 input（PR3 diff 合成の素材）が取れている。
        let edit_input = evs.iter().find_map(|e| match e {
            EchoesEvent::ToolCall { name, input, .. } if name == "Edit" => Some(input),
            _ => None,
        });
        let edit_input = edit_input.expect("Edit tool_call がある");
        assert_eq!(edit_input.get("old_string").unwrap(), "line two");
        assert_eq!(edit_input.get("new_string").unwrap(), "LINE TWO");
        assert!(
            edit_input
                .get("file_path")
                .and_then(|p| p.as_str())
                .is_some_and(|p| p.ends_with("sample.txt")),
            "file_path が取れている"
        );
    }
}
