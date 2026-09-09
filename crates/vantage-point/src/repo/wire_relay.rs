//! wire relay — wiremsg の repo 側 proxy 層（R2-a: daemon 中央 store への relay、doc 61）。
//!
//! store 直結のロジックは daemon/wire_ops.rs (daemon 側) に移設済。 repo の責務は
//! 「アドレス正規化 (N1) → daemon へ relay」 のみ。
//!
//! 受付は `unison_server::dispatch_repo_method`。旧 HTTP wrapper（`http/health.rs`）は撤去済で、呼び手は QUIC dispatch だけ。

use super::state::AppState;

/// agent address を canonical (qualified) 形に正規化する (wiremsg N1、 refactor R1 PR-B)
///
/// bare `"agent"` を qualified (`agent@<repo>`) に正規化する。
///
/// 現行 MCP (`SelfLane::from_address`) は main も canonical `agent@<repo>` を
/// 自前で送るため、本関数は実質 **冪等な素通し + 後方互換 (旧 client / bare 送信者) 用の
/// 防御層**。bare を残す理由: 旧 bare 送信が来ても store 識別子を qualified 一本に揃え、
/// cross-process 返信 (`agent@<repo>` 宛 forward) が bare query と完全一致せず届かない
/// バグ (B2、 レビュー mem_1CbuxQuNRwHBiZgBVUWVfN) を防ぐため。
/// bare 以外 (qualified / board@... / runner@... 等) はそのまま返す。
///
/// ⚠️ 正規化先 `self_repo` は「繋いだ repo の repo」なので、bare のままだと誤 repo 接続で
/// identity が化ける (= 旧 main バグの根)。だから identity の SSOT は MCP 側 canonical
/// 送出に移した。本関数は qualified を受けたら何もしない (= repo 非依存) のが正常運用。
fn normalize_agent_addr(addr: &str, self_repo: &str) -> String {
    if addr == "agent" {
        format!("agent@{}", self_repo)
    } else {
        addr.to_string()
    }
}

/// wiremsg を送信する (R2-a: daemon 中央 store への proxy)
///
/// payload: `{ from, to: [..], body, reply_to? }`
///
/// repo の責務はアドレス正規化 (N1: bare `"agent"` → `"agent@<self_repo>"`) のみ。
/// 保存・notify・local_seq 採番・body coerce は全て daemon 側
/// ([`crate::daemon::wire_ops`])。 cross-process forward は中央化で概念ごと消滅。
pub(crate) async fn handle_wire_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_send: 'from' required".to_string())?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| normalize_agent_addr(s, &state.repo_name))
                .collect()
        })
        .unwrap_or_default();
    let mut forwarded = serde_json::json!({
        "from": from,
        "to": to,
    });
    if let Some(body) = payload.get("body") {
        forwarded["body"] = body.clone();
    }
    if let Some(reply_to) = payload.get("reply_to") {
        forwarded["reply_to"] = reply_to.clone();
    }
    super::daemon_wire::call("/api/wire/send", forwarded).await
}

/// wiremsg を受信する (R2-a: daemon 中央 store への proxy、 long-poll は daemon 側)
///
/// payload: `{ agent, timeout? }` — timeout の clamp (default 5s / max 30s) も daemon 側。
pub(crate) async fn handle_wire_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?;
    let timeout = payload.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);
    super::daemon_wire::call(
        "/api/wire/recv",
        serde_json::json!({ "agent": agent, "timeout": timeout }),
    )
    .await
}

/// wiremsg の ancestor-chain (系譜) を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ message_id }` — agent 文脈不要のため正規化なしで relay。
pub(crate) async fn handle_wire_thread(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _ = state; // thread は repo 文脈 (正規化) 不要。 signature は他 handler と統一
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/thread",
        serde_json::json!({ "message_id": message_id }),
    )
    .await
}

/// wiremsg の agent 関与最新 message を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の 5-state FSM derive で使う。
pub(crate) async fn handle_wire_latest_msg(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_latest_msg: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/latest-msg",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の agent 発 未 ack needs_user を取得する (daemon proxy、 read-only)
///
/// payload: `{ agent }` → `{ status, message }`。 `flow_progress` の `AwaitingUser` 判定で使う。
pub(crate) async fn handle_wire_needs_user_pending(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_needs_user_pending: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/needs-user-pending",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の per-agent 未読 count を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の集約 view / `wire_inbox` MCP tool で使う。
pub(crate) async fn handle_wire_unread_count(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_unread_count: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/unread-count",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg を ack する (R2-a 新設、 決定 D3: cursor 非破壊の ack 台帳への proxy)
///
/// payload: `{ message_id, agent }` → `{ status, acked }`
pub(crate) async fn handle_wire_ack(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::normalize_agent_addr;

    #[test]
    fn normalize_bare_agent_to_qualified() {
        assert_eq!(normalize_agent_addr("agent", "vp"), "agent@vp");
    }

    #[test]
    fn normalize_keeps_qualified_and_other_addrs() {
        assert_eq!(normalize_agent_addr("agent@vp", "vp"), "agent@vp");
        assert_eq!(normalize_agent_addr("agent@other", "vp"), "agent@other");
        assert_eq!(normalize_agent_addr("agent@vp/sub", "vp"), "agent@vp/sub");
        assert_eq!(normalize_agent_addr("board@vp", "vp"), "board@vp");
    }
}
