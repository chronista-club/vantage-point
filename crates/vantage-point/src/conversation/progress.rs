//! 会話 stream から sidebar 用の現在値だけを畳む。本文や tool の結果は保持しない。
use super::ConversationEvent;
use crate::lane::session_registry::SessionKey;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressPhase {
    Requesting,
    Working,
    #[default]
    Waiting,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationProgressView {
    pub phase: ProgressPhase,
    pub now: Option<String>,
    pub error: bool,
}

#[derive(Debug, Default)]
pub struct ConversationProgress {
    phase: ProgressPhase,
    self_report: Option<String>,
    automatic: Option<String>,
    requests: BTreeMap<String, String>,
    replaying: bool,
    error: bool,
}

fn short(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(180)
        .collect()
}

fn tool_summary(name: &str, input: &serde_json::Value) -> String {
    let detail = [
        "description",
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
    ]
    .iter()
    .find_map(|key| input.get(key).and_then(serde_json::Value::as_str));
    short(&detail.map_or_else(|| name.to_string(), |text| format!("{name}: {text}")))
}

impl ConversationProgress {
    pub fn view(&self) -> ConversationProgressView {
        ConversationProgressView {
            phase: if self.requests.is_empty() {
                self.phase
            } else {
                ProgressPhase::Requesting
            },
            now: self
                .requests
                .values()
                .next()
                .cloned()
                .or_else(|| self.self_report.clone())
                .or_else(|| self.automatic.clone()),
            error: self.error,
        }
    }

    pub fn respond(&mut self, id: &str) {
        if self.requests.remove(id).is_some() && self.requests.is_empty() {
            self.phase = ProgressPhase::Working;
        }
    }

    fn finish(&mut self, phase: ProgressPhase) {
        self.phase = phase;
        self.requests.clear();
        self.self_report = None;
        self.automatic = None;
    }

    pub fn apply(&mut self, event: &ConversationEvent) {
        match event {
            ConversationEvent::ReplayStart => {
                self.replaying = true;
                return;
            }
            ConversationEvent::ReplayEnd { .. } => {
                self.replaying = false;
                return;
            }
            _ if self.replaying => return,
            _ => {}
        }
        match event {
            ConversationEvent::SessionInit { .. } => {
                *self = Self::default();
            }
            ConversationEvent::Question {
                request_id,
                questions,
            } => {
                let text = questions
                    .first()
                    .map(|q| q.question.as_str())
                    .unwrap_or("回答をお願いします");
                self.requests.insert(request_id.clone(), short(text));
            }
            ConversationEvent::PermissionRequest {
                request_id,
                tool_name,
                ..
            } => {
                self.requests.insert(
                    request_id.clone(),
                    short(&format!("{tool_name} の実行を承認してください")),
                );
            }
            ConversationEvent::ToolCall { id, name, input } => {
                self.phase = ProgressPhase::Working;
                self.error = false;
                self.self_report = None;
                self.automatic = Some(tool_summary(name, input));
                if name == "AskUserQuestion" {
                    let text = input
                        .pointer("/questions/0/question")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("回答をお願いします");
                    self.requests.insert(id.clone(), short(text));
                }
            }
            ConversationEvent::ToolCallUpdate { tool_use_id, .. } => self.respond(tool_use_id),
            ConversationEvent::NowLine { text } => self.self_report = Some(short(text)),
            ConversationEvent::MessageChunk { .. }
            | ConversationEvent::ThoughtChunk { .. }
            | ConversationEvent::CodexMessage { .. } => {
                self.phase = ProgressPhase::Working;
                self.error = false;
            }
            ConversationEvent::CodexInteractions { requests } => {
                self.requests.retain(|id, _| !id.starts_with("codex:"));
                for request in requests.iter().filter(|r| r.blocking) {
                    self.requests.insert(
                        format!("codex:{}", request.request_id),
                        short(&request.title),
                    );
                }
            }
            ConversationEvent::CodexInteractionResult {
                request_id,
                error: None,
            } => self.respond(&format!("codex:{request_id}")),
            ConversationEvent::TurnCompleted { .. } => self.finish(ProgressPhase::Waiting),
            ConversationEvent::Error { .. } => {
                self.finish(ProgressPhase::Stopped);
                self.error = true;
            }
            ConversationEvent::EngineExited { .. } => self.finish(ProgressPhase::Stopped),
            _ => {}
        }
    }
}

/// Topic の購読数に依存しない、session ごとの揮発投影。lane snapshot が読み出す。
#[derive(Default)]
pub struct ConversationProgressStore(Mutex<HashMap<(String, SessionKey), ConversationProgress>>);

impl ConversationProgressStore {
    pub fn observe(&self, lane: &str, session: SessionKey, event: &ConversationEvent) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry((lane.to_string(), session))
            .or_default()
            .apply(event);
    }

    pub fn view(&self, lane: &str, session: SessionKey) -> Option<ConversationProgressView> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(lane.to_string(), session))
            .map(ConversationProgress::view)
    }

    pub fn respond(&self, lane: &str, session: SessionKey, request_id: &str) {
        if let Some(progress) = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(&(lane.to_string(), session))
        {
            progress.respond(request_id);
        }
    }
}

// mem_1Cg7MpHWBdf6u9CYjyEAaf
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(id: &str, name: &str, input: serde_json::Value) -> ConversationEvent {
        ConversationEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            input,
        }
    }

    #[test]
    fn requests_survive_unrelated_output_and_clear_only_by_matching_response() {
        let mut progress = ConversationProgress::default();
        progress.apply(&ConversationEvent::PermissionRequest {
            request_id: "p1".into(),
            tool_name: "Bash".into(),
            input: json!({}),
        });
        progress.apply(&ConversationEvent::MessageChunk {
            text: "補足".into(),
        });
        progress.respond("other");
        assert_eq!(progress.view().phase, ProgressPhase::Requesting);
        assert!(progress.view().now.unwrap().contains("Bash"));
        progress.respond("p1");
        assert_eq!(progress.view().phase, ProgressPhase::Working);
    }

    #[test]
    fn ask_tool_result_clears_only_its_own_question() {
        let mut progress = ConversationProgress::default();
        progress.apply(&tool(
            "q1",
            "AskUserQuestion",
            json!({"questions":[{"question":"どちらにしますか？"}]}),
        ));
        assert_eq!(progress.view().phase, ProgressPhase::Requesting);
        assert_eq!(progress.view().now.as_deref(), Some("どちらにしますか？"));
        progress.apply(&ConversationEvent::ToolCallUpdate {
            tool_use_id: "other".into(),
            content: String::new(),
            is_error: false,
        });
        assert_eq!(progress.view().phase, ProgressPhase::Requesting);
        progress.apply(&ConversationEvent::ToolCallUpdate {
            tool_use_id: "q1".into(),
            content: "A".into(),
            is_error: false,
        });
        assert_eq!(progress.view().phase, ProgressPhase::Working);
    }

    #[test]
    fn next_tool_replaces_self_report_but_request_takes_precedence() {
        let mut progress = ConversationProgress::default();
        progress.apply(&tool("t1", "Bash", json!({"command":"cargo test"})));
        assert_eq!(progress.view().now.as_deref(), Some("Bash: cargo test"));
        progress.apply(&ConversationEvent::NowLine {
            text: "回帰を確認中".into(),
        });
        assert_eq!(progress.view().now.as_deref(), Some("回帰を確認中"));
        progress.apply(&tool("t2", "Read", json!({"file_path":"src/main.rs"})));
        assert_eq!(progress.view().now.as_deref(), Some("Read: src/main.rs"));
        progress.apply(&ConversationEvent::PermissionRequest {
            request_id: "p".into(),
            tool_name: "Write".into(),
            input: json!({}),
        });
        progress.apply(&ConversationEvent::NowLine {
            text: "自己申告".into(),
        });
        assert!(progress.view().now.unwrap().contains("Write"));
    }

    #[test]
    fn historical_requests_do_not_replace_live_state() {
        let mut progress = ConversationProgress::default();
        progress.apply(&tool("t", "Read", json!({"file_path":"current.rs"})));
        let before = progress.view();
        progress.apply(&ConversationEvent::ReplayStart);
        progress.apply(&tool(
            "old",
            "AskUserQuestion",
            json!({"questions":[{"question":"古い質問"}]}),
        ));
        progress.apply(&ConversationEvent::EngineExited {
            message: "過去".into(),
        });
        progress.apply(&ConversationEvent::ReplayEnd { in_flight: false });
        assert_eq!(progress.view(), before);
    }

    #[test]
    fn stop_clears_request_and_error_remains_an_overlay() {
        let mut progress = ConversationProgress::default();
        progress.apply(&tool("q", "AskUserQuestion", json!({})));
        progress.apply(&ConversationEvent::Error {
            message: "failed".into(),
        });
        progress.apply(&ConversationEvent::EngineExited {
            message: "closed".into(),
        });
        assert_eq!(progress.view().phase, ProgressPhase::Stopped);
        assert!(progress.view().error);
        progress.apply(&tool("next", "Read", json!({})));
        assert_eq!(progress.view().phase, ProgressPhase::Working);
        assert!(!progress.view().error);
    }
}
