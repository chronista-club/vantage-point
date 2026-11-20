//! Lane conversation pump — gui 版の terminal_pump（doc 32 §3）。
//!
//! 1 つの Lane の [`ClaudeHost`] が broadcast する [`ConversationEvent`] を購読し、
//! per-lane topic (`repo/conversation/data/{lane}/event`) に [`RepoMessage::ConversationEvent`]
//! として route する。これにより gui の構造化イベントが単一 topic 空間に乗り、
//! daemon 経由で vp-app へ届く（terminal_pump と完全に同型）。
//!
//! data / calculations / actions:
//! - calculations: なし（pump は I/O bridge）
//! - actions: broadcast recv → topic route（副作用）

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::conversation::ConversationEvent;
use crate::conversation::replay_log::{self, ReplayLogTap};
use crate::conversation::replay_source::{AttachFuture, ReplayAttach, ReplayPort};
use crate::protocol::RepoMessage;
use crate::repo::topic_router::TopicRouter;

pub type HistoryRecovery = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
pub fn spawn_lane_conversation_pump(
    lane: String,
    session: crate::lane::session_registry::SessionKey,
    rx: broadcast::Receiver<ConversationEvent>,
    topic_router: Arc<TopicRouter>,
    replay_log: Option<ReplayLogTap>,
    activity: Arc<std::sync::atomic::AtomicU64>,
    turn_active: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    spawn_recovering_conversation_pump(
        lane,
        session,
        rx,
        topic_router,
        replay_log,
        activity,
        turn_active,
        None,
        None,
    )
}

/// 1 session の chat host output broadcast を購読し、`ConversationEvent` topic に流す pump を spawn。
///
/// - `lane`: LaneAddress の Display 形（`"vp/root"` 等）。topic key 化は `TopicRouter` が担う。
///   ⚠️ session key を lane 名に埋めない（doc 38 落とし穴① — topic は per-lane のまま、
///   session は message の別 field で運ぶ）。
/// - `session`: 発生元 session の VP 採番 key（doc 38。N=1 特殊ケースは 1）。
/// - `rx`: chat host の `subscribe()` で得た ConversationEvent の broadcast receiver。
/// - `topic_router`: repo の topic_router。
/// - `replay_log`: `Some` = 配信 event を disk に per-session 記録して replay 源にする。
///   **native 履歴を使わない engine にだけ渡す** — Claude / Codex は native 履歴が SSOT
///   なので `None`（二重化しない）。記録は配送と独立（書き込み失敗は warn するだけ）。
/// - `replay`: `Some` = 履歴を host の控えから配る engine（ACP — grok / opencode / agy）。
///   「配り直して」の依頼（chat を開いた時 / lag）を受けたら、host の `attach()` で控えと続きの
///   受け口を一緒に取り、`ReplayStart` → `SessionInit` → 控え → `ReplayEnd` を**この pump が**
///   流してから、受け口を差し替える（[`deliver_replay`]）。engine の出力と履歴を topic に流すのが
///   この pump だけなので、控えと続きの event が抜けも重なりも無く並ぶ。
///
/// host の受け口が閉じたら終了する（ACP は pump が履歴の口を通して host を握っているので、
/// engine を畳む時の abort か、依頼の口が落ちることで終わる）。lag 時は warn し、Codex は recover で、
/// ACP は配り直しで最新履歴を再配送する。
#[allow(clippy::too_many_arguments)]
pub fn spawn_recovering_conversation_pump(
    lane: String,
    session: crate::lane::session_registry::SessionKey,
    mut rx: broadcast::Receiver<ConversationEvent>,
    topic_router: Arc<TopicRouter>,
    replay_log: Option<ReplayLogTap>,
    activity: Arc<std::sync::atomic::AtomicU64>,
    turn_active: Arc<std::sync::atomic::AtomicBool>,
    recover: Option<HistoryRecovery>,
    mut replay: Option<ReplayPort>,
) -> JoinHandle<()> {
    let generation = topic_router.conversation_progress.begin(&lane, session);
    tokio::spawn(async move {
        let mut tap = replay_log.map(ReplayTap::new);
        let mut replaying = false;
        // 進行中の `attach()`（会話の確定を待つことがある）。待つ間も続きの event は流し続ける。
        let mut attaching: Option<AttachFuture> = None;
        loop {
            let received = tokio::select! {
                // engine の出力を先に読む。`attach()` が済んだ時に古い受け口に残っている本文や tool は、
                // 控えに入っているか、新しい受け口にも出る。控えに入らない event（`TurnCompleted` /
                // `SessionInit` / `Error` 等）は、最後に読んでから控えを取るまでの間に出た分だけ、
                // 古い受け口と一緒に落ちることがある（下の後始末で turn の状態と進みを合わせる）。
                biased;
                received = rx.recv() => received,
                request = next_replay_request(&mut replay), if attaching.is_none() => {
                    match (request, replay.as_mut()) {
                        (Some(()), Some(port)) => {
                            // 溜まっていた依頼は、この 1 回で足りる（控えはこれから取る）。
                            while port.requests.try_recv().is_ok() {}
                            attaching = Some(port.source.attach());
                        }
                        // 頼む側（host）が居なくなった。受け口の Closed で pump も終わる。
                        _ => replay = None,
                    }
                    continue;
                }
                attached = poll_attach(&mut attaching), if attaching.is_some() => {
                    attaching = None;
                    // 控えを待つ間に来た依頼も、今から配る 1 回で足りる。
                    if let Some(port) = replay.as_mut() {
                        while port.requests.try_recv().is_ok() {}
                    }
                    let init = replay.as_ref().and_then(|port| port.source.session_init());
                    let in_flight = attached.as_ref().ok().map(|attached| attached.in_flight);
                    if let Some(next) =
                        deliver_replay(&topic_router, &lane, session, init, attached).await
                    {
                        rx = next;
                        // 古い受け口と一緒に、控えに入らない event（`TurnCompleted` 等）を落とした
                        // かもしれない（最後に読んでから控えを取るまでの間に出た分）。turn の状態は
                        // 控えの真値に合わせる — 終わった turn を「実行中」のまま残すと、idle の
                        // engine が寝かされない。
                        if let Some(in_flight) = in_flight {
                            turn_active.store(in_flight, std::sync::atomic::Ordering::Relaxed);
                            if !in_flight {
                                settle_progress(&topic_router, &lane, session, generation);
                            }
                        }
                    }
                    continue;
                }
            };
            match received {
                Ok(event) => {
                    // 履歴を再配送する経路ではなく、実 engine の出力でだけ現在値を進める。
                    topic_router
                        .conversation_progress
                        .observe_engine(&lane, session, generation, &event);
                    // roster `last_activity_at` の gui 側供給源（tui は PtySlot.last_output_at）。
                    // pump は全 chat engine の event が必ず通る唯一の点なので、ここ 1 箇所で足りる。
                    activity.store(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    match &event {
                        ConversationEvent::ReplayStart => replaying = true,
                        ConversationEvent::ReplayEnd { .. } => replaying = false,
                        _ => {}
                    }
                    if matches!(&event, ConversationEvent::TurnCompleted { .. }) && !replaying {
                        let lane = lane.clone();
                        let event = event.clone();
                        let at = activity.load(std::sync::atomic::Ordering::Relaxed);
                        // registry の load-modify-save を async worker で実行しない。
                        let result = tokio::task::spawn_blocking(move || {
                            persist_response_event(
                                &crate::config::vp_state_dir(),
                                &lane,
                                session,
                                &event,
                                at,
                                false,
                            )
                        })
                        .await;
                        if !matches!(result, Ok(Ok(_))) {
                            tracing::warn!("response timestamp could not be saved: {result:?}");
                        }
                    }
                    if let Some(active) = turn_activity_of(&event) {
                        turn_active.store(active, std::sync::atomic::Ordering::Relaxed);
                    }
                    // 配送前に tap する（route は event を move で消費するため参照で先に記録）。
                    // 配信と記録は独立系統 = tap の失敗は route を止めない。
                    if let Some(tap) = tap.as_mut() {
                        tap.on_event(&event);
                    }
                    topic_router
                        .route(RepoMessage::ConversationEvent {
                            lane: lane.clone(),
                            session,
                            event,
                        })
                        .await;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "lane conversation pump lagged: {n} events dropped (lane={lane}#{session})"
                    );
                    if let Some(recover) = &recover {
                        // 古い tail を捨ててから、host の同じ ordered stream に snapshot を要求。
                        rx = rx.resubscribe();
                        recover();
                    } else if let (Some(port), None) = (replay.as_ref(), attaching.as_ref()) {
                        // ACP: 落とした分は host の控えにある。控えから配り直す。
                        attaching = Some(port.source.attach());
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // 終了時に coalesce 中の MessageChunk を取りこぼさない（turn 未完のまま
                    // engine が落ちても、そこまでの本文を replay 源に残す）。
                    if let Some(tap) = tap.as_mut() {
                        tap.flush();
                    }
                    tracing::debug!(
                        "lane conversation pump 終了 (host dropped, lane={lane}#{session})"
                    );
                    break;
                }
            }
        }
    })
}

/// 次の「配り直して」の依頼を待つ。履歴の口を持たない engine では永久に待つ（select の枝を寝かせる）。
async fn next_replay_request(replay: &mut Option<ReplayPort>) -> Option<()> {
    match replay {
        Some(port) => port.requests.recv().await,
        None => std::future::pending().await,
    }
}

/// 進行中の `attach()` を待つ。無ければ永久に待つ（select の枝を寝かせる）。
async fn poll_attach(attaching: &mut Option<AttachFuture>) -> Result<ReplayAttach, String> {
    match attaching {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// 控えを `ReplayStart` 〜 `ReplayEnd` で包んで topic に流し、続きの受け口を返す。
///
/// 並び: `ReplayStart` → `SessionInit`（あれば）→ 省いた旨の `Notice`（控えが上限で切られた時）→
/// 控え → `ReplayEnd { in_flight }`。
///
/// `attach()` が失敗した時は**何も流さず**、受け口は替えない（None を返す）。
/// - `ReplayStart` は送らない — GUI はあれを見て会話表示を消すので、履歴を取れなかった時に、
///   出ている会話まで消してしまう（Codex の履歴の失敗と同じ規律）
/// - 理由の `Error` もここでは出さない — 会話を確定できなかった時は、共有 host が自分で `Error` を
///   出してから `EngineExited` を出す（この pump が engine の出力として流す）。ここでも出すと、
///   同じ理由が 2 件並ぶ
///
/// ⚠️ **流す処理は別の task に預けて、終わりまで待つ**。pump は engine を畳む時に abort される
/// （[`crate::conversation::ChatEngineSlot`] の Drop）。この関数の中で直接 route すると、
/// `ReplayStart` を流した直後に abort されて `ReplayEnd` が出ないことがあり、GUI の「応答中」が
/// 下りなくなる。預けた task は pump が abort されても最後まで流す。
async fn deliver_replay(
    topic_router: &Arc<TopicRouter>,
    lane: &str,
    session: crate::lane::session_registry::SessionKey,
    session_init: Option<ConversationEvent>,
    attached: Result<ReplayAttach, String>,
) -> Option<broadcast::Receiver<ConversationEvent>> {
    let (batch, next) = match attached {
        Ok(attached) => (
            replay_batch(
                session_init,
                attached.events,
                attached.in_flight,
                attached.truncated,
            ),
            Some(attached.rx),
        ),
        Err(message) => {
            tracing::warn!(
                "conversation replay: 履歴を取れません (lane={lane}#{session}): {message}"
            );
            return None;
        }
    };
    let count = batch.len();
    let router = Arc::clone(topic_router);
    let target = lane.to_string();
    let delivery = tokio::spawn(async move {
        for event in batch {
            router
                .route(RepoMessage::ConversationEvent {
                    lane: target.clone(),
                    session,
                    event,
                })
                .await;
        }
    });
    if let Err(e) = delivery.await {
        tracing::warn!(
            "conversation replay: 配送の task が落ちました (lane={lane}#{session}): {e}"
        );
    }
    tracing::info!(
        "conversation replay（host の控え）: {count} events を配送 (lane={lane}#{session})"
    );
    next
}

/// turn が終わっているのに進みの投影（sidebar の「作業中」）が作業中のままなら、終端に合わせる。
///
/// 受け口の差し替えで `TurnCompleted` を落とした時のための後始末（呼ぶのは、配り直した控えが
/// 「turn の最中ではない」と言った時だけ）。投影に `TurnCompleted` を当てるだけで、topic には流さない。
/// すでに終端（待ち / 停止）なら触らない — turn の後に届いた `vp now` の自己申告を消さないため。
///
/// ⚠️ 応答の時刻（`persist_response_event`）はここでは書かない。落とした `TurnCompleted` が
/// 本当にあったかは分からない（控えが言うのは「今 turn の最中ではない」だけ）。その turn の
/// 応答の時刻は 1 回抜けることがある。
fn settle_progress(
    topic_router: &Arc<TopicRouter>,
    lane: &str,
    session: crate::lane::session_registry::SessionKey,
    generation: u64,
) {
    use crate::conversation::progress::ProgressPhase;
    let phase = topic_router
        .conversation_progress
        .view(lane, session)
        .map(|view| view.phase);
    if matches!(
        phase,
        Some(ProgressPhase::Working | ProgressPhase::Requesting)
    ) {
        topic_router.conversation_progress.observe_engine(
            lane,
            session,
            generation,
            &ConversationEvent::TurnCompleted {
                session_id: String::new(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            },
        );
    }
}

/// 控えを replay の 1 本に包む（純関数）。
fn replay_batch(
    session_init: Option<ConversationEvent>,
    events: Vec<ConversationEvent>,
    in_flight: bool,
    truncated: bool,
) -> Vec<ConversationEvent> {
    let mut batch = Vec::with_capacity(events.len() + 4);
    // `ReplayStart` を見て GUI が会話表示を消すので、`SessionInit` はその後に置く
    // （`conversation_replay::splice_session_init` と同じ位置）。
    batch.push(ConversationEvent::ReplayStart);
    batch.extend(session_init);
    if truncated {
        batch.push(ConversationEvent::Notice {
            message: "会話が長いため、古い部分と長い出力の一部を省いて表示しています".to_string(),
        });
    }
    batch.extend(events);
    batch.push(ConversationEvent::ReplayEnd { in_flight });
    batch
}

/// 履歴や途中の出力では応答時刻を更新しない。base はテストで隔離する。
fn persist_response_event(
    base: &Path,
    lane: &str,
    session: crate::lane::session_registry::SessionKey,
    event: &ConversationEvent,
    at: u64,
    replaying: bool,
) -> std::io::Result<bool> {
    if replaying {
        return Ok(false);
    }
    let ConversationEvent::TurnCompleted { session_id, .. } = event else {
        return Ok(false);
    };
    let Some(addr) = crate::repo::lane::parse_address(lane) else {
        return Ok(false);
    };
    crate::lane::session_registry::record_response_in(
        base, &addr.repo, &addr.name, session, session_id, at,
    )
}

/// この event が turn の進行状態をどう動かすか。`None` = 動かさない（判断材料でない）。
///
/// **turn 開始の event は存在しない**ので「終端を見たか」で推定する: 作業の event が来たら
/// 実行中、終端（[`ConversationEvent::TurnCompleted`] / `Error` / `EngineExited`）で終了。
///
/// ⚠️ **これが無いと長い tool 実行中の engine を殺す**。tool 実行の間 engine は event を
/// 出さないので、活動時刻だけでは「暇」と区別が付かない（実測: release build が 72 分
/// 無音 — 2026-08-28）。idle teardown はこの述語を **and 条件**として要求する。
///
/// `Question` / `PermissionRequest` は「人間待ち」= engine は動いていないが、**答える先が
/// 消えては困る**ので実行中に倒す。`NowLine`（`vp now` 由来）/ replay 系 / `SessionInit` は
/// turn の外なので `None`（現在の状態を保つ）。
fn turn_activity_of(event: &ConversationEvent) -> Option<bool> {
    match event {
        ConversationEvent::CodexInteractions { requests }
            if requests.iter().any(|r| r.item_id.is_none() || r.can_accept) =>
        {
            Some(true)
        }
        ConversationEvent::CodexHistory { in_flight, .. } => Some(*in_flight),
        ConversationEvent::TurnCompleted { .. }
        | ConversationEvent::Error { .. }
        | ConversationEvent::EngineExited { .. } => Some(false),
        ConversationEvent::UserMessage { .. }
        | ConversationEvent::MessageChunk { .. }
        | ConversationEvent::CodexMessage { .. }
        | ConversationEvent::ThoughtChunk { .. }
        | ConversationEvent::ToolCall { .. }
        | ConversationEvent::ToolCallUpdate { .. }
        | ConversationEvent::SubagentMessage { .. }
        | ConversationEvent::Plan { .. }
        | ConversationEvent::Question { .. }
        | ConversationEvent::PermissionRequest { .. } => Some(true),
        _ => None,
    }
}

/// pump の replay-log tap 状態。coalesce 用の pending buffer を持ち、配信 event を disk に写す。
///
/// coalesce の狙い（`file を細切れにしない + 順序保存`）:
/// - live の [`ConversationEvent::MessageChunk`] は 1 token 前後の高頻度 delta。1 行 1 delta で書くと
///   file が肥大 + 読みが遅い。tap 内で pending String に蓄積し、**記録対象の非 chunk event が
///   来る直前 / turn 完了 / pump 終了**で 1 本の MessageChunk として flush する
struct ReplayTap {
    tap: ReplayLogTap,
    /// `vp_state_dir()`（pump は長寿命 task なので spawn 時に 1 度だけ解決）。
    base: PathBuf,
    /// coalesce 中の本文（flush で 1 本の MessageChunk になる）。
    pending: String,
}

impl ReplayTap {
    fn new(tap: ReplayLogTap) -> Self {
        Self {
            tap,
            base: crate::config::vp_state_dir(),
            pending: String::new(),
        }
    }

    fn on_event(&mut self, event: &ConversationEvent) {
        tap_event(&self.base, &self.tap, &mut self.pending, event);
    }

    fn flush(&mut self) {
        flush_pending(&self.base, &self.tap, &mut self.pending);
    }
}

/// 1 event を replay log に反映する（coalesce 込み、純関数 = base 注入でテスト可）。
///
/// 記録する kind: `MessageChunk`（coalesce）/ `ToolCall` / `ToolCallUpdate` / `Plan` / `TurnCompleted`。
/// 記録しない kind とその理由:
/// - `SessionInit`: attach 時の eager resume で live 再発火する（過去分を焼き込むと二重化）
/// - `ThoughtChunk`: claude replay も thinking を復元しない parity + 量（thinking は暗号化復元不可）
/// - `Error`: 一時状態の再生は害（過去のエラーが会話に固定される）
/// - `Question` / `PermissionRequest`: control 面（doc 35「transcript に commit されない」と同じ扱い）
/// - `ReplayStart` / `ReplayEnd`: replay 制御マーカー（記録すると入れ子で二重 clear）
/// - `UserMessage`: pump には流れない（submit 成功後に unison_server が別途 append する）
/// - `NowLine`: 揮発の自己申告（doc 51 §1 A3b）— 過去の「今」を再生すると嘘になる
fn tap_event(base: &Path, tap: &ReplayLogTap, pending: &mut String, event: &ConversationEvent) {
    match event {
        // 高頻度 delta は貯めるだけ（ここでは書かない）。
        ConversationEvent::MessageChunk { text } => pending.push_str(text),
        // turn 完了: 貯めた本文を flush → turn 完了を書く → サイズ制御（turn 境界のみ）。
        ConversationEvent::TurnCompleted { .. } => {
            flush_pending(base, tap, pending);
            persist(base, tap, event);
            if let Err(e) = replay_log::truncate_if_needed_in(
                base,
                &tap.repo,
                &tap.label,
                replay_log::MAX_BYTES,
            ) {
                tracing::warn!(
                    "conversation replay-log truncate に失敗（label={}）: {e}",
                    tap.label
                );
            }
        }
        // 記録対象の構造化 event: 順序保存のため pending を先に flush してから書く。
        ConversationEvent::ToolCall { .. }
        | ConversationEvent::ToolCallUpdate { .. }
        | ConversationEvent::Plan { .. } => {
            flush_pending(base, tap, pending);
            persist(base, tap, event);
        }
        // 記録しない event（理由は関数 doc）。pending は据え置き（次の記録 event / 終了で flush）。
        _ => {}
    }
}

/// 貯めた本文を 1 本の MessageChunk として書き出し、pending を空にする（空なら no-op）。
fn flush_pending(base: &Path, tap: &ReplayLogTap, pending: &mut String) {
    if pending.is_empty() {
        return;
    }
    let event = ConversationEvent::MessageChunk {
        text: std::mem::take(pending),
    };
    persist(base, tap, &event);
}

/// 1 event を append（失敗は warn のみ = 配送を止めない）。
fn persist(base: &Path, tap: &ReplayLogTap, event: &ConversationEvent) {
    if let Err(e) = replay_log::append_in(base, &tap.repo, &tap.label, event) {
        tracing::warn!(
            "conversation replay-log 追記に失敗（label={}）: {e}",
            tap.label
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn interactions_keep_host_alive_while_waiting_for_user() {
        let request = crate::conversation::event::CodexInteraction {
            elicitation: None,
            cancel_on_deny: None,
            item_id: None,
            request_id: "codex:test:1".into(),
            kind: "question".into(),
            title: "質問".into(),
            details: String::new(),
            questions: vec![],
            blocking: true,
            can_accept: true,
        };
        assert_eq!(
            turn_activity_of(&ConversationEvent::CodexInteractions {
                requests: vec![request]
            }),
            Some(true)
        );
        assert_eq!(
            turn_activity_of(&ConversationEvent::CodexInteractions { requests: vec![] }),
            None
        );
    }

    #[tokio::test]
    async fn lag_discards_stale_deltas_and_requests_ordered_history() {
        let router = Arc::new(TopicRouter::new());
        let (tx, rx) = broadcast::channel(2);
        let (_, mut events) = router
            .subscribe("repo/conversation/data/vp~root/event")
            .await;
        for _ in 0..4 {
            tx.send(ConversationEvent::MessageChunk {
                text: "古い差分".into(),
            })
            .unwrap();
        }
        let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let recovery = Arc::new(move || {
            tx.send(ConversationEvent::CodexHistory {
                thread_id: "t".into(),
                events: vec![],
                user_message_ids: vec![],
                in_flight: false,
                truncated: false,
            })
            .unwrap();
        });
        let pump = spawn_recovering_conversation_pump(
            "vp/root".into(),
            2,
            rx,
            router,
            None,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            active.clone(),
            Some(recovery),
            None,
        );
        let (_, message) = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        pump.abort();
        assert!(matches!(
            message,
            RepoMessage::ConversationEvent {
                event: ConversationEvent::CodexHistory { .. },
                ..
            }
        ));
        assert!(!active.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// pump が ConversationEvent を per-lane topic に route し、subscriber が受け取れる。
    #[tokio::test]
    async fn test_pump_routes_conversation_event_to_per_lane_topic() {
        let router = Arc::new(TopicRouter::new());
        let (tx, rx) = broadcast::channel::<ConversationEvent>(16);

        // conversation data は非 retained なので route 前に subscribe が要る。
        let (_id, mut srx) = router
            .subscribe("repo/conversation/data/vp~root/event")
            .await;

        // claude 相当 = tap なし（transcript が SSOT）。
        let activity = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let turn_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _h = spawn_lane_conversation_pump(
            "vp/root".to_string(),
            2,
            rx,
            router.clone(),
            None,
            Arc::clone(&activity),
            Arc::clone(&turn_active),
        );

        tx.send(ConversationEvent::MessageChunk {
            text: "hello".to_string(),
        })
        .expect("send");

        let (topic, msg) = tokio::time::timeout(Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        // event が pump を通過した = 活動時刻が bump されている（roster enrich の供給源）。
        assert!(
            activity.load(std::sync::atomic::Ordering::Relaxed) > 0,
            "pump 通過で last_activity が bump されること"
        );
        // 作業 event なので turn 実行中に倒れる（idle teardown の guard）。
        assert!(
            turn_active.load(std::sync::atomic::Ordering::Relaxed),
            "MessageChunk 通過で turn_active が立つこと"
        );
        // doc 38 落とし穴①: session が topic key（lane 部分）に混入しないこと。
        assert_eq!(topic, "repo/conversation/data/vp~root/event");
        match msg {
            RepoMessage::ConversationEvent {
                lane,
                session,
                event,
            } => {
                assert_eq!(lane, "vp/root");
                assert_eq!(session, 2, "session は message の別 field で運ぶ");
                assert_eq!(
                    event,
                    ConversationEvent::MessageChunk {
                        text: "hello".into()
                    }
                );
            }
            other => panic!("想定外の message: {other:?}"),
        }
    }

    #[test]
    fn response_time_persists_only_live_completions_for_the_current_session() {
        use crate::lane::session_registry as registry;
        let tmp = tempfile::tempdir().unwrap();
        registry::set_conversation_in(
            tmp.path(),
            "test",
            "lead",
            "claude",
            1,
            Some("response-test"),
        )
        .unwrap();
        let event = ConversationEvent::TurnCompleted {
            session_id: "response-test".into(),
            cost_usd: None,
            context_tokens: None,
            context_window: None,
        };
        assert!(
            !persist_response_event(tmp.path(), "test/lane/lead", 1, &event, 1000, true).unwrap()
        );
        assert!(
            persist_response_event(tmp.path(), "test/lane/lead", 1, &event, 1000, false).unwrap()
        );
        assert!(
            !persist_response_event(
                tmp.path(),
                "test/lane/lead",
                1,
                &ConversationEvent::MessageChunk {
                    text: "tool progress".into()
                },
                2000,
                false
            )
            .unwrap()
        );
        assert!(
            !persist_response_event(tmp.path(), "test/lane/lead", 2, &event, 3000, false).unwrap()
        );
        assert_eq!(
            registry::load_in(tmp.path(), "test", "lead", "claude").sessions[0].response_at(),
            Some(1000)
        );
        registry::set_conversation_in(
            tmp.path(),
            "test",
            "lead",
            "claude",
            1,
            Some("next-conversation"),
        )
        .unwrap();
        assert!(
            !persist_response_event(tmp.path(), "test/lane/lead", 1, &event, 4000, false).unwrap()
        );
        assert_eq!(
            registry::load_in(tmp.path(), "test", "lead", "claude").sessions[0].response_at(),
            None
        );
    }

    /// idle teardown の guard（`turn_activity_of`）: 作業 event で実行中に倒れ、終端で暇に戻る。
    ///
    /// ⚠️ **長い tool 実行を殺さないための述語**。ToolCall の後に何時間 event が
    /// 来なくても「実行中」のままでなければならない（活動時刻だけでは守れない）。
    #[test]
    fn turn_activity_tracks_work_until_terminator() {
        use ConversationEvent as E;
        // 作業の event = 実行中
        for e in [
            E::MessageChunk { text: "x".into() },
            E::CodexMessage {
                item_id: "turn/message".into(),
                text: "x".into(),
                questions: Vec::new(),
                append: true,
            },
            E::ThoughtChunk { text: "x".into() },
            E::UserMessage { text: "x".into() },
        ] {
            assert_eq!(turn_activity_of(&e), Some(true), "作業 event: {e:?}");
        }
        // 終端 = 暇
        for e in [
            E::TurnCompleted {
                session_id: "s".into(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            },
            E::Error {
                message: "x".into(),
            },
            E::EngineExited {
                message: "x".into(),
            },
        ] {
            assert_eq!(turn_activity_of(&e), Some(false), "終端 event: {e:?}");
        }
        // turn の外 = 状態を動かさない（`vp now` で「実行中」に化けない）
        assert_eq!(turn_activity_of(&E::NowLine { text: "x".into() }), None);
        assert_eq!(turn_activity_of(&E::ReplayEnd { in_flight: false }), None);
    }

    /// tap の coalesce（純関数 `tap_event` / `flush_pending` を base 注入で直接検証）:
    /// 「chunk 3 連発 → ToolCall → TurnCompleted」を流すと、file には
    /// 「coalesced MessageChunk 1 本 → ToolCall → TurnCompleted」の順で残る。
    #[test]
    fn tap_coalesces_chunks_and_preserves_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tap = ReplayLogTap {
            repo: "vp".to_string(),
            label: "root#2".to_string(),
        };
        let mut pending = String::new();

        // 本文 delta 3 連発 → pending に貯まるだけ（まだ書かれない）。
        for part in ["Hel", "lo, ", "world"] {
            tap_event(
                tmp.path(),
                &tap,
                &mut pending,
                &ConversationEvent::MessageChunk {
                    text: part.to_string(),
                },
            );
        }
        assert!(
            replay_log::load_in(tmp.path(), "vp", "root#2").is_empty(),
            "chunk はまだ flush されていない"
        );

        // ToolCall（記録対象の非 chunk）→ pending を先に flush してから ToolCall を書く。
        tap_event(
            tmp.path(),
            &tap,
            &mut pending,
            &ConversationEvent::ToolCall {
                id: "t1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            },
        );
        // TurnCompleted → turn を書く（pending は空なので追加 flush なし）。
        tap_event(
            tmp.path(),
            &tap,
            &mut pending,
            &ConversationEvent::TurnCompleted {
                session_id: "s".to_string(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            },
        );

        let events = replay_log::load_in(tmp.path(), "vp", "root#2");
        assert_eq!(
            events,
            vec![
                ConversationEvent::MessageChunk {
                    text: "Hello, world".to_string()
                },
                ConversationEvent::ToolCall {
                    id: "t1".to_string(),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                },
                ConversationEvent::TurnCompleted {
                    session_id: "s".to_string(),
                    cost_usd: None,
                    context_tokens: None,
                    context_window: None,
                },
            ],
            "coalesced 1 本 → ToolCall → TurnCompleted の順"
        );
    }

    /// 記録しない event（ThoughtChunk / Error 等）は書かれず、pending も壊さない。
    /// flush は記録対象 event / 明示 flush（pump 終了 = Closed 相当）でのみ起きる。
    #[test]
    fn tap_skips_non_recorded_events_and_flush_on_close() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tap = ReplayLogTap {
            repo: "vp".to_string(),
            label: "root".to_string(),
        };
        let mut pending = String::new();

        tap_event(
            tmp.path(),
            &tap,
            &mut pending,
            &ConversationEvent::MessageChunk {
                text: "keep".to_string(),
            },
        );
        // ThoughtChunk は記録しない & pending を flush もしない。
        tap_event(
            tmp.path(),
            &tap,
            &mut pending,
            &ConversationEvent::ThoughtChunk {
                text: "secret thinking".to_string(),
            },
        );
        // Error も記録しない。
        tap_event(
            tmp.path(),
            &tap,
            &mut pending,
            &ConversationEvent::Error {
                message: "transient".to_string(),
            },
        );
        assert!(
            replay_log::load_in(tmp.path(), "vp", "root").is_empty(),
            "記録対象が来るまで何も書かれない"
        );

        // 明示 flush で貯めた本文だけが 1 本残る（thinking / error は残らない）。
        flush_pending(tmp.path(), &tap, &mut pending);
        assert_eq!(
            replay_log::load_in(tmp.path(), "vp", "root"),
            vec![ConversationEvent::MessageChunk {
                text: "keep".to_string()
            }],
            "記録しない event は残さず、貯めた本文だけ flush される"
        );
    }

    // ---- 履歴を host の控えから配る（ACP engine、conversation::replay_source）----

    use crate::conversation::acp_host::testing::{FakeAgent, Script, agent_text, user_text};
    use crate::conversation::acp_host::{AcpAgentHost, AcpEngine, AcpHostConfig};
    use crate::conversation::replay_source::{ReplayRequester, ReplaySource};

    const TOPIC: &str = "repo/conversation/data/vp~root/event";

    /// 画面に見える会話。GUI と同じ規則で畳む: `ReplayStart` で消し、発話と本文を足す
    /// （続く本文は 1 つにまとめる）。
    #[derive(Default)]
    struct Screen {
        lines: Vec<(char, String)>,
    }

    impl Screen {
        fn apply(&mut self, event: &ConversationEvent) {
            match event {
                ConversationEvent::ReplayStart => self.lines.clear(),
                ConversationEvent::UserMessage { text } => self.lines.push(('U', text.clone())),
                ConversationEvent::MessageChunk { text } => match self.lines.last_mut() {
                    Some(('A', last)) => last.push_str(text),
                    _ => self.lines.push(('A', text.clone())),
                },
                _ => {}
            }
        }

        /// agent の本文を全部つないだもの（発話の区切りに依らずに、抜けと重なりを見る）。
        fn answers(&self) -> String {
            self.lines
                .iter()
                .filter(|(who, _)| *who == 'A')
                .map(|(_, text)| text.as_str())
                .collect()
        }

        fn count_user(&self, text: &str) -> usize {
            self.lines
                .iter()
                .filter(|(who, line)| *who == 'U' && line == text)
                .count()
        }
    }

    async fn next_event(
        topic: &mut tokio::sync::mpsc::Receiver<(String, RepoMessage)>,
    ) -> ConversationEvent {
        let (_, message) = tokio::time::timeout(Duration::from_secs(5), topic.recv())
            .await
            .expect("topic の event を待ち切れない")
            .expect("topic open");
        match message {
            RepoMessage::ConversationEvent { event, .. } => event,
            other => panic!("想定外の message: {other:?}"),
        }
    }

    /// turn の最中に何度配り直しても、画面に見える会話に抜けも重なりも無い（lead の条件 a）。
    ///
    /// 本物の `AcpClientHost` に偽の agent をつなぎ、pump を通して topic で見る。過去の会話 2 件を
    /// load で流し、turn の終わりまでに配り直しを 4 回頼む（発話を送る前に 1 回、turn の途中 =
    /// 本文を 1 文字ずつ 10 回流す間に 3 回。turn の後にもう 1 回頼むので、test 全体では 5 回）。頼む時機と本文の到着の前後はわざと決めていない — どの順になっても
    /// 成り立つことを見る。実際に何回配り直されるかも決めていない（1 回以上、とだけ確かめる）。
    ///
    /// この test は時機を決めていないので、「受け口を差し替えない」壊し方は捉えない（実際に壊して
    /// 緑のままだった）。その壊し方は `event_between_the_last_read_and_the_snapshot_appears_once` が
    /// 時機を作って捉える。ここで落ちるのは、控えそのものが欠ける・重なる場合（host が送信の時に
    /// 発話を控えに入れない、load の流れを会話中の event としても流す、など）。
    #[tokio::test]
    async fn redelivery_in_the_middle_of_a_turn_neither_drops_nor_repeats() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, connector) = FakeAgent::new(Script {
            replay: vec![user_text("前の発話"), agent_text("前の答え")],
            ..Script::default()
        });
        let host = AcpAgentHost::spawn_with(
            AcpHostConfig {
                engine: AcpEngine::Agy,
                cwd: "/tmp/vp-acp".into(),
                repo: "vptest-acp-pump".into(),
                lane: "lead".into(),
                lane_label: "lead".into(),
                session_key: 1,
                session_id: Some("sess-1".into()),
                config_intent: Vec::new(),
            },
            connector,
        )
        .expect("起動");
        let router = Arc::new(TopicRouter::new());
        let (_id, mut topic) = router.subscribe(TOPIC).await;
        let _pump = spawn_recovering_conversation_pump(
            "vp/root".into(),
            2,
            host.subscribe(),
            router.clone(),
            None,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            None,
            host.take_replay_port(),
        );

        // chat を開いた（会話の確定の前に頼んでも、確定を待って配る）。
        assert!(host.request_replay());
        host.submit("今の発話").await.expect("submit");
        // turn が始まってから本文を流す（発話が届く前の update は、会話がまだ無いので捨てられる）。
        agent.wait_for("session/prompt").await;
        for i in 0..10 {
            agent.update(agent_text(&i.to_string()));
            if i % 3 == 1 {
                host.request_replay();
            }
            tokio::task::yield_now().await;
        }
        agent.end_turn();

        let mut screen = Screen::default();
        let mut replays = 0;
        let mut in_replay = false;
        loop {
            let event = next_event(&mut topic).await;
            screen.apply(&event);
            match event {
                ConversationEvent::ReplayStart => {
                    assert!(!in_replay, "配り直しが入れ子になった");
                    in_replay = true;
                    replays += 1;
                }
                ConversationEvent::ReplayEnd { .. } => {
                    assert!(in_replay, "ReplayStart の無い ReplayEnd");
                    in_replay = false;
                }
                ConversationEvent::TurnCompleted { .. } => {
                    assert!(!in_replay, "配り直しの途中に turn の終わりが割り込んだ");
                    break;
                }
                _ => {}
            }
        }
        assert!(replays >= 1, "少なくとも 1 回は配り直している");
        // turn の終わりの時点: 前の答えと今の答えが、欠けず重ならず 1 回ずつ。
        assert_eq!(screen.answers(), "前の答え0123456789", "{:?}", screen.lines);
        assert_eq!(screen.count_user("前の発話"), 1, "{:?}", screen.lines);
        assert!(screen.count_user("今の発話") <= 1, "{:?}", screen.lines);

        // turn の後にもう一度開き直す: 控えは会話の全体を、起きた順で持っている。
        // （turn の途中で頼んだ分が後から配られることもあるので、止まるまで読む）
        host.request_replay();
        let mut last_in_flight = None;
        while let Ok(Some((_, RepoMessage::ConversationEvent { event, .. }))) =
            tokio::time::timeout(Duration::from_millis(300), topic.recv()).await
        {
            screen.apply(&event);
            if let ConversationEvent::ReplayEnd { in_flight } = event {
                last_in_flight = Some(in_flight);
            }
        }
        assert_eq!(last_in_flight, Some(false), "turn は終わっている");
        assert_eq!(
            screen.lines,
            [
                ('U', "前の発話".to_string()),
                ('A', "前の答え".to_string()),
                ('U', "今の発話".to_string()),
                ('A', "0123456789".to_string()),
            ]
        );
    }

    /// 偽の履歴の出どころ（`attach()` の契約だけを持つ）。
    struct FakeSource {
        tx: broadcast::Sender<ConversationEvent>,
        history: Arc<std::sync::Mutex<Vec<ConversationEvent>>>,
        fail: Option<String>,
        /// 次の `attach()` が控えを取る**直前**に出す event（pump が最後に受け口を読んでから
        /// 控えを取るまでの間に、engine が event を出した場面を作る）。
        just_before_snapshot: Arc<std::sync::Mutex<Option<ConversationEvent>>>,
        /// `attach()` が返す `in_flight`（既定は true = turn の最中）。
        in_flight: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeSource {
        fn new(fail: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                tx: broadcast::channel(4).0,
                history: Arc::default(),
                fail: fail.map(str::to_string),
                just_before_snapshot: Arc::default(),
                in_flight: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            })
        }

        /// 控えへの追加と送信を同じ lock の中で行う（本物の host と同じ契約）。
        fn emit(&self, event: ConversationEvent) {
            emit_into(&self.history, &self.tx, event);
        }
    }

    fn emit_into(
        history: &std::sync::Mutex<Vec<ConversationEvent>>,
        tx: &broadcast::Sender<ConversationEvent>,
        event: ConversationEvent,
    ) {
        let mut history = history.lock().unwrap();
        history.push(event.clone());
        let _ = tx.send(event);
    }

    impl ReplaySource for FakeSource {
        fn attach(&self) -> AttachFuture {
            let (tx, history, fail, late, in_flight) = (
                self.tx.clone(),
                Arc::clone(&self.history),
                self.fail.clone(),
                Arc::clone(&self.just_before_snapshot),
                Arc::clone(&self.in_flight),
            );
            // 控えを取るのは poll された時（本物の host と同じ — 呼んだ時ではない）。
            Box::pin(async move {
                if let Some(message) = fail {
                    return Err(message);
                }
                if let Some(event) = late.lock().unwrap().take() {
                    emit_into(&history, &tx, event);
                }
                let history = history.lock().unwrap();
                Ok(ReplayAttach {
                    events: history.clone(),
                    in_flight: in_flight.load(std::sync::atomic::Ordering::Relaxed),
                    truncated: false,
                    rx: tx.subscribe(),
                })
            })
        }

        fn session_init(&self) -> Option<ConversationEvent> {
            None
        }
    }

    fn chunk(text: &str) -> ConversationEvent {
        ConversationEvent::MessageChunk { text: text.into() }
    }

    /// 配り直しの途中で pump が止められても、`ReplayStart` だけ出て `ReplayEnd` が出ない、を
    /// 作らない（lead の条件 b）。pump は engine を畳む時に abort される。
    ///
    /// 履歴は 300 件にしている。`route` は tokio の RwLock を待つので、1 つの task が続けて
    /// 128 回ほど通ると tokio が強制的に譲らせる（協調の予算）— 長い履歴では配送の途中に
    /// abort の入る隙ができる。50 件では隙ができず、下の壊し方でも緑のままだった。
    ///
    /// 落ちるべき壊し方: `deliver_replay` の中で直接 route する（別の task に預けない）。
    /// 途中で abort されて、`ReplayEnd` が出ない。
    #[tokio::test]
    async fn aborting_the_pump_mid_delivery_still_ends_the_replay() {
        let router = Arc::new(TopicRouter::new());
        let (_id, mut topic) = router.subscribe(TOPIC).await;
        let source = FakeSource::new(None);
        for i in 0..300 {
            source.emit(chunk(&i.to_string()));
        }
        let attached = source.attach().await;
        let router_for_task = router.clone();
        let delivering = tokio::spawn(async move {
            deliver_replay(&router_for_task, "vp/root", 2, None, attached).await
        });
        // 配り始めた所で止める（pump の abort と同じ）。
        tokio::task::yield_now().await;
        delivering.abort();

        let mut seen = Vec::new();
        loop {
            let event = next_event(&mut topic).await;
            let end = matches!(event, ConversationEvent::ReplayEnd { .. });
            seen.push(event);
            if end {
                break;
            }
        }
        assert_eq!(seen.first(), Some(&ConversationEvent::ReplayStart));
        assert_eq!(seen.len(), 302, "ReplayStart + 300 件 + ReplayEnd");
    }

    /// 履歴を取れなかった時（engine が会話を確定できなかった）は、pump からは何も足さない。
    /// `ReplayStart` を送らず（出ている会話を消さない）、理由の `Error` は共有 host が engine の出力と
    /// して出したものだけが流れる（pump が同じ理由をもう 1 件足さない）。
    /// 落ちるべき壊し方: `ReplayStart` → `Error` → `ReplayEnd` で包む — GUI が会話表示を消す。
    #[tokio::test]
    async fn failed_attach_adds_nothing_and_the_hosts_own_error_flows_through() {
        let router = Arc::new(TopicRouter::new());
        let (_id, mut topic) = router.subscribe(TOPIC).await;
        let source = FakeSource::new(Some("会話を確定できません: session/new: 認証が要ります"));
        let (requester, requests) = ReplayRequester::channel();
        let _pump = spawn_recovering_conversation_pump(
            "vp/root".into(),
            2,
            source.tx.subscribe(),
            router.clone(),
            None,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            None,
            Some(ReplayPort {
                source: source.clone(),
                requests,
            }),
        );
        assert!(requester.request());
        assert!(
            tokio::time::timeout(Duration::from_millis(200), topic.recv())
                .await
                .is_err(),
            "ReplayStart も ReplayEnd も、pump からの Error も送らない"
        );
        // 共有 host が出した Error は、engine の出力としてそのまま流れる。
        let own = ConversationEvent::Error {
            message: "会話を確定できませんでした（session/new: 認証が要ります）".into(),
        };
        source.tx.send(own.clone()).unwrap();
        assert_eq!(next_event(&mut topic).await, own);
    }

    /// 溜まった依頼は 1 回の配り直しにまとめる（起動時に demand が 3 つ重なっても、全量を 3 回は
    /// 配らない）。配り直しの後は、turn の状態を控えの真値に合わせる。
    /// 落ちるべき壊し方: 依頼を 1 件ずつ取り出して、そのたびに配る — `ReplayStart` が 3 回出る。
    #[tokio::test]
    async fn queued_requests_collapse_into_one_delivery_and_turn_state_follows_the_history() {
        let router = Arc::new(TopicRouter::new());
        let (_id, mut topic) = router.subscribe(TOPIC).await;
        let source = FakeSource::new(None);
        source.emit(chunk("a"));
        let (requester, requests) = ReplayRequester::channel();
        // pump が起きる前に 3 つ溜める。
        for _ in 0..3 {
            assert!(requester.request());
        }
        let turn_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _pump = spawn_recovering_conversation_pump(
            "vp/root".into(),
            2,
            source.tx.subscribe(),
            router.clone(),
            None,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            turn_active.clone(),
            None,
            Some(ReplayPort {
                source: source.clone(),
                requests,
            }),
        );
        let mut starts = 0;
        while let Ok(Some((_, RepoMessage::ConversationEvent { event, .. }))) =
            tokio::time::timeout(Duration::from_millis(300), topic.recv()).await
        {
            if event == ConversationEvent::ReplayStart {
                starts += 1;
            }
        }
        assert_eq!(starts, 1);
        // 偽の出どころは in_flight = true を返す。
        assert!(turn_active.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// pump が最後に受け口を読んでから控えを取るまでの間に出た event は、控えに入っていて、
    /// 古い受け口にも残っている。配り直しの後は `attach()` が返した受け口に差し替えるので、
    /// 古い受け口の分は読まず、画面には 1 回だけ出る。
    ///
    /// 落ちるべき壊し方: 配り直しの後も古い受け口を読み続ける（差し替えない）。控えで出した
    /// event が `ReplayEnd` の後にもう 1 回出る。
    #[tokio::test]
    async fn event_between_the_last_read_and_the_snapshot_appears_once() {
        let router = Arc::new(TopicRouter::new());
        let (_id, mut topic) = router.subscribe(TOPIC).await;
        let source = FakeSource::new(None);
        source.emit(chunk("a"));
        *source.just_before_snapshot.lock().unwrap() = Some(chunk("b"));
        let (requester, requests) = ReplayRequester::channel();
        let _pump = spawn_recovering_conversation_pump(
            "vp/root".into(),
            2,
            source.tx.subscribe(),
            router.clone(),
            None,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            None,
            Some(ReplayPort {
                source: source.clone(),
                requests,
            }),
        );
        assert!(requester.request());
        let mut screen = Screen::default();
        loop {
            let event = next_event(&mut topic).await;
            screen.apply(&event);
            if matches!(event, ConversationEvent::ReplayEnd { .. }) {
                break;
            }
        }
        // 差し替えた受け口から、続きが届く。
        source.emit(chunk("c"));
        while let Ok(Some((_, RepoMessage::ConversationEvent { event, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), topic.recv()).await
        {
            screen.apply(&event);
        }
        assert_eq!(screen.answers(), "abc", "{:?}", screen.lines);
    }

    /// 受け口の差し替えで `TurnCompleted` を落としても（最後に読んでから控えを取るまでの間に turn が
    /// 終わった）、進みの投影（sidebar の「作業中」）と turn の状態は、控えの真値（turn は終わって
    /// いる）に合う。
    ///
    /// 落ちるべき壊し方: 配り直しの後に何もしない — 進みが working のまま、次の event が来るまで残る。
    #[tokio::test]
    async fn turn_end_lost_at_the_swap_still_settles_progress_and_turn_state() {
        use crate::conversation::progress::ProgressPhase;
        let router = Arc::new(TopicRouter::new());
        let (_id, mut topic) = router.subscribe(TOPIC).await;
        let source = FakeSource::new(None);
        let (requester, requests) = ReplayRequester::channel();
        let turn_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let _pump = spawn_recovering_conversation_pump(
            "vp/root".into(),
            2,
            source.tx.subscribe(),
            router.clone(),
            None,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            turn_active.clone(),
            None,
            Some(ReplayPort {
                source: source.clone(),
                requests,
            }),
        );
        // turn の最中: 本文が流れて、進みは working。
        source.emit(chunk("a"));
        assert_eq!(next_event(&mut topic).await, chunk("a"));
        assert_eq!(
            router
                .conversation_progress
                .view("vp/root", 2)
                .map(|v| v.phase),
            Some(ProgressPhase::Working)
        );
        assert!(turn_active.load(std::sync::atomic::Ordering::Relaxed));

        // pump が `TurnCompleted` を見ないまま turn が終わり、配り直しが来る。
        source
            .in_flight
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(requester.request());
        loop {
            if matches!(
                next_event(&mut topic).await,
                ConversationEvent::ReplayEnd { in_flight: false }
            ) {
                break;
            }
        }
        // 配送の後の後始末が済むのを、期限つきで待つ（pump の task がいつ進むかに依らない）。
        let phase = || {
            router
                .conversation_progress
                .view("vp/root", 2)
                .map(|v| v.phase)
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while turn_active.load(std::sync::atomic::Ordering::Relaxed)
            || phase() != Some(ProgressPhase::Waiting)
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn の状態と進みが終端にならない: turn_active={} progress={:?}",
                turn_active.load(std::sync::atomic::Ordering::Relaxed),
                phase()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// pump が取りこぼした（broadcast の lag）時は、host の控えから配り直す。画面は欠けない。
    /// 落ちるべき壊し方: lag で warn するだけ（今までの ACP の動き）— 落とした本文が画面から欠ける。
    #[tokio::test]
    async fn lag_redelivers_from_the_history() {
        let router = Arc::new(TopicRouter::new());
        let (_id, mut topic) = router.subscribe(TOPIC).await;
        let source = FakeSource::new(None);
        let (_requester, requests) = ReplayRequester::channel();
        let rx = source.tx.subscribe();
        // 受け口の容量（4）を超えて流してから pump を起こす = 最初の recv が Lagged になる。
        for i in 0..8 {
            source.emit(chunk(&i.to_string()));
        }
        let _pump = spawn_recovering_conversation_pump(
            "vp/root".into(),
            2,
            rx,
            router.clone(),
            None,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            None,
            Some(ReplayPort {
                source: source.clone(),
                requests,
            }),
        );
        let mut screen = Screen::default();
        loop {
            let event = next_event(&mut topic).await;
            screen.apply(&event);
            if matches!(event, ConversationEvent::ReplayEnd { .. }) {
                break;
            }
        }
        assert_eq!(screen.answers(), "01234567");
    }

    /// 控えを replay の 1 本に包む並び。`SessionInit` は `ReplayStart` の直後、切られた控えには
    /// 省いた旨の `Notice` を足す。
    #[test]
    fn replay_batch_orders_markers_init_notice_and_history() {
        let init = ConversationEvent::SessionInit {
            session_id: "s".into(),
            model: None,
            permission_mode: None,
            cwd: None,
            tools: Vec::new(),
            mcp_servers: Vec::new(),
            slash_commands: Vec::new(),
            command_docs: Default::default(),
        };
        let batch = replay_batch(Some(init.clone()), vec![chunk("a")], true, true);
        assert_eq!(batch[0], ConversationEvent::ReplayStart);
        assert_eq!(batch[1], init);
        assert!(matches!(batch[2], ConversationEvent::Notice { .. }));
        assert_eq!(batch[3], chunk("a"));
        assert_eq!(batch[4], ConversationEvent::ReplayEnd { in_flight: true });
        assert_eq!(
            replay_batch(None, Vec::new(), false, false),
            [
                ConversationEvent::ReplayStart,
                ConversationEvent::ReplayEnd { in_flight: false }
            ]
        );
    }
}
