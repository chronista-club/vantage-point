//! 委譲 (delegation) の TheWorld 中央 store HTTP handler（doc 28 §4 / §6）。
//!
//! SP の `process/delegation.rs` が `world_wire::call("/api/delegation/*")` で叩く先。
//! store（`DelegationStore`、SurrealDB backing）への CRUD/遷移のみを担う — wake（誰を起こすか）
//! は SP-local の責務（store は data + transition、wake は actions として分離）。
//!
//! - `POST /api/delegation/create`         — delegate: record 作成（state=active）
//! - `POST /api/delegation/complete`       — complete: Outcome 遷移、更新後 record を返す
//! - `POST /api/delegation/respond`        — respond: Active へ戻す、更新後 record を返す
//! - `POST /api/delegation/mark_delivered` — wake の woke 結果を記録（B/C 用）
//!
//! 未知 id は `{ "error": ... }` を返す（`world_wire::call` が Err に変換 → SP handler が Err）。

use crate::capability::DelegationStore;
use crate::process::delegation::Outcome;

/// record を SP に返す共通 JSON 形（requester / doer / task で wake prompt を組む）。
fn record_json(d: &crate::process::delegation::Delegation) -> serde_json::Value {
    serde_json::json!({
        "id": d.id,
        "requester": d.requester,
        "doer": d.doer,
        "task": d.task,
        // state は serde で snake_case 文字列（"active" / "done" / "awaiting_response" ...）。
        "state": serde_json::to_value(d.state).unwrap_or(serde_json::Value::Null),
        "outcome": d.outcome.as_ref().map(|o| serde_json::to_value(o).ok()),
    })
}

/// "wire" Unison channel の `delegation/*` method dispatch (daemon `handle_wire_channel` から呼ぶ)。
///
/// 旧 `world_delegation_*_handler` (axum POST /api/delegation/*) を unison channel に移行 (doc 27 §62)。
/// wire と同じ transport (`world_wire::call`) を共有するため同 channel に相乗りする (path 分岐)。
/// `method` は path `"/api/delegation/<m>"` から prefix を剥いだ sub-part
/// (= `"create"` / `"complete"` / `"respond"` / `"poll"` / `"mark_delivered"` / `"list"`)。
///
/// store への CRUD/遷移のみ（wake = 誰を起こすか は SP-local の責務）。エラーは Err(String) で返り、
/// channel handler が `{"error": ...}` フレームに詰める（未知 id も Err → SP handler が Err 化、旧挙動を保持）。
pub(crate) async fn dispatch_delegation(
    store: &DelegationStore,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        // delegate: record 作成（id は SP 採番済を受ける）。
        "create" => {
            let (id, requester, doer, task) = match (
                payload.get("id").and_then(|v| v.as_str()),
                payload.get("requester").and_then(|v| v.as_str()),
                payload.get("doer").and_then(|v| v.as_str()),
                payload.get("task").and_then(|v| v.as_str()),
            ) {
                (Some(i), Some(r), Some(d), Some(t)) => (i, r, d, t),
                _ => return Err("delegation create: id/requester/doer/task required".to_string()),
            };
            store
                .create(id, requester, doer, task)
                .await
                .map(|d| record_json(&d))
                .map_err(|e| format!("delegation create: {e}"))
        }
        // complete: Outcome 遷移、更新後 record を返す。
        "complete" => {
            let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
                return Err("delegation complete: 'id' required".to_string());
            };
            let outcome: Outcome = match payload
                .get("outcome")
                .cloned()
                .map(serde_json::from_value::<Outcome>)
            {
                Some(Ok(o)) => o,
                _ => return Err("delegation complete: invalid outcome".to_string()),
            };
            match store.apply_complete(id, &outcome).await {
                Ok(Some(d)) => Ok(record_json(&d)),
                Ok(None) => Err(format!("complete: unknown delegation id: {id}")),
                Err(e) => Err(format!("delegation complete: {e}")),
            }
        }
        // respond: Active へ戻す、更新後 record を返す。
        "respond" => {
            let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
                return Err("delegation respond: 'id' required".to_string());
            };
            match store.apply_respond(id).await {
                Ok(Some(d)) => Ok(record_json(&d)),
                Ok(None) => Err(format!("respond: unknown delegation id: {id}")),
                Err(e) => Err(format!("delegation respond: {e}")),
            }
        }
        // pull-hook（C）: agent 関与の undelivered 委譲を返す（`vp wire hook-check` がターン頭に叩く）。
        "poll" => {
            let Some(agent) = payload.get("agent").and_then(|v| v.as_str()) else {
                return Err("delegation poll: 'agent' required".to_string());
            };
            store
                .poll_for_agent(agent)
                .await
                .map(|ds| {
                    let arr: Vec<serde_json::Value> = ds.iter().map(record_json).collect();
                    serde_json::json!({ "delegations": arr })
                })
                .map_err(|e| format!("delegation poll: {e}"))
        }
        // 観測（Canvas Pane 可視化、doc 28 §7/§2）: 全委譲を created_at 昇順で返す read-only。
        // `vp wire deleg-thread` が Canvas 表示用に叩く（書き込み・wake には一切関与しない観測専用）。
        "list" => store
            .list_all()
            .await
            .map(|ds| {
                let arr: Vec<serde_json::Value> = ds.iter().map(record_json).collect();
                serde_json::json!({ "delegations": arr })
            })
            .map_err(|e| format!("delegation list: {e}")),
        // wake の woke 結果を記録（best-effort、B/C 用）。
        "mark_delivered" => {
            let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
                return Err("delegation mark_delivered: 'id' required".to_string());
            };
            let delivered = payload
                .get("delivered")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            store
                .mark_delivered(id, delivered)
                .await
                .map(|()| serde_json::json!({ "status": "ok" }))
                .map_err(|e| format!("delegation mark_delivered: {e}"))
        }
        _ => Err(format!("不明な delegation method: {method}")),
    }
}
