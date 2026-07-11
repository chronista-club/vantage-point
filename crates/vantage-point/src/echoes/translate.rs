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
//!
//! ## transcript commit 境界（[`Ingested::commits_transcript`]）
//!
//! claude は完成した message を disk の transcript(jsonl) に flush する。 その瞬間は stream 上でも
//! 観測できる: **`assistant` 全文スナップショット行 / `user` message 行**が、transcript の 1 行と
//! 1:1 で対応する（実測: transcript は content block ごとに 1 行、 stream の snapshot も block
//! ごとに 1 本）。 [`EchoesTranslator`] はこれを `commits_transcript` として通知し、
//! [`super::host::EchoesAgentHost`] が「まだ disk に無い streaming 中の tail」を切り出すのに使う
//! （replay 時に transcript の後ろへ継ぐため）。
//!
//! ⚠️ snapshot 行は対応する `content_block_stop` の **前**に来る（実測）。 つまり本翻訳器が
//! `content_block_stop` で発火する [`EchoesEvent::ToolCall`] は「commit 済み block の遅れた表現」
//! であり、 tail に含めてはならない（transcript replay と二重化する）。 tail に載せてよいのは
//! commit 前にしか存在しない増分 = [`EchoesEvent::MessageChunk`] / [`EchoesEvent::ThoughtChunk`]。

use std::collections::HashMap;

use serde::Deserialize;

use super::event::{EchoesEvent, PlanEntry};

/// 1 engine プロセスの stream を通す翻訳器（プロセスごとに 1 個、可変状態を持つ）。
#[derive(Debug, Default)]
pub struct EchoesTranslator {
    /// content block index → 進行中の block 状態。
    blocks: HashMap<u64, BlockState>,
}

/// [`EchoesTranslator::ingest`] の結果 — 「この行が生んだ event」と「この行が disk に commit したか」。
#[derive(Debug, Default, PartialEq)]
pub struct Ingested {
    /// GUI へ流す event 列（0 個以上）。
    pub events: Vec<EchoesEvent>,
    /// この行が transcript(jsonl) の 1 行に対応する = ここまでの内容は disk 側に存在する。
    /// [`super::host::EchoesAgentHost`] が in-flight tail を切るのに使う（module doc 参照）。
    pub commits_transcript: bool,
}

impl Ingested {
    /// event だけの結果（commit 境界ではない）。
    fn events(events: Vec<EchoesEvent>) -> Self {
        Self {
            events,
            commits_transcript: false,
        }
    }
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

    /// stream-json の 1 行を食わせ、0 個以上の [`EchoesEvent`] と commit 境界フラグを得る。
    ///
    /// parse 不能な行・未知 type は握り潰して空を返す（stream を止めない）。
    pub fn ingest(&mut self, line: &str) -> Ingested {
        let line = line.trim();
        if line.is_empty() {
            return Ingested::default();
        }
        match serde_json::from_str::<RawLine>(line) {
            Ok(raw) => self.translate(raw),
            Err(e) => {
                tracing::debug!("echoes: stream-json parse 失敗（無視）: {e} - line: {line}");
                Ingested::default()
            }
        }
    }

    fn translate(&mut self, raw: RawLine) -> Ingested {
        match raw {
            RawLine::System(sys) => Ingested::events(self.on_system(sys)),
            RawLine::StreamEvent { event } => Ingested::events(self.on_stream_event(event)),
            // user(tool_result) / assistant(全文スナップショット) はどちらも transcript の 1 行に
            // 対応する = ここまでが disk に載った合図。 assistant の中身は delta の累積なので捨てる。
            RawLine::User { message } => Ingested {
                events: on_user(message),
                commits_transcript: true,
            },
            RawLine::Assistant => Ingested {
                events: Vec::new(),
                commits_transcript: true,
            },
            RawLine::Result(res) => Ingested::events(vec![on_result(res)]),
            // rate_limit_event 等は破棄。
            RawLine::Other => Ingested::default(),
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
    /// assistant 全文スナップショット。 中身は delta の累積なので使わないが、 **transcript に
    /// 1 行 flush された合図**として commit 境界に使う（module doc 参照）。
    Assistant,
    Result(RawResult),
    /// rate_limit_event / 未知 type を全て吸収。
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
        let evs = t.ingest(line).events;
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
        let think = t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"考え"}}}"#).events;
        assert_eq!(
            think,
            vec![EchoesEvent::ThoughtChunk {
                text: "考え".into()
            }]
        );

        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#);
        let text = t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"hello"}}}"#).events;
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
        let evs = t
            .ingest(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#)
            .events;
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
        let evs = t
            .ingest(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#)
            .events;
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
        let evs = t.ingest(r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-1","type":"tool_result","content":"PERMISSION_TEST_OK","is_error":false}]}}"#).events;
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
        let evs = t.ingest(r#"{"type":"result","subtype":"success","session_id":"sid-1","is_error":false,"total_cost_usd":0.012}"#).events;
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
            assert!(t.ingest(line).events.is_empty(), "should ignore: {line}");
        }
    }

    /// commit 境界: assistant スナップショット / user 行だけが `commits_transcript` を立てる。
    /// stream_event（delta 等）は disk に何も書かないので立てない。
    #[test]
    fn only_message_lines_commit_transcript() {
        let mut t = EchoesTranslator::new();
        let snapshot = t.ingest(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"本文"}]},"session_id":"s"}"#,
        );
        assert!(snapshot.commits_transcript, "assistant 行は commit 境界");
        assert!(snapshot.events.is_empty(), "中身は使わない（delta の累積）");

        let user = t.ingest(
            r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-1","type":"tool_result","content":"ok","is_error":false}]}}"#,
        );
        assert!(user.commits_transcript, "user 行は commit 境界");
        assert_eq!(user.events.len(), 1, "ToolCallUpdate は出る");

        for line in [
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}}"#,
            r#"{"type":"stream_event","event":{"type":"message_stop"}}"#,
            r#"{"type":"result","subtype":"success","session_id":"s","is_error":false}"#,
            r#"{"type":"rate_limit_event"}"#,
            "not json",
        ] {
            assert!(
                !t.ingest(line).commits_transcript,
                "commit 境界ではない: {line}"
            );
        }
    }

    /// ゴールデン回帰（in-flight tail 設計の前提）: tool_use block では
    /// **assistant スナップショット（commit）が `content_block_stop`（ToolCall 発火）より先**に来る。
    ///
    /// つまり ToolCall は「既に disk にある block の遅れた表現」であり、
    /// [`super::super::host::EchoesAgentHost`] の in-flight tail に載せてはならない。
    /// この順序が逆転すると tail が ToolCall を掴んで replay が二重化する。
    #[test]
    fn tool_use_snapshot_commits_before_tool_call_fires() {
        let raw = include_str!("testdata/turn_read_edit.jsonl");
        let mut t = EchoesTranslator::new();
        let mut commits = 0usize;
        let mut commits_at_first_tool_call = None;

        for line in raw.lines() {
            let out = t.ingest(line);
            if out
                .events
                .iter()
                .any(|e| matches!(e, EchoesEvent::ToolCall { .. }))
            {
                commits_at_first_tool_call = Some(commits);
                break;
            }
            if out.commits_transcript {
                commits += 1;
            }
        }
        // 実測の 1 ターン目: thinking block の snapshot → tool_use block の snapshot →
        // content_block_stop(ToolCall)。 よって ToolCall 発火時点で commit は 2 本済んでいる。
        // snapshot が stop の後に来る実装なら 1 本しか観測されない。
        assert_eq!(
            commits_at_first_tool_call,
            Some(2),
            "ToolCall 発火時点で当該 tool_use block は既に commit 済みのはず"
        );
    }

    /// ゴールデン: 実測の Read→Edit→text 完全ターン（Step 0 spike B）を通す。
    #[test]
    fn golden_full_turn_read_edit() {
        let raw = include_str!("testdata/turn_read_edit.jsonl");
        let mut t = EchoesTranslator::new();
        let mut evs = Vec::new();
        for line in raw.lines() {
            evs.extend(t.ingest(line).events);
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
