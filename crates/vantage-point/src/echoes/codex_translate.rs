//! codex `--json` JSONL → [`EchoesEvent`] 純翻訳（Act II、codex engine 用）
//!
//! [`super::cursor_translate`] と同じ「状態機械 + golden test」設計。codex exec の stream は
//! **item ライフサイクル型**（`item.started` / `item.completed` が item 全体を運ぶ）で、
//! cursor の token delta 型とは形が違うため翻訳器を分ける。
//!
//! イベント対応（doc 37 §7 の翻訳表、公式 docs の documented shape 準拠）:
//!
//! | codex --json | → EchoesEvent |
//! |---|---|
//! | `thread.started {thread_id}` | `SessionInit`（record-from-init の材料） |
//! | `item.completed` item.type=`agent_message` | `MessageChunk`（全文 1 発） |
//! | `item.completed` item.type=`reasoning` | `ThoughtChunk` |
//! | `item.started` item.type=`command_execution` 等 | `ToolCall` |
//! | `item.completed` item.type=`command_execution` 等 | `ToolCallUpdate`（exit_code≠0 = error） |
//! | `turn.completed {usage}` | `TurnCompleted` |
//! | `turn.failed {error}` / `error` | `Error` |
//!
//! ## 保守的 emit（⚠️ delta 未実測、doc 37 §7 empirical gap #1）
//!
//! `agent_message` / `reasoning` の本文は **`item.completed` でのみ** emit する（started /
//! updated は無視）。codex が incremental delta を流す版でも、completed 1 発なら二重描画は
//! 構造的に起きない（streaming 感は落ちるが正しさ優先）。live 実測で delta 形が確定したら
//! ここだけ緩める。
//!
//! ## item payload は Value で受ける
//!
//! 外側の event type（`thread.started` 等）は docs で確定しているが、item 内の field 名は
//! 版差リスクがある（`type` / `item_type` 等）。item は `serde_json::Value` で受けて helper で
//! 読み、未知 field / 型ズレで行ごと落とさない（fail-open、実測後に型を締める）。

use serde::Deserialize;

use super::event::EchoesEvent;
use super::turn_host::TurnTranslator;

/// 素テキスト buffer の上限（壊れた出力の暴走で無限成長させない防御、cursor 版と同値）。
const PLAIN_TEXT_CAP: usize = 8192;

/// 1 codex turn の stdout stream を通す翻訳器（turn ごとに 1 個、可変状態を持つ）。
#[derive(Debug, Default)]
pub struct CodexTranslator {
    /// `thread.started` で観測した thread_id（TurnCompleted の session_id 補完用）。
    session_id: Option<String>,
    /// assistant/tool 由来の実イベントを出したか（host の self-heal 判定用）。
    produced_content: bool,
    /// 終端（turn.completed / turn.failed / error）を観測したか。
    saw_result: bool,
    /// `item.started` で ToolCall を emit 済みの item id（completed 側の欠け対策）。
    started_items: std::collections::HashSet<String>,
    /// JSON でない行（未ログインエラー等）を溜める buffer。
    plain_text: String,
}

impl CodexTranslator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn translate(&mut self, raw: RawLine) -> Vec<EchoesEvent> {
        match raw {
            RawLine::ThreadStarted { thread_id } => {
                let Some(id) = thread_id.filter(|s| !s.is_empty()) else {
                    return Vec::new();
                };
                self.session_id = Some(id.clone());
                vec![EchoesEvent::SessionInit {
                    session_id: id,
                    // codex の thread.started は model / cwd 等を運ばない → None / 空で埋める。
                    model: None,
                    permission_mode: None,
                    cwd: None,
                    tools: Vec::new(),
                    mcp_servers: Vec::new(),
                    slash_commands: Vec::new(),
                }]
            }
            RawLine::ItemStarted { item } => self.on_item(item, ItemPhase::Started),
            RawLine::ItemCompleted { item } => self.on_item(item, ItemPhase::Completed),
            // updated は中間形（plan 進捗等）。v1 は describない — completed で確定形だけ描く
            //（agent_message の incremental 版が存在した場合の二重描画をここで構造的に防ぐ）。
            RawLine::ItemUpdated => Vec::new(),
            RawLine::TurnCompleted => {
                self.saw_result = true;
                vec![EchoesEvent::TurnCompleted {
                    session_id: self.session_id.clone().unwrap_or_default(),
                    // codex usage は token 数を運ぶが context window 総量が無い → ゲージは出さない
                    //（cost も同様）。live 実測後に埋める余地あり。
                    cost_usd: None,
                    context_tokens: None,
                    context_window: None,
                }]
            }
            RawLine::TurnFailed { error } => {
                self.saw_result = true;
                vec![EchoesEvent::Error {
                    message: error
                        .and_then(|e| e.message)
                        .unwrap_or_else(|| "codex turn failed".to_string()),
                }]
            }
            RawLine::Error { message } => {
                // 単独 error イベント。この後 turn.completed が来ない版もあり得るため終端扱いに
                // する（host の「応答なく終了」Error との二重化を防ぐ）。
                self.saw_result = true;
                vec![EchoesEvent::Error {
                    message: message.unwrap_or_else(|| "codex error".to_string()),
                }]
            }
            RawLine::TurnStarted | RawLine::Other => Vec::new(),
        }
    }

    /// item イベントの翻訳。本文系（agent_message / reasoning）は completed のみ、
    /// tool 系は started=ToolCall / completed=ToolCallUpdate。
    fn on_item(&mut self, item: serde_json::Value, phase: ItemPhase) -> Vec<EchoesEvent> {
        let id = item_str(&item, &["id"]).unwrap_or_default();
        let item_type = item_str(&item, &["type", "item_type"]).unwrap_or_default();
        match (item_type.as_str(), phase) {
            // 本文系: completed の全文だけを 1 発 emit（module doc の保守的 emit）。
            ("agent_message", ItemPhase::Completed) => {
                let Some(text) = item_str(&item, &["text"]).filter(|t| !t.is_empty()) else {
                    return Vec::new();
                };
                self.produced_content = true;
                vec![EchoesEvent::MessageChunk { text }]
            }
            ("reasoning", ItemPhase::Completed) => {
                let Some(text) = item_str(&item, &["text"]).filter(|t| !t.is_empty()) else {
                    return Vec::new();
                };
                self.produced_content = true;
                vec![EchoesEvent::ThoughtChunk { text }]
            }
            ("agent_message" | "reasoning", ItemPhase::Started) => Vec::new(),
            // tool 系（command_execution / file_change / mcp_tool_call / web_search / …）。
            (_, ItemPhase::Started) => {
                self.produced_content = true;
                self.started_items.insert(id.clone());
                vec![EchoesEvent::ToolCall {
                    id,
                    name: tool_name(&item_type),
                    input: tool_input(&item),
                }]
            }
            (_, ItemPhase::Completed) => {
                self.produced_content = true;
                let mut out = Vec::new();
                // started を観測していない item の completed（欠け / 将来の一発完結 item）は
                // ToolCall を先に補完して行が浮かないようにする。
                if !self.started_items.remove(&id) {
                    out.push(EchoesEvent::ToolCall {
                        id: id.clone(),
                        name: tool_name(&item_type),
                        input: tool_input(&item),
                    });
                }
                let exit_code = item.get("exit_code").and_then(|v| v.as_i64());
                let content = item_str(&item, &["aggregated_output", "output"])
                    .unwrap_or_else(|| tool_input(&item).to_string());
                out.push(EchoesEvent::ToolCallUpdate {
                    tool_use_id: id,
                    content,
                    is_error: exit_code.is_some_and(|c| c != 0),
                });
                out
            }
        }
    }
}

impl TurnTranslator for CodexTranslator {
    fn ingest(&mut self, line: &str) -> Vec<EchoesEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        match serde_json::from_str::<RawLine>(trimmed) {
            Ok(raw) => self.translate(raw),
            Err(_) => {
                // JSON でない行（未ログイン案内等）はイベントにせず buffer へ。
                // plain_text_tail で host が Error message に使う。
                if self.plain_text.len() < PLAIN_TEXT_CAP {
                    if !self.plain_text.is_empty() {
                        self.plain_text.push('\n');
                    }
                    self.plain_text.push_str(trimmed);
                }
                Vec::new()
            }
        }
    }

    fn saw_result(&self) -> bool {
        self.saw_result
    }

    fn produced_content(&self) -> bool {
        self.produced_content
    }

    fn plain_text_tail(&self) -> String {
        self.plain_text.clone()
    }
}

/// item イベントの相（started / completed）。
#[derive(Clone, Copy)]
enum ItemPhase {
    Started,
    Completed,
}

/// item Value から最初に見つかった string field を取る（field 名の版差を alias 群で吸収）。
fn item_str(item: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| item.get(k).and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// item type から GUI 表示用の tool 名を導く（既知は短名、未知は type そのまま）。
fn tool_name(item_type: &str) -> String {
    match item_type {
        "command_execution" => "shell".to_string(),
        "mcp_tool_call" => "mcp".to_string(),
        other => other.to_string(),
    }
}

/// ToolCall の input 表示: 既知 field（command）を優先し、無ければ item 全体を渡す。
fn tool_input(item: &serde_json::Value) -> serde_json::Value {
    if let Some(cmd) = item.get("command") {
        return serde_json::json!({ "command": cmd });
    }
    item.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 行列を通して全イベントを平坦化する helper。
    fn run(lines: &[&str]) -> (Vec<EchoesEvent>, CodexTranslator) {
        let mut t = CodexTranslator::new();
        let mut evs = Vec::new();
        for line in lines {
            evs.extend(t.ingest(line));
        }
        (evs, t)
    }

    const THREAD: &str =
        r#"{"type":"thread.started","thread_id":"0196f9a2-1234-4abc-9def-0123456789ab"}"#;
    const TURN_START: &str = r#"{"type":"turn.started"}"#;
    const CMD_STARTED: &str = r#"{"type":"item.started","item":{"id":"item_0","type":"command_execution","command":"bash -lc ls","status":"in_progress"}}"#;
    const CMD_DONE: &str = r#"{"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"bash -lc ls","aggregated_output":"Cargo.toml\nsrc","exit_code":0,"status":"completed"}}"#;
    const REASONING: &str =
        r#"{"type":"item.completed","item":{"id":"item_1","type":"reasoning","text":"考える"}}"#;
    const MESSAGE: &str = r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"完了しました"}}"#;
    const TURN_DONE: &str = r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":20}}"#;

    /// thread → command → reasoning → message → turn.completed の一連が期待列になる。
    #[test]
    fn full_turn_maps_to_expected_events() {
        let (evs, t) = run(&[
            THREAD,
            TURN_START,
            CMD_STARTED,
            CMD_DONE,
            REASONING,
            MESSAGE,
            TURN_DONE,
        ]);
        assert_eq!(
            evs,
            vec![
                EchoesEvent::SessionInit {
                    session_id: "0196f9a2-1234-4abc-9def-0123456789ab".into(),
                    model: None,
                    permission_mode: None,
                    cwd: None,
                    tools: Vec::new(),
                    mcp_servers: Vec::new(),
                    slash_commands: Vec::new(),
                },
                EchoesEvent::ToolCall {
                    id: "item_0".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"command":"bash -lc ls"}),
                },
                EchoesEvent::ToolCallUpdate {
                    tool_use_id: "item_0".into(),
                    content: "Cargo.toml\nsrc".into(),
                    is_error: false,
                },
                EchoesEvent::ThoughtChunk {
                    text: "考える".into()
                },
                EchoesEvent::MessageChunk {
                    text: "完了しました".into()
                },
                EchoesEvent::TurnCompleted {
                    session_id: "0196f9a2-1234-4abc-9def-0123456789ab".into(),
                    cost_usd: None,
                    context_tokens: None,
                    context_window: None,
                },
            ]
        );
        assert!(t.saw_result());
        assert!(t.produced_content());
    }

    /// exit_code≠0 の command は is_error=true。
    #[test]
    fn failed_command_is_error() {
        let done = r#"{"type":"item.completed","item":{"id":"i","type":"command_execution","command":"false","aggregated_output":"","exit_code":1}}"#;
        let (evs, _) = run(&[CMD_STARTED_FOR_I, done]);
        assert_eq!(
            evs.last(),
            Some(&EchoesEvent::ToolCallUpdate {
                tool_use_id: "i".into(),
                content: "".into(),
                is_error: true,
            })
        );
    }
    const CMD_STARTED_FOR_I: &str =
        r#"{"type":"item.started","item":{"id":"i","type":"command_execution","command":"false"}}"#;

    /// started を観測していない completed は ToolCall を補完してから Update を出す。
    #[test]
    fn completed_without_started_backfills_toolcall() {
        let done = r#"{"type":"item.completed","item":{"id":"w","type":"web_search","query":"rust","status":"completed"}}"#;
        let (evs, _) = run(&[done]);
        assert_eq!(evs.len(), 2, "ToolCall 補完 + Update");
        assert!(
            matches!(&evs[0], EchoesEvent::ToolCall { id, name, .. } if id == "w" && name == "web_search")
        );
        assert!(
            matches!(&evs[1], EchoesEvent::ToolCallUpdate { tool_use_id, is_error, .. } if tool_use_id == "w" && !is_error)
        );
    }

    /// turn.failed は Error に化け、終端扱いになる。
    #[test]
    fn turn_failed_becomes_error() {
        let line = r#"{"type":"turn.failed","error":{"message":"boom"}}"#;
        let (evs, t) = run(&[line]);
        assert_eq!(
            evs,
            vec![EchoesEvent::Error {
                message: "boom".into()
            }]
        );
        assert!(t.saw_result());
    }

    /// 本文系は started では emit しない（completed 全文の 1 発のみ = 二重描画の構造的防止）。
    #[test]
    fn agent_message_only_emits_on_completed() {
        let started =
            r#"{"type":"item.started","item":{"id":"m","type":"agent_message","text":"partial"}}"#;
        let updated = r#"{"type":"item.updated","item":{"id":"m","type":"agent_message","text":"partial more"}}"#;
        let completed = r#"{"type":"item.completed","item":{"id":"m","type":"agent_message","text":"full text"}}"#;
        let (evs, _) = run(&[started, updated, completed]);
        assert_eq!(
            evs,
            vec![EchoesEvent::MessageChunk {
                text: "full text".into()
            }]
        );
    }

    /// 素テキスト行（未ログイン案内等）はイベントにならず plain_text_tail に溜まる。
    #[test]
    fn plain_text_lines_go_to_tail_not_events() {
        let login = "Not logged in. Run `codex login` to authenticate.";
        let (evs, t) = run(&[login]);
        assert!(evs.is_empty());
        assert_eq!(t.plain_text_tail(), login);
        assert!(!t.saw_result(), "終端を見ていない = 応答なし終了");
        assert!(!t.produced_content(), "中身ゼロ = self-heal 候補");
    }

    /// 未知 event type は捨てる（将来の追加 type で落ちない）。
    #[test]
    fn unknown_event_types_are_dropped() {
        let unknown = r#"{"type":"some.future_event","foo":1}"#;
        let (evs, _) = run(&[unknown]);
        assert!(evs.is_empty());
    }

    /// item の type field 版差（item_type）も alias で読める。
    #[test]
    fn item_type_alias_is_accepted() {
        let alt = r#"{"type":"item.completed","item":{"id":"a","item_type":"agent_message","text":"hi"}}"#;
        let (evs, _) = run(&[alt]);
        assert_eq!(evs, vec![EchoesEvent::MessageChunk { text: "hi".into() }]);
    }
}

// =============================================================================
// 生 codex --json スキーマ（外側 type は docs 確定、item は Value で fail-open）
// =============================================================================

/// codex --json の 1 行（外側 tag = "type"）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawLine {
    #[serde(rename = "thread.started")]
    ThreadStarted {
        #[serde(default)]
        thread_id: Option<String>,
    },
    #[serde(rename = "turn.started")]
    TurnStarted,
    #[serde(rename = "turn.completed")]
    TurnCompleted,
    #[serde(rename = "turn.failed")]
    TurnFailed {
        #[serde(default)]
        error: Option<RawError>,
    },
    #[serde(rename = "item.started")]
    ItemStarted { item: serde_json::Value },
    #[serde(rename = "item.updated")]
    ItemUpdated,
    #[serde(rename = "item.completed")]
    ItemCompleted { item: serde_json::Value },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        message: Option<String>,
    },
    /// 未知 type を全て吸収。
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawError {
    #[serde(default)]
    message: Option<String>,
}
