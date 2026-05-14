//! VP-169 msgbox v2 — Whitesnake-primary substrate trait + impl
//!
//! SDG (doc 19) §4 で確定した DB primary 設計の **受け皿** (Phase 2 PR-1)。
//! 既存 `Router` / `Handle` (= mpsc-based) には影響なし、 新 substrate のための空間。
//!
//! ## 構成
//!
//! - [`MsgboxStore`] trait: 新 substrate の operations 定義
//! - [`WhitesnakeStore`]: SurrealDB embedded で trait を impl (= SDG「Whitesnake = primary store」)
//! - [`MsgboxStats`]: status 別 row 数 (`vp msgbox status` 用)
//!
//! ## Phase 2-5 roadmap
//!
//! | Phase | 内容 |
//! |---|---|
//! | Phase 2 (本 PR) | trait + impl + schema 受け皿、 既存挙動 0 影響 |
//! | Phase 3 | dual write (= 新規 msg を mpsc + 新 store 両方へ) + claim 機構 |
//! | Phase 4 | consumer を mpsc → 新 store に切替 |
//! | Phase 5 | 旧 mpsc + Router / Handle 削除、 用語最終 sweep |
//!
//! ## 参照
//!
//! - SDG: `docs/design/19-msgbox-whitesnake-primary.md`
//! - spike report: `docs/design/20-spike-report.md` (= 性能 + path A 確定の根拠)
//! - spike test: `crates/vantage-point/tests/spike_msgbox_live.rs` (= VP-172 で vp-db merge 後の path)

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::capability::msgbox::Message;
use crate::capability::msgbox_registry::{Address, parse_address};

/// 現在時刻 (Unix epoch ms)
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// MsgboxStats
// =============================================================================

/// `vp msgbox status` 用、 status 別 row 数
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MsgboxStats {
    pub active: u64,
    pub dead_letter: u64,
    pub archived: u64,
}

// =============================================================================
// MsgboxStore trait
// =============================================================================

/// VP-169 Whitesnake-primary substrate の operations 定義 (SDG §4 全体)
///
/// trait 化することで Phase 3 dual write 期間中に旧 / 新 substrate を並走可能にする。
/// Phase 5 で mpsc 削除後は本 trait の impl が単独 primary。
#[async_trait]
pub trait MsgboxStore: Send + Sync {
    /// msg を `msgs` table に INSERT (status='active'、 SDG §4.1)
    ///
    /// Message の `from` / `to` field を `parse_address` で分解し、 to_actor / to_lane /
    /// to_project / to_world 等を補填する。 parse error は warn log + 旧 raw addr で fallback。
    async fn insert(&self, msg: &Message) -> Result<()>;

    /// atomic claim 1 件 (= SDG §4.3 concurrent recv first-class)
    ///
    /// Path A transaction wrap (= spike Q3 finding に基づく)。 `(actor, lane)` 宛 active 未消費
    /// 未 claim row を ts ASC, id ASC で 1 件取得 + claim_id stamp。
    /// stale claim (= claimed_at + 30s < now) も再取得可能。
    async fn claim(&self, actor: &str, lane: &str, consumer_id: &str) -> Result<Option<Message>>;

    /// msg_id を consumed_at = now で mark (SDG §4.7)
    async fn mark_consumed(&self, msg_id: &str) -> Result<()>;

    /// status 別 row 数を取得 (`vp msgbox status` 用)
    async fn stats(&self) -> Result<MsgboxStats>;
}

// =============================================================================
// WhitesnakeStore — SurrealDB embedded impl
// =============================================================================

/// VP-169 primary store impl (= SDG 「Whitesnake = primary store」 物理化)
///
/// `Surreal<Any>` を Arc で持つ。 既存 `Whitesnake` (= filesystem DISC) とは module 分離で
/// 同名衝突回避、 Phase 5 で旧 Whitesnake (filesystem) を rename or 廃止予定。
///
/// 主 query path:
/// - [`insert`]: `CREATE type::record('msgs', $id) CONTENT {...}`
/// - [`claim`]: transaction wrap (BEGIN; SELECT ORDER BY LIMIT; UPDATE; COMMIT)
/// - [`mark_consumed`]: `UPDATE type::record('msgs', $id) SET consumed_at=$now`
/// - [`stats`]: `SELECT status, count() AS n FROM msgs GROUP BY status`
#[derive(Clone)]
pub struct WhitesnakeStore {
    db: Arc<Surreal<Any>>,
}

impl WhitesnakeStore {
    /// 既存 `Surreal<Any>` connection から store を構築
    pub fn new(db: Arc<Surreal<Any>>) -> Self {
        Self { db }
    }

    /// underlying connection を参照 (= LIVE Query 等の advanced 操作用)
    pub fn db(&self) -> &Surreal<Any> {
        &self.db
    }

    /// `Message::to` / `from` を分解して parsed fields を返す
    ///
    /// 戻り値: `(actor, lane, project, world)`、 parse error 時は all None で best-effort。
    fn parse_addr(
        addr: &str,
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        match parse_address(addr) {
            Ok(Address::Local { actor }) => (Some(actor), Some("lead".into()), None, None),
            Ok(Address::Port { actor, .. }) => (Some(actor), Some("lead".into()), None, None),
            Ok(Address::Project {
                actor,
                world,
                project,
                lane,
            }) => {
                let lane_str = if lane.is_empty() {
                    "lead".to_string()
                } else {
                    lane.join("/")
                };
                (Some(actor), Some(lane_str), Some(project), world)
            }
            Err(_) => (None, None, None, None),
        }
    }
}

#[async_trait]
impl MsgboxStore for WhitesnakeStore {
    async fn insert(&self, msg: &Message) -> Result<()> {
        let now = now_ms();
        let (to_actor, to_lane, to_project, to_world) = Self::parse_addr(&msg.to);
        let (from_actor, from_lane, from_project, from_world) = Self::parse_addr(&msg.from);

        let kind_str = match msg.kind {
            crate::capability::msgbox::MessageKind::Direct => "direct",
            crate::capability::msgbox::MessageKind::Notification => "notification",
            crate::capability::msgbox::MessageKind::Request => "request",
            crate::capability::msgbox::MessageKind::Response => "response",
        };

        self.db
            .query(
                r#"
                CREATE type::record('msgs', $id) CONTENT {
                    id: $id, ts: $ts, kind: $kind, payload: $payload, reply_to: $reply_to,
                    to_addr: $to_addr, to_actor: $to_actor, to_lane: $to_lane,
                    to_project: $to_project, to_world: $to_world,
                    from_addr: $from_addr, from_actor: $from_actor, from_lane: $from_lane,
                    from_project: $from_project, from_world: $from_world,
                    expires_at: $expires_at, manual_ack: $manual_ack,
                    forwarded_at: $forwarded_at, consumed_at: $consumed_at,
                    status: 'active', status_at: $now,
                    claim_id: NONE, claimed_at: NONE,
                    trace: []
                }
                "#,
            )
            .bind(("id", msg.id.clone()))
            .bind(("ts", msg.timestamp))
            .bind(("kind", kind_str.to_string()))
            .bind(("payload", msg.payload.clone()))
            .bind(("reply_to", msg.reply_to.clone()))
            .bind(("to_addr", msg.to.clone()))
            .bind(("to_actor", to_actor.unwrap_or_default()))
            .bind(("to_lane", to_lane.unwrap_or_else(|| "lead".to_string())))
            .bind(("to_project", to_project))
            .bind(("to_world", to_world))
            .bind(("from_addr", msg.from.clone()))
            .bind(("from_actor", from_actor.unwrap_or_default()))
            .bind(("from_lane", from_lane.unwrap_or_else(|| "lead".to_string())))
            .bind(("from_project", from_project))
            .bind(("from_world", from_world))
            .bind(("expires_at", msg.expires_at))
            .bind(("manual_ack", msg.manual_ack))
            .bind(("forwarded_at", msg.forwarded_at))
            .bind(("consumed_at", msg.consumed_at))
            .bind(("now", now))
            .await
            .map_err(|e| anyhow::anyhow!("msgbox_v2 insert failed: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("msgbox_v2 insert check failed: {}", e))?;
        Ok(())
    }

    async fn claim(&self, actor: &str, lane: &str, consumer_id: &str) -> Result<Option<Message>> {
        let now = now_ms();
        let mut res = self
            .db
            .query(
                r#"
                BEGIN;
                LET $candidates = SELECT * FROM msgs
                    WHERE status='active' AND to_actor = $actor AND to_lane = $lane
                      AND consumed_at IS NONE
                      AND (claim_id IS NONE OR claimed_at + 30000 < $now)
                    ORDER BY ts ASC, id ASC LIMIT 1;
                LET $target_id = $candidates[0].id;
                UPDATE $target_id SET claim_id = $cid, claimed_at = $now;
                COMMIT;
                RETURN $candidates[0];
                "#,
            )
            .bind(("actor", actor.to_string()))
            .bind(("lane", lane.to_string()))
            .bind(("cid", consumer_id.to_string()))
            .bind(("now", now))
            .await
            .map_err(|e| anyhow::anyhow!("msgbox_v2 claim failed: {}", e))?;

        // RETURN $candidates[0] は最終 statement、 take(0) で取得
        let row: Option<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("msgbox_v2 claim take failed: {}", e))?;

        let Some(row_value) = row else {
            return Ok(None);
        };
        if row_value.is_null() {
            return Ok(None);
        }

        // SurrealDB row → Message に hydrate
        let msg = Self::row_to_message(&row_value)?;
        Ok(Some(msg))
    }

    async fn mark_consumed(&self, msg_id: &str) -> Result<()> {
        let now = now_ms();
        self.db
            .query("UPDATE type::record('msgs', $id) SET consumed_at = $now")
            .bind(("id", msg_id.to_string()))
            .bind(("now", now))
            .await
            .map_err(|e| anyhow::anyhow!("msgbox_v2 mark_consumed failed: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("msgbox_v2 mark_consumed check failed: {}", e))?;
        Ok(())
    }

    async fn stats(&self) -> Result<MsgboxStats> {
        let mut res = self
            .db
            .query("SELECT status, count() AS n FROM msgs GROUP BY status")
            .await
            .map_err(|e| anyhow::anyhow!("msgbox_v2 stats failed: {}", e))?;

        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("msgbox_v2 stats take failed: {}", e))?;

        let mut stats = MsgboxStats::default();
        for row in rows {
            let status = row["status"].as_str().unwrap_or_default();
            let n = row["n"].as_u64().unwrap_or(0);
            match status {
                "active" => stats.active = n,
                "dead_letter" => stats.dead_letter = n,
                "archived" => stats.archived = n,
                _ => {} // 未知 status は無視 (= 将来拡張余地)
            }
        }
        Ok(stats)
    }
}

impl WhitesnakeStore {
    /// SurrealDB row JSON を `Message` に hydrate
    ///
    /// row は schema 通り field を持つが、 一部 field (= consumed_at NONE 等) は null 化。
    /// 既存 `Message` struct は `#[serde(default)]` pattern なので default で吸収。
    fn row_to_message(row: &serde_json::Value) -> Result<Message> {
        use crate::capability::msgbox::MessageKind;

        let kind = match row["kind"].as_str().unwrap_or("direct") {
            "notification" => MessageKind::Notification,
            "request" => MessageKind::Request,
            "response" => MessageKind::Response,
            _ => MessageKind::Direct,
        };

        // id は SurrealDB record id (`msgs:<uuid>` 形式) or string、 string 部分を抽出
        let id_raw = row["id"].clone();
        let id = if let Some(s) = id_raw.as_str() {
            // `msgs:<uuid>` → `<uuid>` に剥がす (= caller は Message.id を uuid として使う)
            s.strip_prefix("msgs:")
                .unwrap_or(s)
                .trim_matches('`')
                .to_string()
        } else {
            // SurrealDB raw record id object (= { "tb": "msgs", "id": "..." })
            row["id"]["id"]
                .as_str()
                .or_else(|| row["id"]["String"].as_str())
                .unwrap_or_default()
                .to_string()
        };

        Ok(Message {
            id,
            from: row["from_addr"].as_str().unwrap_or_default().to_string(),
            to: row["to_addr"].as_str().unwrap_or_default().to_string(),
            kind,
            payload: row["payload"].clone(),
            timestamp: row["ts"].as_u64().unwrap_or_default(),
            reply_to: row["reply_to"].as_str().map(|s| s.to_string()),
            expires_at: row["expires_at"].as_u64(),
            manual_ack: row["manual_ack"].as_bool().unwrap_or(false),
            forwarded_at: row["forwarded_at"].as_u64(),
            consumed_at: row["consumed_at"].as_u64(),
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::msgbox::{Message, MessageKind};

    /// kv-mem で msgs schema を define 済の Surreal を返す
    async fn make_test_store() -> WhitesnakeStore {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("kv-mem connect");
        db.use_ns("vp").use_db("vp").await.expect("use_ns/db");
        // vp-db SCHEMA_SQL の msgs table 部分を再現 (= テスト独立性)
        db.query(
            r#"
            DEFINE TABLE msgs SCHEMAFULL;
            DEFINE FIELD id ON msgs TYPE string;
            DEFINE FIELD ts ON msgs TYPE number;
            DEFINE FIELD kind ON msgs TYPE string DEFAULT 'direct';
            DEFINE FIELD payload ON msgs TYPE object FLEXIBLE;
            DEFINE FIELD reply_to ON msgs TYPE option<string>;
            DEFINE FIELD to_addr ON msgs TYPE string;
            DEFINE FIELD to_actor ON msgs TYPE string;
            DEFINE FIELD to_lane ON msgs TYPE string;
            DEFINE FIELD to_project ON msgs TYPE option<string>;
            DEFINE FIELD to_world ON msgs TYPE option<string>;
            DEFINE FIELD from_addr ON msgs TYPE string;
            DEFINE FIELD from_actor ON msgs TYPE string;
            DEFINE FIELD from_lane ON msgs TYPE string;
            DEFINE FIELD from_project ON msgs TYPE option<string>;
            DEFINE FIELD from_world ON msgs TYPE option<string>;
            DEFINE FIELD expires_at ON msgs TYPE option<number>;
            DEFINE FIELD manual_ack ON msgs TYPE bool DEFAULT false;
            DEFINE FIELD forwarded_at ON msgs TYPE option<number>;
            DEFINE FIELD consumed_at ON msgs TYPE option<number>;
            DEFINE FIELD status ON msgs TYPE string DEFAULT 'active';
            DEFINE FIELD status_at ON msgs TYPE number;
            DEFINE FIELD claim_id ON msgs TYPE option<string>;
            DEFINE FIELD claimed_at ON msgs TYPE option<number>;
            DEFINE FIELD trace ON msgs TYPE array DEFAULT [];
            DEFINE INDEX recv_idx ON msgs FIELDS status, to_actor, to_lane, consumed_at;
            "#,
        )
        .await
        .expect("schema query")
        .check()
        .expect("schema check");

        WhitesnakeStore::new(Arc::new(db))
    }

    #[tokio::test]
    async fn test_insert_and_stats() {
        let store = make_test_store().await;
        // payload は schema 上 TYPE object FLEXIBLE、 JSON object でラップ
        let msg = Message::new("agent@vp/lead", "agent@vp/lead", MessageKind::Direct)
            .with_payload(&serde_json::json!({ "text": "hello" }));
        store.insert(&msg).await.expect("insert");

        let stats = store.stats().await.expect("stats");
        assert_eq!(stats.active, 1);
        assert_eq!(stats.dead_letter, 0);
        assert_eq!(stats.archived, 0);
    }

    #[tokio::test]
    async fn test_parse_addr() {
        // Local form
        let (actor, lane, project, world) = WhitesnakeStore::parse_addr("agent");
        assert_eq!(actor.as_deref(), Some("agent"));
        assert_eq!(lane.as_deref(), Some("lead"));
        assert!(project.is_none());
        assert!(world.is_none());

        // Project form with lane
        let (actor, lane, project, world) = WhitesnakeStore::parse_addr("agent@vp/lead");
        assert_eq!(actor.as_deref(), Some("agent"));
        assert_eq!(lane.as_deref(), Some("lead"));
        assert_eq!(project.as_deref(), Some("vp"));
        assert!(world.is_none());

        // Multi-segment lane
        let (actor, lane, project, _) = WhitesnakeStore::parse_addr("agent@vp/worker/chore");
        assert_eq!(actor.as_deref(), Some("agent"));
        assert_eq!(lane.as_deref(), Some("worker/chore"));
        assert_eq!(project.as_deref(), Some("vp"));

        // Parse error → all None
        let (actor, lane, project, world) = WhitesnakeStore::parse_addr("invalid$$$$");
        assert!(actor.is_none());
        assert!(lane.is_none());
        assert!(project.is_none());
        assert!(world.is_none());
    }

    /// Phase 3 carry-over (= spike Q3 finding と同じ):
    /// SurrealDB v3.0.4 で `BEGIN; LET $candidates = SELECT ... LIMIT 1; LET $target_id =
    /// $candidates[0].id; ...` の subquery extraction が想定通り動かない (= target_id が null)。
    /// Phase 3 PR (= claim 機構導入) で path A/B/C のうち working な path を確定、 race-free
    /// verification + bench を実機で。 詳細 `docs/design/20-spike-report.md` §4 (Q3 partial)。
    #[tokio::test]
    #[ignore = "Phase 3 carry-over: spike Q3 と同じく $candidates[0].id 抽出が動かない (doc 20 §4)"]
    async fn test_claim_and_mark_consumed() {
        let store = make_test_store().await;

        // 3 msg insert (= payload object でラップ)
        for i in 0..3 {
            let mut msg = Message::new("agent@vp/lead", "agent@vp/lead", MessageKind::Direct)
                .with_payload(&serde_json::json!({ "text": format!("msg-{}", i) }));
            msg.id = format!("test-msg-{}", i);
            store.insert(&msg).await.expect("insert");
        }

        // claim 1 件
        let claimed = store
            .claim("agent", "lead", "consumer-1")
            .await
            .expect("claim 1");
        assert!(claimed.is_some(), "1 件 claim できるはず");
        let claimed_msg = claimed.unwrap();

        // mark consumed
        store
            .mark_consumed(&claimed_msg.id)
            .await
            .expect("mark consumed");

        // 残 active = 2
        let stats = store.stats().await.expect("stats");
        assert_eq!(
            stats.active, 3,
            "3 件 active のまま (= consume せず claim だけ)"
        );
    }

    #[tokio::test]
    async fn test_claim_empty_returns_none() {
        let store = make_test_store().await;
        let claimed = store
            .claim("agent", "lead", "consumer-x")
            .await
            .expect("claim");
        assert!(claimed.is_none(), "msg がない時 None を返すはず");
    }
}
