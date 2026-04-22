//! Phase A projection — lane_map + permission_audit のみ。
//!
//! Worker A VP-74 roadmap (mem_1CaHMKbdGUoqxvNSu3VjR8) Phase A 優先戦略:
//! Lane-as-Process (VP-77) との同期効果が最大なため、lane_map を先行実装。
//! permission_audit は TH / Dev Panel の dogfood canary として併設。
//!
//! canvas_state / hd_sessions / user_context / build_status は Phase B/C で
//! 別 sub-issue として実装 (本 skeleton は trait パターンのみ提供)。

use std::sync::Arc;

use async_trait::async_trait;

use crate::creo::Event;
use crate::db::VpDb;

/// Projection の共通 interface。Bus subscriber が matches() で絞り込み apply() する。
#[async_trait]
pub trait Projection: Send + Sync {
    /// projection 名 (logging / metric 用)。
    fn name(&self) -> &'static str;

    /// この projection が event を扱うべきかの判定。
    fn matches(&self, event: &Event) -> bool;

    /// event を projection state に反映。
    async fn apply(&self, event: &Event) -> anyhow::Result<()>;
}

/// lane_map projection の event 対象判定 (pure fn、test 可能)。
///
/// project scope の lifecycle / state topic を対象 (lane 単位の fold)。
pub fn matches_lane_map(ev: &Event) -> bool {
    ev.topic.starts_with("project/")
        && (ev.topic.contains("/lifecycle/") || ev.topic.contains("/state/"))
}

/// permission_audit projection の event 対象判定 (pure fn、test 可能)。
///
/// PP routing 決定 と HD/TH permission command を監査対象とする。
pub fn matches_permission_audit(ev: &Event) -> bool {
    ev.topic == "project/pp/command/route"
        || ev.topic.starts_with("project/hd/command/permission")
        || ev.topic.starts_with("project/th/command/permission")
}

/// lane_map: ActorRef.lane ごとに last_event / last_topic / updated_at を追跡。
///
/// VP-77 §8.5 "Lane state は event log から materialize される projection" の実体。
pub struct LaneMapProjection {
    pub db: Arc<VpDb>,
}

#[async_trait]
impl Projection for LaneMapProjection {
    fn name(&self) -> &'static str {
        "lane_map"
    }

    fn matches(&self, ev: &Event) -> bool {
        matches_lane_map(ev)
    }

    async fn apply(&self, _event: &Event) -> anyhow::Result<()> {
        // TODO: R1 Phase A 本実装 (VP-74 Day 2-3)
        // UPSERT lane_map
        //   SET last_event_id = $event.id,
        //       last_topic    = $event.topic,
        //       updated_at    = $event.timestamp
        //   WHERE project = $event.source.project
        //     AND lane    = $event.source.lane
        Ok(())
    }
}

/// permission_audit: The Hand Permission Gate の決定を append-only で記録。
///
/// VP-72 D-5 (TH Permission Gate) と VP-77 §5.4 (L1+ autonomy の安全装置) の共通ログ。
pub struct PermissionAuditProjection {
    pub db: Arc<VpDb>,
}

#[async_trait]
impl Projection for PermissionAuditProjection {
    fn name(&self) -> &'static str {
        "permission_audit"
    }

    fn matches(&self, ev: &Event) -> bool {
        matches_permission_audit(ev)
    }

    async fn apply(&self, _event: &Event) -> anyhow::Result<()> {
        // TODO: R1 Phase A 本実装
        // INSERT INTO permission_audit
        //   { ts: $event.timestamp,
        //     actor: $event.source.canonical(),
        //     topic: $event.topic,
        //     payload_digest: sha256($event.payload),
        //     decision: None } -- decision は別 event で後付け更新
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Projection の matching ロジックを pure fn (matches_lane_map / matches_permission_audit)
    //! で直接テストする。`Arc<VpDb>` を作らずに済むので、skeleton 段階でも動く。

    use super::*;
    use crate::creo::{ActorRef, CreoContent, CreoFormat, Event};

    fn make_event(topic: &str) -> Event {
        Event::new(
            topic,
            ActorRef {
                stand: "hd".into(),
                lane: "lead".into(),
                project: "vantage-point".into(),
            },
            CreoContent {
                format: CreoFormat::Text,
                body: serde_json::json!({"text": "x"}),
                source: None,
                memory_ref: None,
            },
        )
    }

    // LaneMap matching

    #[test]
    fn lane_map_matches_lifecycle() {
        assert!(matches_lane_map(&make_event(
            "project/hd/lifecycle/session-started"
        )));
    }

    #[test]
    fn lane_map_matches_state() {
        assert!(matches_lane_map(&make_event("project/sc/state/item-added")));
    }

    #[test]
    fn lane_map_skips_notify() {
        assert!(!matches_lane_map(&make_event("project/hd/notify/message")));
    }

    #[test]
    fn lane_map_skips_user_scope() {
        assert!(!matches_lane_map(&make_event("user/user/command/click")));
    }

    // PermissionAudit matching

    #[test]
    fn audit_matches_pp_route() {
        assert!(matches_permission_audit(&make_event(
            "project/pp/command/route"
        )));
    }

    #[test]
    fn audit_matches_hd_permission_prefix() {
        assert!(matches_permission_audit(&make_event(
            "project/hd/command/permission-request"
        )));
    }

    #[test]
    fn audit_matches_th_permission_prefix() {
        assert!(matches_permission_audit(&make_event(
            "project/th/command/permission-grant"
        )));
    }

    #[test]
    fn audit_skips_unrelated() {
        assert!(!matches_permission_audit(&make_event(
            "project/hd/notify/message"
        )));
    }
}
