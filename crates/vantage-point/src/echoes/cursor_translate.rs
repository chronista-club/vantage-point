//! cursor-agent stream-json → [`EchoesEvent`] 純翻訳（Act II、cursor engine 用）
//!
//! [`super::translate`]（claude 版）と同じ「生スキーマ struct + 状態機械 + golden test」設計を
//! 借りるが、cursor-agent は claude と CLI surface が違う（`--input-format` 不在 = ターンごと
//! spawn、doc `cursor-engine.md` §Act II）。イベント形も別なので翻訳器を分ける。
//!
//! 変換は engine 非依存の GUI 語彙 [`EchoesEvent`] へ落とす — GUI（chatview）と topic 配線は
//! claude と共通のまま、翻訳器を 1 個足すだけで cursor が乗る（多 engine 方針、`event` module doc）。
//!
//! ## `--stream-partial-output` の delta 判定（実機確認済み）
//!
//! `assistant` イベントは 3 種類が同じ形で来る:
//! - `timestamp_ms` **あり** & `model_call_id` **無し** = 新規テキスト（delta）→ [`EchoesEvent::MessageChunk`]
//! - `timestamp_ms` **無し** = バッファ済み全文の flush（重複、捨てる）
//! - `model_call_id` **あり** = tool call 直前の flush（重複、捨てる）
//!
//! delta 以外を素通しすると全文が二重・三重に描画されるため、この規則で新規分だけを拾う。
//!
//! ## result 安全網
//!
//! `--stream-partial-output` が効かない build では delta が一切来ず、終端 `result` に全文だけが
//! 載る。その turn で MessageChunk を 1 つも出していなければ、result 全文を MessageChunk として
//! 先に emit してから [`EchoesEvent::TurnCompleted`] を出す（空 chat 化を防ぐ）。
//!
//! ## 素テキスト行（未ログイン等）
//!
//! JSON parse に失敗した行（`Error: Authentication required...` 等）はイベントにせず内部 buffer に
//! 溜め、[`CursorTranslator::plain_text_tail`] で host が取り出す（Error message の材料）。

use serde::Deserialize;

use super::event::EchoesEvent;

/// 素テキスト buffer の上限（壊れた cursor の暴走出力で無限成長させない防御）。
const PLAIN_TEXT_CAP: usize = 8192;

/// 1 cursor turn の stdout stream を通す翻訳器（turn ごとに 1 個、可変状態を持つ）。
#[derive(Debug, Default)]
pub(crate) struct CursorTranslator {
    /// `system/init` で観測した session_id（= chatId）。result に session_id が無い版の補完用。
    session_id: Option<String>,
    /// この turn で MessageChunk を 1 つでも出したか（result 安全網の発火条件）。
    emitted_text: bool,
    /// assistant/tool 由来の実イベント（MessageChunk / ToolCall / ToolCallUpdate）を出したか。
    /// host の self-heal（resume 失敗判定）が「中身ゼロ」を検知するのに使う。
    produced_content: bool,
    /// 終端 `result` を観測したか（host が「正常終端したか」を判定する材料）。
    saw_result: bool,
    /// JSON でない行（未ログインエラー等）を溜める buffer。
    plain_text: String,
}

impl CursorTranslator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// cursor stream-json の 1 行を食わせ、0 個以上の [`EchoesEvent`] を得る。
    ///
    /// JSON でない行・未知 type は握り潰す（未知 type は空、素テキストは buffer へ退避）。
    pub(crate) fn ingest(&mut self, line: &str) -> Vec<EchoesEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        match serde_json::from_str::<RawLine>(trimmed) {
            Ok(raw) => self.translate(raw),
            Err(_) => {
                // JSON でない行（未ログイン時の `Error: Authentication required...` 等）は
                // イベントにせず buffer へ。plain_text_tail で host が Error message に使う。
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

    /// 正常終端（`result`）を観測したか。false = プロセスが応答なく落ちた（未ログイン等）。
    pub(crate) fn saw_result(&self) -> bool {
        self.saw_result
    }

    /// assistant/tool の実イベントを 1 つでも出したか（host の self-heal 判定用）。
    pub(crate) fn produced_content(&self) -> bool {
        self.produced_content
    }

    /// JSON でなかった行の蓄積（未ログインエラー本文など）。
    pub(crate) fn plain_text_tail(&self) -> String {
        self.plain_text.clone()
    }

    fn translate(&mut self, raw: RawLine) -> Vec<EchoesEvent> {
        match raw {
            RawLine::System(sys) => self.on_system(sys),
            // 送った prompt のエコー。GUI は submit 時に optimistic user bubble を出す（claude 経路
            // も送信 prompt をエコーしない）ので捨てる。
            RawLine::User => Vec::new(),
            RawLine::Assistant(a) => self.on_assistant(a),
            RawLine::ToolCall(tc) => self.on_tool_call(tc),
            RawLine::Result(r) => self.on_result(r),
            RawLine::Other => Vec::new(),
        }
    }

    fn on_system(&mut self, sys: RawSystem) -> Vec<EchoesEvent> {
        if sys.subtype.as_deref() != Some("init") {
            return Vec::new();
        }
        let Some(session_id) = sys.session_id else {
            return Vec::new();
        };
        self.session_id = Some(session_id.clone());
        vec![EchoesEvent::SessionInit {
            session_id,
            model: sys.model,
            permission_mode: sys.permission_mode,
            cwd: sys.cwd,
            // cursor init は tools / mcp / slash_commands を stream に載せない → 空で埋める。
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            slash_commands: Vec::new(),
        }]
    }

    fn on_assistant(&mut self, a: RawAssistant) -> Vec<EchoesEvent> {
        // delta 判定: timestamp_ms あり & model_call_id 無し のみが新規テキスト。
        let is_delta = a.timestamp_ms.is_some() && a.model_call_id.is_none();
        if !is_delta {
            return Vec::new();
        }
        let text: String = a
            .message
            .content
            .iter()
            .filter_map(|b| b.text.as_deref())
            .collect();
        if text.is_empty() {
            return Vec::new();
        }
        self.emitted_text = true;
        self.produced_content = true;
        vec![EchoesEvent::MessageChunk { text }]
    }

    fn on_tool_call(&mut self, tc: RawToolCall) -> Vec<EchoesEvent> {
        let call_id = tc.call_id.unwrap_or_default();
        // tool_call オブジェクトの唯一の key（例 "readToolCall"）が tool の種別、値が中身。
        let Some(obj) = tc.tool_call else {
            return Vec::new();
        };
        let Some((key, inner)) = obj.as_object().and_then(|m| m.iter().next()) else {
            return Vec::new();
        };
        match tc.subtype.as_deref() {
            Some("started") => {
                let name = tool_name_from_key(key);
                let input = inner
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                self.produced_content = true;
                vec![EchoesEvent::ToolCall {
                    id: call_id,
                    name,
                    input,
                }]
            }
            Some("completed") => {
                let result = inner.get("result");
                // result に "success" key が無ければ失敗扱い（cursor の tool 結果は success/error の
                // どちらかを内包する）。
                let is_error = !result
                    .and_then(|r| r.as_object())
                    .map(|o| o.contains_key("success"))
                    .unwrap_or(false);
                let content = result.map(|r| r.to_string()).unwrap_or_default();
                self.produced_content = true;
                vec![EchoesEvent::ToolCallUpdate {
                    tool_use_id: call_id,
                    content,
                    is_error,
                }]
            }
            // started / completed 以外の subtype（progress 等）は今は描かない。
            _ => Vec::new(),
        }
    }

    fn on_result(&mut self, r: RawResult) -> Vec<EchoesEvent> {
        self.saw_result = true;
        if r.is_error {
            return vec![EchoesEvent::Error {
                message: r
                    .result
                    .unwrap_or_else(|| "cursor-agent turn error".to_string()),
            }];
        }
        let session_id = r
            .session_id
            .or_else(|| self.session_id.clone())
            .unwrap_or_default();
        let mut out = Vec::new();
        // 安全網: この turn で MessageChunk を 1 つも出していなければ、result 全文を先に MessageChunk
        // として emit する（`--stream-partial-output` が効かない build で空 chat 化を防ぐ）。
        if !self.emitted_text
            && let Some(full) = r.result.filter(|s| !s.is_empty())
        {
            self.emitted_text = true;
            self.produced_content = true;
            out.push(EchoesEvent::MessageChunk { text: full });
        }
        out.push(EchoesEvent::TurnCompleted {
            session_id,
            // cursor stream は cost / context ゲージを載せない → None（GUI はゲージを出さないだけ）。
            cost_usd: None,
            context_tokens: None,
            context_window: None,
        });
        out
    }
}

/// tool_call の key（例 "readToolCall"）から短い tool 名（"read"）を導出する。
///
/// 末尾 "ToolCall" を落として得られる短名。落ちない（末尾が違う / 落とすと空）なら key そのまま。
fn tool_name_from_key(key: &str) -> String {
    key.strip_suffix("ToolCall")
        .filter(|s| !s.is_empty())
        .unwrap_or(key)
        .to_string()
}

// =============================================================================
// 生 cursor stream-json スキーマ（doc `cursor-engine.md` §Act II の実測形）
// =============================================================================

/// cursor stream-json の 1 行（外側 tag = "type"）。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawLine {
    System(RawSystem),
    /// 送った prompt のエコー（中身は見ない）。
    User,
    Assistant(RawAssistant),
    ToolCall(RawToolCall),
    Result(RawResult),
    /// 未知 type を全て吸収。
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
    /// cursor init は camelCase（`permissionMode`）。
    #[serde(default, rename = "permissionMode")]
    permission_mode: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// `assistant` 行。delta 判定のため top-level の `timestamp_ms` / `model_call_id` の**有無**を見る。
#[derive(Debug, Deserialize)]
struct RawAssistant {
    #[serde(default)]
    message: RawAssistantMessage,
    /// 有無だけを見る（delta = 有り）。`Option` + `default` で absent/null → None、値 → Some。
    #[serde(default)]
    timestamp_ms: Option<serde_json::Value>,
    /// 有無だけを見る（tool call 直前 flush = 有り → 捨てる）。
    #[serde(default)]
    model_call_id: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAssistantMessage {
    #[serde(default)]
    content: Vec<RawContentBlock>,
}

/// content block（text 以外は text=None で無視）。
#[derive(Debug, Deserialize)]
struct RawContentBlock {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawToolCall {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    call_id: Option<String>,
    /// `{"readToolCall": {"args":{…}, "result":{…}?}}` の 1 key オブジェクト。
    #[serde(default)]
    tool_call: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 行列を通して全イベントを平坦化する helper。
    fn run(lines: &[&str]) -> (Vec<EchoesEvent>, CursorTranslator) {
        let mut t = CursorTranslator::new();
        let mut evs = Vec::new();
        for line in lines {
            evs.extend(t.ingest(line));
        }
        (evs, t)
    }

    const INIT: &str = r#"{"type":"system","subtype":"init","apiKeySource":"login","cwd":"/tmp/x","session_id":"chat_abc-1","model":"Claude Sonnet","permissionMode":"default"}"#;
    // delta（timestamp_ms あり / model_call_id 無し）= 新規テキスト。
    const DELTA_HELLO: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello "}]},"session_id":"chat_abc-1","timestamp_ms":1700000000000}"#;
    const DELTA_WORLD: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"world"}]},"session_id":"chat_abc-1","timestamp_ms":1700000000001}"#;
    const TOOL_STARTED: &str = r#"{"type":"tool_call","subtype":"started","call_id":"call-1","tool_call":{"readToolCall":{"args":{"path":"foo.rs"}}},"session_id":"chat_abc-1"}"#;
    const TOOL_COMPLETED: &str = r#"{"type":"tool_call","subtype":"completed","call_id":"call-1","tool_call":{"readToolCall":{"args":{"path":"foo.rs"},"result":{"success":{"content":"data"}}}},"session_id":"chat_abc-1"}"#;
    const RESULT_OK: &str = r#"{"type":"result","subtype":"success","duration_ms":1200,"duration_api_ms":900,"is_error":false,"result":"All done","session_id":"chat_abc-1","request_id":"req-1"}"#;

    /// init → delta×2 → tool started/completed → result の一連が期待 EchoesEvent 列になる。
    /// delta で本文を出しているので result の安全網は発火しない。
    #[test]
    fn full_turn_maps_to_expected_events() {
        let (evs, t) = run(&[
            INIT,
            DELTA_HELLO,
            DELTA_WORLD,
            TOOL_STARTED,
            TOOL_COMPLETED,
            RESULT_OK,
        ]);
        assert_eq!(
            evs,
            vec![
                EchoesEvent::SessionInit {
                    session_id: "chat_abc-1".into(),
                    model: Some("Claude Sonnet".into()),
                    permission_mode: Some("default".into()),
                    cwd: Some("/tmp/x".into()),
                    tools: Vec::new(),
                    mcp_servers: Vec::new(),
                    slash_commands: Vec::new(),
                },
                EchoesEvent::MessageChunk {
                    text: "Hello ".into()
                },
                EchoesEvent::MessageChunk {
                    text: "world".into()
                },
                EchoesEvent::ToolCall {
                    id: "call-1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path":"foo.rs"}),
                },
                EchoesEvent::ToolCallUpdate {
                    tool_use_id: "call-1".into(),
                    content: serde_json::json!({"success":{"content":"data"}}).to_string(),
                    is_error: false,
                },
                EchoesEvent::TurnCompleted {
                    session_id: "chat_abc-1".into(),
                    cost_usd: None,
                    context_tokens: None,
                    context_window: None,
                },
            ]
        );
        assert!(t.saw_result());
        assert!(t.produced_content());
    }

    /// delta 判定規則: timestamp_ms 無し（バッファ全文 flush）と model_call_id 付き（tool 前 flush）は
    /// どちらも捨てる。
    #[test]
    fn delta_rule_discards_buffered_and_toolcall_flushes() {
        // timestamp_ms 無し = バッファ済み全文の flush。
        let buffered = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello world"}]},"session_id":"s"}"#;
        // model_call_id あり = tool call 直前 flush。
        let tool_flush = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me read"}]},"session_id":"s","timestamp_ms":1700000000002,"model_call_id":"mc-1"}"#;
        let (evs, _) = run(&[buffered, tool_flush]);
        assert!(
            evs.is_empty(),
            "buffered / tool-flush は delta でない = 捨てる"
        );
    }

    /// result 安全網: この turn で delta を 1 つも出していなければ、result 全文が MessageChunk になる。
    #[test]
    fn result_emits_full_text_when_no_delta() {
        let (evs, _) = run(&[INIT, RESULT_OK]);
        assert_eq!(
            evs,
            vec![
                EchoesEvent::SessionInit {
                    session_id: "chat_abc-1".into(),
                    model: Some("Claude Sonnet".into()),
                    permission_mode: Some("default".into()),
                    cwd: Some("/tmp/x".into()),
                    tools: Vec::new(),
                    mcp_servers: Vec::new(),
                    slash_commands: Vec::new(),
                },
                EchoesEvent::MessageChunk {
                    text: "All done".into()
                },
                EchoesEvent::TurnCompleted {
                    session_id: "chat_abc-1".into(),
                    cost_usd: None,
                    context_tokens: None,
                    context_window: None,
                },
            ]
        );
    }

    /// is_error の result は Error に化ける（安全網 MessageChunk は出さない）。
    #[test]
    fn error_result_becomes_error_event() {
        let line = r#"{"type":"result","subtype":"error","is_error":true,"result":"boom","session_id":"s"}"#;
        let (evs, t) = run(&[line]);
        assert_eq!(
            evs,
            vec![EchoesEvent::Error {
                message: "boom".into()
            }]
        );
        assert!(t.saw_result());
    }

    /// 素テキスト行（未ログインエラー等）はイベントにならず plain_text_tail に溜まる。
    #[test]
    fn plain_text_lines_go_to_tail_not_events() {
        let auth = "Error: Authentication required. Please run 'agent login' first, or set CURSOR_API_KEY.";
        let (evs, t) = run(&[auth]);
        assert!(evs.is_empty(), "素テキストはイベントにしない");
        assert_eq!(t.plain_text_tail(), auth);
        assert!(!t.saw_result(), "result を見ていない = 応答なし終了");
        assert!(!t.produced_content(), "中身ゼロ = self-heal 候補");
    }

    /// tool 名導出: 末尾 "ToolCall" を落とす。落ちなければ key そのまま。
    #[test]
    fn tool_name_derivation() {
        assert_eq!(tool_name_from_key("readToolCall"), "read");
        assert_eq!(tool_name_from_key("shellToolCall"), "shell");
        assert_eq!(
            tool_name_from_key("ToolCall"),
            "ToolCall",
            "落とすと空は不採用"
        );
        assert_eq!(tool_name_from_key("weird"), "weird");
    }

    /// completed で result に "success" key が無ければ is_error=true。
    #[test]
    fn tool_completed_without_success_is_error() {
        let line = r#"{"type":"tool_call","subtype":"completed","call_id":"c-2","tool_call":{"shellToolCall":{"args":{},"result":{"error":{"message":"nope"}}}},"session_id":"s"}"#;
        let (evs, _) = run(&[line]);
        assert_eq!(
            evs,
            vec![EchoesEvent::ToolCallUpdate {
                tool_use_id: "c-2".into(),
                content: serde_json::json!({"error":{"message":"nope"}}).to_string(),
                is_error: true,
            }]
        );
    }

    /// user エコー行 / 未知 type は捨てる（空を返す）。
    #[test]
    fn user_echo_and_unknown_are_dropped() {
        let user = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"session_id":"s"}"#;
        let unknown = r#"{"type":"some_future_type","foo":1}"#;
        let (evs, _) = run(&[user, unknown]);
        assert!(evs.is_empty());
    }
}
