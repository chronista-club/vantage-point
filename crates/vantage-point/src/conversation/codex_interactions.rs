//! Codex の未回答 server request（design 67）。native ID と表示用 ID は別の名前空間。
use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Value, json};

use super::event::{CodexInteraction, CodexQuestion, ConversationEvent, QuestionOption};
use super::host::PermissionDecision;

const MAX_DETAIL_BYTES: usize = 64 * 1024;
const MAX_PENDING: usize = 8;

struct Pending {
    native_id: Value,
    turn: String,
    view: CodexInteraction,
    responding: bool,
}

pub(super) struct Interactions {
    generation: uuid::Uuid,
    next_id: u64,
    pending: BTreeMap<String, Pending>,
    file_changes: HashMap<String, Value>,
}

impl Default for Interactions {
    fn default() -> Self {
        Self {
            generation: uuid::Uuid::new_v4(),
            next_id: 0,
            pending: BTreeMap::new(),
            file_changes: HashMap::new(),
        }
    }
}

impl Interactions {
    pub fn snapshot(&self) -> ConversationEvent {
        ConversationEvent::CodexInteractions {
            requests: self.pending.values().map(|p| p.view.clone()).collect(),
        }
    }

    pub fn clear(&mut self) {
        self.pending.clear();
        self.file_changes.clear();
    }

    /// fileChange の詳細は approval request より前の item/started が供給する。
    pub fn observe(&mut self, method: &str, params: &Value) -> bool {
        if method == "item/started"
            && params["item"]["type"] == "fileChange"
            && let (Some(turn), Some(id)) =
                (params["turnId"].as_str(), params["item"]["id"].as_str())
        {
            let changes = &params["item"]["changes"];
            if changes.is_array()
                && changes.to_string().len() <= MAX_DETAIL_BYTES
                && self.file_changes.len() < MAX_PENDING
            {
                self.file_changes
                    .insert(format!("{turn}/{id}"), changes.clone());
            }
        }
        let before = self.pending.len();
        if method == "serverRequest/resolved" {
            self.pending
                .retain(|_, p| p.native_id != params["requestId"]);
        } else if method == "turn/completed"
            && let Some(turn) = params["turn"]["id"].as_str()
        {
            self.pending.retain(|_, p| p.turn != turn);
            self.file_changes
                .retain(|key, _| !key.starts_with(&format!("{turn}/")));
        }
        before != self.pending.len()
    }

    pub fn receive(
        &mut self,
        id: &Value,
        method: &str,
        params: &Value,
        thread: Option<&str>,
        turn: Option<&str>,
    ) -> Result<(), String> {
        if !(id.is_string() || id.as_i64().is_some()) {
            return Err("不正な request ID".into());
        }
        if thread.is_none()
            || params["threadId"].as_str() != thread
            || turn.is_none()
            || params["turnId"].as_str() != turn
        {
            return Err("質問・承認の対象会話または turn が一致しません".into());
        }
        if self.pending.values().any(|p| p.native_id == *id) {
            return Err("重複した未回答 request ID".into());
        }
        if self.pending.len() >= MAX_PENDING || params.to_string().len() > MAX_DETAIL_BYTES {
            return Err("質問・承認が表示可能な上限を超えています".into());
        }
        let (kind, title) = match method {
            "item/tool/requestUserInput" => ("question", "Codex からの質問"),
            "item/commandExecution/requestApproval"
                if params["networkApprovalContext"].is_object() =>
            {
                ("command", "ネットワーク接続の承認")
            }
            "item/commandExecution/requestApproval" => ("command", "コマンド実行の承認"),
            "item/fileChange/requestApproval" => ("file_change", "ファイル変更の承認"),
            _ => return Err(format!("VP はこの要求に未対応です: {method}")),
        };
        let mut questions = Vec::new();
        let mut detail = serde_json::Map::new();
        let mut can_accept = true;
        if kind == "question" {
            let rows = params["questions"]
                .as_array()
                .filter(|rows| !rows.is_empty() && rows.len() <= 16)
                .ok_or("質問がありません")?;
            let mut ids = HashSet::new();
            for q in rows {
                let id = q["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("質問 ID がありません")?;
                if !ids.insert(id) {
                    return Err("質問 ID が重複しています".into());
                }
                let options = match &q["options"] {
                    Value::Null => Vec::new(),
                    Value::Array(options) => options
                        .iter()
                        .map(|o| {
                            Ok(QuestionOption {
                                label: o["label"]
                                    .as_str()
                                    .ok_or("選択肢 label がありません")?
                                    .into(),
                                description: o["description"].as_str().unwrap_or_default().into(),
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                    _ => return Err("不正な質問選択肢".into()),
                };
                questions.push(CodexQuestion {
                    id: id.into(),
                    header: q["header"].as_str().unwrap_or_default().into(),
                    question: q["question"].as_str().ok_or("質問文がありません")?.into(),
                    options,
                    is_secret: q["isSecret"].as_bool().unwrap_or(false),
                });
            }
        } else {
            for key in [
                "command",
                "cwd",
                "reason",
                "networkApprovalContext",
                "additionalPermissions",
                "grantRoot",
            ] {
                if let Some(value) = params.get(key).filter(|v| !v.is_null()) {
                    detail.insert(key.into(), value.clone());
                }
            }
            if kind == "command" {
                if let Some(choices) = params.get("availableDecisions").filter(|v| !v.is_null()) {
                    let choices = choices.as_array().ok_or("不正な承認選択肢")?;
                    can_accept = choices.contains(&json!("accept"));
                    // 最小の拒否応答ができない要求は未対応として native へ戻す。
                    if !choices.contains(&json!("decline")) {
                        return Err("今回のみの拒否をサポートしない承認要求".into());
                    }
                }
                let has_target = params["command"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())
                    || params["networkApprovalContext"]["host"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty())
                    || params["additionalPermissions"]
                        .as_object()
                        .is_some_and(|p| !p.is_empty());
                if !has_target {
                    can_accept = false;
                    detail.insert(
                        "notice".into(),
                        json!("承認対象を取得できないため許可できません。"),
                    );
                }
            } else {
                let key = format!(
                    "{}/{}",
                    turn.unwrap_or_default(),
                    params["itemId"].as_str().unwrap_or_default()
                );
                if let Some(changes) = self.file_changes.remove(&key) {
                    detail.insert("changes".into(), changes);
                } else {
                    can_accept = false;
                    detail.insert(
                        "notice".into(),
                        json!(
                            "変更内容を取得できないため許可できません。拒否して再試行してください。"
                        ),
                    );
                }
            }
        }
        self.next_id += 1;
        let request_id = format!("codex:{}:{}", self.generation, self.next_id);
        let view = CodexInteraction {
            request_id: request_id.clone(),
            kind: kind.into(),
            title: title.into(),
            details: if detail.is_empty() {
                String::new()
            } else {
                serde_json::to_string_pretty(&detail).unwrap_or_default()
            },
            questions,
            blocking: kind != "question" || params["isBlocking"].as_bool().unwrap_or(true),
            can_accept,
        };
        self.pending.insert(
            request_id,
            Pending {
                native_id: id.clone(),
                turn: turn.unwrap_or_default().into(),
                view,
                responding: false,
            },
        );
        Ok(())
    }

    /// 検証を終えるまでは要求を消費しない。回答本文は保存しない。
    pub fn begin_response(
        &mut self,
        request_id: &str,
        decision: &PermissionDecision,
    ) -> Result<String, String> {
        let pending = self
            .pending
            .get_mut(request_id)
            .ok_or("この質問・承認はすでに終了しています")?;
        if pending.responding {
            return Err("回答を送信中です".into());
        }
        let result = match decision {
            PermissionDecision::Deny { .. } if pending.view.kind == "question" => {
                json!({"answers":{}})
            }
            PermissionDecision::Deny { .. } => json!({"decision":"decline"}),
            PermissionDecision::Allow { answers } => {
                if !pending.view.can_accept {
                    return Err("この要求は許可できません。内容を確認し拒否してください。".into());
                }
                if pending.view.kind == "question" {
                    let input = answers
                        .as_ref()
                        .and_then(Value::as_object)
                        .ok_or("質問への回答がありません")?;
                    if input.len() != pending.view.questions.len() {
                        return Err("質問と回答の数が一致しません".into());
                    }
                    let mut mapped = serde_json::Map::new();
                    for q in &pending.view.questions {
                        let answer = input
                            .get(&q.id)
                            .and_then(Value::as_str)
                            .filter(|a| !a.trim().is_empty() && a.len() <= MAX_DETAIL_BYTES)
                            .ok_or("未回答または長すぎる回答があります")?;
                        mapped.insert(q.id.clone(), json!({"answers":[answer]}));
                    }
                    json!({"answers":mapped})
                } else {
                    json!({"decision":"accept"})
                }
            }
        };
        pending.responding = true;
        Ok(json!({"id":pending.native_id,"result":result}).to_string())
    }

    pub fn finish_response(&mut self, request_id: &str) {
        self.pending.remove(request_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> Value {
        json!({"threadId":"thread","turnId":"turn","isBlocking":false,"questions":[
            {"id":"one","question":"同じ文面","header":"一つ目","isSecret":true},
            {"id":"two","question":"同じ文面","header":"二つ目","options":[{"label":"A","description":"候補"}]}
        ]})
    }

    #[test]
    fn interactions_answers_use_question_ids_and_preserve_native_id_type() {
        let mut pending = Interactions::default();
        pending
            .receive(
                &json!(7),
                "item/tool/requestUserInput",
                &question(),
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        pending
            .receive(
                &json!("7"),
                "item/tool/requestUserInput",
                &question(),
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        let ids: Vec<_> = pending.pending.keys().cloned().collect();
        let invalid = PermissionDecision::Allow {
            answers: Some(json!({"同じ文面":"lost"})),
        };
        assert!(pending.begin_response(&ids[0], &invalid).is_err());
        let response: Value = serde_json::from_str(
            &pending
                .begin_response(
                    &ids[0],
                    &PermissionDecision::Allow {
                        answers: Some(json!({"one":"secret","two":"A"})),
                    },
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            response,
            json!({"id":7,"result":{"answers":{"one":{"answers":["secret"]},"two":{"answers":["A"]}}}})
        );
        assert!(pending.begin_response(&ids[0], &invalid).is_err());
        assert!(
            !serde_json::to_string(&pending.snapshot())
                .unwrap()
                .contains("\"secret\"")
        );
        assert!(pending.observe("serverRequest/resolved", &json!({"requestId":7})));
        assert_eq!(pending.pending.len(), 1);
        let response: Value = serde_json::from_str(
            &pending
                .begin_response(
                    &ids[1],
                    &PermissionDecision::Deny {
                        message: String::new(),
                    },
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response, json!({"id":"7","result":{"answers":{}}}));
    }

    #[test]
    fn interactions_approval_requires_visible_changes_and_offered_decisions() {
        let mut pending = Interactions::default();
        let params = json!({"threadId":"thread","turnId":"turn","itemId":"file","reason":"修正"});
        pending
            .receive(
                &json!(1),
                "item/fileChange/requestApproval",
                &params,
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        let id = pending.pending.keys().next().unwrap().clone();
        assert!(
            pending
                .begin_response(&id, &PermissionDecision::Allow { answers: None })
                .is_err()
        );
        pending.clear();
        pending.observe("item/started", &json!({"turnId":"turn","item":{"id":"file","type":"fileChange","changes":[{"path":"a.txt","diff":"+new"}]}}));
        pending
            .receive(
                &json!(2),
                "item/fileChange/requestApproval",
                &params,
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        let id = pending.pending.keys().next().unwrap().clone();
        assert!(pending.pending[&id].view.details.contains("+new"));
        assert_eq!(
            serde_json::from_str::<Value>(
                &pending
                    .begin_response(&id, &PermissionDecision::Allow { answers: None })
                    .unwrap()
            )
            .unwrap(),
            json!({"id":2,"result":{"decision":"accept"}})
        );
        let params = json!({"threadId":"thread","turnId":"turn","command":"ls","availableDecisions":["decline"]});
        pending
            .receive(
                &json!(3),
                "item/commandExecution/requestApproval",
                &params,
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        let id = pending
            .pending
            .iter()
            .find(|(_, p)| p.native_id == 3)
            .unwrap()
            .0
            .clone();
        assert!(
            pending
                .begin_response(&id, &PermissionDecision::Allow { answers: None })
                .is_err()
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                &pending
                    .begin_response(
                        &id,
                        &PermissionDecision::Deny {
                            message: String::new()
                        }
                    )
                    .unwrap()
            )
            .unwrap()["result"],
            json!({"decision":"decline"})
        );
    }

    #[test]
    fn interactions_approval_identifies_target_and_releases_file_previews() {
        let mut pending = Interactions::default();
        for (id, params, title, acceptable) in [
            (
                1,
                json!({"networkApprovalContext":{"host":"example.com","protocol":"https"}}),
                "ネットワーク接続の承認",
                true,
            ),
            (
                2,
                json!({"reason":"必要です","cwd":"/work"}),
                "コマンド実行の承認",
                false,
            ),
        ] {
            let mut params = params;
            params["threadId"] = json!("thread");
            params["turnId"] = json!("turn");
            pending
                .receive(
                    &json!(id),
                    "item/commandExecution/requestApproval",
                    &params,
                    Some("thread"),
                    Some("turn"),
                )
                .unwrap();
            let view = &pending
                .pending
                .values()
                .find(|p| p.native_id == id)
                .unwrap()
                .view;
            assert_eq!(view.title, title);
            assert_eq!(view.can_accept, acceptable);
        }
        pending.clear();
        for id in 0..12 {
            let item = format!("file-{id}");
            pending.observe("item/started", &json!({"turnId":"turn","item":{"id":item,"type":"fileChange","changes":[{"path":"a.txt","diff":"+new"}]}}));
            pending
                .receive(
                    &json!(id),
                    "item/fileChange/requestApproval",
                    &json!({"threadId":"thread","turnId":"turn","itemId":item}),
                    Some("thread"),
                    Some("turn"),
                )
                .unwrap();
            let view = &pending.pending.values().next().unwrap().view;
            assert!(view.can_accept, "sequential approval {id}");
            let request_id = view.request_id.clone();
            pending.finish_response(&request_id);
        }
    }

    #[test]
    fn interactions_reject_wrong_thread_and_expire_by_turn_and_host_generation() {
        let mut pending = Interactions::default();
        assert!(
            pending
                .receive(
                    &json!(1),
                    "item/tool/requestUserInput",
                    &question(),
                    Some("other"),
                    Some("turn")
                )
                .is_err()
        );
        pending
            .receive(
                &json!(1),
                "item/tool/requestUserInput",
                &question(),
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        let id = pending.pending.keys().next().unwrap().clone();
        assert!(!pending.observe("turn/completed", &json!({"turn":{"id":"other"}})));
        assert!(pending.observe("turn/completed", &json!({"turn":{"id":"turn"}})));
        assert!(
            pending
                .begin_response(&id, &PermissionDecision::Allow { answers: None })
                .is_err()
        );
        let mut fresh = Interactions::default();
        fresh
            .receive(
                &json!(1),
                "item/tool/requestUserInput",
                &question(),
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        assert_ne!(id, *fresh.pending.keys().next().unwrap());
        assert!(
            fresh
                .begin_response(
                    &id,
                    &PermissionDecision::Deny {
                        message: String::new()
                    }
                )
                .is_err()
        );
    }
}
