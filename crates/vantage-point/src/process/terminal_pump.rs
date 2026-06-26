//! Lane terminal pump — doc 27 §4.1 S1。
//!
//! 1 つの Lane PtySlot の出力 broadcast を購読し、 per-lane terminal topic
//! (`process/terminal/data/{lane}/out`) に `LaneTerminalOutput` として route する。
//! これにより Lane の PTY 出力が単一 topic 空間に乗り、 World 経由で WebView へ届く
//! (raw WebSocket `/ws/terminal` 退役の置換)。
//!
//! ## production の lifecycle
//!
//! pump 関数自体は permanent。 「いつ start/stop するか」 は段階で進化する:
//! - S1 (本ファイル): pump ロジックを確立 + 単体検証。
//! - S2: TopicRouter の demand hook で **subscriber が居る間だけ** start/stop
//!   (= demand-driven production)。 producer を lazy にして無駄 stream を消す。
//!
//! data / calculations / actions の分離:
//! - calculations: なし (pump は I/O bridge)
//! - actions: broadcast recv → topic route (副作用)

use std::sync::Arc;

use base64::Engine;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::process::topic_router::TopicRouter;
use crate::protocol::ProcessMessage;

/// 1 Lane の PtySlot output broadcast を購読し、 `LaneTerminalOutput` topic に流す pump を spawn。
///
/// - `lane`: LaneAddress の Display 形 (`"vp/conductor"` / `"vp/performer/foo"`)。 vp-app が
///   `/ws/terminal?lane=` に渡していた値と一致させ、 topic key 化は `TopicRouter` が担う。
/// - `rx`: `LanePool::subscribe_output(addr)` で得た PtySlot output の broadcast receiver。
/// - `topic_router`: SP の topic_router。 route 先 (World へは ingest が転送する)。
///
/// PtySlot drop (broadcast Closed) で pump は自然終了する。 lag 時は drop を warn して継続。
pub fn spawn_lane_terminal_pump(
    lane: String,
    mut rx: broadcast::Receiver<Vec<u8>>,
    topic_router: Arc<TopicRouter>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let engine = base64::engine::general_purpose::STANDARD;
        loop {
            match rx.recv().await {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    let data = engine.encode(&bytes);
                    topic_router
                        .route(ProcessMessage::LaneTerminalOutput {
                            lane: lane.clone(),
                            data,
                        })
                        .await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("lane terminal pump lagged: {n} chunks dropped (lane={lane})");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!("lane terminal pump 終了 (PtySlot dropped, lane={lane})");
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

    /// pump が PtySlot 出力を per-lane topic に route し、 subscriber が受け取れる。
    #[tokio::test]
    async fn test_pump_routes_to_per_lane_topic() {
        let router = Arc::new(TopicRouter::new());
        let (tx, rx) = broadcast::channel::<Vec<u8>>(16);

        // 先に subscriber を張る (terminal data は非 retained なので route 前に subscribe が要る)。
        let (_id, mut srx) = router
            .subscribe("process/terminal/data/vp~conductor/out")
            .await;

        let _h = spawn_lane_terminal_pump("vp/conductor".to_string(), rx, router.clone());

        tx.send(b"hello".to_vec()).expect("send");

        let (topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(topic, "process/terminal/data/vp~conductor/out");
        match msg {
            ProcessMessage::LaneTerminalOutput { lane, data } => {
                assert_eq!(lane, "vp/conductor");
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .expect("base64");
                assert_eq!(decoded, b"hello");
            }
            other => panic!("想定外の message: {other:?}"),
        }
    }

    /// 空バイト列は route しない (no-op)。
    #[tokio::test]
    async fn test_pump_skips_empty() {
        let router = Arc::new(TopicRouter::new());
        let (tx, rx) = broadcast::channel::<Vec<u8>>(16);
        let (_id, mut srx) = router
            .subscribe("process/terminal/data/vp~conductor/out")
            .await;
        let _h = spawn_lane_terminal_pump("vp/conductor".to_string(), rx, router.clone());

        tx.send(Vec::new()).expect("send empty");
        tx.send(b"x".to_vec()).expect("send x");

        // 空は skip され、 最初に届くのは "x"。
        let (_topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        match msg {
            ProcessMessage::LaneTerminalOutput { data, .. } => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .expect("base64");
                assert_eq!(decoded, b"x");
            }
            other => panic!("想定外: {other:?}"),
        }
    }
}
