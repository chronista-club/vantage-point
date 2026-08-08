//! stream-json → [`ConversationEvent`] 翻訳層（claude engine 用、PR1 で凍結）
//!
//! claude 2.1.197 の `--output-format stream-json --include-partial-messages`
//! が吐く JSONL を、GUI 語彙 [`ConversationEvent`] の列へ変換する状態機械。
//! 実スキーマの根拠は design doc 32 §10（Step 0 スパイク）。
//!
//! ## 状態機械の要点
//! - streaming 本文/thinking は `stream_event` の `content_block_delta` から取る
//!   （`assistant` 全文 message は delta の累積スナップショットなので使わない）。
//! - `content_block_delta` は `event.index` を持つ。`content_block_start` で
//!   index → block 種別を記録し、delta を種別に応じて振り分ける。
//! - tool の完全 input は `input_json_delta.partial_json` を index ごとに蓄積し、
//!   `content_block_stop` で 1 度だけ parse して [`ConversationEvent::ToolCall`] を発火。
//! - tool 結果は `user` message の `tool_result` から [`ConversationEvent::ToolCallUpdate`]。
//! - `TodoWrite` の ToolCall は [`ConversationEvent::Plan`] へ導出する。
//!
//! ## transcript commit 境界（[`Ingested::commits_transcript`]）
//!
//! claude は完成した message を disk の transcript(jsonl) に flush する。 その瞬間は stream 上でも
//! 観測できる: **`assistant` 全文スナップショット行 / `user` message 行**が、transcript の 1 行と
//! 1:1 で対応する（実測: transcript は content block ごとに 1 行、 stream の snapshot も block
//! ごとに 1 本）。 [`ClaudeTranslator`] はこれを `commits_transcript` として通知し、
//! [`super::host::ClaudeHost`] が「まだ disk に無い streaming 中の tail」を切り出すのに使う
//! （replay 時に transcript の後ろへ継ぐため）。
//!
//! ⚠️ snapshot 行は対応する `content_block_stop` の **前**に来る（実測）。 つまり本翻訳器が
//! `content_block_stop` で発火する [`ConversationEvent::ToolCall`] は「commit 済み block の遅れた表現」
//! であり、 tail に含めてはならない（transcript replay と二重化する）。 tail に載せてよいのは
//! commit 前にしか存在しない増分 = [`ConversationEvent::MessageChunk`] / [`ConversationEvent::ThoughtChunk`]。

use std::collections::HashMap;

use serde::Deserialize;

use super::event::{ConversationEvent, PlanEntry, SubagentRole};

/// 1 engine プロセスの stream を通す翻訳器（プロセスごとに 1 個、可変状態を持つ）。
#[derive(Debug, Default)]
pub struct ClaudeTranslator {
    /// content block index → 進行中の block 状態。
    blocks: HashMap<u64, BlockState>,
    /// 最後に観測した assistant snapshot の usage 合算（= 現在の context 占有 tokens）。
    ///
    /// assistant 行は本文としては破棄する（delta の累積スナップショット）が、`message.usage`
    /// はこの行にしか載らない一次情報なので、ここに退避して [`ConversationEvent::TurnCompleted`] の
    /// context ゲージに載せる。⚠️ `result.usage` は turn 内 iteration の**合算**で
    /// cache_read が重複計上されるため分子には使えない（Step 0 実測）。
    last_context_tokens: Option<u64>,
    /// init の model id。`result.modelUsage` が複数 entry（subagent が別 model を使った turn）
    /// のとき、本 session の entry を突き合わせるために保持する。
    session_model: Option<String>,
    /// 現 turn で `content_block_delta`（本文の streaming 供給）を 1 度でも観測したか。
    ///
    /// **slash command 等の synthetic turn を拾うための不変条件**（2026-07-20 実測）:
    /// 通常 turn は `delta*` → `assistant`(累積 snapshot) → `result` と流れるので snapshot の
    /// 本文は重複＝破棄でよい。だが CLI 内で完結する turn（`/model` `/context` 等の slash、
    /// API 往復ゼロ・`model: "<synthetic>"`）は **delta を 1 つも出さず assistant snapshot だけで
    /// 本文を運ぶ**ため、破棄すると GUI が完全な無音になる（gui で slash を打つと 30 分待っても
    /// 何も出ない、の真因）。「delta 未観測なら snapshot が唯一の一次情報」で拾う — engine 側の
    /// `<synthetic>` という実装詳細に結合しない判定にする。
    /// turn 終端（`result`）で false に戻す。
    saw_text_delta: bool,
}

/// [`ClaudeTranslator::ingest`] の結果 — 「この行が生んだ event」と「この行が disk に commit したか」。
#[derive(Debug, Default, PartialEq)]
pub struct Ingested {
    /// GUI へ流す event 列（0 個以上）。
    pub events: Vec<ConversationEvent>,
    /// この行が transcript(jsonl) の 1 行に対応する = ここまでの内容は disk 側に存在する。
    /// [`super::host::ClaudeHost`] が in-flight tail を切るのに使う（module doc 参照）。
    pub commits_transcript: bool,
}

impl Ingested {
    /// event だけの結果（commit 境界ではない）。
    fn events(events: Vec<ConversationEvent>) -> Self {
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

impl ClaudeTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// stream-json の 1 行を食わせ、0 個以上の [`ConversationEvent`] と commit 境界フラグを得る。
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
                tracing::debug!("conversation: stream-json parse 失敗（無視）: {e} - line: {line}");
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
            // subagent 由来（parent 付き）は親から隔離する。 親の handler に流すと 3 つ壊れる:
            //   1. tool_result が親の ToolCallUpdate になり、親の item 列に結び先が無く孤児化する
            //   2. commit 境界を誤って立て、in-flight tail の切り出しがズレる（「応答中の永久居座り」
            //      と同じ層の事故）
            //   3. 本文が親の発話として描かれる（親が言っていないことを言ったことになる）
            RawLine::User {
                message,
                parent_tool_use_id: Some(parent),
            } => Ingested::events(subagent_user_events(&parent, message)),
            RawLine::User {
                message,
                parent_tool_use_id: None,
            } => Ingested {
                events: on_user(message),
                commits_transcript: true,
            },
            RawLine::Assistant {
                message,
                parent_tool_use_id: Some(parent),
            } => Ingested::events(subagent_assistant_events(&parent, message)),
            RawLine::Assistant {
                message,
                parent_tool_use_id: None,
            } => {
                // 本文は delta の累積なので通常は描画しないが、`message.usage` だけ context ゲージの
                // 分子として退避する（usage はこの行にしか載らない一次情報。cc-status が
                // transcript から掘るのと同じ値）。assistant 行自体は transcript に 1 行 flush
                // された合図なので commit 境界を立てる。
                if let Some(u) = message.usage {
                    self.last_context_tokens = Some(
                        u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens,
                    );
                }
                // ⚠️ 例外 = delta 未観測（[`Self::saw_text_delta`]）: CLI 内で完結する synthetic turn
                // （slash command 等）は delta を出さないので、この snapshot が本文の唯一の担い手。
                // 破棄すると GUI が無音になる。subagent 行（delta が来ない）と同じ理屈で拾う。
                let events = if self.saw_text_delta {
                    Vec::new()
                } else {
                    snapshot_text_events(message.content)
                };
                Ingested {
                    events,
                    commits_transcript: true,
                }
            }
            RawLine::Result(res) => Ingested::events(vec![self.on_result(res)]),
            // rate_limit_event 等は破棄。
            RawLine::Other => Ingested::default(),
        }
    }

    fn on_system(&mut self, sys: RawSystem) -> Vec<ConversationEvent> {
        // init 以外の system subtype（hook_started / status / thinking_tokens …）は破棄。
        if sys.subtype.as_deref() != Some("init") {
            return Vec::new();
        }
        let Some(session_id) = sys.session_id else {
            return Vec::new();
        };
        // modelUsage 突き合わせ用（context ゲージの分母選択）。
        self.session_model = sys.model.clone();
        let mcp_servers = sys
            .mcp_servers
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.name)
            .collect();
        vec![ConversationEvent::SessionInit {
            session_id,
            model: sys.model,
            permission_mode: sys.permission_mode,
            cwd: sys.cwd,
            tools: sys.tools.unwrap_or_default(),
            mcp_servers,
            slash_commands: sys.slash_commands.unwrap_or_default(),
            // 説明は pump が filesystem から注ぐ（この層は CLI の行を写すだけ）。
            command_docs: Default::default(),
        }]
    }

    fn on_stream_event(&mut self, event: StreamEventBody) -> Vec<ConversationEvent> {
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

    fn on_delta(&mut self, index: u64, delta: RawDelta) -> Vec<ConversationEvent> {
        match delta {
            RawDelta::TextDelta { text } => {
                // 本文が streaming で供給された = 後続の assistant snapshot は重複（[`Self::saw_text_delta`]）。
                self.saw_text_delta = true;
                vec![ConversationEvent::MessageChunk { text }]
            }
            RawDelta::ThinkingDelta { thinking } => {
                vec![ConversationEvent::ThoughtChunk { text: thinking }]
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

    fn on_block_stop(&mut self, index: u64) -> Vec<ConversationEvent> {
        // tool_use block の終端でだけ ToolCall / Plan を発火する。
        match self.blocks.remove(&index) {
            Some(BlockState::ToolUse { id, name, json_buf }) => {
                let input: serde_json::Value = if json_buf.trim().is_empty() {
                    serde_json::Value::Object(Default::default())
                } else {
                    serde_json::from_str(&json_buf).unwrap_or_else(|e| {
                        tracing::debug!("conversation: tool input JSON parse 失敗 {name}: {e}");
                        serde_json::Value::Object(Default::default())
                    })
                };
                // TodoWrite は plan ウィジェット用に Plan へ導出。
                if name == "TodoWrite"
                    && let Some(plan) = plan_from_todowrite(&input)
                {
                    return vec![plan];
                }
                vec![ConversationEvent::ToolCall { id, name, input }]
            }
            _ => Vec::new(),
        }
    }

    fn on_result(&mut self, res: RawResult) -> ConversationEvent {
        // turn 終端 — 次 turn のために delta 観測フラグを戻す（[`Self::saw_text_delta`]）。
        self.saw_text_delta = false;
        if res.is_error {
            ConversationEvent::Error {
                message: res
                    .result
                    .unwrap_or_else(|| "engine turn error".to_string()),
            }
        } else {
            ConversationEvent::TurnCompleted {
                session_id: res.session_id,
                cost_usd: res.total_cost_usd,
                context_tokens: self.last_context_tokens,
                context_window: self.context_window(&res.model_usage),
            }
        }
    }

    /// `result.modelUsage` から本 session の context window（ゲージの分母）を選ぶ。
    ///
    /// 複数 entry になるのは subagent が別 model を使った turn。init の model id と
    /// 前方一致する entry を優先し（init は `claude-haiku-4-5`、key は
    /// `claude-haiku-4-5-20251001` のように長短どちらもあり得るため双方向で見る）、
    /// 突き合わせられなければ context 占有が最大の entry = main loop とみなして倒す。
    fn context_window(&self, usage: &HashMap<String, RawModelUsage>) -> Option<u64> {
        let pick = self
            .session_model
            .as_deref()
            .and_then(|m| {
                usage
                    .iter()
                    .find(|(k, _)| k.starts_with(m) || m.starts_with(k.as_str()))
            })
            .or_else(|| {
                usage
                    .iter()
                    .max_by_key(|(_, v)| v.input_tokens + v.cache_read_input_tokens)
            });
        pick.and_then(|(_, v)| v.context_window)
    }
}

// =============================================================================
// user message（tool_result）の変換（状態不要 = 自由関数）
// =============================================================================

/// subagent の assistant スナップショットから発話を取り出す。
///
/// 親と違い delta が来ないので、このスナップショットが唯一の担い手（module doc の「本文は捨てる」
/// は**親の行に限った話**）。`usage` も親の context ゲージではないので退避しない。
/// assistant snapshot の content を本文 event に写す（delta 未観測 turn 専用 — [`ClaudeTranslator::saw_text_delta`]）。
///
/// slash command 等の synthetic turn は delta を出さないため、この snapshot が本文の唯一の
/// 一次情報になる。thinking block は gui で暗号化復元不可の前例（doc 39）と揃えて出さず、
/// text だけを [`ConversationEvent::MessageChunk`] に写す（chatview は chunk を assistant バブルに畳む）。
fn snapshot_text_events(content: Vec<RawAssistantContent>) -> Vec<ConversationEvent> {
    content
        .into_iter()
        .filter_map(|block| match block {
            RawAssistantContent::Text { text } if !text.is_empty() => {
                Some(ConversationEvent::MessageChunk { text })
            }
            _ => None,
        })
        .collect()
}

fn subagent_assistant_events(parent: &str, message: RawAssistantMessage) -> Vec<ConversationEvent> {
    message
        .content
        .into_iter()
        .filter_map(|block| {
            let (role, text) = match block {
                RawAssistantContent::Text { text } => (SubagentRole::Text, text),
                RawAssistantContent::Thinking { thinking } => (SubagentRole::Thinking, thinking),
                RawAssistantContent::Other => return None,
            };
            (!text.is_empty()).then(|| ConversationEvent::SubagentMessage {
                parent_tool_use_id: parent.to_string(),
                role,
                text,
            })
        })
        .collect()
}

/// subagent の user 行 = 与えられた指示。
///
/// ⚠️ ここに来る `tool_result` は **subagent 自身が回した tool** のもので、親の tool 列には
/// 結び先が無い。 親の [`on_user`] に流すと孤児 `ToolCallUpdate` を撃つので、text 以外は捨てる。
fn subagent_user_events(parent: &str, message: RawUserMessage) -> Vec<ConversationEvent> {
    message
        .content
        .into_iter()
        .filter_map(|block| match block {
            RawUserContent::Text { text } if !text.is_empty() => {
                Some(ConversationEvent::SubagentMessage {
                    parent_tool_use_id: parent.to_string(),
                    role: SubagentRole::Prompt,
                    text,
                })
            }
            _ => None,
        })
        .collect()
}

fn on_user(message: RawUserMessage) -> Vec<ConversationEvent> {
    message
        .content
        .into_iter()
        .filter_map(|block| match block {
            RawUserContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(ConversationEvent::ToolCallUpdate {
                tool_use_id,
                content: tool_result_text(&content),
                is_error,
            }),
            // 親の user 行に text block は来ない（実測: tool_result のみ）が、来ても捨てる。
            // 従来 text は `Other` に吸われて捨てられていたので、挙動を変えないのが正。
            RawUserContent::Text { .. } | RawUserContent::Other => None,
        })
        .collect()
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
pub(super) fn plan_from_todowrite(input: &serde_json::Value) -> Option<ConversationEvent> {
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
    Some(ConversationEvent::Plan { entries })
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
        /// 非 None = subagent 由来（`--forward-subagent-text` 有効時のみ現れる）。
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    /// assistant 全文スナップショット。 中身は delta の累積なので描画しないが 2 つの役割を持つ:
    /// (1) **transcript に 1 行 flush された合図**として commit 境界に使う（module doc 参照）、
    /// (2) `message.usage` は context ゲージの分子（usage はこの行にしか載らない一次情報）。
    /// `message` 欠落の assistant 行でも commit 境界は立てたいので `serde(default)`。
    Assistant {
        #[serde(default)]
        message: RawAssistantMessage,
        /// 非 None = subagent 由来。 この行が subagent 発話の**唯一の**担い手になる
        /// （実測: parent 付きの `stream_event` は存在しない = delta で来ない）。
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    Result(RawResult),
    /// rate_limit_event / 未知 type を全て吸収。
    #[serde(other)]
    Other,
}

/// assistant snapshot 行の `message`（usage 以外は見ない）。
/// `Default` は `RawLine::Assistant` の `#[serde(default)]`（message 欠落許容）用。
#[derive(Debug, Default, Deserialize)]
struct RawAssistantMessage {
    #[serde(default)]
    usage: Option<RawUsage>,
    /// subagent 行でのみ読む（親の本文は delta 経由なのでこの field は使わない）。
    #[serde(default)]
    content: Vec<RawAssistantContent>,
}

/// assistant snapshot の content block。 subagent 発話の取り出しにのみ使う。
/// thinking の本文 field は `text` ではなく `thinking`（実測）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawAssistantContent {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Other,
}

/// API usage block のうち context 計算に要る 3 field（現在の prompt 占有量）。
#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
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
    /// subagent 行でのみ現れる（親の user 行は tool_result のみ）。
    Text {
        #[serde(default)]
        text: String,
    },
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
    /// ⚠️ result 行では camelCase（init の `permissionMode` と同じく混在スキーマ）。
    #[serde(default, rename = "modelUsage")]
    model_usage: HashMap<String, RawModelUsage>,
}

/// `result.modelUsage` の 1 entry（context ゲージの分母 + 主 model 判定に要る field のみ）。
#[derive(Debug, Deserialize)]
struct RawModelUsage {
    #[serde(default, rename = "inputTokens")]
    input_tokens: u64,
    #[serde(default, rename = "cacheReadInputTokens")]
    cache_read_input_tokens: u64,
    #[serde(default, rename = "contextWindow")]
    context_window: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// subagent の assistant 行（実測形）から SubagentMessage を取り出す。
    ///
    /// 実測 2026-07-17 (claude 2.1.212): parent 付き行は top-level に `parent_tool_use_id` を持ち、
    /// thinking の本文 field は `text` ではなく `thinking`。
    #[test]
    fn subagent_assistant_becomes_subagent_message() {
        let mut t = ClaudeTranslator::new();
        let think = r#"{"type":"assistant","parent_tool_use_id":"toolu_1","subagent_type":"general-purpose","message":{"role":"assistant","content":[{"type":"thinking","thinking":"掛け算する","signature":"x"}]}}"#;
        let text = r#"{"type":"assistant","parent_tool_use_id":"toolu_1","message":{"role":"assistant","content":[{"type":"text","text":"42"}]}}"#;
        assert_eq!(
            t.ingest(think).events,
            vec![ConversationEvent::SubagentMessage {
                parent_tool_use_id: "toolu_1".into(),
                role: SubagentRole::Thinking,
                text: "掛け算する".into(),
            }]
        );
        assert_eq!(
            t.ingest(text).events,
            vec![ConversationEvent::SubagentMessage {
                parent_tool_use_id: "toolu_1".into(),
                role: SubagentRole::Text,
                text: "42".into(),
            }]
        );
    }

    /// ★ synthetic turn（slash command）の本文を落とさない。
    ///
    /// 実測 2026-07-20 (claude 2.1.215): `/model opus` 等 CLI 内で完結する turn は
    /// `init → assistant(model:"<synthetic>", 本文入り) → result` と流れ、**delta を 1 つも出さない**。
    /// assistant snapshot を「delta の累積＝重複」として捨てると GUI が完全な無音になる
    /// （gui で slash を打って 30 分待っても何も出ない、の真因）。
    #[test]
    fn synthetic_turn_without_delta_surfaces_snapshot_text() {
        let mut t = ClaudeTranslator::new();
        let synth = r#"{"type":"assistant","message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"Set model to Opus 4.8 for this session only"}]}}"#;
        assert_eq!(
            t.ingest(synth).events,
            vec![ConversationEvent::MessageChunk {
                text: "Set model to Opus 4.8 for this session only".into(),
            }]
        );
    }

    /// ★ 通常 turn では snapshot を拾わない（delta との二重描画を防ぐ）。
    ///
    /// delta で本文が流れた後の assistant snapshot は累積の重複なので、従来どおり破棄する。
    /// turn 終端（result）で観測フラグが戻り、次の synthetic turn は再び拾えることも併せて固める。
    #[test]
    fn streamed_turn_discards_snapshot_then_next_synthetic_turn_recovers() {
        let mut t = ClaudeTranslator::new();
        let delta = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"やあ"}}}"#;
        assert_eq!(
            t.ingest(delta).events,
            vec![ConversationEvent::MessageChunk {
                text: "やあ".into()
            }]
        );
        // delta 済み = snapshot は重複なので捨てる（二重描画しない）。
        let snapshot = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"やあ"}]}}"#;
        assert_eq!(t.ingest(snapshot).events, Vec::new());
        // turn 終端でフラグが戻る。
        let result = r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s1"}"#;
        assert!(matches!(
            t.ingest(result).events.as_slice(),
            [ConversationEvent::TurnCompleted { .. }]
        ));
        // 次 turn が synthetic なら再び snapshot を拾える。
        let synth = r#"{"type":"assistant","message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"/context の結果"}]}}"#;
        assert_eq!(
            t.ingest(synth).events,
            vec![ConversationEvent::MessageChunk {
                text: "/context の結果".into(),
            }]
        );
    }

    /// ★ subagent 行は commit 境界を立てない。
    ///
    /// 立ててしまうと「親の本文が disk に載った」と誤認し、host が in-flight tail を切る位置を
    /// 誤る（= 「応答中の永久居座り」と同じ層の事故）。 親の行だけが transcript の 1 行に対応する。
    #[test]
    fn subagent_line_does_not_commit_transcript() {
        let mut t = ClaudeTranslator::new();
        let sub = r#"{"type":"assistant","parent_tool_use_id":"toolu_1","message":{"role":"assistant","content":[{"type":"text","text":"42"}]}}"#;
        assert!(!t.ingest(sub).commits_transcript);
        // 親の assistant 行は従来どおり commit 境界を立てる（退行していない）。
        let parent = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"本文"}]}}"#;
        assert!(t.ingest(parent).commits_transcript);
    }

    /// subagent へ与えられた指示は Prompt として運ぶ（user 発話にはしない）。
    #[test]
    fn subagent_user_text_becomes_prompt() {
        let mut t = ClaudeTranslator::new();
        let line = r#"{"type":"user","parent_tool_use_id":"toolu_1","task_description":"calc","message":{"role":"user","content":[{"type":"text","text":"6x7 は?"}]}}"#;
        assert_eq!(
            t.ingest(line).events,
            vec![ConversationEvent::SubagentMessage {
                parent_tool_use_id: "toolu_1".into(),
                role: SubagentRole::Prompt,
                text: "6x7 は?".into(),
            }]
        );
    }

    /// ★ subagent 自身が回した tool の結果を、親の ToolCallUpdate にしない。
    ///
    /// 親の item 列には結び先が無いので、流すと孤児 update を撃つ（GUI が warning を出す経路）。
    #[test]
    fn subagent_tool_result_does_not_touch_parent_tools() {
        let mut t = ClaudeTranslator::new();
        let line = r#"{"type":"user","parent_tool_use_id":"toolu_1","message":{"role":"user","content":[{"tool_use_id":"child-tool","type":"tool_result","content":"ok","is_error":false}]}}"#;
        let got = t.ingest(line);
        assert!(
            got.events.is_empty(),
            "子の tool_result は親へ流さない: {:?}",
            got.events
        );
        assert!(!got.commits_transcript);
    }

    /// 親の user(tool_result) は従来どおり ToolCallUpdate になる（退行していない）。
    #[test]
    fn parent_user_tool_result_still_updates() {
        let mut t = ClaudeTranslator::new();
        let line = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-1","type":"tool_result","content":"ok","is_error":false}]}}"#;
        let got = t.ingest(line);
        assert_eq!(got.events.len(), 1);
        assert!(got.commits_transcript);
    }

    /// 実測の init 行から SessionInit を取り出せる。
    #[test]
    fn parses_session_init() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp/x","session_id":"sid-1","model":"claude-haiku-4-5","permissionMode":"acceptEdits","tools":["Bash","Edit"],"mcp_servers":[{"name":"vantage-point","status":"connected"}],"slash_commands":["a","b"]}"#;
        let mut t = ClaudeTranslator::new();
        let evs = t.ingest(line).events;
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            ConversationEvent::SessionInit {
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
        let mut t = ClaudeTranslator::new();
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}}"#);
        let think = t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"考え"}}}"#).events;
        assert_eq!(
            think,
            vec![ConversationEvent::ThoughtChunk {
                text: "考え".into()
            }]
        );

        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}}"#);
        let text = t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"hello"}}}"#).events;
        assert_eq!(
            text,
            vec![ConversationEvent::MessageChunk {
                text: "hello".into()
            }]
        );
    }

    /// tool_use は partial_json を蓄積し、stop で完全 input 付き ToolCall を出す。
    #[test]
    fn assembles_tool_call_from_partial_json() {
        let mut t = ClaudeTranslator::new();
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu-1","name":"Bash","input":{}}}}"#);
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"echo "}}}"#);
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"hi\"}"}}}"#);
        let evs = t
            .ingest(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#)
            .events;
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            ConversationEvent::ToolCall { id, name, input } => {
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
        let mut t = ClaudeTranslator::new();
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu-2","name":"TodoWrite","input":{}}}}"#);
        t.ingest(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"todos\":[{\"content\":\"step1\",\"status\":\"in_progress\",\"activeForm\":\"doing step1\"}]}"}}}"#);
        let evs = t
            .ingest(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#)
            .events;
        assert_eq!(
            evs,
            vec![ConversationEvent::Plan {
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
        let mut t = ClaudeTranslator::new();
        let evs = t.ingest(r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"tu-1","type":"tool_result","content":"PERMISSION_TEST_OK","is_error":false}]}}"#).events;
        assert_eq!(
            evs,
            vec![ConversationEvent::ToolCallUpdate {
                tool_use_id: "tu-1".into(),
                content: "PERMISSION_TEST_OK".into(),
                is_error: false,
            }]
        );
    }

    /// result/success は TurnCompleted。usage を見ていない turn では context は None。
    #[test]
    fn result_becomes_turn_completed() {
        let mut t = ClaudeTranslator::new();
        let evs = t.ingest(r#"{"type":"result","subtype":"success","session_id":"sid-1","is_error":false,"total_cost_usd":0.012}"#).events;
        assert_eq!(
            evs,
            vec![ConversationEvent::TurnCompleted {
                session_id: "sid-1".into(),
                cost_usd: Some(0.012),
                context_tokens: None,
                context_window: None,
            }]
        );
    }

    /// context ゲージ: assistant snapshot の usage（最後の値）が分子、
    /// result の modelUsage.contextWindow が分母として TurnCompleted に載る。
    /// ⚠️ result.usage（turn 合算、cache_read 重複計上）ではなく最終 assistant usage を使う。
    #[test]
    fn turn_completed_carries_context_gauge() {
        let mut t = ClaudeTranslator::new();
        t.ingest(r#"{"type":"assistant","message":{"role":"assistant","content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5,"output_tokens":1}}}"#);
        t.ingest(r#"{"type":"assistant","message":{"role":"assistant","content":[],"usage":{"input_tokens":8,"cache_read_input_tokens":200,"cache_creation_input_tokens":2,"output_tokens":1}}}"#);
        let evs = t.ingest(r#"{"type":"result","subtype":"success","session_id":"sid-1","is_error":false,"total_cost_usd":0.01,"modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":18,"cacheReadInputTokens":300,"contextWindow":200000}}}"#);
        assert_eq!(
            evs.events,
            vec![ConversationEvent::TurnCompleted {
                session_id: "sid-1".into(),
                cost_usd: Some(0.01),
                context_tokens: Some(210), // 最後の assistant: 8 + 200 + 2
                context_window: Some(200000),
            }]
        );
    }

    /// modelUsage が複数 entry（subagent が別 model を使った turn）でも、
    /// init の model と前方一致する entry の contextWindow を選ぶ。
    #[test]
    fn context_window_prefers_session_model_entry() {
        let mut t = ClaudeTranslator::new();
        t.ingest(r#"{"type":"system","subtype":"init","session_id":"s","model":"claude-fable-5"}"#);
        t.ingest(r#"{"type":"assistant","message":{"role":"assistant","content":[],"usage":{"input_tokens":1,"cache_read_input_tokens":1,"cache_creation_input_tokens":1}}}"#);
        // subagent (haiku) の方が usage が大きくても、session model の entry が勝つ。
        let evs = t.ingest(r#"{"type":"result","subtype":"success","session_id":"s","is_error":false,"modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":999,"cacheReadInputTokens":999999,"contextWindow":1000000},"claude-fable-5":{"inputTokens":10,"cacheReadInputTokens":100,"contextWindow":200000}}}"#);
        match &evs.events[..] {
            [ConversationEvent::TurnCompleted { context_window, .. }] => {
                assert_eq!(*context_window, Some(200000));
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    /// ノイズ行（hook / status / rate_limit / delta 済みの assistant スナップショット）は無視。
    #[test]
    fn ignores_noise_lines() {
        let mut t = ClaudeTranslator::new();
        // 本文を delta で供給済みにしておく — この文脈でのみ assistant snapshot は重複＝ノイズ。
        // delta 未観測の snapshot は本文の唯一の担い手なので拾う（synthetic turn、別テストで固める）。
        t.ingest(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"snapshot"}}}"#,
        );
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
        let mut t = ClaudeTranslator::new();
        // 通常 turn の形を作る（本文は delta で供給済み）。この前提でのみ snapshot は重複＝破棄になる
        // — delta 未観測の synthetic turn では拾う（synthetic_turn_without_delta_surfaces_snapshot_text）。
        t.ingest(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"本文"}}}"#,
        );
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
    /// [`super::super::host::ClaudeHost`] の in-flight tail に載せてはならない。
    /// この順序が逆転すると tail が ToolCall を掴んで replay が二重化する。
    #[test]
    fn tool_use_snapshot_commits_before_tool_call_fires() {
        let raw = include_str!("testdata/turn_read_edit.jsonl");
        let mut t = ClaudeTranslator::new();
        let mut commits = 0usize;
        let mut commits_at_first_tool_call = None;

        for line in raw.lines() {
            let out = t.ingest(line);
            if out
                .events
                .iter()
                .any(|e| matches!(e, ConversationEvent::ToolCall { .. }))
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
        let mut t = ClaudeTranslator::new();
        let mut evs = Vec::new();
        for line in raw.lines() {
            evs.extend(t.ingest(line).events);
        }

        let session_inits = evs
            .iter()
            .filter(|e| matches!(e, ConversationEvent::SessionInit { .. }))
            .count();
        let tool_calls: Vec<&str> = evs
            .iter()
            .filter_map(|e| match e {
                ConversationEvent::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let has_thought = evs
            .iter()
            .any(|e| matches!(e, ConversationEvent::ThoughtChunk { .. }));
        let text: String = evs
            .iter()
            .filter_map(|e| match e {
                ConversationEvent::MessageChunk { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let turn_completed = evs
            .iter()
            .filter(|e| matches!(e, ConversationEvent::TurnCompleted { .. }))
            .count();
        let updates = evs
            .iter()
            .filter(|e| matches!(e, ConversationEvent::ToolCallUpdate { .. }))
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

        // context ゲージが実測 turn から取れる（分子 = 最終 assistant usage 8+3932+34463、
        // 分母 = modelUsage.contextWindow）。result.usage の合算 106,945 ではないことが肝。
        let ctx = evs.iter().find_map(|e| match e {
            ConversationEvent::TurnCompleted {
                context_tokens,
                context_window,
                ..
            } => Some((*context_tokens, *context_window)),
            _ => None,
        });
        assert_eq!(ctx, Some((Some(38403), Some(200000))), "context ゲージ");

        // Edit tool_call の完全 input（PR3 diff 合成の素材）が取れている。
        let edit_input = evs.iter().find_map(|e| match e {
            ConversationEvent::ToolCall { name, input, .. } if name == "Edit" => Some(input),
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
