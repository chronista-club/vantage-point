//! EventBus — creo::Event の pub/sub ハブ。
//!
//! tokio::broadcast ベース、alias 自動解決、validation 失敗は Err で reject。
//! 複数 subscriber に fan-out、受信側は各自 `Subscription::recv()` で受け取る。

use std::sync::Arc;

use tokio::sync::{RwLock, broadcast};

use super::alias::AliasTable;
use super::validator::{ValidationError, validate_topic};
use crate::creo::Event;

/// Bus 本体。`Arc` で clone して各 Stand から共有する。
pub struct Bus {
    tx: broadcast::Sender<Event>,
    aliases: Arc<RwLock<AliasTable>>,
}

/// 外部から Bus を扱う handle (内部的に `Arc<Bus>`)。
#[derive(Clone)]
pub struct BusHandle {
    inner: Arc<Bus>,
}

/// 1 subscriber の receive 窓口。
pub struct Subscription {
    rx: broadcast::Receiver<Event>,
}

impl Bus {
    /// 新 Bus を作成。`capacity` は broadcast channel のバッファ長。
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _) = broadcast::channel(capacity);
        Arc::new(Self {
            tx,
            aliases: Arc::new(RwLock::new(AliasTable::seeded())),
        })
    }

    /// alias 解決 + validation してから配信。receiver 0 でも Ok を返す。
    pub async fn publish(&self, mut event: Event) -> Result<(), BusError> {
        // alias 解決 (short → canonical)
        if let Some(canonical) = self.aliases.read().await.resolve(&event.topic) {
            event.topic = canonical;
        }
        // schema validation
        validate_topic(&event.topic)?;
        // 配信 (receiver 0 でも Ok、broadcast::error::SendError は receiver 不在を示すだけ)
        let _ = self.tx.send(event);
        Ok(())
    }

    /// 新しい購読。過去 event は配信されない (Retained は Phase B 以降)。
    pub fn subscribe(&self) -> Subscription {
        Subscription {
            rx: self.tx.subscribe(),
        }
    }
}

impl BusHandle {
    pub fn new(bus: Arc<Bus>) -> Self {
        Self { inner: bus }
    }

    pub async fn publish(&self, event: Event) -> Result<(), BusError> {
        self.inner.publish(event).await
    }

    pub fn subscribe(&self) -> Subscription {
        self.inner.subscribe()
    }
}

impl Subscription {
    /// 次の event を待つ。lag 発生時は `RecvError::Lagged` を返す。
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        self.rx.recv().await
    }
}

/// Bus 操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("topic validation failed: {0}")]
    Validation(#[from] ValidationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creo::{ActorRef, CreoContent, CreoFormat};

    fn sample_actor() -> ActorRef {
        ActorRef {
            stand: "hd".into(),
            lane: "lead".into(),
            project: "vantage-point".into(),
        }
    }

    fn sample_content() -> CreoContent {
        CreoContent {
            format: CreoFormat::Text,
            body: serde_json::json!({"text": "hi"}),
            source: None,
            memory_ref: None,
        }
    }

    #[tokio::test]
    async fn publish_subscribe_roundtrip() {
        let bus = Bus::new(16);
        let handle = BusHandle::new(bus.clone());
        let mut sub = handle.subscribe();

        let ev = Event::new(
            "project/hd/notify/message",
            sample_actor(),
            sample_content(),
        );
        handle.publish(ev.clone()).await.unwrap();

        let got = sub.recv().await.unwrap();
        assert_eq!(got.topic, "project/hd/notify/message");
        assert_eq!(got.source, ev.source);
    }

    #[tokio::test]
    async fn alias_auto_resolves_on_publish() {
        let bus = Bus::new(16);
        let handle = BusHandle::new(bus.clone());
        let mut sub = handle.subscribe();

        // alias "hd.message" で publish → canonical に展開されて配信
        let ev = Event::new("hd.message", sample_actor(), sample_content());
        handle.publish(ev).await.unwrap();

        let got = sub.recv().await.unwrap();
        assert_eq!(got.topic, "project/hd/notify/message");
    }

    #[tokio::test]
    async fn invalid_topic_rejected() {
        let bus = Bus::new(16);
        let handle = BusHandle::new(bus);

        // alias にも canonical にも該当しない garbage
        let ev = Event::new("foo/bar", sample_actor(), sample_content());
        let err = handle.publish(ev).await.unwrap_err();
        assert!(matches!(err, BusError::Validation(_)));
    }

    #[tokio::test]
    async fn publish_without_subscriber_is_ok() {
        let bus = Bus::new(16);
        let handle = BusHandle::new(bus);

        let ev = Event::new(
            "project/hd/notify/message",
            sample_actor(),
            sample_content(),
        );
        // 購読者なしでも Ok
        handle.publish(ev).await.unwrap();
    }
}
