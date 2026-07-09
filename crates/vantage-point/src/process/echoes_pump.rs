//! Lane echoes pump — Act II 版の terminal_pump（doc 32 §3）。
//!
//! 1 つの Lane の [`EchoesAgentHost`] が broadcast する [`EchoesEvent`] を購読し、
//! per-lane topic (`process/echoes/data/{lane}/event`) に [`ProcessMessage::EchoesEvent`]
//! として route する。これにより Act II の構造化イベントが単一 topic 空間に乗り、
//! World 経由で vp-app へ届く（terminal_pump と完全に同型）。
//!
//! data / calculations / actions:
//! - calculations: なし（pump は I/O bridge）
//! - actions: broadcast recv → topic route（副作用）

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::echoes::EchoesEvent;
use crate::process::topic_router::TopicRouter;
use crate::protocol::ProcessMessage;

/// 1 Lane の EchoesAgentHost output broadcast を購読し、`EchoesEvent` topic に流す pump を spawn。
///
/// - `lane`: LaneAddress の Display 形（`"vp/conductor"` 等）。topic key 化は `TopicRouter` が担う。
/// - `rx`: `EchoesAgentHost::subscribe()` で得た EchoesEvent の broadcast receiver。
/// - `topic_router`: SP の topic_router。
///
/// Host drop（broadcast Closed）で pump は自然終了する。lag 時は drop を warn して継続。
pub fn spawn_lane_echoes_pump(
    lane: String,
    mut rx: broadcast::Receiver<EchoesEvent>,
    topic_router: Arc<TopicRouter>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    topic_router
                        .route(ProcessMessage::EchoesEvent {
                            lane: lane.clone(),
                            event,
                        })
                        .await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("lane echoes pump lagged: {n} events dropped (lane={lane})");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!("lane echoes pump 終了 (host dropped, lane={lane})");
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// pump が EchoesEvent を per-lane topic に route し、subscriber が受け取れる。
    #[tokio::test]
    async fn test_pump_routes_echoes_event_to_per_lane_topic() {
        let router = Arc::new(TopicRouter::new());
        let (tx, rx) = broadcast::channel::<EchoesEvent>(16);

        // echoes data は非 retained なので route 前に subscribe が要る。
        let (_id, mut srx) = router
            .subscribe("process/echoes/data/vp~conductor/event")
            .await;

        let _h = spawn_lane_echoes_pump("vp/conductor".to_string(), rx, router.clone());

        tx.send(EchoesEvent::MessageChunk {
            text: "hello".to_string(),
        })
        .expect("send");

        let (topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(topic, "process/echoes/data/vp~conductor/event");
        match msg {
            ProcessMessage::EchoesEvent { lane, event } => {
                assert_eq!(lane, "vp/conductor");
                assert_eq!(
                    event,
                    EchoesEvent::MessageChunk {
                        text: "hello".into()
                    }
                );
            }
            other => panic!("想定外の message: {other:?}"),
        }
    }
}
