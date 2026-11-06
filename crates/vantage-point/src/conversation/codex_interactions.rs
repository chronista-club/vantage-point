//! Codex の未回答 server request（design 67）。台帳（受理・許可可否・応答の形）は共有
//! `agent-chat-host`（design 86）。ここは VP の表示語彙への射影と回答の読み替えだけを持つ。
use agent_chat_host::codex_interactions::{
    self as shared, Decision, Interaction, InteractionKind, Notice,
};
use serde_json::{Map, Value, json};

use super::event::{CodexInteraction, ConversationEvent};
use super::host::PermissionDecision;

#[derive(Default)]
pub(super) struct Interactions(shared::Interactions);

impl Interactions {
    pub fn snapshot(&self) -> ConversationEvent {
        ConversationEvent::CodexInteractions {
            requests: self.0.snapshot().iter().map(project).collect(),
        }
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// 未回答が減ったら true（snapshot の再送が要る）。
    pub fn observe(&mut self, method: &str, params: &Value) -> bool {
        self.0.observe(method, params)
    }

    pub fn receive(
        &mut self,
        id: &Value,
        method: &str,
        params: &Value,
        thread: Option<&str>,
        turn: Option<&str>,
    ) -> Result<(), String> {
        self.0.receive(id, method, params, thread, turn).map(|_| ())
    }

    /// 検証を終えるまでは要求を消費しない。回答本文は保存しない。
    pub fn begin_response(
        &mut self,
        request_id: &str,
        decision: &PermissionDecision,
    ) -> Result<String, String> {
        let kind = self
            .0
            .get(request_id)
            .ok_or("この質問・承認はすでに終了しています")?
            .kind;
        let decision = match decision {
            PermissionDecision::Deny { .. } => Decision::Deny,
            // 権限要求に選択なしの許可は受けない（nightly の VP `codex_permissions::response` と同じ Err）。
            // 共有台帳の `Allow{answers:None}` は「要求した権限を turn 一括で許可」= GFP が UI で明示する
            // 経路で、VP の UI は必ず選択を送る。設定省略を full access の選択と読み替えない（design 86）。
            PermissionDecision::Allow { answers: None } if kind == InteractionKind::Permissions => {
                return Err("権限の選択がありません".into());
            }
            // MCP 手続きの UI は中断を `{"action":"cancel"}` の回答として送る。
            PermissionDecision::Allow { answers }
                if kind.is_mcp() && answers.as_ref() == Some(&json!({"action":"cancel"})) =>
            {
                Decision::Cancel
            }
            PermissionDecision::Allow { answers } => Decision::Allow {
                answers: answers.clone(),
            },
        };
        self.0
            .begin_response(request_id, &decision)
            .map(|reply| reply.to_string())
    }

    pub fn finish_response(&mut self, request_id: &str) {
        self.0.finish_response(request_id);
    }
}

fn copy(detail: &mut Map<String, Value>, view: &Interaction, keys: &[(&str, &str)]) {
    for (key, label) in keys {
        if let Some(value) = view.detail.get(*key) {
            detail.insert((*label).into(), value.clone());
        }
    }
}

/// 共有台帳の要求を VP のカード（`codex_interactions` の語彙）へ写す。
fn project(view: &Interaction) -> CodexInteraction {
    // MCP のツール実行承認は、VP の UI では項目の無い MCP 手続きとして見せる（会話内許可は未表示）。
    let (kind, title) = match view.kind {
        InteractionKind::Question => ("question", "Codex からの質問"),
        InteractionKind::McpApproval | InteractionKind::McpElicitation => {
            ("mcp_elicitation", "MCP サーバーからの手続き")
        }
        InteractionKind::Permissions => ("permissions", "追加権限の承認"),
        InteractionKind::Network => ("command", "ネットワーク接続の承認"),
        InteractionKind::Command => ("command", "コマンド実行の承認"),
        InteractionKind::FileChange => ("file_change", "ファイル変更の承認"),
    };
    let mut detail = Map::new();
    match view.kind {
        // 専用の表示がサーバー名と検証済みの項目を持つ。
        InteractionKind::Question | InteractionKind::McpApproval => {}
        InteractionKind::McpElicitation => {
            if let Some(Notice::UnsupportedMcp(message)) = &view.notice {
                detail.insert("回答できない理由".into(), json!(message));
                copy(&mut detail, view, &[("serverName", "MCP サーバー")]);
            }
        }
        InteractionKind::Permissions => {
            copy(
                &mut detail,
                view,
                &[
                    ("environmentId", "実行環境"),
                    ("cwd", "作業ディレクトリ"),
                    ("reason", "理由"),
                ],
            );
            if let Some(Notice::InvalidPermissions(message)) = &view.notice {
                detail.insert("許可できない理由".into(), json!(message));
            }
        }
        InteractionKind::Command | InteractionKind::Network | InteractionKind::FileChange => {
            copy(
                &mut detail,
                view,
                &[
                    ("command", "command"),
                    ("cwd", "cwd"),
                    ("reason", "reason"),
                    ("networkApprovalContext", "networkApprovalContext"),
                    ("additionalPermissions", "additionalPermissions"),
                    ("grantRoot", "grantRoot"),
                    ("changes", "changes"),
                ],
            );
            let notice = match view.notice {
                Some(Notice::MissingTarget) => Some("承認対象を取得できないため許可できません。"),
                Some(Notice::MissingChanges) => {
                    Some("変更内容を取得できないため許可できません。拒否して再試行してください。")
                }
                _ => None,
            };
            if let Some(notice) = notice {
                detail.insert("notice".into(), json!(notice));
            }
        }
    }
    CodexInteraction {
        elicitation: view.elicitation.clone(),
        cancel_on_deny: view.cancel_on_deny.then_some(true),
        item_id: None,
        request_id: view.request_id.clone(),
        kind: kind.into(),
        title: title.into(),
        details: if detail.is_empty() {
            String::new()
        } else {
            serde_json::to_string_pretty(&detail).unwrap_or_default()
        },
        questions: view.questions.clone(),
        blocking: view.blocking,
        can_accept: view.can_accept,
    }
}

// 台帳の判断（受理・許可可否・応答の形）は共有 crate のテストが持つ。ここは射影と読み替え。
#[cfg(test)]
mod tests {
    use super::*;

    fn requests(pending: &Interactions) -> Vec<CodexInteraction> {
        let ConversationEvent::CodexInteractions { requests } = pending.snapshot() else {
            unreachable!()
        };
        requests
    }

    fn reply(pending: &mut Interactions, id: &str, decision: PermissionDecision) -> Value {
        serde_json::from_str(&pending.begin_response(id, &decision).unwrap()).unwrap()
    }

    #[test]
    fn interactions_project_titles_and_details_in_vp_vocabulary() {
        let mut pending = Interactions::default();
        for (id, params) in [
            (
                1,
                json!({"networkApprovalContext":{"host":"example.com","protocol":"https"}}),
            ),
            (
                2,
                json!({"reason":"必要です","cwd":"/work","environmentId":"local"}),
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
        }
        let views = requests(&pending);
        assert_eq!(views[0].kind, "command");
        assert_eq!(views[0].title, "ネットワーク接続の承認");
        assert!(views[0].can_accept);
        assert_eq!(views[1].title, "コマンド実行の承認");
        assert!(!views[1].can_accept);
        assert!(views[1].details.contains("承認対象を取得できない"));
        // コマンド承認の詳細は従来どおり native の項目名。実行環境は出さない
        assert!(views[1].details.contains("\"cwd\""));
        assert!(!views[1].details.contains("environmentId"));

        pending.clear();
        pending
            .receive(
                &json!(3),
                "item/permissions/requestApproval",
                &json!({"threadId":"thread","turnId":"turn","cwd":"/work","environmentId":"local",
                    "permissions":{"network":{"enabled":true}}}),
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        let view = &requests(&pending)[0];
        assert_eq!(view.title, "追加権限の承認");
        assert!(view.details.contains("作業ディレクトリ") && view.details.contains("実行環境"));
        assert!(!view.details.contains("network"));
        assert_eq!(view.questions.len(), 2);
    }

    /// ツール実行承認は VP では項目の無い MCP 手続きとして見せ、従来の回答で応答できる。
    #[test]
    fn interactions_tool_call_approval_keeps_the_mcp_card_and_answers() {
        let params = json!({"threadId":"thread","turnId":"turn","serverName":"docs",
            "mode":"form","message":"Allow Search?",
            "requestedSchema":{"type":"object","properties":{}},
            "_meta":{"codex_approval_kind":"mcp_tool_call","persist":["session"]}});
        for (decision, action) in [
            (
                PermissionDecision::Allow {
                    answers: Some(json!({})),
                },
                "accept",
            ),
            (
                PermissionDecision::Allow {
                    answers: Some(json!({"action":"cancel"})),
                },
                "cancel",
            ),
            (
                PermissionDecision::Deny {
                    message: String::new(),
                },
                "decline",
            ),
        ] {
            let mut pending = Interactions::default();
            pending
                .receive(
                    &json!(9),
                    "mcpServer/elicitation/request",
                    &params,
                    Some("thread"),
                    Some("turn"),
                )
                .unwrap();
            let view = requests(&pending).remove(0);
            assert_eq!(view.kind, "mcp_elicitation");
            assert!(view.details.is_empty());
            let elicitation = view.elicitation.unwrap();
            assert_eq!(elicitation.server_name, "docs");
            assert!(elicitation.fields.is_empty());
            assert_eq!(
                reply(&mut pending, &view.request_id, decision),
                json!({"id":9,"result":{"action":action}})
            );
        }
    }

    /// 権限要求に選択なしの許可（`answers: None`）は受けない — nightly の VP（`codex_permissions::response`）
    /// と同じ Err。共有台帳の `Allow{answers:None}` は「要求した権限を turn 一括で許可」（GFP が UI で
    /// 明示する経路）なので、VP の射影で止めないと設定省略が full access の選択に化ける（design 86）。
    #[test]
    fn interactions_permissions_reject_allow_without_selection() {
        let mut pending = Interactions::default();
        pending
            .receive(
                &json!(1),
                "item/permissions/requestApproval",
                &json!({"threadId":"thread","turnId":"turn","permissions":{"fileSystem":{
                    "read":null,"write":null,
                    "entries":[{"access":"write","path":{"type":"path","path":"/data"}}]}}}),
                Some("thread"),
                Some("turn"),
            )
            .unwrap();
        let id = requests(&pending)[0].request_id.clone();
        let err = pending
            .begin_response(&id, &PermissionDecision::Allow { answers: None })
            .expect_err("選択なしの許可は Err");
        assert!(err.contains("権限の選択がありません"), "{err}");
        // 検証で止めた要求は消費されず、明示の選択で改めて応答できる。
        assert_eq!(
            reply(
                &mut pending,
                &id,
                PermissionDecision::Allow {
                    answers: Some(json!({"p0":"allow"}))
                }
            )["result"]["scope"],
            json!("turn")
        );
    }

    /// 一般の MCP 手続きは型付きの値と中断を従来の形で返す。未対応形式は理由を見せる。
    #[test]
    fn interactions_mcp_form_answers_and_unsupported_reason() {
        let mut pending = Interactions::default();
        pending
            .receive(
                &json!("mcp-1"),
                "mcpServer/elicitation/request",
                &json!({"threadId":"thread","turnId":null,"serverName":"documents","mode":"form",
                    "message":"設定してください","requestedSchema":{"type":"object",
                    "properties":{"count":{"type":"integer"}},"required":["count"]}}),
                Some("thread"),
                None,
            )
            .unwrap();
        let id = requests(&pending)[0].request_id.clone();
        assert_eq!(
            reply(
                &mut pending,
                &id,
                PermissionDecision::Allow {
                    answers: Some(json!({"field:count":"2"}))
                }
            ),
            json!({"id":"mcp-1","result":{"action":"accept","content":{"count":2},"_meta":null}})
        );
        // 送信中は二重回答を受けない
        assert!(
            pending
                .begin_response(
                    &id,
                    &PermissionDecision::Deny {
                        message: String::new()
                    }
                )
                .is_err()
        );

        let mut unsupported = Interactions::default();
        unsupported
            .receive(
                &json!(1),
                "mcpServer/elicitation/request",
                &json!({"threadId":"thread","turnId":null,"serverName":"test","mode":"url",
                    "url":"javascript:alert(1)"}),
                Some("thread"),
                None,
            )
            .unwrap();
        let view = requests(&unsupported).remove(0);
        assert!(!view.can_accept);
        assert!(view.details.contains("回答できない理由") && view.details.contains("MCP サーバー"));
        assert_eq!(
            reply(
                &mut unsupported,
                &view.request_id,
                PermissionDecision::Allow {
                    answers: Some(json!({"action":"cancel"}))
                }
            )["result"],
            json!({"action":"cancel","content":null,"_meta":null})
        );
    }
}
