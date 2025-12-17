//! WebSocket connection hub

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::protocol::DaemonMessage;

/// WebSocket hub for managing connections and broadcasting messages
#[derive(Clone)]
pub struct Hub {
    /// Broadcast sender for messages to all connected clients
    tx: broadcast::Sender<DaemonMessage>,
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
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonMessage> {
        self.tx.subscribe()
    }

    /// Broadcast a message to all connected clients
    pub fn broadcast(&self, msg: DaemonMessage) {
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
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}
