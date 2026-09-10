//! Editor bridge (doc 48 Phase 2) — MCP → GUI Editor Mode の request-response。
//!
//! `editor_fields` / `editor_values` / `editor_set`（doc 48）と `layout_get` / `layout_set` /
//! `layout_history`（doc 49 LE-15）は同じ配管: request_id を発行して `RepoState::editor_pending` に
//! oneshot を登録し、`RepoMessage::EditorCommand` を broadcast、GUI（vp-app）が `editor_result` で
//! 返した payload で解決する。往路（`handle_editor_command`）と復路（`handle_editor_result`）は
//! 同じ `editor_pending` を触るのでここに同居する。
//!
//! 棚卸し 9-2 段階 2（doc 63 §2、PR-S2b）: handler は `RepoState` を受け取らず、要る 2 つ
//! （`editor_pending` / `hub`）だけを束ねた [`EditorContext`] を受け取る。呼び手は
//! `RepoState::editor()` で作る。この module は `RepoState` を import しない。

use super::hub::Hub;
use crate::protocol::RepoMessage;
use std::collections::HashMap;

/// editor bridge の pending 応答 map（request_id → oneshot）。`RepoState.editor_pending` の型。
pub(crate) type EditorPending =
    tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>;

/// editor bridge が要る依存だけの借用 context（doc 63 §2 段階 2）。
///
/// `BoardContext` と同型の `Copy` 借用。2 field とも非 `Option`（`Option<&T>` は「DB 接続失敗で
/// 無い」`vpdb` 専用の形）。往路は timeout まで 1 つの借用の中で待つ（spawn しない）ので借用で足りる。
#[derive(Clone, Copy)]
pub(crate) struct EditorContext<'a> {
    /// 往路が登録し、復路が `request_id` で解決する。timeout 時は往路が remove
    pub pending: &'a EditorPending,
    /// `EditorCommand` の broadcast 先（canvas channel、非 retained event topic）
    pub hub: &'a Hub,
}

/// GUI 応答待ちの上限。MCP 側 outer timeout (5s、`quic_call`) より短くすること
/// (VP-163: server が client より長く待つと channel reset → 空振りリトライになる)。
const EDITOR_BRIDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// MCP の editor_fields / editor_values / editor_set を GUI に転送して応答を待つ。
///
/// request_id を発行して `editor_pending` に oneshot を登録し、`EditorCommand` を
/// broadcast (canvas channel、非 retained event topic)。vp-app が webview で評価した
/// 結果を `editor_result` で返すと oneshot が解決する。timeout = GUI 不在 / 対象
/// repo 未表示 / Editor Mode 未 mount。
pub(crate) async fn handle_editor_command(
    ctx: EditorContext<'_>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let op = method.strip_prefix("editor_").unwrap_or(method).to_string();
    let field_id = payload.get("id").and_then(|v| v.as_str()).map(String::from);
    let value = payload.get("value").cloned();
    if op == "set" {
        if field_id.as_deref().unwrap_or("").is_empty() {
            return Err("editor_set: id 必須".to_string());
        }
        if value.is_none() {
            return Err("editor_set: value 必須".to_string());
        }
    }

    let request_id = crate::trace_log::new_trace_id();
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    ctx.pending.lock().await.insert(request_id.clone(), tx);
    ctx.hub.broadcast(RepoMessage::EditorCommand {
        request_id: request_id.clone(),
        op,
        field_id,
        value,
    });

    match tokio::time::timeout(EDITOR_BRIDGE_TIMEOUT, rx).await {
        Ok(Ok(body)) => Ok(body),
        // timeout / sender drop: pending を掃除してから明示エラー
        // (残すと map が leak し、遅延応答が別 request に誤配されうる)
        _ => {
            ctx.pending.lock().await.remove(&request_id);
            Err(
                "editor bridge timeout — vp-app が起動して当該 repo を表示しているか確認"
                    .to_string(),
            )
        }
    }
}

/// GUI (vp-app) からの editor 応答。request_id で pending oneshot を解決する。
///
/// 不在 key = timeout 済の stale 応答。エラーにせず無視する (idempotent) —
/// GUI 側は応答の成否で挙動を変えないため。
pub(crate) async fn handle_editor_result(
    ctx: EditorContext<'_>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("editor_result: request_id 必須")?;
    let body = payload
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(tx) = ctx.pending.lock().await.remove(request_id) {
        let _ = tx.send(body);
    }
    Ok(serde_json::json!({"status": "ok"}))
}

#[cfg(test)]
mod tests {
    /// 棚卸し 9-2 段階 2（doc 63 §2）: **editor bridge の handler は `RepoState` 無しで動く。**
    ///
    /// `Mutex<HashMap>` と `Hub` だけで [`super::EditorContext`] を組み、往路 → 復路の相関を通す。
    /// `RepoState` を組まないこと自体が証明（handler が State の別 field を読み始めれば compile で落ちる）。
    /// 下の 3 本（`RepoState::editor()` 経由）は結線側の網。
    #[tokio::test]
    async fn editor_handlers_need_only_the_editor_context() {
        use super::{EditorContext, EditorPending, handle_editor_command, handle_editor_result};
        use crate::protocol::RepoMessage;
        use crate::repo::hub::Hub;

        let pending: EditorPending = tokio::sync::Mutex::new(std::collections::HashMap::new());
        let hub = Hub::new();
        let ctx = EditorContext {
            pending: &pending,
            hub: &hub,
        };
        let mut hub_rx = hub.subscribe();

        let (cmd_res, ()) = tokio::join!(
            handle_editor_command(ctx, "layout_get", serde_json::json!({})),
            async {
                let msg = hub_rx.recv().await.expect("EditorCommand broadcast");
                let RepoMessage::EditorCommand { request_id, op, .. } = msg else {
                    panic!("EditorCommand 以外が broadcast された");
                };
                assert_eq!(
                    op, "layout_get",
                    "editor_ prefix が無い method は op = method"
                );
                handle_editor_result(
                    ctx,
                    serde_json::json!({ "request_id": request_id, "payload": { "layout": "L" } }),
                )
                .await
                .expect("editor_result ok");
            }
        );
        assert_eq!(cmd_res.expect("roundtrip")["layout"], "L");
        assert!(pending.lock().await.is_empty(), "解決後の pending は空");
    }

    /// doc 48 Phase 2: editor bridge の相関 — command が pending を作り broadcast、
    /// GUI 相当の `editor_result` が request_id で解決して呼び出し元に payload が返る。
    #[tokio::test]
    async fn editor_command_roundtrip_resolves_via_result() {
        use super::{handle_editor_command, handle_editor_result};
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state().await;
        // broadcast より先に購読しておかないと EditorCommand を取りこぼす
        let mut hub_rx = state.hub.subscribe();

        let (cmd_res, ()) = tokio::join!(
            handle_editor_command(state.editor(), "editor_values", serde_json::json!({})),
            async {
                let msg = hub_rx.recv().await.expect("EditorCommand broadcast");
                let RepoMessage::EditorCommand { request_id, op, .. } = msg else {
                    panic!("EditorCommand 以外が broadcast された");
                };
                assert_eq!(op, "values");
                handle_editor_result(
                    state.editor(),
                    serde_json::json!({
                        "request_id": request_id,
                        "payload": { "values": { "sb.text.base": 13 } }
                    }),
                )
                .await
                .expect("editor_result ok");
            }
        );
        let body = cmd_res.expect("roundtrip 成功");
        assert_eq!(body["values"]["sb.text.base"], 13);
        // 解決後の pending は空 (leak しない)
        assert!(state.editor_pending.lock().await.is_empty());
    }

    /// 不在 request_id への応答 (= timeout 済 stale) はエラーにせず no-op で吸収する。
    #[tokio::test]
    async fn editor_result_with_unknown_request_id_is_noop_ok() {
        use super::handle_editor_result;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state().await;
        let r = handle_editor_result(
            state.editor(),
            serde_json::json!({"request_id": "gone", "payload": {}}),
        )
        .await;
        assert!(r.is_ok());
    }

    /// editor_set は id / value 必須 (broadcast 前に弾く = pending を作らない)。
    #[tokio::test]
    async fn editor_set_requires_id_and_value() {
        use super::handle_editor_command;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state().await;
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"id": "x"}),
            serde_json::json!({"value": 1}),
        ] {
            assert!(
                handle_editor_command(state.editor(), "editor_set", payload.clone())
                    .await
                    .is_err(),
                "payload {payload} が弾かれていない"
            );
        }
        assert!(state.editor_pending.lock().await.is_empty());
    }
}
