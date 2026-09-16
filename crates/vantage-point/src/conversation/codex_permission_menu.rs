//! 権限は native が所有する。候補取得では設定を書かず、明示選択だけを変更する。
use super::super::event::{CodexPermissionChoice, ConversationEvent};
use super::{RpcInner, native_queue};
use serde_json::{Value, json};
use std::sync::Arc;

struct Choice {
    view: CodexPermissionChoice,
    profile: String,
    approval: Option<&'static str>,
    reviewer: Option<&'static str>,
}

fn allowed(requirements: &Value, key: &str, value: &str) -> bool {
    requirements[key].is_null()
        || requirements[key]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == value))
}

async fn choices(inner: &RpcInner, deadline: tokio::time::Instant) -> anyhow::Result<Vec<Choice>> {
    let cwd = inner
        .state
        .lock()
        .expect("rpc state lock")
        .config
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.cwd.clone())
        .ok_or_else(|| {
            anyhow::anyhow!("会話の作業場所を確認できません。再接続してから変更してください。")
        })?;
    let requirements =
        native_queue::rpc_until(inner, "configRequirements/read", json!({}), deadline).await?;
    anyhow::ensure!(
        requirements
            .get("requirements")
            .is_some_and(|r| r.is_null() || r.is_object()),
        "Codex の管理設定を確認できません。"
    );
    let requirements = &requirements["requirements"];
    let mut profiles = Vec::new();
    let mut cursors = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let page = native_queue::rpc_until(
            inner,
            "permissionProfile/list",
            json!({"cwd":cwd,"limit":100,"cursor":cursor}),
            deadline,
        )
        .await?;
        let rows = page["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("権限候補を確認できません。"))?;
        profiles.extend(rows.iter().cloned());
        anyhow::ensure!(
            profiles.len() <= 1000,
            "権限候補が多すぎるため読み込みを中断しました。"
        );
        match page.get("nextCursor") {
            Some(Value::Null) => break,
            Some(Value::String(next))
                if !next.is_empty() && next.len() <= 32768 && !cursors.contains(next) =>
            {
                cursors.push(next.clone());
                cursor = json!(next);
            }
            _ => anyhow::bail!("権限候補の続きを確認できません。"),
        }
    }
    let mut result = Vec::new();
    for (id, label, description, profile, approval, reviewer) in [
        (
            "read-only",
            "承認を求める",
            "読み取りを基本にし、編集やネット利用は承認を求めます。",
            ":read-only",
            "on-request",
            "user",
        ),
        (
            "standard",
            "標準",
            "作業範囲内の編集を許可し、範囲外の操作は承認を求めます。",
            ":workspace",
            "on-request",
            "user",
        ),
        (
            "auto-review",
            "代わりに承認",
            "標準の作業範囲を使い、承認が必要な操作を自動レビューへ渡します。",
            ":workspace",
            "on-request",
            "auto_review",
        ),
        (
            "full-access",
            "フルアクセス",
            "ファイルとネットワークへのアクセス制限を外します。",
            ":danger-full-access",
            "never",
            "user",
        ),
    ] {
        let available = profiles.iter().find(|p| p["id"] == profile);
        let reason = if available.is_none() {
            Some("この Codex では対応する権限を確認できません。")
        } else if available.is_some_and(|p| p["allowed"] != true)
            || !allowed(requirements, "allowedApprovalPolicies", approval)
            || !allowed(requirements, "allowedApprovalsReviewers", reviewer)
            || (reviewer == "auto_review"
                && (requirements["featureRequirements"]["guardian_approval"] == false
                    || requirements["featureRequirements"]["auto_review"] == false))
        {
            Some("管理設定により選択できません。")
        } else {
            None
        };
        result.push(Choice {
            view: CodexPermissionChoice {
                id: id.into(),
                label: label.into(),
                description: description.into(),
                disabled_reason: reason.map(str::to_owned),
            },
            profile: profile.into(),
            approval: Some(approval),
            reviewer: Some(reviewer),
        });
    }
    for profile in profiles {
        let Some(id) = profile["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 32768 && !id.starts_with(':'))
        else {
            continue;
        };
        result.push(Choice {
            view: CodexPermissionChoice {
                id: format!("profile:{id}"),
                label: format!("カスタム: {id}"),
                description: profile["description"]
                    .as_str()
                    .filter(|s| s.len() <= 32768)
                    .unwrap_or("名前付きプロファイル。承認方法と承認先は現在の設定を保持します。")
                    .into(),
                disabled_reason: (profile["allowed"] != true)
                    .then(|| "管理設定により選択できません。".into()),
            },
            profile: id.into(),
            approval: None,
            reviewer: None,
        });
    }
    Ok(result)
}

pub(super) async fn control(
    inner: Arc<RpcInner>,
    thread: String,
    action: Value,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(25);
    // GUI が切断しても書き込み途中で future を破棄しない。
    tokio::spawn(async move {
        let change = action["kind"] == "permissions";
        if change && action["choice"] == "full-access" {
            anyhow::ensure!(
                action["confirmed"] == true,
                "フルアクセスの適用範囲を確認してください。"
            );
        }
        {
            let mut state = inner.state.lock().expect("rpc state lock");
            anyhow::ensure!(
                !state.dead && state.thread_id.as_deref() == Some(&thread),
                "会話が切り替わっています。"
            );
            anyhow::ensure!(
                !state.turn_active
                    && state.queue.is_empty()
                    && state.native_queue.items.is_empty()
                    && !state.queue_busy
                    && !state.queue_dirty
                    && !state.queue_refreshing
                    && state.native_queue.ready,
                "応答と待機入力の完了後に権限を変更してください。"
            );
            anyhow::ensure!(
                state.config.runtime.is_some(),
                "実効権限の取得完了後に変更してください。"
            );
            state.queue_busy = true;
            state.config.permission_choices = None;
            let event = RpcInner::config_event(&state);
            inner.emit_locked(&mut state, event);
        }
        let mut update_attempted = false;
        let result = async {
            let choices = choices(&inner, deadline).await?;
            {
                let mut state = inner.state.lock().expect("rpc state lock");
                anyhow::ensure!(
                    !state.dead && state.thread_id.as_deref() == Some(&thread),
                    "設定確認中に接続が終了しました。"
                );
                state.config.permission_choices =
                    Some(choices.iter().map(|c| c.view.clone()).collect());
                let event = RpcInner::config_event(&state);
                inner.emit_locked(&mut state, event);
            }
            if !change {
                return Ok(());
            }
            let choice = choices
                .iter()
                .find(|c| action["choice"] == c.view.id)
                .ok_or_else(|| anyhow::anyhow!("未対応の権限です。候補を開き直してください。"))?;
            anyhow::ensure!(
                choice.view.disabled_reason.is_none(),
                "{}",
                choice
                    .view
                    .disabled_reason
                    .as_deref()
                    .unwrap_or("選択できません。")
            );
            let (approval, reviewer) = {
                let state = inner.state.lock().expect("rpc state lock");
                anyhow::ensure!(
                    !state.dead
                        && state.thread_id.as_deref() == Some(&thread)
                        && !state.turn_active
                        && state.queue.is_empty()
                        && state.native_queue.items.is_empty()
                        && !state.queue_dirty
                        && !state.queue_refreshing
                        && state.native_queue.ready,
                    "候補の確認中に会話が更新されました。応答と待機入力の完了後に変更してください。"
                );
                let runtime = state
                    .config
                    .runtime
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("実効権限が未確認です。"))?;
                // 同値更新には native 通知が来ない。確認済みの組み込み設定なら
                // 変更要求を送らず完了する（名前付き profile の再読込は省略しない）。
                if runtime.preset.as_deref() == Some(choice.view.id.as_str()) {
                    return Ok(());
                }
                (
                    choice
                        .approval
                        .map(str::to_owned)
                        .unwrap_or_else(|| runtime.approval.clone()),
                    choice
                        .reviewer
                        .map(str::to_owned)
                        .or_else(|| runtime.reviewer.clone()),
                )
            };
            let mut params = json!({"threadId":thread,"permissions":choice.profile});
            if let Some(approval) = choice.approval {
                params["approvalPolicy"] = json!(approval);
            }
            if let Some(reviewer) = choice.reviewer {
                params["approvalsReviewer"] = json!(reviewer);
            }
            let mut events = inner.event_tx.subscribe();
            update_attempted = true;
            native_queue::rpc_until(&inner, "thread/settings/update", params, deadline).await?;
            // キャッシュ一致では成功にしない。購読開始後に届いた native 通知が必要。
            tokio::time::timeout_at(deadline, async {
                loop {
                    let event = events
                        .recv()
                        .await
                        .map_err(|_| anyhow::anyhow!("権限変更の通知を確認できません。"))?;
                    let state = inner.state.lock().expect("rpc state lock");
                    anyhow::ensure!(
                        !state.dead && state.thread_id.as_deref() == Some(&thread),
                        "設定変更中に接続が終了しました。"
                    );
                    if let ConversationEvent::CodexConfig {
                        config: Some(config),
                        ..
                    } = event
                        && let Some(runtime) = config.runtime
                        && runtime.profile.as_deref() == Some(&choice.profile)
                        && runtime.approval == approval
                        && runtime.reviewer == reviewer
                    {
                        return Ok(());
                    }
                }
            })
            .await
            .map_err(|_| {
                anyhow::anyhow!("権限変更の実効値を確認できません。自動再送はしていません。")
            })?
        }
        .await;
        let mut state = inner.state.lock().expect("rpc state lock");
        state.queue_busy = false;
        if result.is_err() && update_attempted {
            // 遅れて反映される可能性があるため、以前の権限を現在値として残さない。
            state.config.runtime = None;
            let event = RpcInner::config_event(&state);
            inner.emit_locked(&mut state, event);
        }
        result
    })
    .await?
}
