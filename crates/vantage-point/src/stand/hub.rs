//! WebSocket connection hub

use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};

use crate::protocol::StandMessage;

/// WebSocket hub for managing connections and broadcasting messages
#[derive(Clone)]
pub struct Hub {
    /// Broadcast sender for messages to all connected clients
    tx: broadcast::Sender<StandMessage>,
    /// Connected client count
    client_count: Arc<RwLock<usize>>,
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(100);
        Self {
            tx,
            client_count: Arc::new(RwLock::new(0)),
        }
    }

    /// Subscribe to messages
    pub fn subscribe(&self) -> broadcast::Receiver<StandMessage> {
        self.tx.subscribe()
    }

    /// Broadcast a message to all connected clients
    pub fn broadcast(&self, msg: StandMessage) {
        // Ignore send errors (no receivers)
        let _ = self.tx.send(msg);
    }

    /// Increment client count
    pub async fn client_connected(&self) {
        let mut count = self.client_count.write().await;
        *count += 1;
        tracing::info!("Client connected (total: {})", *count);
    }

    /// Decrement client count
    pub async fn client_disconnected(&self) {
        let mut count = self.client_count.write().await;
        *count = count.saturating_sub(1);
        tracing::info!("Client disconnected (total: {})", *count);
    }

    /// Get current client count
    pub async fn client_count(&self) -> usize {
        *self.client_count.read().await
    }

    /// Check if any clients are connected
    pub async fn has_clients(&self) -> bool {
        *self.client_count.read().await > 0
    }

    /// Get the broadcast sender (for capability event bridge)
    pub fn sender(&self) -> broadcast::Sender<StandMessage> {
        self.tx.clone()
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Content;

    #[test]
    fn test_hub_new() {
        let hub = Hub::new();
        // Should be able to subscribe
        let _rx = hub.subscribe();
    }

    #[test]
    fn test_hub_default() {
        let hub = Hub::default();
        let _rx = hub.subscribe();
    }

    #[tokio::test]
    async fn test_client_count_initial() {
        let hub = Hub::new();
        assert_eq!(hub.client_count().await, 0);
        assert!(!hub.has_clients().await);
    }

    #[tokio::test]
    async fn test_client_connected_disconnected() {
        let hub = Hub::new();

        // Connect
        hub.client_connected().await;
        assert_eq!(hub.client_count().await, 1);
        assert!(hub.has_clients().await);

        // Connect another
        hub.client_connected().await;
        assert_eq!(hub.client_count().await, 2);

        // Disconnect one
        hub.client_disconnected().await;
        assert_eq!(hub.client_count().await, 1);
        assert!(hub.has_clients().await);

        // Disconnect last
        hub.client_disconnected().await;
        assert_eq!(hub.client_count().await, 0);
        assert!(!hub.has_clients().await);
    }

    #[tokio::test]
    async fn test_client_disconnected_saturating() {
        let hub = Hub::new();
        // Disconnect when count is 0 should not underflow
        hub.client_disconnected().await;
        assert_eq!(hub.client_count().await, 0);
    }

    #[test]
    fn test_broadcast_no_receivers() {
        let hub = Hub::new();
        // Should not panic when no receivers
        hub.broadcast(StandMessage::Ping);
    }

    #[test]
    fn test_broadcast_with_receiver() {
        let hub = Hub::new();
        let mut rx = hub.subscribe();

        hub.broadcast(StandMessage::Ping);

        // Should receive the message
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, StandMessage::Ping));
    }

    #[test]
    fn test_broadcast_show_message() {
        let hub = Hub::new();
        let mut rx = hub.subscribe();

        let msg = StandMessage::Show {
            pane_id: "main".to_string(),
            content: Content::Markdown("# Hello".to_string()),
            append: false,
        };
        hub.broadcast(msg);

        let received = rx.try_recv().unwrap();
        match received {
            StandMessage::Show { pane_id, .. } => {
                assert_eq!(pane_id, "main");
            }
            _ => panic!("Expected Show message"),
        }
    }

    #[test]
    fn test_sender() {
        let hub = Hub::new();
        let sender = hub.sender();
        let mut rx = hub.subscribe();

        // Send via sender directly
        sender.send(StandMessage::Ping).unwrap();

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, StandMessage::Ping));
    }
}
