//! wire（agent 間 messaging）の inbox 取得 — daemon の "wire" channel への一発 ask。
//!
//! 旧 `app/mod.rs` の `wire_fetch_payload`（棚卸し 項目 6 / 6-1 #8、2026-09-08。本文は順序付き diff で一致、
//! 差分は `pub(crate)` のみ）。lane address → wire agent address の変換は `crate::lane_address`。

use crate::daemon::conn::SharedDaemonConn;
use crate::lane_address::lane_key_to_wire_agent;

/// Wire inbox (doc 34 §4 V1): Daemon "wire" channel に read-only request を投げて
/// `{address, agent, history, unread}` payload を組み立てる (エラーは `{address, error}`)。
///
/// **wire/recv は使わない** — per-agent 単一 cursor を GUI が進めると lane の claude から
/// 未読を横取りするため、 cursor 不触りの wire/history + wire/unread-count のみを叩く。
/// `ack_message_id` が Some なら先に wire/ack を実行してから fetch する (ack → 最新状態の
/// 再描画を 1 往復に畳む)。
pub(crate) async fn wire_fetch_payload(
    mut conn: SharedDaemonConn,
    address: String,
    ack_message_id: Option<String>,
) -> serde_json::Value {
    let Some(agent) = lane_key_to_wire_agent(&address) else {
        return serde_json::json!({ "address": address, "error": "wire address を持たない lane" });
    };
    let Some(client) = conn.wait_client().await else {
        return serde_json::json!({ "address": address, "error": "Daemon 未接続" });
    };
    let channel = match client.open_channel("wire").await {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({ "address": address, "error": format!("wire channel: {e}") });
        }
    };
    if let Some(id) = ack_message_id {
        // ack は台帳の意味論どおり「処理済み宣言」。 GUI からの手動 ack は dogfood の
        // オペレーション手段 (needs_user relay 等の規約整合は doc 34 §7 で継続検討)。
        let _ = channel
            .request::<serde_json::Value, serde_json::Value>(
                "wire/ack",
                &serde_json::json!({ "message_id": id, "agent": agent }),
            )
            .await;
    }
    let history = channel
        .request::<serde_json::Value, serde_json::Value>(
            "wire/history",
            &serde_json::json!({ "agent": agent }),
        )
        .await
        .unwrap_or_else(|e| serde_json::json!({ "error": format!("wire/history: {e}") }));
    let unread = channel
        .request::<serde_json::Value, serde_json::Value>(
            "wire/unread-count",
            &serde_json::json!({ "agent": agent }),
        )
        .await
        .unwrap_or_else(|e| serde_json::json!({ "error": format!("wire/unread-count: {e}") }));
    serde_json::json!({ "address": address, "agent": agent, "history": history, "unread": unread })
}
