//! Event — Stand Ensemble の単位メッセージ
//!
//! `payload: CreoContent` が全 Stand 間を流れる。`causation` で why? tree を構築可。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::content::CreoContent;
use super::topic::Topic;
use super::ui::CreoUI;

/// Time-ordered UUID (v7) として発行される event の id。
pub type EventId = Uuid;

/// Actor reference — canonical address `{stand}.{lane}@{project}` の構成要素。
///
/// Mailbox address (VP-24) と互換: `canonical()` で文字列化できる。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ActorRef {
    pub stand: String,
    pub lane: String,
    pub project: String,
}

impl ActorRef {
    /// `hd.lead@vantage-point` 形式の正規化文字列を返す。
    pub fn canonical(&self) -> String {
        format!("{}.{}@{}", self.stand, self.lane, self.project)
    }
}

/// Stand Ensemble の event stream を流れる最小単位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub topic: Topic,
    pub source: ActorRef,
    pub timestamp: DateTime<Utc>,

    /// 親 event の id (why? tree の親リンク)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation: Option<EventId>,

    pub payload: CreoContent,

    /// 描画 hint (任意同梱)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<CreoUI>,
}

impl Event {
    /// 新規 event を UUID v7 と現在時刻で組み立てる。
    pub fn new(topic: impl Into<Topic>, source: ActorRef, payload: CreoContent) -> Self {
        Self {
            id: Uuid::now_v7(),
            topic: topic.into(),
            source,
            timestamp: Utc::now(),
            causation: None,
            payload,
            ui: None,
        }
    }

    /// 親 event への因果リンクを設定して返す (builder 風)。
    pub fn with_causation(mut self, parent: EventId) -> Self {
        self.causation = Some(parent);
        self
    }

    /// 描画 hint を同梱して返す。
    pub fn with_ui(mut self, ui: CreoUI) -> Self {
        self.ui = Some(ui);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creo::format::CreoFormat;

    fn sample_actor() -> ActorRef {
        ActorRef {
            stand: "hd".into(),
            lane: "lead".into(),
            project: "vantage-point".into(),
        }
    }

    fn sample_content() -> CreoContent {
        CreoContent {
            format: CreoFormat::Markdown,
            body: serde_json::json!({"text": "hi"}),
            source: None,
            memory_ref: None,
        }
    }

    #[test]
    fn actor_ref_canonical() {
        assert_eq!(sample_actor().canonical(), "hd.lead@vantage-point");
    }

    #[test]
    fn new_event_uses_uuid_v7() {
        let ev = Event::new(
            "project/hd/notify/message",
            sample_actor(),
            sample_content(),
        );
        assert_eq!(ev.id.get_version_num(), 7);
        assert!(ev.causation.is_none());
        assert!(ev.ui.is_none());
    }

    #[test]
    fn with_causation_sets_parent() {
        let parent = Uuid::now_v7();
        let ev = Event::new(
            "project/sc/state/item-added",
            sample_actor(),
            sample_content(),
        )
        .with_causation(parent);
        assert_eq!(ev.causation, Some(parent));
    }

    #[test]
    fn event_serde_roundtrip() {
        let ev = Event::new(
            "project/sc/state/item-added",
            sample_actor(),
            sample_content(),
        );
        let s = serde_json::to_string(&ev).unwrap();
        let back: Event = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, ev.id);
        assert_eq!(back.topic, ev.topic);
        assert_eq!(back.source, ev.source);
    }
}
