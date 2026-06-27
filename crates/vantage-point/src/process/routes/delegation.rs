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

use std::sync::Arc;

use axum::{Json, extract::State};

use super::super::state::AppState;
use crate::process::delegation::Outcome;

/// store を取り出す共通前処理（World DB 接続失敗時は error JSON）。
macro_rules! delegation_store {
    ($state:expr) => {
        match $state.delegation_store.as_ref() {
            Some(s) => s,
            None => {
                return Json(serde_json::json!({
                    "error": "delegation store not initialized (TheWorld DB 接続失敗 or SP mode)"
                }));
            }
        }
    };
}

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

/// POST /api/delegation/create — delegate（id は SP 採番済を受ける）。
pub async fn world_delegation_create_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let store = delegation_store!(state);
    let (id, requester, doer, task) = match (
        payload.get("id").and_then(|v| v.as_str()),
        payload.get("requester").and_then(|v| v.as_str()),
        payload.get("doer").and_then(|v| v.as_str()),
        payload.get("task").and_then(|v| v.as_str()),
    ) {
        (Some(i), Some(r), Some(d), Some(t)) => (i, r, d, t),
        _ => {
            return Json(
                serde_json::json!({ "error": "delegation create: id/requester/doer/task required" }),
            );
        }
    };
    match store.create(id, requester, doer, task).await {
        Ok(d) => Json(record_json(&d)),
        Err(e) => Json(serde_json::json!({ "error": format!("delegation create: {e}") })),
    }
}

/// POST /api/delegation/complete — complete（Outcome 遷移、更新後 record を返す）。
pub async fn world_delegation_complete_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let store = delegation_store!(state);
    let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
        return Json(serde_json::json!({ "error": "delegation complete: 'id' required" }));
    };
    let outcome: Outcome = match payload
        .get("outcome")
        .cloned()
        .map(serde_json::from_value::<Outcome>)
    {
        Some(Ok(o)) => o,
        _ => {
            return Json(serde_json::json!({ "error": "delegation complete: invalid outcome" }));
        }
    };
    match store.apply_complete(id, &outcome).await {
        Ok(Some(d)) => Json(record_json(&d)),
        Ok(None) => {
            Json(serde_json::json!({ "error": format!("complete: unknown delegation id: {id}") }))
        }
        Err(e) => Json(serde_json::json!({ "error": format!("delegation complete: {e}") })),
    }
}

/// POST /api/delegation/respond — respond（Active へ戻す、更新後 record を返す）。
pub async fn world_delegation_respond_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let store = delegation_store!(state);
    let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
        return Json(serde_json::json!({ "error": "delegation respond: 'id' required" }));
    };
    match store.apply_respond(id).await {
        Ok(Some(d)) => Json(record_json(&d)),
        Ok(None) => {
            Json(serde_json::json!({ "error": format!("respond: unknown delegation id: {id}") }))
        }
        Err(e) => Json(serde_json::json!({ "error": format!("delegation respond: {e}") })),
    }
}

/// POST /api/delegation/poll — pull-hook（C）: agent 関与の undelivered 委譲を返す。
///
/// `vp wire hook-check` がターン頭に叩く。`{delegations: [record...]}` を返す（空配列もあり）。
pub async fn world_delegation_poll_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let store = delegation_store!(state);
    let Some(agent) = payload.get("agent").and_then(|v| v.as_str()) else {
        return Json(serde_json::json!({ "error": "delegation poll: 'agent' required" }));
    };
    match store.poll_for_agent(agent).await {
        Ok(ds) => {
            let arr: Vec<serde_json::Value> = ds.iter().map(record_json).collect();
            Json(serde_json::json!({ "delegations": arr }))
        }
        Err(e) => Json(serde_json::json!({ "error": format!("delegation poll: {e}") })),
    }
}

/// POST /api/delegation/mark_delivered — wake の woke 結果を記録（best-effort）。
pub async fn world_delegation_mark_delivered_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let store = delegation_store!(state);
    let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
        return Json(serde_json::json!({ "error": "delegation mark_delivered: 'id' required" }));
    };
    let delivered = payload
        .get("delivered")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match store.mark_delivered(id, delivered).await {
        Ok(()) => Json(serde_json::json!({ "status": "ok" })),
        Err(e) => Json(serde_json::json!({ "error": format!("delegation mark_delivered: {e}") })),
    }
}
