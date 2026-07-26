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

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use tokio::sync::{RwLock, broadcast};
use tokio::task::JoinHandle;

use crate::lane::session_registry::SessionKey;
use crate::process::lanes_state::LanePool;
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

/// lane の terminal topic（concrete）。購読・demand 計上・pump route が共有する 1 つの形
/// （lane key は LaneAddress の `/` を `~` に encode する、 `TopicRouter::topic_for` と同じ規約）。
pub fn lane_topic(lane: &str) -> String {
    format!("process/terminal/data/{}/out", lane.replace('/', "~"))
}

/// 1 本の terminal pump の台帳 entry。
///
/// `slot_pid` は **張った先の slot 実体の pid = pump の identity**（doc 53 R2 / doc 54
/// 「identity は実体に」）。「slot が差し替わったか」を呼び手の知識（restart の mode / act の
/// 向き）でなく、 live slot の pid との照合で決める — これが reconcile の actual 側。
pub struct TerminalPump {
    /// attach した時点の slot の pid（照合キー。 pid は slot の生涯で不変）。
    pub slot_pid: u32,
    /// attach した時点の**購読の世代**（`TopicRouter::subscriber_epoch`）。
    ///
    /// slot pid が「server 側の生産（張り直すべきか）」を答えるのに対し、これは
    /// 「**client が画面を持っているか**（replay を流すべきか）」を答える。GUI が再起動すると
    /// slot は同じまま購読者だけが入れ替わるので、pid だけでは「変化なし」と誤答して
    /// **新しい GUI に過去の画面が届かない**（doc 53 §6.5.0）。2 つの問いに 1 つの述語で
    /// 答えていたのを分けたもの（[[one-predicate-three-properties]]）。
    pub subscriber_epoch: Option<u64>,
    /// pump task の handle。 撤去は abort、 source 断 (slot drop) では自然終了する。
    pub handle: JoinHandle<()>,
}

/// demand-driven terminal pump の lane → session → pump 台帳（`AppState::terminal_pumps`）。
/// 外側 key は LaneAddress の Display 形 (`"<project>/root"` 等)。
pub type TerminalPumps = HashMap<String, HashMap<SessionKey, TerminalPump>>;

/// reconcile が attach する 1 件分: (session, slot pid, attach_output の replay + rx)。
type PumpAttach = (SessionKey, u32, (Vec<u8>, broadcast::Receiver<Vec<u8>>));

/// [`reconcile_lane_pumps`] の結果（ログ・テスト用の観測値）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PumpReconcile {
    /// 新設 / 差替した pump 数（= replay が飛んだ pane 数）。
    pub attached: usize,
    /// 撤去した pump 数（slot 消滅 / demand 消滅 / 差替の旧側）。
    pub removed: usize,
    /// 触らなかった健在 pump 数（= 無傷の兄弟 pane 数）。
    pub kept: usize,
}

/// 指定 lane の terminal pump を「あるべき状態」に合わせる（doc 53 R2: pump の reconcile 化）。
///
/// - intent = demand（topic の現在購読者 > 0、 [`TopicRouter::demand_active`]）×
///   生きた slot の (session, pid) 一覧（[`LanePool::slot_pids`]）
/// - actual = `terminal_pumps` の (session, 張った時の slot_pid)
///
/// pid が一致する pump は**触らない** — 兄弟 pane に clear + 全 replay を撃たない保証が
/// scope 引数（旧 `respawn_terminal_pump` の `only`）でなく構造から出る（team-b 10 回目の
/// regression の構造化）。不一致 / 欠落は attach_output → pump 新設（replay-on-attach）、
/// desired に無い pump は撤去。demand が無ければ desired = ∅（= 全撤去、 lazy production）。
///
/// 呼び手（demand hook / 動詞の末尾 / boot 復元後）は全員ただの**契機**であって判断を
/// 持たない。冪等なので契機が重なっても 1 本に収束する（二重 demand / 復元中の demand edge
/// との race — doc 50 §4.7 の「直さないと決めた 1 件」はこの収束性で消える）。
pub async fn reconcile_lane_pumps(
    lane_pool: &RwLock<LanePool>,
    terminal_pumps: &RwLock<TerminalPumps>,
    topic_router: &Arc<TopicRouter>,
    lane: &str,
) -> PumpReconcile {
    reconcile_lane_pumps_inner(lane_pool, terminal_pumps, topic_router, lane, false).await
}

/// **client が「画面を持っていない」と名乗ったときの reconcile**（`force_replay = true`）。
///
/// 通常の reconcile は「pump を張り直す必要があるか」で attach を決めるので、pump も購読者も
/// 変わっていなければ replay は流れない（それが正しい — 生きた console を無闇に clear しない）。
/// ところが **client が replay を受け取り損ねた**場合、その事実は server から観測できない:
///
/// - GUI 再起動の replay は **webview が JS を読み込む前**（実測 0.4 秒前）に届いて捨てられる
/// - terminal の replay は一度きりなので二度と来ない → console が黒いまま（doc 53 §6.5.0）
///
/// JS が ready を名乗った後に client がこれを要求する（`terminal_demand_start` の `replay:true`）。
/// **client の明示的な要求を server 側の推測で断らない** — 二重描画は replay 先頭の
/// clear prefix が吸収する（本 module 冒頭の doc）。
pub async fn reconcile_lane_pumps_forcing_replay(
    lane_pool: &RwLock<LanePool>,
    terminal_pumps: &RwLock<TerminalPumps>,
    topic_router: &Arc<TopicRouter>,
    lane: &str,
) -> PumpReconcile {
    reconcile_lane_pumps_inner(lane_pool, terminal_pumps, topic_router, lane, true).await
}

async fn reconcile_lane_pumps_inner(
    lane_pool: &RwLock<LanePool>,
    terminal_pumps: &RwLock<TerminalPumps>,
    topic_router: &Arc<TopicRouter>,
    lane: &str,
    force_replay: bool,
) -> PumpReconcile {
    let Some(addr) = LanePool::parse_address(lane) else {
        return PumpReconcile::default();
    };
    let demand = topic_router.demand_active(&lane_topic(lane));

    // actual の snapshot。finished handle は「不在」と数える（pump は source 断で自然終了
    // する — 万一 slot が生きたまま pump だけ死んだ場合も差替対象に落ちる保険）。
    // 今 その topic を見ている購読の世代（GUI 再起動で必ず増える）。
    let epoch = topic_router.subscriber_epoch(&lane_topic(lane)).await;
    let current: HashMap<SessionKey, (u32, Option<u64>)> = {
        let pumps = terminal_pumps.read().await;
        pumps
            .get(lane)
            .map(|m| {
                m.iter()
                    .filter(|(_, p)| !p.handle.is_finished())
                    .map(|(k, p)| (*k, (p.slot_pid, p.subscriber_epoch)))
                    .collect()
            })
            .unwrap_or_default()
    };

    // intent 側: (session, pid) 列挙と attach を**同一 read guard 内で**原子的に行う
    // （列挙と subscribe の間に slot が差し替わると replay snapshot と rx の境界がずれる）。
    // attach は差替が要る session だけ — 健在な pump の session には replay を撃たない。
    let (live, attaches) = {
        let pool = lane_pool.read().await;
        let live = pool.slot_pids(&addr);
        let attaches: Vec<PumpAttach> = if demand {
            live.iter()
                // **2 つの理由**で張り直す（片方だけでは足りない）:
                // ① slot が差し替わった（pid 不一致）= server 側の生産が別物になった
                // ② 購読者が入れ替わった（epoch 不一致）= client が画面を持っていない
                //    → GUI 再起動は ② だけが動く（slot は生きたまま）。pid だけ見ていた頃は
                //      「変化なし」と判定して replay を落としていた（doc 53 §6.5.0）。
                // replay の二重描画は clear prefix が吸収する（既存 xterm が生きたまま
                // demand が立ち直る場合を想定済み — 本 module 冒頭の doc）。
                .filter(|(s, pid)| force_replay || current.get(s) != Some(&(*pid, epoch)))
                .filter_map(|&(s, pid)| pool.attach_output(&addr, Some(s)).map(|a| (s, pid, a)))
                .collect()
        } else {
            Vec::new()
        };
        (live, attaches)
    };

    // 適用: 新設の差し込み + desired 外の撤去。abort は lock を手放してから。
    let mut result = PumpReconcile::default();
    let mut removed = Vec::new();
    {
        let mut pumps = terminal_pumps.write().await;
        let lane_pumps = pumps.entry(lane.to_string()).or_default();
        for (session, pid, (replay, rx)) in attaches {
            let handle = spawn_lane_terminal_pump(
                lane.to_string(),
                session,
                replay,
                rx,
                topic_router.clone(),
            );
            result.attached += 1;
            if let Some(old) = lane_pumps.insert(
                session,
                TerminalPump {
                    subscriber_epoch: epoch,
                    slot_pid: pid,
                    handle,
                },
            ) {
                removed.push(old.handle);
            }
        }
        // desired = demand ? live の session 集合 : ∅
        let stale: Vec<SessionKey> = lane_pumps
            .keys()
            .copied()
            .filter(|k| !demand || !live.iter().any(|(s, _)| s == k))
            .collect();
        for k in stale {
            if let Some(p) = lane_pumps.remove(&k) {
                removed.push(p.handle);
            }
        }
        result.kept = lane_pumps.len() - result.attached;
        if lane_pumps.is_empty() {
            pumps.remove(lane);
        }
    }
    result.removed = removed.len();
    for h in removed {
        h.abort();
    }
    if result != PumpReconcile::default() {
        tracing::info!(
            "terminal pump reconcile (lane={lane}, demand={demand}, attached={}, removed={}, kept={})",
            result.attached,
            result.removed,
            result.kept
        );
    }
    result
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
