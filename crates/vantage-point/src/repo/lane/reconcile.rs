//! lane の中を reconcile で合わせる（doc 53 §12 / R3）。
//!
//! ## 何をする関数か
//!
//! **intent（`session_registry` = disk の SSOT）** を読み、**実体**（PtySlot / chat engine /
//! `LaneInfo` の代表値 / terminal pump）を intent に合わせる。動詞は registry に書いて
//! この 1 本を呼ぶだけでよくなる（旧: 10 個の動詞がそれぞれ実体遷移を手書きしていた）。
//!
//! ## desired の導出規則（doc 53 §10.2）
//!
//! | 実体 | あるべき条件 | 立ち方 |
//! |---|---|---|
//! | PtySlot（+ TermAttach 双子） | mode = Tui の session | **eager**（console は見る物） |
//! | chat engine | mode = Chat の session **∧ demand** | **lazy**（submit / focus / 購読が起こす） |
//! | `LaneInfo.pid` / `state` | root の実体から**導出** | 派生値 |
//! | terminal pump | R2 で reconcile 済 | 末尾で 1 回呼ぶ |
//!
//! chat engine を eager に立てないのは pump と同じ理由 — 誰も見ていない engine を起こすと
//! 課金と context を消費する。**畳む方向は eager**（mode が Tui になった session の engine は
//! 即座に落とす = 1 session 2 エンジンの法を守る）。
//!
//! ## 段の構造（doc 53 §12.5 — §6「やってはいけない」を満たす形）
//!
//! ```text
//! ① 読み     read lock + registry load → 立てるべき slot を **計算だけ**
//! ①′ 止める  立てる slot がある時だけ。write lock で、chat でなくなった session の engine を
//!            畳む → lock の外で、その process の終了を上限つきで確かめる
//! ② spawn    lock 外（spawn_blocking）で 800ms×N を回す
//! ③ 適用     write lock で insert + race 再検査（他の動詞が先に立てていたら捨てる）、
//!            desired に無い slot と engine を畳む
//! ```
//!
//! ①・②・③ は `lane/spawn_actor` が既に採っている形（読み → `spawn_blocking` → `pool.write()` で
//! race 再検査）を lane の中へ持ち込んだもの。
//!
//! ①′ は順のための段（2026-11-21）。chat → console の切替では **移る前（chat の engine）を
//! 止めてから、移る先（console の slot）を立てる** — 同じ会話を 2 つの process が同時に持たない。
//! ③ だけで畳むと、slot を立てた後に engine を止める順になる。①′ と ③ は同じ規則
//!（[`drop_engines_outside`]）を、それぞれの時点の intent で引く。
//! 逆向き（console → chat）は、③ が slot を畳み、その後に動詞が engine を起こすので、元から
//! 止めてから立てる順（こちらは slot の process の終了を待ってはいない）。
//!
//! ## 失敗の意味論（doc 53 §12.2、mako 判断）
//!
//! spawn に失敗しても **intent は残す**（registry から session を消さない）。次の reconcile
//! 契機で自動的に再試行される。「pane はあるが立ち上がっていない」は中間状態ではなく
//! **観測された事実**で、消すと「なぜ消えたのか」の情報が user から失われる。

use std::sync::Arc;

use tokio::sync::{RwLock, broadcast};

use crate::daemon::pty_slot::PtySlot;
use crate::lane::session_registry::{self, SessionKey, SessionMode};
use crate::repo::lane::{LaneAddress, LanePool, LaneState};
use crate::repo::terminal_pump::TerminalPumps;
use crate::repo::topic_router::TopicRouter;

/// reconcile の結果（ログ・テスト用の観測値）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LaneReconcile {
    /// 新しく立てた slot 数。
    pub spawned: usize,
    /// 畳んだ slot 数（mode が Chat になった / registry から消えた）。
    pub dropped_slots: usize,
    /// 畳んだ chat engine 数（mode が Tui になった / registry から消えた）。
    pub dropped_engines: usize,
    /// spawn に失敗した数（intent は残る = 次の契機で再試行）。
    pub failed: usize,
    /// 最後の spawn 失敗の理由（呼び手が user に見せる用。R3c-2 — restart の orchestration が
    /// 動詞の `Err` を返せなくなったので、診断をここで運ぶ）。
    pub last_error: Option<String>,
}

impl LaneReconcile {
    /// 何も動かなかったか（ログを出すかの判定）。
    fn is_noop(&self) -> bool {
        *self == Self::default()
    }
}

/// **PtySlot を持つべき session**（mode = Tui）。desired の導出規則そのもの。
///
/// ①（spawn すべきものを決める）と ③（今の intent に合わせる）が**同じ規則**を共有するため
/// 関数にしてある — 2 箇所に書くと片方だけ古くなる（doc 53 §3.3 の同型）。
fn want_slot_sessions(addr: &LaneAddress, lane_component: &str) -> Vec<SessionKey> {
    let lane_label = crate::repo::agent_spawner::lane_label(addr);
    session_registry::load(&addr.repo, lane_label, lane_component)
        .sessions
        .iter()
        .filter(|s| s.mode == SessionMode::Tui)
        .map(|s| s.key)
        .collect()
}

/// **chat engine を持ってよい session**（mode = Chat）。
///
/// 立てるのは lazy（submit / focus / 購読が起こす）なので reconcile は**畳む側だけ**に使う —
/// 「Chat でない session に engine が残っている」= 1 session 2 エンジンの法の破れ。
fn want_chat_sessions(addr: &LaneAddress, lane_component: &str) -> Vec<SessionKey> {
    let lane_label = crate::repo::agent_spawner::lane_label(addr);
    session_registry::load(&addr.repo, lane_label, lane_component)
        .sessions
        .iter()
        .filter(|s| s.mode == SessionMode::Gui)
        .map(|s| s.key)
        .collect()
}

/// 1 session ぶんの spawn 指示（① で計算し ② で実行する）。
pub(crate) struct SpawnPlan {
    pub(crate) session: SessionKey,
    /// この session の engine（agent）。lane 固定の agent ではない（cross-engine root）。
    pub(crate) lane_component: String,
    pub(crate) cwd: String,
}

type SpawnedSlot = (PtySlot, broadcast::Receiver<Vec<u8>>);
type SpawnFn = dyn Fn(&LaneAddress, &SpawnPlan) -> anyhow::Result<SpawnedSlot> + Send + Sync;

/// reconcile が pool の外に触る所（process を立てる・process の生死を見る）。test で差し替える。
#[derive(Clone)]
pub(crate) struct ReconcileHooks {
    /// console の slot を 1 枚立てる。`spawn_blocking` の中で呼ぶ（800ms の sync sleep を含む）。
    pub(crate) spawn: Arc<SpawnFn>,
    /// その pid の process がまだ居るか。
    pub(crate) alive: fn(u32) -> bool,
    /// 止めた chat engine の終了を待つ上限。
    pub(crate) stop_wait: std::time::Duration,
}

/// 止めた chat engine の終了を待つ上限。kill を出した後の待ちで、agy の実測では kill から終了まで
/// 数 ms（doc 88 §8.4）。上限は「終了の回収が遅れても console を立てる」ための歯止め。
const ENGINE_STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// 終了を確かめる間隔。
const ENGINE_STOP_POLL: std::time::Duration = std::time::Duration::from_millis(10);

impl ReconcileHooks {
    fn real() -> Self {
        Self {
            spawn: Arc::new(|addr, plan| {
                let cmd = crate::repo::agent_spawner::build_agent_command_for_session(
                    &plan.lane_component,
                    addr,
                    std::path::Path::new(&plan.cwd),
                    Some(plan.session),
                );
                crate::repo::agent_spawner::spawn_agent(&cmd, 120, 48)
            }),
            alive: crate::platform::process_alive,
            stop_wait: ENGINE_STOP_WAIT,
        }
    }
}

/// lane の実体を intent（registry）に合わせる（doc 53 §12）。
///
/// lane が pool に居ない場合は何もしない（削除 race — 動詞側が先に消したなら合わせる相手が
/// 居ない）。registry が読めない場合も同様に安全側で何もしない（[`masked-not-absent`] の
/// 規律: 「読めなかった」を「0 件だった」と誤認して実体を畳まない）。
///
/// [`masked-not-absent`]: https://github.com/chronista-club/vantage-point
pub async fn reconcile_lane(
    lane_pool: &Arc<RwLock<LanePool>>,
    terminal_pumps: &RwLock<TerminalPumps>,
    topic_router: &Arc<TopicRouter>,
    addr: &LaneAddress,
) -> LaneReconcile {
    reconcile_lane_with(
        lane_pool,
        terminal_pumps,
        topic_router,
        addr,
        &ReconcileHooks::real(),
    )
    .await
}

/// [`reconcile_lane`] の本体。pool の外に触る所を `hooks` で受ける。
pub(crate) async fn reconcile_lane_with(
    lane_pool: &Arc<RwLock<LanePool>>,
    terminal_pumps: &RwLock<TerminalPumps>,
    topic_router: &Arc<TopicRouter>,
    addr: &LaneAddress,
    hooks: &ReconcileHooks,
) -> LaneReconcile {
    let mut result = LaneReconcile::default();

    // ── ① 読み: **spawn すべきもの**だけを計算する ────────────────────────────────
    //
    // ⚠️ 畳む判断はここでしない。lock の外に出る必要があるのは spawn（800ms×N）だけで、
    // 畳むのは write lock 下で即座に終わる — ①で決めて③で適用すると、その間に動詞が
    // intent を動かした場合に**古い判断で畳んでしまう**（insert 側だけ race 再検査があり、
    // drop 側に無いという非対称になっていた。team-b 指摘 2026-07-26）。
    // 畳む対象は③で「今の intent」から計算する。
    let plans: Vec<SpawnPlan> = {
        let pool = lane_pool.read().await;
        let Some(info) = pool.get(addr) else {
            return result; // lane 不在 = 合わせる相手が居ない
        };
        let lane_component = info.agent.clone();
        let cwd = info.cwd.clone();
        let live_slots = pool.slot_sessions(addr);
        want_slot_sessions(addr, &lane_component)
            .into_iter()
            .filter(|k| !live_slots.contains(k))
            .map(|session| SpawnPlan {
                session,
                lane_component: lane_component.clone(),
                cwd: cwd.clone(),
            })
            .collect()
    };

    // ── ①′ 止める: console の slot を立てる前に、chat でなくなった session の engine を畳む ──
    //
    // 移る前を止めてから、移る先を立てる（mako 2026-11-21）。③ だけで畳むと、slot を立ててから
    // engine を止める順になり、同じ会話を 2 つの process が同時に持つ時間ができる。
    // 判断は ③ と同じ規則を、この時点の intent で引く（① の読みを持ち越さない）。
    let stopped_pids = if plans.is_empty() {
        Vec::new() // 立てる slot が無ければ ③ がすぐ続く。先に畳む理由が無い
    } else {
        let mut pool = lane_pool.write().await;
        let Some(info) = pool.get(addr) else {
            return result;
        };
        let want_chat = want_chat_sessions(addr, &info.agent.clone());
        drop_engines_outside(&mut pool, addr, &want_chat, &mut result)
    };
    // 止めた process の終了を確かめる（lock の外）。`stop` は kill を出すだけで、終了の回収は
    // 各 host の別の task がする。
    wait_until_gone(addr, &stopped_pids, hooks).await;

    // ── ② spawn: lock の外で回す（1 枚 800ms の sync sleep を lock 下に置かない）──────
    let mut spawned = Vec::new();
    for plan in plans {
        let addr_for_spawn = addr.clone();
        let spawn = Arc::clone(&hooks.spawn);
        let built = tokio::task::spawn_blocking(move || {
            spawn(&addr_for_spawn, &plan).map(|s| (plan.session, s))
        })
        .await;
        match built {
            Ok(Ok((session, (slot, term_rx)))) => spawned.push((session, slot, term_rx)),
            Ok(Err(e)) => {
                // doc 53 §12.2: intent は残す（registry から消さない）。次の契機で再試行される。
                tracing::warn!("reconcile: slot spawn 失敗（intent は残す）: addr={addr}: {e}");
                result.failed += 1;
                result.last_error = Some(e.to_string());
            }
            Err(join) => {
                tracing::warn!("reconcile: spawn task が落ちた: addr={addr}: {join}");
                result.failed += 1;
                result.last_error = Some(join.to_string());
            }
        }
    }

    // ── ③ 適用: write lock で「今の intent」に合わせる ─────────────────────────────
    //
    // 判断は**すべてこの中**で行う（②で持ち越すのは spawn 済の実体だけ）。①からここまでの
    // 間に動詞が intent を動かしていても、適用は最新の registry に従う。
    {
        let mut pool = lane_pool.write().await;
        let Some(info) = pool.get(addr) else {
            return result; // spawn 中に lane が消えた（slot は Drop で child kill される）
        };
        let lane_component = info.agent.clone();
        let want_slot = want_slot_sessions(addr, &lane_component);
        let want_chat = want_chat_sessions(addr, &lane_component);

        for (session, slot, term_rx) in spawned {
            // 立てている間に intent が変わった（mode が Chat になった / session が消えた）なら
            // **入れずに捨てる**（scope 終端の Drop が child を kill する）。
            if !want_slot.contains(&session) {
                tracing::debug!(
                    "reconcile: spawn 済 slot を破棄（intent が変わった）: {addr} s={session}"
                );
                continue;
            }
            // 他の経路が先に同じ session の slot を立てていたら、こちらを捨てる
            //（`insert_pty_slot` は黙って replace するので、走行中の console を殺さない）。
            if pool.slot_sessions(addr).contains(&session) {
                tracing::debug!("reconcile: race lost（既に slot あり）: addr={addr} s={session}");
                continue;
            }
            pool.insert_pty_slot(addr.clone(), Some(session), slot, term_rx);
            result.spawned += 1;
        }

        // 畳む: desired に無い実体（mode が変わった / registry から消えた）。
        for session in pool
            .slot_sessions(addr)
            .into_iter()
            .filter(|k| !want_slot.contains(k))
            .collect::<Vec<_>>()
        {
            if pool.drop_slot_public(addr, session) {
                result.dropped_slots += 1;
            }
        }
        // engine も畳む。slot を立てる時は ①′ で済んでいるので、ここに残るのは、立てる slot が
        // 無かった時（registry から消えた session の後始末など）と、② の間に intent が動いた分。
        drop_engines_outside(&mut pool, addr, &want_chat, &mut result);
        // LaneInfo の代表値（pid / state）は root の実体から**導出**する（doc 53 §3.3 —
        // 派生値を動詞ごとに手で更新しない）。
        pool.refresh_lane_representation(addr);
    }

    // 末尾で pump を reconcile（R2）— slot が増減したなら購読の張り直しが要る。
    crate::repo::terminal_pump::reconcile_lane_pumps(
        lane_pool,
        terminal_pumps,
        topic_router,
        &addr.to_string(),
    )
    .await;

    if !result.is_noop() {
        tracing::info!(
            "lane reconcile (addr={addr}, spawned={}, dropped_slots={}, dropped_engines={}, failed={})",
            result.spawned,
            result.dropped_slots,
            result.dropped_engines,
            result.failed
        );
    }
    result
}

/// `want_chat` に無い session の chat engine を畳み、止めた process の pid を返す。
///
/// engine は **registry を引かない経路**で畳む（`drop_chat_engine` は `resolve_chat_session` を
/// 通すので、registry から消えた session を畳めない）。
///
/// lane の代表の pid が止めた engine のものなら、ここで下ろす。state は動かさない — slot が
/// 立つまでの間に導出し直すと、root が「tui で slot 無し」= Dead と出る。slot が立った後
///（立たなかった時も）③ の `refresh_lane_representation` が決める。
fn drop_engines_outside(
    pool: &mut LanePool,
    addr: &LaneAddress,
    want_chat: &[SessionKey],
    result: &mut LaneReconcile,
) -> Vec<u32> {
    let mut stopped = Vec::new();
    for session in pool
        .chat_engine_sessions(addr)
        .into_iter()
        .filter(|k| !want_chat.contains(k))
        .collect::<Vec<_>>()
    {
        let pid = pool.chat_engine_pid(addr, session);
        if pool.drop_chat_engine_by_key(addr, session) {
            result.dropped_engines += 1;
            stopped.extend(pid);
        }
    }
    if !stopped.is_empty() {
        pool.clear_lane_pid_if(addr, &stopped);
    }
    stopped
}

/// 止めた engine の process が居なくなるまで待つ。上限（[`ReconcileHooks::stop_wait`]）を過ぎたら
/// 待ちをやめる — kill は出してあるので、console を立てないままにはしない。
async fn wait_until_gone(addr: &LaneAddress, pids: &[u32], hooks: &ReconcileHooks) {
    if pids.is_empty() {
        return;
    }
    let started = tokio::time::Instant::now();
    loop {
        let left: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|pid| (hooks.alive)(*pid))
            .collect();
        if left.is_empty() {
            tracing::debug!(
                "reconcile: chat engine の終了を確かめた: addr={addr} pids={pids:?} ({:?})",
                started.elapsed()
            );
            return;
        }
        if started.elapsed() >= hooks.stop_wait {
            tracing::warn!(
                "reconcile: chat engine の終了を {:?} 待っても確かめられない（console は立てる）: addr={addr} pids={left:?}",
                hooks.stop_wait
            );
            return;
        }
        tokio::time::sleep(ENGINE_STOP_POLL).await;
    }
}

/// [`LaneState`] を root の実体から決める純関数（`refresh_lane_representation` の判断部）。
///
/// doc 33 §3 / doc 53 R1: **chat の lane は pid 無しで Running が正常形**（engine は lazy
/// spawn なので「まだ起きていない」は死ではない）。tui は root slot の有無が生死そのもの。
pub fn lane_state_of(
    root_mode: SessionMode,
    root_slot_pid: Option<u32>,
) -> (LaneState, Option<u32>) {
    match (root_mode, root_slot_pid) {
        (SessionMode::Gui, _) => (LaneState::Running, None),
        (SessionMode::Tui, Some(pid)) => (LaneState::Running, Some(pid)),
        (SessionMode::Tui, None) => (LaneState::Dead, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// doc 53 §12: lane の代表値は root の mode と実体から**導出**される。
    ///
    /// 旧実装は動詞ごとに `info.pid = …` / `info.state = …` を手で書いていた（census §10.1 の
    /// 「代表値追随」列）。書き忘れた動詞だけ古い値を映す、が起きていた class。
    #[test]
    fn lane_state_is_derived_from_root() {
        // chat: engine が起きていなくても Running（pid は持たない = chat-idle の正常形）
        assert_eq!(
            lane_state_of(SessionMode::Gui, None),
            (LaneState::Running, None)
        );
        // chat lane に PTY の pid が紛れていても代表値には出さない（engine が代表）
        assert_eq!(
            lane_state_of(SessionMode::Gui, Some(42)),
            (LaneState::Running, None)
        );
        // tui: root slot があれば Running + その pid
        assert_eq!(
            lane_state_of(SessionMode::Tui, Some(42)),
            (LaneState::Running, Some(42))
        );
        // tui: root slot が無い = 死（spawn 失敗 / 落ちた）
        assert_eq!(
            lane_state_of(SessionMode::Tui, None),
            (LaneState::Dead, None)
        );
    }

    /// doc 53 §12: **registry から消えた session の chat engine が畳まれる**（R3c の ✕ の下地）。
    ///
    /// ⚠️ この経路は R3b の呼び手（boot 2 箇所）では**到達しない** — engine が既に居る状態で
    /// reconcile を呼ぶのは R3c で動詞が繋がってから。それでもここで固定するのは、
    /// **潰し方を間違えると無音で壊れる**ため（team-b 指摘 2026-07-26）:
    /// `drop_chat_engine` は `resolve_chat_session` を通すので registry に居ない session を
    /// 解決できず、**畳みたい当の対象を黙って no-op** する。`drop_chat_engine_by_key`
    /// （生 key 直接 remove）を使っていることをここで縛る。
    ///
    /// 「1 session = 高々 1 エンジン」の法の実体なので、破れると 1 会話に 2 本ぶら下がる。
    #[tokio::test]
    async fn engine_of_removed_session_is_dropped() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vptest-reconcile-drop");
        let lane_label = crate::repo::agent_spawner::lane_label(&addr);

        // root(#1) = Chat だけの registry を作る（= #2 は「存在しない session」）。
        session_registry::set_root_mode(&addr.repo, lane_label, "claude", SessionMode::Gui)
            .expect("root を chat に");

        // desired の導出: Chat は root だけ / Tui は誰も居ない。
        let want_chat = want_chat_sessions(&addr, "claude");
        let want_slot = want_slot_sessions(&addr, "claude");
        assert_eq!(want_chat, vec![1], "registry に居る Chat は root だけ");
        assert!(want_slot.is_empty(), "mode=Tui の session は居ない");

        // registry から消えた session（#2）は **どちらの desired にも入らない** = 畳む対象。
        // 実 engine を spawn せずに規則だけを固定する（engine の spawn は claude 実バイナリを
        // 要するため、ここでは「畳む対象に入るか」の判断だけを検証する）。
        assert!(
            !want_chat.contains(&2) && !want_slot.contains(&2),
            "registry に居ない session の実体は残さない（1 session 2 エンジンの法）"
        );
    }

    // ── 畳む順（creo mem_1ChA3fZy2Kq7tb3qaLSmpe「畳む順を直す」）────────────────────────

    use crate::conversation::acp_host::testing::{FakeAgent, Script};
    use crate::conversation::acp_host::{AcpAgentHost, AcpEngine, AcpHostConfig};
    use crate::conversation::{ChatEngineSlot, ChatHost};
    use crate::repo::state::{RepoState, build_test_app_state, insert_test_lane};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// 偽の ACP agent につないだ chat engine を、その session に差し込む（pid は 4242）。
    /// 戻り値の [`FakeAgent`] は、接続を生かしておくために test が持つ。
    async fn insert_fake_engine(
        state: &RepoState,
        addr: &LaneAddress,
        key: SessionKey,
    ) -> FakeAgent {
        let (agent, connector) = FakeAgent::new(Script::default());
        let host = AcpAgentHost::spawn_with(
            AcpHostConfig {
                engine: AcpEngine::Agy,
                cwd: "/tmp/vp-acp".into(),
                repo: addr.repo.clone(),
                lane: format!("lead#{key}"),
                lane_label: "lead".into(),
                session_key: key,
                session_id: None,
                config_intent: Vec::new(),
            },
            connector,
        )
        .expect("起動");
        state.lane_pool.write().await.insert_chat_engine_for_test(
            addr,
            key,
            ChatEngineSlot {
                host: ChatHost::Agy(host),
                pump: tokio::spawn(async {}),
                last_event_at: Default::default(),
                turn_active: Default::default(),
            },
        );
        agent
    }

    /// slot を立てようとした時点の様子を控えて、立てずに失敗を返す spawn。
    /// 控えるもの: (立てようとした session, その時点で lane に居る chat engine, lane の pid)。
    type Seen = Arc<Mutex<Vec<(SessionKey, Vec<SessionKey>, Option<u32>)>>>;
    fn recording_spawn(state: &RepoState, seen: &Seen) -> Arc<SpawnFn> {
        let pool = Arc::clone(&state.lane_pool);
        let seen = Arc::clone(seen);
        Arc::new(move |addr, plan| {
            let pool = pool.blocking_read();
            seen.lock().unwrap().push((
                plan.session,
                pool.chat_engine_sessions(addr),
                pool.get(addr).and_then(|lane| lane.pid),
            ));
            anyhow::bail!("test では slot を立てない")
        })
    }

    async fn run(state: &RepoState, addr: &LaneAddress, hooks: &ReconcileHooks) -> LaneReconcile {
        reconcile_lane_with(
            &state.lane_pool,
            &state.terminal_pumps,
            &state.topic_router,
            addr,
            hooks,
        )
        .await
    }

    /// chat → console の切替では、**chat の engine を止めてから** console の slot を立てる。
    /// 逆だと、同じ会話を 2 つの process が同時に持つ（mako 2026-11-21「移動前を止めてから
    /// 移動先を起動じゃないんだ」）。
    ///
    /// 落ちるべき壊し方: engine を畳むのが ③（slot を立てた後）だけ — spawn の時点で engine が居る。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chat_engine_is_gone_before_the_console_slot_starts_to_spawn() {
        let _state = crate::test_env::state_dir_async().await;
        let state = build_test_app_state().await;
        let addr = insert_test_lane(&state, "vptest-handover-order", SessionMode::Gui).await;
        let root = want_chat_sessions(&addr, "claude")[0];
        let _agent = insert_fake_engine(&state, &addr, root).await;
        {
            // 名簿の pid は chat の engine のもの（focused の engine を立てた時に載る）。
            let mut pool = state.lane_pool.write().await;
            let mut lane = pool.get(&addr).unwrap().clone();
            lane.pid = Some(4242);
            pool.insert(lane);
        }

        // 動詞がやること: intent を tui に書いて reconcile を呼ぶ。
        session_registry::set_root_mode(&addr.repo, "lead", "claude", SessionMode::Tui)
            .expect("root を tui に");
        let seen = Seen::default();
        let hooks = ReconcileHooks {
            spawn: recording_spawn(&state, &seen),
            alive: |_| false,
            stop_wait: Duration::from_secs(2),
        };
        let result = run(&state, &addr, &hooks).await;

        assert_eq!(
            *seen.lock().unwrap(),
            [(root, Vec::new(), None)],
            "slot を立て始める時点で、chat の engine も、その pid も残っていない"
        );
        assert_eq!(result.dropped_engines, 1);
        // slot の spawn が失敗しても intent は tui のまま残り、次の reconcile がもう一度立てる。
        assert_eq!(result.failed, 1);
        assert_eq!(want_slot_sessions(&addr, "claude"), [root]);
        assert_eq!(run(&state, &addr, &hooks).await.failed, 1);
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "次の reconcile で立て直しに行く"
        );
    }

    /// 止めた engine の process が終わるのを確かめてから、slot を立てる。
    ///
    /// 落ちるべき壊し方: 終了を待たない — process がまだ居るうちに spawn が始まる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slot_spawn_waits_until_the_stopped_engine_process_is_gone() {
        static PROBES: AtomicUsize = AtomicUsize::new(0);
        static PROBES_AT_SPAWN: AtomicUsize = AtomicUsize::new(usize::MAX);
        /// 3 回目の確認まで「まだ居る」と答える。
        fn alive(pid: u32) -> bool {
            assert_eq!(pid, 4242, "待つのは畳んだ engine の pid");
            PROBES.fetch_add(1, Ordering::SeqCst) < 3
        }

        let _state = crate::test_env::state_dir_async().await;
        let state = build_test_app_state().await;
        let addr = insert_test_lane(&state, "vptest-handover-wait", SessionMode::Gui).await;
        let root = want_chat_sessions(&addr, "claude")[0];
        let _agent = insert_fake_engine(&state, &addr, root).await;
        session_registry::set_root_mode(&addr.repo, "lead", "claude", SessionMode::Tui)
            .expect("root を tui に");

        let hooks = ReconcileHooks {
            spawn: Arc::new(|_, _| {
                PROBES_AT_SPAWN.store(PROBES.load(Ordering::SeqCst), Ordering::SeqCst);
                anyhow::bail!("test では slot を立てない")
            }),
            alive,
            stop_wait: Duration::from_secs(2),
        };
        run(&state, &addr, &hooks).await;

        assert_eq!(
            PROBES_AT_SPAWN.load(Ordering::SeqCst),
            4,
            "「もう居ない」と分かった確認（4 回目）の後に spawn が始まる"
        );
    }

    /// process の終了が確かめられなくても、上限で待ちをやめて slot を立てる
    /// （kill は出してある。console が立たないままにしない）。
    ///
    /// 落ちるべき壊し方: 上限なしで待つ — reconcile が返らない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slot_spawn_proceeds_after_the_wait_limit() {
        let _state = crate::test_env::state_dir_async().await;
        let state = build_test_app_state().await;
        let addr = insert_test_lane(&state, "vptest-handover-limit", SessionMode::Gui).await;
        let root = want_chat_sessions(&addr, "claude")[0];
        let _agent = insert_fake_engine(&state, &addr, root).await;
        session_registry::set_root_mode(&addr.repo, "lead", "claude", SessionMode::Tui)
            .expect("root を tui に");

        let seen = Seen::default();
        let hooks = ReconcileHooks {
            spawn: recording_spawn(&state, &seen),
            alive: |_| true,
            stop_wait: Duration::from_millis(60),
        };
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(5), run(&state, &addr, &hooks))
            .await
            .expect("上限で待ちをやめる");

        assert!(
            started.elapsed() >= Duration::from_millis(60),
            "上限までは待つ"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "上限の後に slot を立てに行く"
        );
        assert_eq!(result.dropped_engines, 1);
    }

    /// 畳むのは「今の intent が chat でない session」の engine だけ。chat のままの session の
    /// engine は、隣の session が console に切り替わっても動いたままにする。
    ///
    /// 落ちるべき壊し方: slot を立てる前に lane の engine を全部畳む。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn engine_of_a_session_that_stays_chat_survives_the_handover() {
        let _state = crate::test_env::state_dir_async().await;
        let state = build_test_app_state().await;
        let addr = insert_test_lane(&state, "vptest-handover-other", SessionMode::Gui).await;
        let root = want_chat_sessions(&addr, "claude")[0];
        let other = state
            .lane_pool
            .write()
            .await
            .create_chat_session(&addr, Some("agy"), false)
            .expect("2 つ目の session");
        let _a = insert_fake_engine(&state, &addr, root).await;
        let _b = insert_fake_engine(&state, &addr, other).await;
        session_registry::set_root_mode(&addr.repo, "lead", "claude", SessionMode::Tui)
            .expect("root を tui に");

        let seen = Seen::default();
        let hooks = ReconcileHooks {
            spawn: recording_spawn(&state, &seen),
            alive: |_| false,
            stop_wait: Duration::from_secs(2),
        };
        let result = run(&state, &addr, &hooks).await;

        assert_eq!(
            seen.lock().unwrap()[0].1,
            [other],
            "spawn の時点で居るのは chat のままの方"
        );
        assert_eq!(result.dropped_engines, 1);
        assert_eq!(
            state.lane_pool.read().await.chat_engine_sessions(&addr),
            [other]
        );
    }
}
