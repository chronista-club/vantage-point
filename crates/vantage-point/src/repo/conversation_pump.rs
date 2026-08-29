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
use crate::protocol::RepoMessage;
use crate::repo::topic_router::TopicRouter;

/// 1 session の chat host output broadcast を購読し、`ConversationEvent` topic に流す pump を spawn。
///
/// - `lane`: LaneAddress の Display 形（`"vp/root"` 等）。topic key 化は `TopicRouter` が担う。
///   ⚠️ session key を lane 名に埋めない（doc 38 落とし穴① — topic は per-lane のまま、
///   session は message の別 field で運ぶ）。
/// - `session`: 発生元 session の VP 採番 key（doc 38。N=1 特殊ケースは 1）。
/// - `rx`: chat host の `subscribe()` で得た ConversationEvent の broadcast receiver。
/// - `topic_router`: repo の topic_router。
/// - `replay_log`: `Some` = 配信 event を disk に per-session 記録して replay 源にする。
///   **transcript を持たない engine（cursor/codex）にだけ渡す** — claude は transcript が SSOT
///   なので `None`（二重化しない）。記録は配送と独立（書き込み失敗は warn するだけ）。
///
/// Host drop（broadcast Closed）で pump は自然終了する。lag 時は drop を warn して継続。
pub fn spawn_lane_conversation_pump(
    lane: String,
    session: crate::lane::session_registry::SessionKey,
    mut rx: broadcast::Receiver<ConversationEvent>,
    topic_router: Arc<TopicRouter>,
    replay_log: Option<ReplayLogTap>,
    activity: Arc<std::sync::atomic::AtomicU64>,
    turn_active: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tap = replay_log.map(ReplayTap::new);
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // roster `last_activity_at` の gui 側供給源（tui は PtySlot.last_output_at）。
                    // pump は全 chat engine の event が必ず通る唯一の点なので、ここ 1 箇所で足りる。
                    activity.store(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                        std::sync::atomic::Ordering::Relaxed,
                    );
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
        ConversationEvent::TurnCompleted { .. }
        | ConversationEvent::Error { .. }
        | ConversationEvent::EngineExited { .. } => Some(false),
        ConversationEvent::UserMessage { .. }
        | ConversationEvent::MessageChunk { .. }
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
}
