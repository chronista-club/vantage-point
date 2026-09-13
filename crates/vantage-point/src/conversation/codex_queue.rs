//! Native Queue is the sole owner of accepted waiting input. VP only caches a view.
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::event::{CodexQueuedInput, ConversationEvent};
use super::{ReqKind, RpcInner};

pub(super) async fn rpc(inner: &RpcInner, method: &str, params: Value) -> anyhow::Result<Value> {
    let (id, receive) = {
        let mut state = inner.state.lock().expect("rpc state lock");
        anyhow::ensure!(!state.dead, "Codex host は終了しています");
        let id = state.alloc(ReqKind::QueueRpc);
        let (send, receive) = tokio::sync::oneshot::channel();
        state.reply_waiters.insert(id, send);
        (id, receive)
    };
    let mut write_completed = false;
    let response = tokio::time::timeout(std::time::Duration::from_secs(35), async {
        inner
            .write_line(&json!({"id":id,"method":method,"params":params}).to_string())
            .await
            .ok()?;
        write_completed = true;
        receive.await.ok()
    })
    .await
    .ok()
    .flatten();
    {
        let mut state = inner.state.lock().expect("rpc state lock");
        state.pending.remove(&id);
        state.reply_waiters.remove(&id);
    }
    if !write_completed {
        retire_broken_writer(inner);
    }
    let response = response.ok_or_else(|| {
        anyhow::anyhow!(
            "送信結果が不明です。自動再送はしていません。待機一覧と会話履歴を確認してください。"
        )
    })?;
    if let Some(error) = response.get("error") {
        anyhow::bail!(
            "Codex が操作を受け付けませんでした: {}",
            super::error_message(error)
        );
    }
    response.get("result").cloned().ok_or_else(|| {
        anyhow::anyhow!("Codex の受理応答を確認できません。待機一覧と会話履歴を確認してください。")
    })
}

/// A cancelled write may leave a partial JSONL record. Never reuse that stream.
fn retire_broken_writer(inner: &RpcInner) {
    {
        let mut state = inner.state.lock().expect("rpc state lock");
        if state.dead {
            return;
        }
        state.dead = true;
        state.turn_active = false;
        state.turn_id = None;
        state.native_queue.ready = false;
        state.native_queue.turn_id = None;
        state.interactions.clear();
        state.async_questions.disconnect();
        state.reply_waiters.clear();
        state.reply_clients.clear();
        state.pending.clear();
        let interactions = state.interaction_snapshot();
        inner.emit_locked(&mut state, interactions);
        inner.emit_locked(&mut state, ConversationEvent::EngineExited {
            message: "Codex への書き込みが完了せず接続を終了しました。送信結果は不明です。待機一覧と会話履歴を確認してください。".into(),
        });
    }
    // No await on stdin: another stalled writer may still own that mutex.
    if let Some(child) = inner.child.lock().expect("child lock").as_mut() {
        let _ = child.start_kill();
    }
}

pub(super) async fn control(
    inner: Arc<RpcInner>,
    thread: String,
    action: Value,
) -> anyhow::Result<()> {
    // Dropping the GUI/RPC caller must not cancel a write halfway through delivery.
    tokio::spawn(async move {
        let (method, params) = {
            let mut state = inner.state.lock().expect("rpc state lock");
            anyhow::ensure!(
                !state.dead && state.thread_id.as_deref() == Some(&thread),
                "会話が切り替わっています。現在の表示を確認してください。"
            );
            let kind = field(&action, "kind")?;
            if kind == "refresh" {
                anyhow::ensure!(
                    !state.queue_busy,
                    "操作の完了後に一覧を再取得してください。"
                );
                state.queue_dirty = true;
                state.native_queue.ready = false;
                drop(state);
                refresh(&inner);
                return Ok(());
            }
            anyhow::ensure!(
                !state.queue_busy && state.native_queue.ready,
                "待機一覧の更新完了後に操作してください。"
            );
            let mut params = json!({"threadId":thread});
            let method = match kind {
                "add" | "steer" => {
                    params["input"] = input(&action)?;
                    params["clientUserMessageId"] = field(&action, "client_id")?.into();
                    if kind == "steer" {
                        let turn = field(&action, "turn_id")?;
                        anyhow::ensure!(
                            state.turn_active && state.turn_id.as_deref() == Some(turn),
                            "宛先ターンは終了または変更されています。入力は送信されていません。"
                        );
                        params["expectedTurnId"] = turn.into();
                        "turn/steer"
                    } else {
                        "thread/queue/add"
                    }
                }
                "update" | "delete" | "start" => {
                    let id = field(&action, "id")?;
                    let item = state
                        .native_queue
                        .items
                        .iter()
                        .find(|item| item.id == id)
                        .ok_or_else(|| {
                            anyhow::anyhow!("待機項目は既に開始または削除されています。")
                        })?;
                    params["queuedSubmissionId"] = id.into();
                    match kind {
                        "update" => {
                            anyhow::ensure!(item.editable, "この形式の待機入力は編集できません。");
                            params["input"] = input(&action)?;
                            "thread/queue/update"
                        }
                        "start" => {
                            anyhow::ensure!(
                                !state.turn_active,
                                "現在の応答の完了後に再開してください。"
                            );
                            "thread/queue/start"
                        }
                        _ => "thread/queue/delete",
                    }
                }
                "reorder" => {
                    let ids = action["ids"]
                        .as_array()
                        .ok_or_else(|| anyhow::anyhow!("順序が不正です"))?;
                    let actual: std::collections::HashSet<_> =
                        ids.iter().filter_map(Value::as_str).collect();
                    let expected: std::collections::HashSet<_> = state
                        .native_queue
                        .items
                        .iter()
                        .map(|i| i.id.as_str())
                        .collect();
                    anyhow::ensure!(
                        ids.len() == actual.len() && actual == expected,
                        "待機一覧が変更されています。最新の一覧で並べ替えてください。"
                    );
                    params["queuedSubmissionIds"] = action["ids"].clone();
                    "thread/queue/reorder"
                }
                _ => anyhow::bail!("未対応の入力操作です"),
            };
            state.queue_busy = true;
            (method, params)
        };
        let result = rpc(&inner, method, params).await.and_then(|result| {
            let confirmed = match method {
                "turn/steer" => result["turnId"].as_str() == action["turn_id"].as_str(),
                "thread/queue/start" => result["turn"]["id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()),
                "thread/queue/add" | "thread/queue/update" => result["queuedSubmission"]["id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()),
                "thread/queue/delete" => result["deleted"] == true,
                "thread/queue/reorder" => result.is_object(),
                _ => false,
            };
            anyhow::ensure!(
                confirmed,
                "操作の受理を確認できません。待機一覧と会話履歴を確認してください。"
            );
            Ok(())
        });
        {
            let mut state = inner.state.lock().expect("rpc state lock");
            state.queue_busy = false;
            state.queue_dirty = true;
            state.native_queue.ready = false;
        }
        refresh(&inner);
        result
    })
    .await?
}

fn field<'a>(action: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    action[key]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 32768)
        .ok_or_else(|| anyhow::anyhow!("入力操作の {key} が不正です"))
}

fn input(action: &Value) -> anyhow::Result<Value> {
    let text = field(action, "text")?;
    anyhow::ensure!(!text.trim().is_empty(), "入力が空です");
    Ok(json!([{"type":"text","text":text,"text_elements":[]}]))
}

/// One refresh worker per host; notifications during pagination invalidate the whole read.
pub(super) fn refresh(inner: &Arc<RpcInner>) {
    {
        let mut state = inner.state.lock().expect("rpc state lock");
        if state.dead || state.thread_id.is_none() || state.queue_refreshing || !state.queue_dirty {
            return;
        }
        state.queue_refreshing = true;
    }
    let inner = inner.clone();
    tokio::spawn(async move {
        loop {
            let thread = {
                let mut state = inner.state.lock().expect("rpc state lock");
                if state.dead {
                    state.queue_refreshing = false;
                    return;
                }
                state.queue_dirty = false;
                state.thread_id.clone().expect("thread id")
            };
            let result = list(&inner, &thread).await;
            let mut state = inner.state.lock().expect("rpc state lock");
            if state.dead || state.thread_id.as_deref() != Some(&thread) {
                state.queue_refreshing = false;
                return;
            }
            if state.queue_dirty {
                continue;
            }
            state.native_queue.thread_id = thread;
            state.native_queue.turn_id = state.turn_id.clone();
            match result {
                Ok(items) => {
                    state.native_queue.items = items;
                    state.native_queue.ready = true;
                    state.native_queue.error = None;
                }
                Err(error) => {
                    state.native_queue.ready = false;
                    state.native_queue.error = Some(error.to_string());
                }
            }
            state.queue_refreshing = false;
            let queue = state.native_queue.clone();
            inner.emit_locked(
                &mut state,
                ConversationEvent::CodexQueue {
                    queue: Some(queue),
                    request_id: None,
                    error: None,
                },
            );
            return;
        }
    });
}

async fn list(inner: &RpcInner, thread: &str) -> anyhow::Result<Vec<CodexQueuedInput>> {
    let mut cursor = Value::Null;
    let mut seen = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    let mut items = Vec::new();
    for _ in 0..16 {
        let result = rpc(
            inner,
            "thread/queue/list",
            json!({"threadId":thread,"cursor":cursor,"limit":100}),
        )
        .await?;
        for row in result["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("待機一覧の形式を確認できません"))?
        {
            let id = field(row, "id")?.to_owned();
            anyhow::ensure!(
                ids.insert(id.clone()),
                "待機一覧の重複を検出しました。更新してください。"
            );
            let input = row["input"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("待機入力の形式を確認できません"))?;
            let editable = !input.is_empty()
                && input.iter().all(|i| {
                    i["type"] == "text"
                        && i["text"].is_string()
                        && i["text_elements"].as_array().is_none_or(Vec::is_empty)
                });
            let text = input
                .iter()
                .map(|i| i["text"].as_str().unwrap_or("［添付・特殊入力］"))
                .collect::<Vec<_>>()
                .join("\n");
            items.push(CodexQueuedInput {
                id,
                client_id: field(row, "clientUserMessageId")?.to_owned(),
                text,
                editable,
            });
        }
        cursor = result["nextCursor"].clone();
        if cursor.is_null() {
            return Ok(items);
        }
        anyhow::ensure!(
            cursor.is_string() && seen.insert(cursor.to_string()),
            "待機一覧のページ情報が不正です"
        );
    }
    anyhow::bail!("待機一覧が表示上限を超えています。Console で確認してください。")
}
