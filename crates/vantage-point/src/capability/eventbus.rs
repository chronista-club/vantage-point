//! Event Bus (REQ-CAP-003)
//!
//! 能力間のイベント通信基盤。
//! パターンベースの購読とブロードキャスト配信を提供する。
//!
//! ## 設計思想
//!
//! - **型安全**: イベントはCapabilityEvent構造体で型付け
//! - **パターンマッチング**: "midi.*" などのパターンで購読
//! - **非同期**: 全ての配信は非同期で実行
//! - **複数購読者**: 一つのイベントを複数の購読者に配信

use crate::capability::core::CapabilityEvent;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast, mpsc};

// =============================================================================
// Subscription (購読)
// =============================================================================

/// イベント購読
pub struct Subscription {
    /// 購読ID（識別用）
    pub id: String,
    /// 購読パターン（例: "midi.*", "agent.response"）
    pub pattern: String,
    /// イベント受信用チャンネル
    receiver: broadcast::Receiver<CapabilityEvent>,
}

impl Subscription {
    /// 次のイベントを受信
    pub async fn recv(&mut self) -> Option<CapabilityEvent> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // バッファが溢れた場合は次のイベントを待つ（ループで継続）
                    continue;
                }
            }
        }
    }

    /// イベントを非同期イテレータとして取得
    pub fn into_stream(self) -> impl futures::Stream<Item = CapabilityEvent> {
        futures::stream::unfold(self, |mut sub| async move {
            sub.recv().await.map(|event| (event, sub))
        })
    }
}

// =============================================================================
// EventBus
// =============================================================================

/// イベントバス
///
/// ## 使用例
///
/// ```ignore
/// let bus = EventBus::new();
///
/// // 購読
/// let mut sub = bus.subscribe("subscriber-1", "midi.*").await;
///
/// // 別タスクで購読をリッスン
/// tokio::spawn(async move {
///     while let Some(event) = sub.recv().await {
///         println!("Received: {}", event.event_type);
///     }
/// });
///
/// // イベント発火
/// let event = CapabilityEvent::new("midi.note_on", "midi-capability");
/// bus.emit(event).await;
/// ```
pub struct EventBus {
    /// ブロードキャスト送信者
    sender: broadcast::Sender<CapabilityEvent>,
    /// 購読者のパターン情報
    subscriptions: Arc<RwLock<HashMap<String, String>>>,
    /// バッファサイズ
    buffer_size: usize,
}

impl EventBus {
    /// 新しいEventBusを作成
    pub fn new() -> Self {
        Self::with_buffer_size(1024)
    }

    /// バッファサイズを指定してEventBusを作成
    pub fn with_buffer_size(size: usize) -> Self {
        let (sender, _) = broadcast::channel(size);
        Self {
            sender,
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            buffer_size: size,
        }
    }

    // -------------------------------------------------------------------------
    // 購読 (subscribe)
    // -------------------------------------------------------------------------

    /// パターンでイベントを購読
    ///
    /// ## 引数
    /// - `id`: 購読者の識別子
    /// - `pattern`: 購読パターン（例: "midi.*", "agent.response"）
    ///
    /// ## パターン構文
    /// - `"midi.*"` - "midi."で始まる全てのイベント
    /// - `"agent.response"` - 完全一致
    /// - `"*"` - 全てのイベント
    pub async fn subscribe(&self, id: &str, pattern: &str) -> Subscription {
        let receiver = self.sender.subscribe();

        let mut subs = self.subscriptions.write().await;
        subs.insert(id.to_string(), pattern.to_string());

        tracing::debug!("EventBus: {} subscribed to '{}'", id, pattern);

        Subscription {
            id: id.to_string(),
            pattern: pattern.to_string(),
            receiver,
        }
    }

    /// 購読を解除
    pub async fn unsubscribe(&self, id: &str) {
        let mut subs = self.subscriptions.write().await;
        if subs.remove(id).is_some() {
            tracing::debug!("EventBus: {} unsubscribed", id);
        }
    }

    /// 購読者数を取得
    pub async fn subscriber_count(&self) -> usize {
        let subs = self.subscriptions.read().await;
        subs.len()
    }

    // -------------------------------------------------------------------------
    // 発火 (emit)
    // -------------------------------------------------------------------------

    /// イベントを発火（ブロードキャスト）
    ///
    /// ## 引数
    /// - `event`: 発火するイベント
    ///
    /// ## 戻り値
    /// - 配信した購読者の数
    pub async fn emit(&self, event: CapabilityEvent) -> usize {
        match self.sender.send(event.clone()) {
            Ok(count) => {
                tracing::trace!(
                    "EventBus: emitted '{}' to {} receivers",
                    event.event_type,
                    count
                );
                count
            }
            Err(_) => {
                // 受信者がいない場合
                0
            }
        }
    }

    /// 送信用チャンネルを取得（Contextに渡す用）
    pub fn sender(&self) -> mpsc::Sender<CapabilityEvent> {
        let broadcast_sender = self.sender.clone();
        let (tx, mut rx) = mpsc::channel::<CapabilityEvent>(self.buffer_size);

        // mpsc → broadcastへのブリッジ
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let _ = broadcast_sender.send(event);
            }
        });

        tx
    }

    // -------------------------------------------------------------------------
    // フィルタリング（購読者側で使用）
    // -------------------------------------------------------------------------

    /// パターンがイベントタイプにマッチするか判定
    pub fn matches(pattern: &str, event_type: &str) -> bool {
        if pattern == "*" {
            return true;
        }

        if pattern.ends_with(".*") {
            let prefix = &pattern[..pattern.len() - 1]; // "midi." を取得
            return event_type.starts_with(prefix);
        }

        if pattern.ends_with('*') {
            let prefix = &pattern[..pattern.len() - 1];
            return event_type.starts_with(prefix);
        }

        pattern == event_type
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// FilteredSubscription (フィルタ付き購読)
// =============================================================================

/// フィルタ付きイベント購読
///
/// パターンにマッチしたイベントのみを受信する
pub struct FilteredSubscription {
    inner: Subscription,
}

impl FilteredSubscription {
    /// 購読からフィルタ付き購読を作成
    pub fn new(subscription: Subscription) -> Self {
        Self {
            inner: subscription,
        }
    }

    /// 次のマッチするイベントを受信
    pub async fn recv(&mut self) -> Option<CapabilityEvent> {
        loop {
            match self.inner.recv().await {
                Some(event) => {
                    if EventBus::matches(&self.inner.pattern, &event.event_type) {
                        return Some(event);
                    }
                    // マッチしない場合は次のイベントを待つ
                }
                None => return None,
            }
        }
    }

    /// パターンを取得
    pub fn pattern(&self) -> &str {
        &self.inner.pattern
    }
}

// =============================================================================
// EventDispatcher (イベントディスパッチャ)
// =============================================================================

/// イベントディスパッチャ
///
/// EventBusとCapabilityRegistryを連携させ、
/// イベントを適切な能力に配信する
pub struct EventDispatcher {
    bus: Arc<EventBus>,
    /// 実行中フラグ
    running: Arc<RwLock<bool>>,
}

impl EventDispatcher {
    /// 新しいディスパッチャを作成
    pub fn new(bus: Arc<EventBus>) -> Self {
        Self {
            bus,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// イベントバスへの参照を取得
    pub fn bus(&self) -> &Arc<EventBus> {
        &self.bus
    }

    /// ディスパッチャが実行中かどうか
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    /// ディスパッチャを停止
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
        tracing::info!("EventDispatcher stopped");
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_matching() {
        // 完全一致
        assert!(EventBus::matches("midi.note_on", "midi.note_on"));
        assert!(!EventBus::matches("midi.note_on", "midi.note_off"));

        // プレフィックスマッチ
        assert!(EventBus::matches("midi.*", "midi.note_on"));
        assert!(EventBus::matches("midi.*", "midi.note_off"));
        assert!(EventBus::matches("midi.*", "midi.cc"));
        assert!(!EventBus::matches("midi.*", "agent.response"));

        // ワイルドカード
        assert!(EventBus::matches("*", "midi.note_on"));
        assert!(EventBus::matches("*", "agent.response"));
        assert!(EventBus::matches("*", "anything"));

        // プレフィックス（ドットなし）
        assert!(EventBus::matches("agent*", "agent.response"));
        assert!(EventBus::matches("agent*", "agent_event"));
    }

    #[tokio::test]
    async fn test_subscribe_and_emit() {
        let bus = EventBus::new();

        let mut sub = bus.subscribe("test-subscriber", "*").await;

        let event = CapabilityEvent::new("test.event", "test-source");
        let count = bus.emit(event.clone()).await;

        assert_eq!(count, 1);

        let received = sub.recv().await.unwrap();
        assert_eq!(received.event_type, "test.event");
        assert_eq!(received.source, "test-source");
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let bus = EventBus::new();

        let mut sub1 = bus.subscribe("sub1", "*").await;
        let mut sub2 = bus.subscribe("sub2", "*").await;
        let mut sub3 = bus.subscribe("sub3", "*").await;

        let event = CapabilityEvent::new("broadcast.event", "broadcaster");
        let count = bus.emit(event).await;

        assert_eq!(count, 3);

        // 全員が同じイベントを受信
        let e1 = sub1.recv().await.unwrap();
        let e2 = sub2.recv().await.unwrap();
        let e3 = sub3.recv().await.unwrap();

        assert_eq!(e1.event_type, "broadcast.event");
        assert_eq!(e2.event_type, "broadcast.event");
        assert_eq!(e3.event_type, "broadcast.event");
    }

    #[tokio::test]
    async fn test_filtered_subscription() {
        let bus = EventBus::new();

        let sub = bus.subscribe("midi-listener", "midi.*").await;
        let mut filtered = FilteredSubscription::new(sub);

        // MIDIイベント
        bus.emit(CapabilityEvent::new("midi.note_on", "midi")).await;
        // 非MIDIイベント
        bus.emit(CapabilityEvent::new("agent.response", "agent"))
            .await;
        // MIDIイベント
        bus.emit(CapabilityEvent::new("midi.note_off", "midi"))
            .await;

        // MIDIイベントのみ受信
        let e1 = filtered.recv().await.unwrap();
        assert_eq!(e1.event_type, "midi.note_on");

        let e2 = filtered.recv().await.unwrap();
        assert_eq!(e2.event_type, "midi.note_off");
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let bus = EventBus::new();

        let _sub1 = bus.subscribe("sub1", "*").await;
        let _sub2 = bus.subscribe("sub2", "*").await;

        assert_eq!(bus.subscriber_count().await, 2);

        bus.unsubscribe("sub1").await;
        assert_eq!(bus.subscriber_count().await, 1);

        bus.unsubscribe("sub2").await;
        assert_eq!(bus.subscriber_count().await, 0);
    }

    #[tokio::test]
    async fn test_sender_for_context() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe("listener", "*").await;

        let sender = bus.sender();

        // senderを通じてイベントを送信
        sender
            .send(CapabilityEvent::new("context.event", "context"))
            .await
            .unwrap();

        // 少し待つ（mpsc→broadcastのブリッジが非同期なため）
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let event = sub.recv().await.unwrap();
        assert_eq!(event.event_type, "context.event");
    }

    #[tokio::test]
    async fn test_event_with_payload() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe("listener", "*").await;

        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct NoteOn {
            note: u8,
            velocity: u8,
        }

        let payload = NoteOn {
            note: 60,
            velocity: 100,
        };

        let event = CapabilityEvent::new("midi.note_on", "midi").with_payload(&payload);

        bus.emit(event).await;

        let received = sub.recv().await.unwrap();
        let parsed: NoteOn = received.payload_as().unwrap();

        assert_eq!(parsed.note, 60);
        assert_eq!(parsed.velocity, 100);
    }
}
