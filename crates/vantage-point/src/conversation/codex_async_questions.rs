//! 非同期質問は発話であり server request ではない（design 69）。
//! 履歴の質問と live の回答権限を分離し、turn 終了だけでは未回答を消さない。
use super::event::{CodexInteraction, CodexQuestion, ConversationEvent, QuestionOption};
use super::host::PermissionDecision;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_BYTES: usize = 64 * 1024;
const MAX_PENDING: usize = 32;

pub(super) fn item_key(params: &Value) -> String {
    format!(
        "{}/{}",
        params["turnId"].as_str().unwrap_or_default(),
        params["itemId"]
            .as_str()
            .or_else(|| params["item"]["id"].as_str())
            .unwrap_or_default()
    )
}

pub(super) fn questions(item: &Value) -> Option<Vec<CodexQuestion>> {
    if item["type"] != "agentMessage" || item["delivery"] != "async" {
        return None;
    }
    let rows = item["questions"].as_array()?;
    if rows.is_empty() || rows.len() > 16 || item.to_string().len() > MAX_BYTES {
        return None;
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let title = row["title"].as_str()?.trim();
            if title.is_empty() {
                return None;
            }
            let options = match &row["options"] {
                Value::Null => Vec::new(),
                Value::Array(options) => options
                    .iter()
                    .map(|option| {
                        Some(QuestionOption {
                            label: option.as_str()?.into(),
                            description: String::new(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
                _ => return None,
            };
            Some(CodexQuestion {
                id: index.to_string(),
                header: String::new(),
                question: title.into(),
                options,
                is_secret: false,
            })
        })
        .collect()
}

#[derive(Clone)]
struct Pending {
    view: CodexInteraction,
    responding: bool,
}

#[derive(Clone)]
pub(super) struct AsyncQuestions {
    generation: uuid::Uuid,
    pending: BTreeMap<String, Pending>,
}

impl Default for AsyncQuestions {
    fn default() -> Self {
        Self {
            generation: uuid::Uuid::new_v4(),
            pending: BTreeMap::new(),
        }
    }
}

impl AsyncQuestions {
    /// 実行中の配送結果は host を越えて推測しない。
    pub fn for_handoff(&self) -> Self {
        let mut saved = self.clone();
        for (id, pending) in &self.pending {
            if pending.responding {
                saved.failed(id, true);
            }
        }
        saved
    }

    /// 同じ ID を UI に残すが、native thread の再開成功前は送信させない。
    pub fn waiting_for_resume(&self) -> Self {
        let mut view = self.clone();
        for pending in view.pending.values_mut() {
            if pending.view.can_accept {
                pending.view.can_accept = false;
                pending.view.details =
                    "同じ会話を Chat で再開すると、続きから回答できます。".into();
            }
        }
        view
    }

    pub fn observe(&mut self, params: &Value) -> bool {
        let Some(questions) = questions(&params["item"]) else {
            return false;
        };
        if self.pending.len() >= MAX_PENDING {
            return false;
        }
        let key = item_key(params);
        let request_id = format!("codex-async:{}:{key}", self.generation);
        if self.pending.contains_key(&request_id) {
            return false;
        }
        self.pending.insert(
            request_id.clone(),
            Pending {
                responding: false,
                view: CodexInteraction {
                    elicitation: None,
                    cancel_on_deny: None,
                    item_id: Some(key),
                    request_id,
                    kind: "async_question".into(),
                    title: "Codex からの質問".into(),
                    details: String::new(),
                    questions,
                    blocking: false,
                    can_accept: true,
                },
            },
        );
        true
    }

    pub fn snapshot(&self, native: ConversationEvent) -> ConversationEvent {
        let ConversationEvent::CodexInteractions { mut requests } = native else {
            unreachable!()
        };
        requests.extend(self.pending.values().map(|p| p.view.clone()));
        ConversationEvent::CodexInteractions { requests }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.pending.contains_key(id)
    }
    pub fn clear(&mut self) {
        self.pending.clear();
    }
    /// 送信中の回答も入力途中の下書きも、接続断だけでは画面から消さない。
    pub fn disconnect(&mut self) {
        for pending in self.pending.values_mut() {
            pending.view.can_accept = false;
            pending.view.details = if pending.responding {
                "送信結果を確認できません。会話履歴を確認してください。"
            } else {
                "接続が終了したため回答できません。入力内容はこの画面に保持しています。"
            }
            .into();
            pending.responding = false;
        }
    }
    pub fn finish(&mut self, id: &str) {
        self.pending.remove(id);
    }

    /// RPC に渡す前に検証・予約する。質問の同文をキーにせず順序 ID で対応づける。
    pub fn begin(
        &mut self,
        id: &str,
        decision: &PermissionDecision,
    ) -> Result<Option<String>, String> {
        let pending = self
            .pending
            .get_mut(id)
            .ok_or("この質問は現在回答できません")?;
        if pending.responding {
            return Err("回答を送信中です".into());
        }
        let PermissionDecision::Allow { answers } = decision else {
            self.finish(id);
            return Ok(None);
        };
        if !pending.view.can_accept {
            return Err("前回の送信結果が不明です。履歴を確認してください。".into());
        }
        let input = answers
            .as_ref()
            .and_then(Value::as_object)
            .ok_or("回答がありません")?;
        if input.len() != pending.view.questions.len() {
            return Err("質問と回答の数が一致しません".into());
        }
        let mut text = String::from("質問への回答です。\n");
        for q in &pending.view.questions {
            let answer = input
                .get(&q.id)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .ok_or("未回答があります")?;
            text.push_str(&format!("\n質問: {}\n回答: {}\n", q.question, answer));
        }
        if text.len() > MAX_BYTES {
            return Err("回答が長すぎます".into());
        }
        pending.responding = true;
        Ok(Some(text))
    }

    pub fn failed(&mut self, id: &str, uncertain: bool) {
        if let Some(p) = self.pending.get_mut(id) {
            p.responding = false;
            if uncertain {
                p.view.can_accept = false;
                p.view.details =
                    "送信結果を確認できません。重複送信を避けるため、会話履歴を確認してください。"
                        .into();
            }
        }
    }
}
