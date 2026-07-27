//! transcript replay — claude の session transcript（jsonl）を [`ConversationEvent`] 列に起こす。
//!
//! ## なぜ要るか
//!
//! gui（chat mode）の会話は `ConversationEvent` stream で GUI に届くが、この topic は
//! **非 retained**（`process/conversation/data/{lane}/event`、category=data）で、履歴は vp-app の
//! webview 内 in-memory ring buffer にしか無い。 つまり **app を再起動すると ChatView が空**
//! になる（engine 側は `--resume` で会話を保持しているのに、描く履歴が無い）。
//!
//! terminal（tui）は PtySlot の raw PTY bytes を replay して画面を復元するが、chat lane は
//! PtySlot を持たないため同じ手は使えない。 代わりに **claude が disk に持つ transcript
//! (`~/.claude/repos/*/<session_id>.jsonl`)** を唯一の履歴 SSOT として読み、`ConversationEvent` に
//! 翻訳して attach 時に replay する（= gui 版 replay-on-attach）。
//!
//! ## [`super::claude_translate`] との違い
//!
//! `translate` は **stream-json**（`stream_event` / `content_block_delta` の増分）を食う。
//! transcript jsonl は **完成 message**（`message.content[]` の block 配列）なので構造が違い、
//! 直接は再利用できない。 ただし tool_result の text 化 / TodoWrite→Plan の導出は意味を
//! 揃える必要があるため、`translate` の helper を共有する。
//!
//! ## 冪等性
//!
//! 先頭に [`ConversationEvent::ReplayStart`] を置く。 GUI はこれを見て会話表示をクリアしてから
//! 後続を fold する。 backend は「新規 attach」と「reconnect / demand 再発火」を区別できない
//! ため（terminal replay の clear-prefix と同型の問題）、単純追記だと再接続のたび会話が
//! 二重化する。 reset してから描き直せば、どちらの経路でも同じ最終状態に収束する。
//!
//! ## 生成中（in-flight）の message
//!
//! claude は message を **完了時にしか** transcript へ flush しない。 よって replay が assistant の
//! 生成中に着地すると、transcript だけでは「生成中 message の直前まで」しか復元できない。
//! この欠けは [`super::host::ClaudeHost`] の **in-flight tail** が埋める（未 commit の
//! `MessageChunk` / `ThoughtChunk` を保持し、replay handler が本 module の出力の後ろへ継ぐ）。
//! 「replay = transcript(commit 済み) ++ tail(未 commit)」で現在状態が厳密に再現される。
//!
//! ## 復元できないもの（transcript 側の制約）
//!
//! - **thinking**: transcript の thinking block は `{"thinking":"","signature":"…"}` の形で
//!   本文が空（signature は暗号化ペイロード）。 つまり **思考内容は disk に残っていない**ので
//!   [`ConversationEvent::ThoughtChunk`] は replay できない。 live 経路（stream-json の
//!   `thinking_delta`）でのみ流れる。 例外として、 **生成中の thinking** は in-flight tail 側に
//!   居るので replay できる。
//! - **image**: ChatView に描く語彙が無いため捨てる（MVP）。
//!
//! data / calculations / actions:
//! - calculations: [`events_from_lines`] / [`drop_orphan_updates`]（純関数、テスト対象）
//! - actions: [`replay_events`]（disk read → calculations に委譲）

use serde_json::Value;

use super::claude_translate::{plan_from_todowrite, tool_result_text};
use super::event::ConversationEvent;

/// replay で GUI に送る event の上限。
///
/// vp-app の console.ts ring buffer が 1000 件なので、それを溢れさせない範囲に収める。
/// 超過分は **古い方から捨てる**（直近の会話が最も価値が高い）。
const REPLAY_EVENT_CAP: usize = 800;

/// session の transcript を読み、replay 用 [`ConversationEvent`] 列を返す。
///
/// 先頭は必ず [`ConversationEvent::ReplayStart`]。 transcript 不在 / 読めない場合は
/// `ReplayStart` だけを返す（= GUI は「空の会話」に収束、live event を待つ）。
pub fn replay_events(session_id: &str) -> Vec<ConversationEvent> {
    let mut out = vec![ConversationEvent::ReplayStart];
    let Some(path) = crate::lane::cc_session::transcript_path(session_id) else {
        return out;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        tracing::warn!("transcript 読込失敗 (replay skip): {}", path.display());
        return out;
    };
    let mut events = events_from_lines(&raw);
    // cap 超過は古い方を捨てる（直近優先）。
    if events.len() > REPLAY_EVENT_CAP {
        let drop = events.len() - REPLAY_EVENT_CAP;
        tracing::debug!("transcript replay: {drop} 件の古い event を切り詰め");
        events.drain(..drop);
        // 切り詰めは ToolCall / ToolCallUpdate のペアを断ち切りうる。 孤児 update を残さない。
        drop_orphan_updates(&mut events);
    }
    out.extend(events);
    out
}

/// 対応する [`ConversationEvent::ToolCall`] を持たない [`ConversationEvent::ToolCallUpdate`] を落とす（純関数）。
///
/// GUI（`chatview.tsx` の `foldInto`）は update を `tool_use_id` で ToolCall に結びつけて done 化する。
/// 結び先が無い update は黙って捨てられる（no-op）ので実害は無いが、 それを **backend 側の不変条件**
/// に格上げしておく: 「replay 列に孤児 update は現れない」。 こうしておけば、 もし GUI が孤児を
/// 観測したらそれは配送順序のバグであって切り詰めの副作用ではない、 と切り分けられる。
///
/// 孤児が生まれる唯一の決定的経路は [`REPLAY_EVENT_CAP`] の切り詰め（ToolCall だけが古くて落ちる）。
fn drop_orphan_updates(events: &mut Vec<ConversationEvent>) {
    let known: std::collections::HashSet<&str> = events
        .iter()
        .filter_map(|e| match e {
            ConversationEvent::ToolCall { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let orphans: std::collections::HashSet<String> = events
        .iter()
        .filter_map(|e| match e {
            ConversationEvent::ToolCallUpdate { tool_use_id, .. }
                if !known.contains(tool_use_id.as_str()) =>
            {
                Some(tool_use_id.clone())
            }
            _ => None,
        })
        .collect();
    if orphans.is_empty() {
        return;
    }
    tracing::debug!(
        "transcript replay: 孤児 tool_call_update を {} 件除去",
        orphans.len()
    );
    events.retain(|e| !matches!(e, ConversationEvent::ToolCallUpdate { tool_use_id, .. } if orphans.contains(tool_use_id)));
}

/// transcript の全行を [`ConversationEvent`] 列へ翻訳する（純関数）。
///
/// `ReplayStart` は含まない（[`replay_events`] が付ける）。
pub fn events_from_lines(raw: &str) -> Vec<ConversationEvent> {
    raw.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .flat_map(|v| line_to_events(&v))
        .collect()
}

/// transcript の 1 行を [`ConversationEvent`] 列へ。 対象外の行は空 Vec。
///
/// 除外するもの:
/// - `isSidechain` — subagent の会話（親 lane の ChatView に混ぜない）
/// - `isMeta` — hook 由来の内部注入
/// - `type` が `user` / `assistant` 以外（`attachment` / `system` / `queue-operation` /
///   `last-prompt` / `pr-link` / `mode` / `custom-title` …）は表示対象でない
fn line_to_events(v: &Value) -> Vec<ConversationEvent> {
    if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || v.get("isMeta").and_then(Value::as_bool) == Some(true)
    {
        return Vec::new();
    }
    let Some(content) = v.pointer("/message/content") else {
        return Vec::new();
    };
    match v.get("type").and_then(Value::as_str) {
        Some("assistant") => assistant_events(content),
        Some("user") => user_events(content, is_human_prompt(v)),
        _ => Vec::new(),
    }
}

/// この user 行が **人間が打った発話** か。
///
/// harness は user role で色々なものを注入する（`task-notification` = 背景 task 完了通知、
/// slash command の `<command-name>` メタ、`<local-command-stdout>` 等）。 これらを ChatView に
/// 出すと会話が汚れるが、タグの blocklist で弾くのは脆い（種類が増え続ける）。
///
/// 実 transcript を観察すると `origin.kind` が正確に区別する（`"human"` / `"task-notification"`
/// / 欠落=slash command 系）。 これを一次判定に使う。 `origin` field 自体が無い古い / 別 version の
/// transcript では判定できないので `true` に倒し、[`strip_injected_blocks`] の
/// ヒューリスティックに委ねる（後方互換 + 二段防御）。
fn is_human_prompt(v: &Value) -> bool {
    match v.pointer("/origin/kind").and_then(Value::as_str) {
        Some(kind) => kind == "human",
        None => v.get("origin").is_none(),
    }
}

/// assistant message の content block 配列 → text / thinking / tool_use。
fn assistant_events(content: &Value) -> Vec<ConversationEvent> {
    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = b.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    out.push(ConversationEvent::MessageChunk {
                        text: text.to_string(),
                    });
                }
            }
            Some("thinking") => {
                if let Some(text) = b.get("thinking").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    out.push(ConversationEvent::ThoughtChunk {
                        text: text.to_string(),
                    });
                }
            }
            Some("tool_use") => {
                let (Some(id), Some(name), Some(input)) = (
                    b.get("id").and_then(Value::as_str),
                    b.get("name").and_then(Value::as_str),
                    b.get("input"),
                ) else {
                    continue;
                };
                // TodoWrite は live 経路と同じく Plan へも導出する（plan ウィジェット）。
                if name == "TodoWrite"
                    && let Some(plan) = plan_from_todowrite(input)
                {
                    out.push(plan);
                }
                out.push(ConversationEvent::ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: input.clone(),
                });
            }
            _ => {}
        }
    }
    out
}

/// user message の content → 発話 or tool_result。
///
/// content は **string（素のプロンプト）** か **block 配列**（tool_result / text / image）。
/// tool_result は `ToolCallUpdate` に、text/string は `UserMessage` に落とす。
/// image は ChatView に描く語彙が無いので黙って捨てる（MVP）。
///
/// `is_human` = [`is_human_prompt`] の判定。 false（= harness 注入）の行からは発話を作らない。
/// tool_result は「engine の道具の結果」であって発話ではないため、この gate の対象外
/// （tool_result 行は origin を持たないので、gate すると全部消えてしまう）。
fn user_events(content: &Value, is_human: bool) -> Vec<ConversationEvent> {
    match content {
        Value::String(s) => {
            if is_human {
                user_text_event(s).into_iter().collect()
            } else {
                Vec::new()
            }
        }
        Value::Array(blocks) => {
            let mut out = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") if is_human => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            out.extend(user_text_event(t));
                        }
                    }
                    Some("tool_result") => {
                        let Some(tool_use_id) = b.get("tool_use_id").and_then(Value::as_str) else {
                            continue;
                        };
                        out.push(ConversationEvent::ToolCallUpdate {
                            tool_use_id: tool_use_id.to_string(),
                            content: b.get("content").map(tool_result_text).unwrap_or_default(),
                            is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        });
                    }
                    _ => {}
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// user 発話 text を [`ConversationEvent::UserMessage`] へ。 空 / 内部注入のみの行は捨てる。
///
/// `<system-reminder>` や `<local-command-*>` は harness が注入する内部文で、user が打った
/// 言葉ではない。 ChatView に出すと会話が汚れるので除去し、残りが空なら event を作らない。
fn user_text_event(text: &str) -> Option<ConversationEvent> {
    let cleaned = strip_injected_blocks(text);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(ConversationEvent::UserMessage {
        text: trimmed.to_string(),
    })
}

/// harness が注入するブロックを取り除く。
///
/// 一次判定は [`is_human_prompt`]（`origin.kind`）。 本関数は
/// (a) 人間の発話に**混ざって**注入される `<system-reminder>` の除去と、
/// (b) `origin` を持たない古い transcript での二段防御、の 2 役。
///
/// 開始タグから対応する終了タグまでを丸ごと落とす。 終了タグが無い（切れた）場合は
/// そこから末尾までを落とす（部分的な注入文を残さない）。
fn strip_injected_blocks(text: &str) -> String {
    const TAGS: [&str; 7] = [
        "system-reminder",
        "task-notification",
        "command-name",
        "command-message",
        "command-args",
        "local-command-stdout",
        "local-command-caveat",
    ];
    let mut s = text.to_string();
    for tag in TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = s.find(&open) {
            let end = match s[start..].find(&close) {
                Some(rel) => start + rel + close.len(),
                None => s.len(),
            };
            s.replace_range(start..end, "");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// assistant の text / thinking / tool_use が対応する event に落ちる。
    #[test]
    fn assistant_blocks_map_to_events() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"考え中"},{"type":"text","text":"やります"},{"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls"}}]}}"#;
        let ev = events_from_lines(line);
        assert_eq!(
            ev,
            vec![
                ConversationEvent::ThoughtChunk {
                    text: "考え中".into()
                },
                ConversationEvent::MessageChunk {
                    text: "やります".into()
                },
                ConversationEvent::ToolCall {
                    id: "tu_1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({"command":"ls"}),
                },
            ]
        );
    }

    /// TodoWrite は Plan も導出する（live 経路 translate と意味を揃える）。
    #[test]
    fn todowrite_derives_plan_before_toolcall() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu_2","name":"TodoWrite","input":{"todos":[{"content":"A","status":"completed","activeForm":"Aing"}]}}]}}"#;
        let ev = events_from_lines(line);
        assert!(matches!(ev[0], ConversationEvent::Plan { .. }), "{ev:?}");
        assert!(matches!(ev[1], ConversationEvent::ToolCall { .. }));
    }

    /// user の素の string は UserMessage、tool_result は ToolCallUpdate。
    #[test]
    fn user_string_and_tool_result_map() {
        let lines = concat!(
            r#"{"type":"user","message":{"role":"user","content":"こんにちは"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"ok","is_error":false}]}}"#
        );
        let ev = events_from_lines(lines);
        assert_eq!(
            ev,
            vec![
                ConversationEvent::UserMessage {
                    text: "こんにちは".into()
                },
                ConversationEvent::ToolCallUpdate {
                    tool_use_id: "tu_1".into(),
                    content: "ok".into(),
                    is_error: false,
                },
            ]
        );
    }

    /// sidechain(subagent) / isMeta / 非会話 type は捨てる。
    #[test]
    fn noise_lines_are_dropped() {
        let lines = concat!(
            r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"subagent"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}"#,
            "\n",
            r#"{"type":"attachment","attachment":{}}"#,
            "\n",
            r#"{"type":"system","subtype":"hook"}"#,
            "\n",
            r#"{"type":"queue-operation","operation":"x"}"#
        );
        assert_eq!(events_from_lines(lines), Vec::new());
    }

    /// harness 注入ブロックは user 発話から除去し、残りが空なら event を作らない。
    #[test]
    fn injected_blocks_are_stripped() {
        let only_injection = r#"{"type":"user","message":{"role":"user","content":"<system-reminder>\nnoise\n</system-reminder>"}}"#;
        assert_eq!(events_from_lines(only_injection), Vec::new());

        let mixed = r#"{"type":"user","message":{"role":"user","content":"<system-reminder>noise</system-reminder>本題です"}}"#;
        assert_eq!(
            events_from_lines(mixed),
            vec![ConversationEvent::UserMessage {
                text: "本題です".into()
            }]
        );
    }

    /// 終了タグ欠落の注入ブロックは末尾まで落とす（部分文を残さない）。
    #[test]
    fn unterminated_injection_is_stripped_to_end() {
        let s = strip_injected_blocks("本題<system-reminder>切れた注入");
        assert_eq!(s.trim(), "本題");
    }

    /// image block は語彙が無いので捨てる（他 block は残る）。
    #[test]
    fn image_block_is_ignored() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"image","source":{}},{"type":"text","text":"見て"}]}}"#;
        assert_eq!(
            events_from_lines(line),
            vec![ConversationEvent::UserMessage {
                text: "見て".into()
            }]
        );
    }

    /// 壊れた行 (非 JSON) は黙って skip し、後続行の変換を止めない。
    #[test]
    fn malformed_line_does_not_abort() {
        let lines = concat!(
            "not json\n",
            r#"{"type":"user","message":{"role":"user","content":"生きてる"}}"#
        );
        assert_eq!(
            events_from_lines(lines),
            vec![ConversationEvent::UserMessage {
                text: "生きてる".into()
            }]
        );
    }

    /// **JSON としては妥当だが構造が壊れている**行も後続を止めない。
    ///
    /// 上の [`malformed_line_does_not_abort`] は「非 JSON」= `serde_json` の parse 段階で
    /// 落ちる行しか見ていない。 実際に事故になるのは **parse は通るが期待した形でない**行の方で、
    /// claude 本体も 2.1.216〜218 で同種の regression を 3 件直している（malformed attachment /
    /// delta attachment を持つ transcript で `--resume` が TypeError / 毎ターン失敗 / crash、
    /// 中断された tool 呼び出しが unpaired `tool_use` block を残す）。
    ///
    /// VP の replay は行ごとに `Value` で受けて必要な field だけ `Option` で拾う設計なので
    /// 同じ穴は開いていない。 それを**不変条件として固定する** — 型付き構造体への一括
    /// deserialize に「整理」すると、1 行の異常で会話全体が復元不能になる。
    #[test]
    fn structurally_broken_entries_do_not_abort() {
        let lines = [
            // message ごと欠落 / null / 配列（content pointer が引けない or 形違い）
            r#"{"type":"user"}"#,
            r#"{"type":"assistant","message":null}"#,
            r#"{"type":"user","message":{"content":{"unexpected":"object"}}}"#,
            // content block が配列でない / block が object でない
            r#"{"type":"assistant","message":{"content":42}}"#,
            r#"{"type":"assistant","message":{"content":["bare string",null,7]}}"#,
            // block の type が欠落 / 未知
            r#"{"type":"assistant","message":{"content":[{"text":"type なし"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"future_block","x":1}]}}"#,
            // tool_use の必須 field 欠落（id / name / input のどれかが無い = 部分的な block）
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","input":{}}]}}"#,
            // 型が違う（text が文字列でない / id が数値）
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":{"nested":true}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":9,"name":"Bash","input":{}}]}}"#,
            // attachment 系（表示対象外の type — claude 2.1.216〜218 が踏んだ形）
            r#"{"type":"attachment","attachment":{"malformed":true}}"#,
            r#"{"type":"attachment"}"#,
            // 最後に生きた行。 ここに到達すれば「1 行の異常で全体が死なない」が示せる
            r#"{"type":"user","message":{"role":"user","content":"生きてる"}}"#,
        ]
        .join("\n");

        assert_eq!(
            events_from_lines(&lines),
            vec![ConversationEvent::UserMessage {
                text: "生きてる".into()
            }],
            "壊れた行は捨て、最後の正常な行だけが event になる"
        );
    }

    /// 空 transcript / 空行だけの transcript でも panic しない（境界）。
    #[test]
    fn empty_and_blank_lines_yield_nothing() {
        assert!(events_from_lines("").is_empty());
        assert!(events_from_lines("\n\n   \n").is_empty());
    }

    /// 実 transcript 由来の回帰: `origin.kind` が human 以外の user 行は発話にしない。
    ///
    /// harness は user role で背景 task 完了通知 (`task-notification`) や slash command の
    /// メタ (`<command-name>` 等) を注入する。 これらが ChatView に user bubble として
    /// 出ると会話が汚れる（実 transcript で観測）。
    #[test]
    fn non_human_origin_user_lines_are_not_speech() {
        let task_notif = r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"role":"user","content":"<task-notification>done</task-notification>"}}"#;
        assert_eq!(events_from_lines(task_notif), Vec::new());

        let slash = r#"{"type":"user","origin":{"kind":"slash-command"},"message":{"role":"user","content":"<command-name>/model</command-name>"}}"#;
        assert_eq!(events_from_lines(slash), Vec::new());

        let human = r#"{"type":"user","origin":{"kind":"human"},"message":{"role":"user","content":"やって"}}"#;
        assert_eq!(
            events_from_lines(human),
            vec![ConversationEvent::UserMessage {
                text: "やって".into()
            }]
        );
    }

    /// tool_result は origin gate の対象外（tool_result 行は origin を持たないが、結果は残す）。
    #[test]
    fn tool_result_survives_origin_gate() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_9","content":"out"}]}}"#;
        assert_eq!(
            events_from_lines(line),
            vec![ConversationEvent::ToolCallUpdate {
                tool_use_id: "tu_9".into(),
                content: "out".into(),
                is_error: false,
            }]
        );
    }

    /// 実 transcript 回帰: thinking block は本文が空（signature のみ）なので event を作らない。
    /// 思考内容は disk に残らない = replay で復元できない、という制約の明示。
    #[test]
    fn redacted_thinking_produces_no_event() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"EooFCok"},{"type":"text","text":"本文"}]}}"#;
        assert_eq!(
            events_from_lines(line),
            vec![ConversationEvent::MessageChunk {
                text: "本文".into()
            }]
        );
    }

    /// 切り詰めで ToolCall が落ちた場合、対応する ToolCallUpdate も落とす（孤児を作らない）。
    /// ペアが揃っている update / 他の event は残す。
    #[test]
    fn orphan_tool_call_updates_are_dropped() {
        let mut events = vec![
            // t0 の ToolCall は切り詰めで既に消えた想定 → update が孤児。
            ConversationEvent::ToolCallUpdate {
                tool_use_id: "t0".into(),
                content: "old".into(),
                is_error: false,
            },
            ConversationEvent::MessageChunk {
                text: "残る".into(),
            },
            ConversationEvent::ToolCall {
                id: "t1".into(),
                name: "Bash".into(),
                input: serde_json::json!({}),
            },
            ConversationEvent::ToolCallUpdate {
                tool_use_id: "t1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];
        drop_orphan_updates(&mut events);
        assert_eq!(
            events,
            vec![
                ConversationEvent::MessageChunk {
                    text: "残る".into()
                },
                ConversationEvent::ToolCall {
                    id: "t1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({}),
                },
                ConversationEvent::ToolCallUpdate {
                    tool_use_id: "t1".into(),
                    content: "ok".into(),
                    is_error: false,
                },
            ]
        );
    }

    /// 孤児が無ければ列は不変（no-op）。
    #[test]
    fn drop_orphan_updates_is_noop_without_orphans() {
        let mut events = vec![
            ConversationEvent::ToolCall {
                id: "t1".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
            },
            ConversationEvent::ToolCallUpdate {
                tool_use_id: "t1".into(),
                content: "x".into(),
                is_error: false,
            },
        ];
        let before = events.clone();
        drop_orphan_updates(&mut events);
        assert_eq!(events, before);
    }

    /// replay_events は transcript 不在でも ReplayStart だけ返す（GUI は空会話に収束）。
    #[test]
    fn replay_events_without_transcript_returns_only_marker() {
        // 実在しない (だが形式は valid な) session id
        let ev = replay_events("00000000-0000-4000-8000-000000000000");
        assert_eq!(ev, vec![ConversationEvent::ReplayStart]);
    }
}
