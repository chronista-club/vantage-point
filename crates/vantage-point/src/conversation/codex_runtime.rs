//! Effective settings are native-owned. Changing collaboration mode never changes permissions.
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::event::CodexRuntime;
use super::{RpcInner, native_queue};

pub(super) fn parse(value: &Value) -> CodexRuntime {
    let sandbox = value.get("sandboxPolicy").unwrap_or(&value["sandbox"]);
    let text = |v: &Value| v.as_str().filter(|s| s.len() <= 32768).map(str::to_owned);
    let approval = text(&value["approvalPolicy"])
        .or_else(|| {
            value["approvalPolicy"]
                .is_object()
                .then(|| value["approvalPolicy"].to_string())
                .filter(|s| s.len() <= 4096)
        })
        .unwrap_or_else(|| "未確認".into());
    let mut writable_roots: Vec<String> = sandbox["writableRoots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(text)
        .collect();
    // workspaceWrite implicitly includes cwd as well as the explicit extra roots.
    if sandbox["type"] == "workspaceWrite"
        && let Some(cwd) = text(&value["cwd"])
        && !writable_roots.contains(&cwd)
    {
        writable_roots.insert(0, cwd);
    }
    CodexRuntime {
        approval,
        sandbox: text(&sandbox["type"]).unwrap_or_else(|| "未確認".into()),
        network_access: sandbox["networkAccess"].as_bool().or_else(
            || match sandbox["networkAccess"].as_str() {
                Some("enabled") => Some(true),
                Some("restricted") => Some(false),
                _ => None,
            },
        ),
        writable_roots,
        profile: text(&value["activePermissionProfile"]["id"]),
        mode: text(&value["collaborationMode"]["mode"]),
    }
}

pub(super) async fn change_mode(
    inner: Arc<RpcInner>,
    thread: String,
    mode: String,
) -> anyhow::Result<()> {
    // All RPCs and the effective-value notification share a budget below the daemon's 30s limit.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(25);
    // The operation survives loss of its GUI caller, just like native Queue mutations.
    tokio::spawn(async move {
        let (model, effort) = {
            let mut state = inner.state.lock().expect("rpc state lock");
            anyhow::ensure!(
                !state.dead && state.thread_id.as_deref() == Some(&thread),
                "会話が切り替わっています。現在の表示を確認してください。"
            );
            anyhow::ensure!(
                matches!(mode.as_str(), "plan" | "default"),
                "未対応のモードです。"
            );
            anyhow::ensure!(
                !state.turn_active
                    && state.queue.is_empty()
                    && state.native_queue.items.is_empty()
                    && !state.queue_busy
                    && !state.queue_refreshing
                    && state.native_queue.ready,
                "応答と待機入力の完了後にモードを変更してください。"
            );
            let model = state
                .config
                .selection
                .as_ref()
                .map(|s| s.model.clone())
                .or_else(|| state.config.model.clone())
                .ok_or_else(|| anyhow::anyhow!("モデルの取得完了後に変更してください。"))?;
            let effort = state
                .config
                .selection
                .as_ref()
                .map(|s| s.effort.clone())
                .or_else(|| state.config.effort.clone());
            state.queue_busy = true;
            (model, effort)
        };
        let result = async {
            let modes =
                native_queue::rpc_until(&inner, "collaborationMode/list", json!({}), deadline)
                    .await?;
            anyhow::ensure!(
                modes["data"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|row| row["mode"] == mode)),
                "この Codex では選択したモードを確認できません。"
            );
            let mut events = inner.event_tx.subscribe();
            native_queue::rpc_until(
                &inner,
                "thread/settings/update",
                json!({
                    "threadId":thread,"collaborationMode":{"mode":mode,"settings":{
                        "model":model,"reasoning_effort":effort,"developer_instructions":null
                    }}
                }),
                deadline,
            )
            .await?;
            tokio::time::timeout_at(deadline, async {
                loop {
                    {
                        let state = inner.state.lock().expect("rpc state lock");
                        anyhow::ensure!(
                            !state.dead && state.thread_id.as_deref() == Some(&thread),
                            "設定変更中に接続が終了しました。"
                        );
                        if state
                            .config
                            .runtime
                            .as_ref()
                            .and_then(|r| r.mode.as_deref())
                            == Some(&mode)
                        {
                            return Ok(());
                        }
                    }
                    events
                        .recv()
                        .await
                        .map_err(|_| anyhow::anyhow!("設定通知を確認できません。"))?;
                }
            })
            .await
            .map_err(|_| {
                anyhow::anyhow!("モード変更の実効値を確認できません。自動再送はしていません。")
            })?
        }
        .await;
        inner.state.lock().expect("rpc state lock").queue_busy = false;
        result
    })
    .await?
}
