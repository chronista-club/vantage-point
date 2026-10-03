//! Address async results to their originating window, never the currently focused one.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
use tao::{event_loop::EventLoopClosed, window::WindowId};

#[derive(Debug, Clone)]
pub struct RoutedEvent<T> {
    /// None is reserved for application-wide menu commands.
    pub window_id: Option<WindowId>,
    pub event: T,
    pub scope: u64,
}

type SendEvent<T> = dyn Fn(RoutedEvent<T>) -> Result<(), EventLoopClosed<T>> + Send + Sync;

#[derive(Clone)]
pub struct EventLoopProxy<T> {
    send: Arc<SendEvent<T>>,
    window_id: Option<WindowId>,
    closed: tokio::sync::watch::Sender<bool>,
    scope: u64,
}

impl<T: Send + 'static> EventLoopProxy<T> {
    pub fn new(
        proxy: tao::event_loop::EventLoopProxy<RoutedEvent<T>>,
        window_id: Option<WindowId>,
    ) -> Self {
        Self::with_sender(window_id, move |event| {
            proxy
                .send_event(event)
                .map_err(|e| EventLoopClosed(e.0.event))
        })
    }

    fn with_sender(
        window_id: Option<WindowId>,
        send: impl Fn(RoutedEvent<T>) -> Result<(), EventLoopClosed<T>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            send: Arc::new(send),
            window_id,
            closed: tokio::sync::watch::channel(false).0,
            scope: NEXT_SCOPE.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn send_event(&self, event: T) -> Result<(), EventLoopClosed<T>> {
        if *self.closed.borrow() {
            return Err(EventLoopClosed(event));
        }
        (self.send)(RoutedEvent {
            window_id: self.window_id,
            event,
            scope: self.scope,
        })
    }

    /// Reject queued replies even if the OS reuses the same native WindowId.
    pub fn accepts(&self, scope: u64) -> bool {
        scope == self.scope && !*self.closed.borrow()
    }

    /// Invalidates every clone; a later window never inherits this sender.
    pub fn close(&self) {
        self.closed.send_replace(true);
    }

    pub async fn closed(&self) {
        let mut rx = self.closed.subscribe();
        let _ = rx.wait_for(|closed| *closed).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // mem_1Cfd9HUeYqn4iKm5YXvgsj
    #[test]
    fn cloned_sender_retains_window_address_and_close_invalidates_all_clones() {
        // Only compared, never passed to native APIs.
        let id = unsafe { WindowId::dummy() };
        let (tx, rx) = std::sync::mpsc::channel();
        let proxy = EventLoopProxy::with_sender(Some(id), move |e| {
            tx.send(e).unwrap();
            Ok(())
        });
        let async_result = proxy.clone();
        async_result.send_event("reply").unwrap();
        let event = rx.recv().unwrap();
        assert_eq!(event.window_id, Some(id));
        assert_eq!(event.event, "reply");
        proxy.close();
        assert!(async_result.send_event("late reply").is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queued_reply_cannot_reach_a_replacement_window_with_the_same_native_id() {
        let id = unsafe { WindowId::dummy() };
        let (tx, rx) = std::sync::mpsc::channel();
        let old = EventLoopProxy::with_sender(Some(id), move |e| {
            tx.send(e).unwrap();
            Ok(())
        });
        old.send_event("queued").unwrap();
        let queued = rx.recv().unwrap();
        assert!(old.accepts(queued.scope));
        old.close();
        let replacement = EventLoopProxy::<&str>::with_sender(Some(id), |_| Ok(()));
        assert!(!old.accepts(queued.scope));
        assert!(!replacement.accepts(queued.scope));
    }

    #[tokio::test]
    async fn closing_a_window_wakes_its_subscriptions_even_without_traffic() {
        let proxy = EventLoopProxy::<()>::with_sender(None, |_| Ok(()));
        let waiting = proxy.clone();
        proxy.close();
        tokio::time::timeout(std::time::Duration::from_millis(50), waiting.closed())
            .await
            .unwrap();
        let other = EventLoopProxy::<()>::with_sender(None, |_| Ok(()));
        assert!(other.send_event(()).is_ok());
    }
}
