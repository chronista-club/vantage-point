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

/// replay snapshot の 1 message あたりの分割サイズ。
///
/// ring buffer 上限 (256KiB) を 1 message で送ると base64 で ~340KiB になり
/// channel message として大きすぎるため、 live chunk 粒度 (PTY read buffer = 4KiB) より
/// 大きめの 32KiB で分割して順次 route する。
const REPLAY_CHUNK: usize = 32 * 1024;

/// replay の冪等化 prefix — clear scrollback + clear screen + cursor home。
///
/// replay は「新規 xterm への画面復元」だけでなく、 **既存 xterm が生きたまま** demand が
/// 撃ち直される経路 (vp-app WS 瞬断の 1→0→1 再 subscribe / SP↔World control 再接続時の
/// `refire_active_demands`) でも走る。 backend は「新規 attach」と「reconnect」を区別できない
/// (どちらも topic への fresh subscribe) ため、 raw replay を単純追記すると既存画面に
/// 二重描画 (ゴースト) が出る。 replay 先頭で端末を clear してから raw を流し直すことで、
/// cold-start でも reconnect でも同一の最終画面に収束する (= 冪等な screen snapshot)。
/// `\x1b[3J` は xterm 拡張の scrollback clear、 `\x1b[2J` は画面 clear、 `\x1b[H` は cursor home。
const REPLAY_CLEAR_PREFIX: &[u8] = b"\x1b[H\x1b[2J\x1b[3J";

/// 1 Lane の 1 session の PtySlot output broadcast を購読し、 `LaneTerminalOutput` topic に
/// 流す pump を spawn。
///
/// - `lane`: LaneAddress の Display 形 (`"vp/root"` / `"vp/performer/foo"`)。 vp-app が
///   `/ws/terminal?lane=` に渡していた値と一致させ、 topic key 化は `TopicRouter` が担う。
/// - `session`: この pump が担う session の VP 採番 key（doc 50 §4.6 A6）。topic は lane 単位で
///   共有し、 session は `LaneTerminalOutput.session` に stamp する（Act II の `route_echoes` と
///   対称 — 同 lane の複数 session が同一 topic に流れ、 World A の xterm が session で振り分ける）。
/// - `replay`: attach 時に先頭配送する直近出力 snapshot。 rx と原子的に取得したもの
///   (`LanePool::attach_output`) を渡せば byte 順序が保たれる (欠落・重複なし)。
///   vp-app 再起動後の新 xterm に前回画面を復元する。 空 Vec = replay なし (従来挙動)。
/// - `rx`: `LanePool::attach_output(addr, session)` で得た PtySlot output の broadcast receiver。
/// - `topic_router`: SP の topic_router。 route 先 (World へは ingest が転送する)。
///
/// PtySlot drop (broadcast Closed) で pump は自然終了する。 lag 時は drop を warn して継続。
pub fn spawn_lane_terminal_pump(
    lane: String,
    session: u32,
    replay: Vec<u8>,
    mut rx: broadcast::Receiver<Vec<u8>>,
    topic_router: Arc<TopicRouter>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let engine = base64::engine::general_purpose::STANDARD;
        // replay 先頭配送 → その後 live stream。 rx は snapshot と同時 subscribe 済なので
        // この順序で全バイトが exactly-once で届く。 先頭に clear prefix を付けて冪等化する
        // (既存 xterm が生きたままの reconnect でも二重描画にならない、 REPLAY_CLEAR_PREFIX 参照)。
        if !replay.is_empty() {
            tracing::info!(
                "terminal pump replay: {} bytes を先頭配送 (lane={lane}, session={session})",
                replay.len()
            );
            let mut framed = Vec::with_capacity(REPLAY_CLEAR_PREFIX.len() + replay.len());
            framed.extend_from_slice(REPLAY_CLEAR_PREFIX);
            framed.extend_from_slice(&replay);
            for chunk in framed.chunks(REPLAY_CHUNK) {
                let data = engine.encode(chunk);
                topic_router
                    .route(ProcessMessage::LaneTerminalOutput {
                        lane: lane.clone(),
                        session,
                        data,
                    })
                    .await;
            }
        }
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
                            session,
                            data,
                        })
                        .await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "lane terminal pump lagged: {n} chunks dropped (lane={lane}, session={session})"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::debug!(
                        "lane terminal pump 終了 (PtySlot dropped, lane={lane}, session={session})"
                    );
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
        let (_id, mut srx) = router.subscribe("process/terminal/data/vp~root/out").await;

        // session を明示（5）で流し、 topic は lane 単位のまま session が message field に
        // stamp される（Design B）ことを往復で確認する。
        let _h = spawn_lane_terminal_pump("vp/root".to_string(), 5, Vec::new(), rx, router.clone());

        tx.send(b"hello".to_vec()).expect("send");

        let (topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(topic, "process/terminal/data/vp~root/out");
        match msg {
            ProcessMessage::LaneTerminalOutput {
                lane,
                session,
                data,
            } => {
                assert_eq!(lane, "vp/root");
                assert_eq!(session, 5, "pump は担当 session を message に stamp する");
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .expect("base64");
                assert_eq!(decoded, b"hello");
            }
            other => panic!("想定外の message: {other:?}"),
        }
    }

    /// replay snapshot が live stream より先に配送される (replay-on-attach の順序保証)。
    #[tokio::test]
    async fn test_pump_replays_before_live() {
        let router = Arc::new(TopicRouter::new());
        let (tx, rx) = broadcast::channel::<Vec<u8>>(16);
        let (_id, mut srx) = router.subscribe("process/terminal/data/vp~root/out").await;

        // live 出力を先に queue しても、 replay が先頭に来る。
        tx.send(b"live".to_vec()).expect("send live");
        let _h = spawn_lane_terminal_pump(
            "vp/root".to_string(),
            1,
            b"replayed-screen".to_vec(),
            rx,
            router.clone(),
        );

        let mut received = Vec::new();
        for _ in 0..2 {
            let (_topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
                .await
                .expect("timeout")
                .expect("recv");
            if let ProcessMessage::LaneTerminalOutput { data, .. } = msg {
                received.push(
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64"),
                );
            }
        }
        // replay は clear prefix で冪等化されてから配送される
        let mut expected_replay = REPLAY_CLEAR_PREFIX.to_vec();
        expected_replay.extend_from_slice(b"replayed-screen");
        assert_eq!(received[0], expected_replay);
        assert_eq!(received[1], b"live");
    }

    /// reconnect (既存 xterm 生存) を想定した 2 回目 replay も clear prefix で始まり、
    /// 冪等 (= 二重描画にならず、 同じ clear+snapshot に収束する) こと。
    #[tokio::test]
    async fn test_pump_replay_starts_with_clear_prefix() {
        let router = Arc::new(TopicRouter::new());
        let (_tx, rx) = broadcast::channel::<Vec<u8>>(16);
        let (_id, mut srx) = router.subscribe("process/terminal/data/vp~root/out").await;
        let _h = spawn_lane_terminal_pump(
            "vp/root".to_string(),
            1,
            b"screen".to_vec(),
            rx,
            router.clone(),
        );

        let (_topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        if let ProcessMessage::LaneTerminalOutput { data, .. } = msg {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(data)
                .expect("base64");
            assert!(
                decoded.starts_with(REPLAY_CLEAR_PREFIX),
                "replay の先頭は clear prefix で冪等化されるはず"
            );
        } else {
            panic!("想定外の message");
        }
    }

    /// replay が REPLAY_CHUNK を超える場合は分割して順序通り配送される。
    #[tokio::test]
    async fn test_pump_replay_chunked() {
        let router = Arc::new(TopicRouter::new());
        let (_tx, rx) = broadcast::channel::<Vec<u8>>(16);
        let (_id, mut srx) = router.subscribe("process/terminal/data/vp~root/out").await;

        // 1.5 chunk 分の replay → 2 message に分割される
        let replay: Vec<u8> = (0..(REPLAY_CHUNK + REPLAY_CHUNK / 2))
            .map(|i| (i % 251) as u8)
            .collect();
        let _h =
            spawn_lane_terminal_pump("vp/root".to_string(), 1, replay.clone(), rx, router.clone());

        let mut reassembled = Vec::new();
        for _ in 0..2 {
            let (_topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
                .await
                .expect("timeout")
                .expect("recv");
            if let ProcessMessage::LaneTerminalOutput { data, .. } = msg {
                reassembled.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64"),
                );
            }
        }
        // clear prefix + replay が分割 → 再結合で復元される
        let mut expected = REPLAY_CLEAR_PREFIX.to_vec();
        expected.extend_from_slice(&replay);
        assert_eq!(reassembled, expected);
    }

    /// 空バイト列は route しない (no-op)。
    #[tokio::test]
    async fn test_pump_skips_empty() {
        let router = Arc::new(TopicRouter::new());
        let (tx, rx) = broadcast::channel::<Vec<u8>>(16);
        let (_id, mut srx) = router.subscribe("process/terminal/data/vp~root/out").await;
        let _h = spawn_lane_terminal_pump("vp/root".to_string(), 1, Vec::new(), rx, router.clone());

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
