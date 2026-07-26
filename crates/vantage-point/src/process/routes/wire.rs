//! daemon 中央 wire store のハンドラ群 (R2-a、 設計 mem_1CbvcJj4ppU3QKH9d7xMpT)
//!
//! wiremsg R2-a で wire store は daemon (`db/machine/`) に中央化された。
//! 本 module は store 直結のロジック層 (`*_store` 関数) と、 run_daemon の Router に
//! 登録する axum wrapper (`daemon_wire_*_handler`) を提供する。 SP 側
//! (`unison_server::handle_wire_*`) はアドレス正規化のみ行い、 ここへ HTTP relay
//! する薄い proxy ([`crate::process::daemon_wire`])。
//!
//! ## アドレス規約 (N1: canonical = qualified 一本)
//!
//! 正規化 (bare `"agent"` → `"agent@<project>"`) は project 文脈を持つ SP の入口で
//! 行う。 daemon は文脈を持たないため正規化せず、 曖昧な bare `"agent"` を
//! **reject** する ([`validate_addr`])。
//!
//! ## local_seq のマシン大域単調性
//!
//! daemon が唯一の writer になったことで、 [`WiremsgStore`] の AtomicU64 採番が
//! そのまま「マシン大域で厳密単調」を満たす (設計決定 C1 の帰結、 追加コード不要)。

use crate::capability::{WireNotifier, WiremsgStore};

/// bare `"agent"` を reject する (daemon は project 文脈が無く正規化できない)
fn validate_addr(addr: &str) -> Result<(), String> {
    if addr == "agent" {
        return Err(
            "wire: bare \"agent\" は中央 store では曖昧です。 SP (port 33000 台) 経由で送るか、 \
             qualified 形 (agent@<project>) を指定してください"
                .to_string(),
        );
    }
    Ok(())
}

/// wire message の `body` を JSON object に正規化する (unison_server から R2-a で移設)。
///
/// MCP client が `body` の schema (typeless な `serde_json::Value`) を string と
/// 解釈し、 JSON object を文字列化して送ってくることがある。 string で来た場合は
/// JSON として parse し直して救済する。 最終的に object でなければ Err —
/// `wire_messages.body` は SurrealDB 上 `TYPE object` なので、 ここで弾く方が
/// SurrealDB の coerce エラーより分かりやすい。
pub(crate) fn coerce_wire_body(
    raw: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let body = match raw {
        // 省略時は空 object。
        None => return Ok(serde_json::json!({})),
        // string で来たら JSON として parse し直す (typeless schema の救済)。
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(&s)
            .map_err(|e| {
                format!(
                    "wire_send: body は JSON object であるべきですが、 string で受信し \
                     JSON としても parse できません: {e}"
                )
            })?,
        Some(v) => v,
    };
    if !body.is_object() {
        return Err(format!(
            "wire_send: body は JSON object であるべきですが object でない値を受信しました ({body})"
        ));
    }
    Ok(body)
}

/// wiremsg を送信する (= 新規 thread の root、 または `reply_to` 指定で reply)
///
/// payload: `{ from, to: [..], body, reply_to? }` — アドレスは qualified 前提
/// (bare は [`validate_addr`] で reject)。
///
/// Operations:
/// - `reply_to` なし → `WiremsgStore::send_root` (root message を INSERT、 `prev = None`)
/// - `reply_to` あり → `WiremsgStore::send_reply` (reply message を INSERT、 reply-all 展開)
/// - 送信後、 notify 対象の待機中 long-poll ([`wire_recv_store`]) を `WireNotifier` で起こす
pub(crate) async fn wire_send_store(
    store: &WiremsgStore,
    notifier: &WireNotifier,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_send: 'from' required".to_string())?
        .to_string();
    validate_addr(&from)?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    for addr in &to {
        validate_addr(addr)?;
    }
    let body = coerce_wire_body(payload.get("body").cloned())?;
    let reply_to = payload
        .get("reply_to")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    match reply_to {
        // ---- reply ----
        Some(prev_id) => {
            let msg = store
                .send_reply(&from, &to, body, &prev_id)
                .await
                .map_err(|e| format!("wire_send (reply) failed: {e}"))?;
            // notify 対象 = reply の to。 send_reply の carry-forward で確定済の参加者集合で、
            // 送信者と left は既に除外済。 thread 全走査は不要 (prev carry-forward の帰結)。
            for agent in &msg.to {
                notifier.notify(agent).await;
            }
            Ok(serde_json::json!({
                "status": "ok",
                "id": msg.id,
                "prev": msg.prev,
                "local_seq": msg.local_seq,
                "notified": msg.to.len(),
            }))
        }
        // ---- new thread (root) ----
        None => {
            let msg = store
                .send_root(&from, &to, body)
                .await
                .map_err(|e| format!("wire_send (root) failed: {e}"))?;
            // 受信者 (= to、 送信者自身は除く) を notify
            for agent in to.iter().filter(|a| **a != from) {
                notifier.notify(agent).await;
            }
            Ok(serde_json::json!({
                "status": "ok",
                "id": msg.id,
                "prev": serde_json::Value::Null,
                "local_seq": msg.local_seq,
                "notified": to.iter().filter(|a| **a != from).count(),
            }))
        }
    }
}

/// wiremsg を受信する (= 呼び出し agent の参加 thread の未読 message を long-poll で取得)
///
/// payload: `{ agent, timeout? }` (timeout default 5s / max 30s)
///
/// Operations:
/// 1. `agent` 宛 (`to_addrs ∋ agent`) で `local_seq > read_cursor` の未読 message を取得
/// 2. 未読があれば返却 → 取得した最新 message の `local_seq` まで cursor を前進
/// 3. 未読が無ければ `WireNotifier` で待機 (timeout あり)、 notify で起床して再 poll
///
/// 取りこぼし防止のため、 `notified()` future を **store poll の前に** 生成する
/// (= `WireNotifier` の struct doc 参照)。
pub(crate) async fn wire_recv_store(
    store: &WiremsgStore,
    notifier: &WireNotifier,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?
        .to_string();
    validate_addr(&agent)?;
    // timeout は msg_recv と同じ default 5s / max 30s
    let timeout_secs = payload
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .min(30);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        // 取りこぼし防止: poll の前に待機 future を準備しておく
        // (poll と await の隙間に来た wire_send notify も拾えるようにする)。
        let notify = notifier.handle(&agent).await;
        let notified = notify.notified();
        tokio::pin!(notified);

        // 未読 message を取得 + read_cursor 前進 (store.recv が両方まとめて実施)。
        let unread = store
            .recv(&agent)
            .await
            .map_err(|e| format!("wire_recv failed: {e}"))?;

        if !unread.is_empty() {
            let value = serde_json::to_value(&unread)
                .map_err(|e| format!("wire_recv serialize failed: {e}"))?;
            return Ok(serde_json::json!({
                "messages": value,
                "count": unread.len(),
            }));
        }

        // 未読なし: deadline まで notify を待つ
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(serde_json::json!({
                "messages": [],
                "count": 0,
                "reason": "timeout",
            }));
        }
        // notify が来たら再 poll、 timeout したら次ループで deadline 判定 → timeout 返却。
        let _ = tokio::time::timeout(remaining, notified).await;
    }
}

/// wiremsg の ancestor-chain (系譜) を取得する (read-only、 cursor 不触り)
///
/// payload: `{ message_id }`。 指定 message から `prev` を root まで辿った系譜
/// (root-first・chronological) を返す。
pub(crate) async fn wire_thread_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?
        .to_string();

    let chain = store
        .ancestor_chain(&message_id)
        .await
        .map_err(|e| format!("wire_thread failed: {e}"))?;
    let value =
        serde_json::to_value(&chain).map_err(|e| format!("wire_thread serialize failed: {e}"))?;
    Ok(serde_json::json!({
        "status": "ok",
        "messages": value,
        "count": chain.len(),
    }))
}

/// wiremsg の per-agent 未読 count を取得する (read-only、 cursor 不触り)
///
/// payload: `{ agent }` → `{ status, total, by_thread }`
/// wire history handler (read-only、 GUI inbox 表示用 — doc 34 §4 V1)
///
/// payload: `{agent, limit?}`。 agent が関与する直近 message(送受信両方)を新しい順で返す。
/// 各 message に GUI 描画用の derive flag を添える:
/// - `inbound`: agent ∈ to(受信)か from == agent(送信)か
/// - `acked`: この agent が ack 済みか(command の「要 ack」badge 用)
///
/// **cursor は一切動かさない** — wire_recv と違い、 GUI が覗いても lane の claude の未読は
/// 減らない(per-agent 単一 cursor の横取り禁止)。
pub(crate) async fn wire_history_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_history: 'agent' required".to_string())?
        .to_string();
    validate_addr(&agent)?;
    // limit は 1..=100 に clamp(default 30 = GUI inbox 1 画面分)
    let limit = payload
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .clamp(1, 100);

    let msgs = store
        .history_for_agent(&agent, limit)
        .await
        .map_err(|e| format!("wire_history failed: {e}"))?;
    let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
    let acked: std::collections::HashSet<String> = store
        .acked_ids_for_agent(&agent, &ids)
        .await
        .map_err(|e| format!("wire_history acks failed: {e}"))?
        .into_iter()
        .collect();

    let messages: Vec<serde_json::Value> = msgs
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "prev": m.prev,
                "from": m.from,
                "to": m.to,
                "body": m.body,
                "created_at": m.created_at,
                "local_seq": m.local_seq,
                "inbound": m.to.contains(&agent),
                "acked": acked.contains(&m.id),
            })
        })
        .collect();
    Ok(serde_json::json!({ "agent": agent, "count": messages.len(), "messages": messages }))
}

pub(crate) async fn wire_unread_count_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_unread_count: 'agent' required".to_string())?
        .to_string();
    validate_addr(&agent)?;

    let by_thread = store
        .unread_count_by_thread(&agent)
        .await
        .map_err(|e| format!("wire_unread_count failed: {e}"))?;
    let total: u64 = by_thread.values().sum();
    Ok(serde_json::json!({
        "status": "ok",
        "total": total,
        "by_thread": by_thread,
    }))
}

/// wiremsg の agent 関与最新 message を取得する (read-only、 cursor 不触り)
///
/// payload: `{ agent }`。 「関与」 = `from_addr == agent` OR `to_addrs CONTAINS agent`。
pub(crate) async fn wire_latest_msg_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_latest_msg: 'agent' required".to_string())?
        .to_string();
    validate_addr(&agent)?;

    let latest = store
        .latest_msg_for_agent(&agent)
        .await
        .map_err(|e| format!("wire_latest_msg failed: {e}"))?;
    Ok(serde_json::json!({
        "status": "ok",
        "message": latest,
    }))
}

/// 指定 agent 発の未 ack `needs_user` wire (最新 1 件) を返す (read-only、 cursor 不触り)
///
/// payload: `{ agent }` → `{ status, message: WireMessage | null }`。
/// dev-flow FSM の `AwaitingUser` 判定入力 (LaneInfo flow_state 投影 / flow progress が使う)。
pub(crate) async fn wire_needs_user_pending_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_needs_user_pending: 'agent' required".to_string())?
        .to_string();
    validate_addr(&agent)?;

    let pending = store
        .pending_needs_user(&agent)
        .await
        .map_err(|e| format!("wire_needs_user_pending failed: {e}"))?;
    Ok(serde_json::json!({
        "status": "ok",
        "message": pending,
    }))
}

/// wiremsg を ack する (R2-a 新設、 決定 D3: cursor 非破壊の ack 台帳)
///
/// payload: `{ message_id, agent }` → `{ status, acked }` (acked: 新規 = true / 冪等 = false)
pub(crate) async fn wire_ack_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    validate_addr(agent)?;
    let newly = store
        .ack(message_id, agent)
        .await
        .map_err(|e| format!("wire_ack failed: {e}"))?;
    Ok(serde_json::json!({ "status": "ok", "acked": newly }))
}

// =============================================================================
// Unison "wire" channel dispatch (L0 portless B-4: 旧 axum wrapper を置換)
// =============================================================================

/// "wire" Unison channel の method dispatch (daemon `handle_wire_channel` から呼ぶ)。
///
/// 旧 `daemon_wire_*_handler` (axum POST /api/wire/*) を unison channel に移行したもの (doc 27 §62)。
/// `method` は `daemon_wire::call` が path `"/api/wire/<m>"` から prefix を剥いだ sub-part
/// (= `"send"` / `"recv"` / `"thread"` / `"unread-count"` / `"latest-msg"` / `"ack"`)。
///
/// store / notifier / delivery_notify は daemon が daemon process AppState と共有する Arc。
/// エラーは Err(String) で返り、channel handler が `{"error": ...}` フレームに詰める
/// (unison 慣習 — 専用 error frame なし)。
pub(crate) async fn dispatch_wire(
    store: &WiremsgStore,
    notifier: &WireNotifier,
    delivery_notify: &tokio::sync::Notify,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        "send" => {
            // R2-b: command 着信なら delivery loop を即 wake。 判定は保存前の素の payload で行う —
            // body が文字列化 JSON の場合 (coerce 救済経路) はここで拾えないが、 その場合も
            // delivery loop の 30s tick が拾うため fail-open (旧 daemon_wire_send_handler から移植)。
            let is_command = payload
                .get("body")
                .and_then(|b| b.get("category"))
                .and_then(|c| c.as_str())
                == Some("command");
            let v = wire_send_store(store, notifier, payload).await?;
            if is_command {
                delivery_notify.notify_one();
            }
            Ok(v)
        }
        "recv" => wire_recv_store(store, notifier, payload).await,
        "thread" => wire_thread_store(store, payload).await,
        "unread-count" => wire_unread_count_store(store, payload).await,
        // doc 34 §4 V1: GUI inbox 用の read-only 履歴 (cursor 不触り)
        "history" => wire_history_store(store, payload).await,
        "latest-msg" => wire_latest_msg_store(store, payload).await,
        "needs-user-pending" => wire_needs_user_pending_store(store, payload).await,
        "ack" => wire_ack_store(store, payload).await,
        _ => Err(format!("不明な wire method: {method}")),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// kv-mem の VpDb (define_schema 済 = wire_acks 含む本物の SCHEMA_SQL) 上に store を作る
    async fn mem_store() -> WiremsgStore {
        let db = crate::db::VpDb::connect_mem().await.expect("connect_mem");
        db.define_schema().await.expect("define_schema");
        WiremsgStore::new(std::sync::Arc::new(db.inner().clone()))
            .await
            .expect("WiremsgStore::new")
    }

    /// bare "agent" は中央 store で reject される (正規化は SP の責務)
    #[tokio::test]
    async fn send_store_rejects_bare_agent() {
        let store = mem_store().await;
        let notifier = WireNotifier::new();
        let err = wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent", "to": ["agent@nexus"], "body": {"k": 1}}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("bare"), "bare reject の説明を含む: {err}");
    }

    /// send → recv の round-trip が store handler 経由で閉じる
    #[tokio::test]
    async fn send_recv_roundtrip_via_store_handlers() {
        let store = mem_store().await;
        let notifier = WireNotifier::new();
        let sent = wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent@vp", "to": ["agent@nexus"], "body": {"kind": "event"}}),
        )
        .await
        .expect("send");
        assert_eq!(sent["status"], "ok");

        let recvd = wire_recv_store(
            &store,
            &notifier,
            json!({"agent": "agent@nexus", "timeout": 0}),
        )
        .await
        .expect("recv");
        assert_eq!(recvd["count"], 1);

        // drain 後の unread-count は 0
        let inbox = wire_unread_count_store(&store, json!({"agent": "agent@nexus"}))
            .await
            .expect("unread_count");
        assert_eq!(inbox["total"], 0);
    }

    /// ack handler の round-trip (新規 true → 冪等 false)
    #[tokio::test]
    async fn ack_store_roundtrip() {
        let store = mem_store().await;
        let notifier = WireNotifier::new();
        let sent = wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent@vp", "to": ["agent@nexus"], "body": {"kind": "command"}}),
        )
        .await
        .expect("send");
        let id = sent["id"].as_str().expect("id");

        let acked = wire_ack_store(&store, json!({"message_id": id, "agent": "agent@nexus"}))
            .await
            .expect("ack");
        assert_eq!(acked["acked"], true);
        let again = wire_ack_store(&store, json!({"message_id": id, "agent": "agent@nexus"}))
            .await
            .expect("再 ack");
        assert_eq!(again["acked"], false);
    }

    /// doc 34 §4 V1: history は read-only(cursor 不触り)で、 inbound / acked flag を正しく付ける
    #[tokio::test]
    async fn history_is_readonly_with_flags() {
        let store = mem_store().await;
        let notifier = WireNotifier::new();
        // 受信 1 件(command、 後で ack)+ 送信 1 件
        let sent_in = wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent@vp", "to": ["agent@nexus"], "body": {"category": "command"}}),
        )
        .await
        .expect("send inbound");
        let in_id = sent_in["id"].as_str().expect("id");
        wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent@nexus", "to": ["agent@vp"], "body": {"kind": "ack"}}),
        )
        .await
        .expect("send outbound");
        wire_ack_store(&store, json!({"message_id": in_id, "agent": "agent@nexus"}))
            .await
            .expect("ack");

        let hist = wire_history_store(&store, json!({"agent": "agent@nexus"}))
            .await
            .expect("history");
        assert_eq!(hist["count"], 2);
        let msgs = hist["messages"].as_array().expect("messages");
        // local_seq 降順 = 新しい順: [0] = 送信(outbound)、 [1] = 受信(inbound, acked)
        assert_eq!(msgs[0]["inbound"], false, "自分の送信は inbound=false");
        assert_eq!(msgs[1]["inbound"], true);
        assert_eq!(msgs[1]["acked"], true, "ack 済み flag");
        assert_eq!(msgs[1]["id"], in_id);

        // read-only の証明: history を何度呼んでも未読は減らない(cursor 不触り)
        let inbox = wire_unread_count_store(&store, json!({"agent": "agent@nexus"}))
            .await
            .expect("unread_count");
        assert_eq!(inbox["total"], 1, "history 後も未読 1 件のまま");
    }

    /// coerce_wire_body: string で来た JSON object は救済、 非 object は Err (移設に伴う回帰確認)
    #[test]
    fn coerce_wire_body_rescues_stringified_object() {
        let coerced = coerce_wire_body(Some(json!("{\"k\": 1}"))).expect("救済");
        assert_eq!(coerced, json!({"k": 1}));
        assert!(coerce_wire_body(Some(json!("[1, 2]"))).is_err());
        assert!(coerce_wire_body(Some(json!(42))).is_err());
        assert_eq!(coerce_wire_body(None).expect("省略は空 object"), json!({}));
    }
}
