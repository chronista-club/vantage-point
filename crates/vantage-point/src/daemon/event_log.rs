//! Event log — agent の episodic memory（rebuild Epic L2 / doc 27 §5-3）
//!
//! ## 役割
//! daemon が build done / test fail / device plug / lane lifecycle 等の event を 1 本の
//! monotonic な log に集約し、`vp events --since <cursor>` で配る。これにより agent（Claude）の
//! **行動間 blind**（前回の action から今までに何が起きたか見えない）を解消する。
//! doc 27 §5「agent = first-class Daemon surface」の observation 経路の 1 つ。
//!
//! ## 設計（MVP）
//! - **in-memory ring**: daemon が always-on（L1 LaunchAgent、#617）になったので、ring でも実質
//!   「session 跨ぎの episodic memory」になる（agent は同じ常駐 daemon に再接続するため log が生き続ける）。
//!   durable 永続（SurrealDB）は follow-up。
//! - **cursor = monotonic u64 `seq`**: `--since N` で `seq > N` を古い順に返す。agent は見た最大 seq を
//!   記録して次回 `--since <それ>` で差分だけ取る。
//! - **汎用 emit**: 誰でも（build task / claude hook / agent / Daemon 内部 feed）`kind` + 任意 `data` で
//!   push できる substrate。typed payload を強制しない（log は free-form、意味付けは kind 文字列）。

use std::collections::VecDeque;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// ring の最大保持件数（超過分は古い方から evict）。always-on daemon の bounded growth 担保。
const RING_CAPACITY: usize = 2000;

/// log に積まれた 1 event。
///
/// `seq` が cursor（monotonic、1 始まり）。`kind` が意味（`"process.up"` / `"build.done"` 等の
/// dotted 文字列、open registry）。`data` は free-form payload。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedEvent {
    /// monotonic cursor（1 始まり、emit ごとに +1）。`--since` の基準。
    pub seq: u64,
    /// 発生時刻（ISO 8601 / RFC 3339）。
    pub ts: String,
    /// event 種別（dotted、例: `"process.up"` / `"process.down"` / `"build.done"` / `"test.fail"`）。
    pub kind: String,
    /// 発生元（任意、例: `"claude@vantage-point/root"` や `"daemon"`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 任意 payload（JSON、kind ごとに形は自由）。
    #[serde(default)]
    pub data: serde_json::Value,
}

/// daemon が保持する event log 本体（in-memory ring + monotonic seq）。
///
/// `DaemonState` と Arc 共有し、channel handler（emit/query）と auto-feed task が同じ実体を触る。
#[derive(Clone, Default)]
pub struct EventLog {
    inner: Arc<RwLock<EventLogInner>>,
}

#[derive(Default)]
struct EventLogInner {
    /// 直近 event の ring（cap = [`RING_CAPACITY`]）。
    ring: VecDeque<LoggedEvent>,
    /// 次に払い出す seq（1 始まり）。
    next_seq: u64,
}

impl EventLog {
    /// 空の log を作る。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(EventLogInner {
                ring: VecDeque::new(),
                next_seq: 1,
            })),
        }
    }

    /// 1 event を log に積み、払い出した `seq` を返す（action）。
    ///
    /// `ts` は呼び出し時刻で自動付与。ring が cap 超過なら最古を 1 件 evict する。
    pub async fn emit(
        &self,
        kind: impl Into<String>,
        source: Option<String>,
        data: serde_json::Value,
    ) -> u64 {
        let mut inner = self.inner.write().await;
        let seq = inner.next_seq.max(1);
        inner.next_seq = seq + 1;
        inner.ring.push_back(LoggedEvent {
            seq,
            ts: chrono::Utc::now().to_rfc3339(),
            kind: kind.into(),
            source,
            data,
        });
        while inner.ring.len() > RING_CAPACITY {
            inner.ring.pop_front();
        }
        seq
    }

    /// `since` より新しい event（`seq > since`）を古い順に最大 `limit` 件返す（calculation）。
    ///
    /// `since = 0` で ring の先頭から。`limit = 0` は「上限なし（ring 全件）」扱い。
    pub async fn query(&self, since: u64, limit: usize) -> Vec<LoggedEvent> {
        let inner = self.inner.read().await;
        let it = inner.ring.iter().filter(|e| e.seq > since).cloned();
        if limit == 0 {
            it.collect()
        } else {
            it.take(limit).collect()
        }
    }

    /// 現在の最大 seq（= 直近 event の cursor、log が空なら 0）。agent の cursor 初期化用。
    pub async fn latest_seq(&self) -> u64 {
        let inner = self.inner.read().await;
        inner.ring.back().map(|e| e.seq).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn emit_assigns_monotonic_seq_from_1() {
        let log = EventLog::new();
        assert_eq!(log.emit("a", None, json!({})).await, 1);
        assert_eq!(log.emit("b", None, json!({})).await, 2);
        assert_eq!(log.emit("c", None, json!({})).await, 3);
        assert_eq!(log.latest_seq().await, 3);
    }

    #[tokio::test]
    async fn query_since_returns_newer_in_order() {
        let log = EventLog::new();
        for k in ["a", "b", "c", "d"] {
            log.emit(k, None, json!({})).await;
        }
        let got = log.query(2, 0).await;
        let kinds: Vec<&str> = got.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["c", "d"], "seq>2 を古い順に返す");
        assert_eq!(got[0].seq, 3);
    }

    #[tokio::test]
    async fn query_respects_limit() {
        let log = EventLog::new();
        for _ in 0..5 {
            log.emit("x", None, json!({})).await;
        }
        assert_eq!(log.query(0, 2).await.len(), 2);
        assert_eq!(log.query(0, 0).await.len(), 5, "limit=0 は全件");
    }

    #[tokio::test]
    async fn ring_evicts_oldest_over_capacity() {
        let log = EventLog::new();
        for _ in 0..(RING_CAPACITY + 10) {
            log.emit("x", None, json!({})).await;
        }
        let all = log.query(0, 0).await;
        assert_eq!(all.len(), RING_CAPACITY, "cap を超えない");
        // 最古 10 件が evict され、先頭 seq は 11。
        assert_eq!(all[0].seq, 11);
        assert_eq!(log.latest_seq().await, (RING_CAPACITY + 10) as u64);
    }

    #[tokio::test]
    async fn emit_records_kind_source_data() {
        let log = EventLog::new();
        log.emit(
            "build.done",
            Some("claude@vp/root".to_string()),
            json!({"exit": 0}),
        )
        .await;
        let e = &log.query(0, 0).await[0];
        assert_eq!(e.kind, "build.done");
        assert_eq!(e.source.as_deref(), Some("claude@vp/root"));
        assert_eq!(e.data["exit"], 0);
        assert!(!e.ts.is_empty());
    }
}
