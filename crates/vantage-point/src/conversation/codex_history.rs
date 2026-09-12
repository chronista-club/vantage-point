//! Codex thread の表示用履歴（design 65）。

use super::ConversationEvent;
use super::codex_rpc_translate::CodexRpcTranslator;
use serde_json::{Value, json};
use std::collections::VecDeque;

pub const MAX_HISTORY_BYTES: usize = 1024 * 1024;

const MAX_EVENTS: usize = 800;
const MAX_ITEM_BYTES: usize = 32 * 1024;

struct HistoryItem {
    turn: String,
    id: String,
    client_id: Option<String>,
    complete: bool,
    restored: bool,
    events: Vec<ConversationEvent>,
    bytes: usize,
}

#[derive(Default)]
pub struct CodexHistory {
    thread_id: String,
    items: VecDeque<HistoryItem>,
    bytes: usize,
    event_count: usize,
    truncated: bool,
}

impl CodexHistory {
    pub fn from_thread(thread: &Value, expected_id: &str) -> Result<Self, String> {
        if thread["id"].as_str() != Some(expected_id) {
            return Err("Codex 履歴の会話 ID が一致しません".into());
        }
        let turns = thread["turns"]
            .as_array()
            .ok_or("Codex 履歴に turns がありません")?;
        let mut history = Self {
            thread_id: expected_id.into(),
            ..Self::default()
        };
        for turn in turns {
            if turn
                .get("itemsView")
                .and_then(Value::as_str)
                .is_some_and(|v| v != "full")
            {
                return Err("Codex 履歴の本文が取得できていません".into());
            }
            let turn_id = turn["id"]
                .as_str()
                .ok_or("Codex 履歴に turn ID がありません")?;
            let items = turn["items"]
                .as_array()
                .ok_or("Codex 履歴に items がありません")?;
            for item in items {
                if item["id"].as_str().is_none() {
                    return Err("Codex 履歴に item ID がありません".into());
                }
                let running_text = turn["status"] == "inProgress"
                    && matches!(item["type"].as_str(), Some("agentMessage" | "reasoning"));
                let completed = item.get("status").and_then(Value::as_str) != Some("inProgress");
                history.ingest(
                    if completed || running_text {
                        "item/completed"
                    } else {
                        "item/started"
                    },
                    &json!({"turnId":turn_id,"item":item}),
                );
                if let Some(row) = history
                    .items
                    .iter_mut()
                    .find(|r| r.turn == turn_id && r.id == item["id"].as_str().unwrap_or(""))
                {
                    row.restored = true;
                    if running_text {
                        row.complete = false;
                    }
                }
            }
            if matches!(
                turn["status"].as_str(),
                Some("completed" | "interrupted" | "failed")
            ) {
                history.ingest("turn/completed", &json!({"turn":turn}));
            }
        }
        Ok(history)
    }

    /// true は既に履歴へ確定済みの item（live の二重配信を抑止する）。
    pub fn ingest(&mut self, method: &str, params: &Value) -> bool {
        let turn = params["turnId"]
            .as_str()
            .or_else(|| params.pointer("/turn/id").and_then(Value::as_str))
            .unwrap_or("");
        let id = params["itemId"]
            .as_str()
            .or_else(|| params.pointer("/item/id").and_then(Value::as_str))
            .unwrap_or("");
        if method == "turn/completed" {
            let marker = format!("turn-end:{turn}");
            if self.items.iter().any(|i| i.turn == turn && i.id == marker) {
                return true;
            }
            self.put(
                HistoryItem {
                    turn: turn.into(),
                    id: marker,
                    client_id: None,
                    complete: true,
                    restored: false,
                    bytes: 0,
                    events: vec![ConversationEvent::TurnCompleted {
                        session_id: self.thread_id.clone(),
                        cost_usd: None,
                        context_tokens: None,
                        context_window: None,
                    }],
                },
                None,
            );
            return false;
        }
        if id.is_empty() {
            return false;
        }
        let existing = self.items.iter().position(|i| i.turn == turn && i.id == id);
        if existing.is_some_and(|p| self.items[p].complete) {
            return true;
        }
        let mut row = if let Some(p) = existing {
            let old = self.items.remove(p).expect("existing item");
            self.bytes -= old.bytes;
            self.event_count -= old.events.len();
            old
        } else {
            HistoryItem {
                turn: turn.into(),
                id: id.into(),
                client_id: None,
                complete: false,
                restored: false,
                events: Vec::new(),
                bytes: 0,
            }
        };
        match method {
            "item/started" | "item/completed" => {
                let item = &params["item"];
                row.complete = method == "item/completed";
                if item["type"] == "userMessage" {
                    let text = item["content"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|v| {
                                    if let Some(text) = v["text"].as_str() {
                                        text.to_owned()
                                    } else {
                                        self.truncated = true;
                                        "［テキスト以外の入力は省略されています］".to_owned()
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    row.events = vec![ConversationEvent::UserMessage { text }];
                    row.client_id = item["clientId"].as_str().map(str::to_owned);
                } else {
                    row.events = CodexRpcTranslator::new().ingest(method, params);
                }
            }
            "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta" => {
                let delta = params["delta"].as_str().unwrap_or("");
                if let Some(
                    ConversationEvent::MessageChunk { text }
                    | ConversationEvent::ThoughtChunk { text },
                ) = row.events.last_mut()
                {
                    text.push_str(delta);
                } else if method == "item/agentMessage/delta" {
                    row.events
                        .push(ConversationEvent::MessageChunk { text: delta.into() });
                } else {
                    row.events
                        .push(ConversationEvent::ThoughtChunk { text: delta.into() });
                }
            }
            _ => {}
        }
        // item 全体を単位として制限し、tool の開始と結果を切り離さない。
        self.put(row, existing);
        false
    }

    pub fn snapshot(&self) -> (Vec<ConversationEvent>, Vec<String>, bool) {
        (
            self.items.iter().flat_map(|i| i.events.clone()).collect(),
            self.items
                .iter()
                .filter_map(|i| i.client_id.clone())
                .collect(),
            self.truncated,
        )
    }

    /// native から途中状態を復元した item の完成は、全文で表示を再同期する。
    pub fn restored_pending_item(&self, params: &Value) -> bool {
        let turn = params["turnId"].as_str().unwrap_or("");
        let id = params
            .pointer("/item/id")
            .and_then(Value::as_str)
            .unwrap_or("");
        self.items
            .iter()
            .any(|item| item.turn == turn && item.id == id && item.restored && !item.complete)
    }

    fn put(&mut self, mut item: HistoryItem, position: Option<usize>) {
        for event in &mut item.events {
            match event {
                ConversationEvent::MessageChunk { text }
                | ConversationEvent::ThoughtChunk { text }
                | ConversationEvent::UserMessage { text } => self.truncated |= trim_text(text),
                ConversationEvent::ToolCallUpdate { content, .. } => {
                    self.truncated |= trim_text(content)
                }
                ConversationEvent::ToolCall { input, .. } => {
                    let mut text = input.to_string();
                    if trim_text(&mut text) {
                        *input = json!({"excerpt":text});
                        self.truncated = true;
                    }
                }
                _ => {}
            }
        }
        item.bytes = serde_json::to_vec(&item.events)
            .expect("serializable history")
            .len()
            + serde_json::to_vec(&item.client_id)
                .expect("serializable client ID")
                .len()
            + 8;
        self.bytes += item.bytes;
        self.event_count += item.events.len();
        if let Some(position) = position {
            self.items.insert(position, item);
        } else {
            self.items.push_back(item);
        }
        // envelope 分を確保。空 item も数えるため未知 protocol 通知で無制限に増えない。
        while self.bytes > MAX_HISTORY_BYTES - 65536
            || self.event_count > MAX_EVENTS
            || self.items.len() > MAX_EVENTS
        {
            let oldest_turn = self
                .items
                .front()
                .map(|i| i.turn.clone())
                .unwrap_or_default();
            let whole_turn = self.items.back().is_some_and(|i| i.turn != oldest_turn);
            while let Some(old) = self.items.pop_front() {
                self.bytes -= old.bytes;
                self.event_count -= old.events.len();
                self.truncated = true;
                if !whole_turn || self.items.front().is_none_or(|i| i.turn != oldest_turn) {
                    break;
                }
            }
        }
    }
}

fn trim_text(text: &mut String) -> bool {
    if text.len() <= MAX_ITEM_BYTES {
        return false;
    }
    let mut start = text.len() - MAX_ITEM_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    *text = format!("…（省略）\n{}", &text[start..]);
    true
}

#[cfg(test)]
mod tests {
    // mem_1Cex9hm7knkwwNWrjqTEBu — native 履歴の識別・更新・表示上限。
    use super::*;
    use serde_json::json;

    fn thread(items: Vec<Value>) -> Value {
        json!({"id":"thread-1", "turns":[{
            "id":"turn-1", "status":"completed", "items":items
        }]})
    }

    #[test]
    fn active_turn_keeps_seed_text_and_accepts_the_next_delta() {
        let mut history = CodexHistory::from_thread(&json!({"id":"thread-1","turns":[{
            "id":"active","status":"inProgress","items":[{"id":"a","type":"agentMessage","text":"途中"}]
        }]}), "thread-1").unwrap();
        assert_eq!(
            history.snapshot().0,
            [ConversationEvent::MessageChunk {
                text: "途中".into()
            }]
        );
        assert!(!history.ingest(
            "item/agentMessage/delta",
            &json!({"turnId":"active","itemId":"a","delta":"の続き"})
        ));
        assert_eq!(
            history.snapshot().0,
            [ConversationEvent::MessageChunk {
                text: "途中の続き".into()
            }]
        );
    }

    #[test]
    fn non_text_user_input_is_visible_as_an_omission() {
        let history = CodexHistory::from_thread(&thread(vec![json!({"id":"u","type":"userMessage","content":[{"type":"image","url":"data:image/png;base64,secret"}]})]), "thread-1").unwrap();
        let (events, _, truncated) = history.snapshot();
        assert!(
            matches!(&events[0], ConversationEvent::UserMessage { text } if !text.is_empty() && !text.contains("secret"))
        );
        assert!(truncated);
    }

    #[test]
    fn parallel_completion_preserves_native_item_order() {
        let mut history =
            CodexHistory::from_thread(&json!({"id":"thread-1","turns":[]}), "thread-1").unwrap();
        for id in ["a", "b"] {
            history.ingest("item/started", &json!({"turnId":"t","item":{"id":id,"type":"commandExecution","command":id,"status":"inProgress"}}));
        }
        for id in ["b", "a"] {
            history.ingest("item/completed", &json!({"turnId":"t","item":{"id":id,"type":"commandExecution","command":id,"status":"completed"}}));
        }
        let calls: Vec<String> = history
            .snapshot()
            .0
            .iter()
            .filter_map(|ev| match ev {
                ConversationEvent::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(calls, ["a", "b"]);
    }

    #[test]
    fn native_console_history_restores_user_assistant_and_paired_tools() {
        let raw = thread(vec![
            json!({"id":"u1","type":"userMessage","clientId":"request-1","content":[{"type":"text","text":"調べて"}]}),
            json!({"id":"a1","type":"agentMessage","text":"確認します"}),
            json!({"id":"tool1","type":"commandExecution","command":"pwd","status":"completed","aggregatedOutput":"/workspace","exitCode":0}),
            json!({"id":"a2","type":"agentMessage","text":"完了です"}),
        ]);
        let mut history = CodexHistory::from_thread(&raw, "thread-1").unwrap();
        let (events, ids, truncated) = history.snapshot();
        assert_eq!(ids, ["request-1"]);
        assert!(!truncated);
        assert_eq!(
            events[0],
            ConversationEvent::UserMessage {
                text: "調べて".into()
            }
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ConversationEvent::ToolCall { id, .. } if id == "tool1"))
        );
        assert!(events.iter().any(|e| matches!(e, ConversationEvent::ToolCallUpdate { tool_use_id, content, .. } if tool_use_id == "tool1" && content == "/workspace")));
        assert!(history.ingest(
            "item/completed",
            &json!({"turnId":"turn-1","item":raw["turns"][0]["items"][1]})
        ));
        assert_eq!(
            history.snapshot().0,
            events,
            "復元済み item を通知でもう一度足さない"
        );
    }

    #[test]
    fn live_item_delta_then_completion_has_one_authoritative_body() {
        let mut history =
            CodexHistory::from_thread(&json!({"id":"thread-1","turns":[]}), "thread-1").unwrap();
        history.ingest("turn/started", &json!({"turn":{"id":"turn-live"}}));
        history.ingest(
            "item/started",
            &json!({"turnId":"turn-live","item":{"type":"agentMessage","id":"a","text":""}}),
        );
        for delta in ["前半", "と後半"] {
            history.ingest(
                "item/agentMessage/delta",
                &json!({"turnId":"turn-live","itemId":"a","delta":delta}),
            );
        }
        let before = history.snapshot().0;
        assert_eq!(
            before,
            [ConversationEvent::MessageChunk {
                text: "前半と後半".into()
            }]
        );
        history.ingest("item/completed", &json!({"turnId":"turn-live","item":{"type":"agentMessage","id":"a","text":"前半と後半"}}));
        assert_eq!(history.snapshot().0, before);
    }

    #[test]
    fn incomplete_or_wrong_thread_is_not_an_empty_success() {
        for raw in [
            json!({"id":"other","turns":[]}),
            json!({"id":"thread-1"}),
            json!({"id":"thread-1","turns":[{"id":"t","status":"completed","itemsView":"summary","items":[]}]}),
        ] {
            assert!(CodexHistory::from_thread(&raw, "thread-1").is_err());
        }
    }

    #[test]
    fn oversized_history_is_bounded_and_reports_omission_without_orphan_tools() {
        let items = (0..1200)
            .map(|i| {
                json!({
                    "id":format!("tool-{i}"),"type":"commandExecution","command":"test",
                    "status":"completed","aggregatedOutput":"結果😀".repeat(8000),"exitCode":0
                })
            })
            .collect();
        let history = CodexHistory::from_thread(&thread(items), "thread-1").unwrap();
        let (events, _, truncated) = history.snapshot();
        assert!(truncated);
        assert!(!events.is_empty());
        assert!(serde_json::to_vec(&events).unwrap().len() <= MAX_HISTORY_BYTES);
        let mut calls = std::collections::HashSet::new();
        for event in &events {
            match event {
                ConversationEvent::ToolCall { id, .. } => {
                    calls.insert(id);
                }
                ConversationEvent::ToolCallUpdate { tool_use_id, .. } => {
                    assert!(calls.contains(tool_use_id))
                }
                _ => {}
            }
        }
    }
}
