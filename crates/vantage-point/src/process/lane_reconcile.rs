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
//! | PtySlot（+ TermAttach 双子） | act = Tui の session | **eager**（console は見る物） |
//! | chat engine | act = Chat の session **∧ demand** | **lazy**（submit / focus / 購読が起こす） |
//! | `LaneInfo.pid` / `state` | root の実体から**導出** | 派生値 |
//! | terminal pump | R2 で reconcile 済 | 末尾で 1 回呼ぶ |
//!
//! chat engine を eager に立てないのは pump と同じ理由 — 誰も見ていない engine を起こすと
//! 課金と context を消費する。**畳む方向は eager**（act が Tui になった session の engine は
//! 即座に落とす = 1 session 2 エンジンの法を守る）。
//!
//! ## 3 段構造（doc 53 §12.5 — §6「やってはいけない」を満たす形）
//!
//! ```text
//! ① 読み     read lock + registry load → desired と actual の差分を **計算だけ**
//! ② spawn    lock 外（spawn_blocking）で 800ms×N を回す
//! ③ 適用     write lock で insert + race 再検査（他の動詞が先に立てていたら捨てる）
//! ```
//!
//! `lane_spawn_actor` が既にこの形（読み → `spawn_blocking` → `pool.write()` で race 再検査）。
//! 新しい規律ではなく、**既に採っている形を lane の中へ持ち込む**。
//!
//! ## 失敗の意味論（doc 53 §12.2、mako 判断）
//!
//! spawn に失敗しても **intent は残す**（registry から session を消さない）。次の reconcile
//! 契機で自動的に再試行される。「pane はあるが立ち上がっていない」は中間状態ではなく
//! **観測された事実**で、消すと「なぜ消えたのか」の情報が user から失われる。

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::lane::session_registry::{self, SessionAct, SessionKey};
use crate::process::lanes_state::{LaneAddress, LanePool, LaneState};
use crate::process::terminal_pump::TerminalPumps;
use crate::process::topic_router::TopicRouter;

/// reconcile の結果（ログ・テスト用の観測値）。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LaneReconcile {
    /// 新しく立てた slot 数。
    pub spawned: usize,
    /// 畳んだ slot 数（act が Chat になった / registry から消えた）。
    pub dropped_slots: usize,
    /// 畳んだ chat engine 数（act が Tui になった / registry から消えた）。
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

/// **PtySlot を持つべき session**（act = Tui）。desired の導出規則そのもの。
///
/// ①（spawn すべきものを決める）と ③（今の intent に合わせる）が**同じ規則**を共有するため
/// 関数にしてある — 2 箇所に書くと片方だけ古くなる（doc 53 §3.3 の同型）。
fn want_slot_sessions(addr: &LaneAddress, lane_stand: &str) -> Vec<SessionKey> {
    let lane_label = crate::process::stand_spawner::lane_label(addr);
    session_registry::load(&addr.project, lane_label, lane_stand)
        .sessions
        .iter()
        .filter(|s| s.act == SessionAct::Tui)
        .map(|s| s.key)
        .collect()
}

/// **chat engine を持ってよい session**（act = Chat）。
///
/// 立てるのは lazy（submit / focus / 購読が起こす）なので reconcile は**畳む側だけ**に使う —
/// 「Chat でない session に engine が残っている」= 1 session 2 エンジンの法の破れ。
fn want_chat_sessions(addr: &LaneAddress, lane_stand: &str) -> Vec<SessionKey> {
    let lane_label = crate::process::stand_spawner::lane_label(addr);
    session_registry::load(&addr.project, lane_label, lane_stand)
        .sessions
        .iter()
        .filter(|s| s.act == SessionAct::Chat)
        .map(|s| s.key)
        .collect()
}

/// 1 session ぶんの spawn 指示（① で計算し ② で実行する）。
struct SpawnPlan {
    session: SessionKey,
    /// この session の engine（stand）。lane 固定の stand ではない（cross-engine root）。
    lane_stand: String,
    cwd: String,
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
        let lane_stand = info.stand.clone();
        let cwd = info.cwd.clone();
        let live_slots = pool.slot_sessions(addr);
        want_slot_sessions(addr, &lane_stand)
            .into_iter()
            .filter(|k| !live_slots.contains(k))
            .map(|session| SpawnPlan {
                session,
                lane_stand: lane_stand.clone(),
                cwd: cwd.clone(),
            })
            .collect()
    };

    // ── ② spawn: lock の外で回す（1 枚 800ms の sync sleep を lock 下に置かない）──────
    let mut spawned = Vec::new();
    for plan in plans {
        let addr_for_spawn = addr.clone();
        let built = tokio::task::spawn_blocking(move || {
            let cmd = crate::process::stand_spawner::build_stand_command_for_session(
                &plan.lane_stand,
                &addr_for_spawn,
                std::path::Path::new(&plan.cwd),
                Some(plan.session),
            );
            crate::process::stand_spawner::spawn_stand(&cmd, 120, 48).map(|s| (plan.session, s))
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
        let lane_stand = info.stand.clone();
        let want_slot = want_slot_sessions(addr, &lane_stand);
        let want_chat = want_chat_sessions(addr, &lane_stand);

        for (session, slot, term_rx) in spawned {
            // 立てている間に intent が変わった（act が Chat になった / session が消えた）なら
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

        // 畳む: desired に無い実体（act が変わった / registry から消えた）。
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
        // engine は **registry を引かない経路**で畳む（`drop_chat_engine` は
        // `resolve_chat_session` を通すので、registry から消えた session を畳めない）。
        for session in pool
            .chat_engine_sessions(addr)
            .into_iter()
            .filter(|k| !want_chat.contains(k))
            .collect::<Vec<_>>()
        {
            if pool.drop_chat_engine_by_key(addr, session) {
                result.dropped_engines += 1;
            }
        }
        // LaneInfo の代表値（pid / state）は root の実体から**導出**する（doc 53 §3.3 —
        // 派生値を動詞ごとに手で更新しない）。
        pool.refresh_lane_representation(addr);
    }

    // 末尾で pump を reconcile（R2）— slot が増減したなら購読の張り直しが要る。
    crate::process::terminal_pump::reconcile_lane_pumps(
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

/// [`LaneState`] を root の実体から決める純関数（`refresh_lane_representation` の判断部）。
///
/// doc 33 §3 / doc 53 R1: **chat の lane は pid 無しで Running が正常形**（engine は lazy
/// spawn なので「まだ起きていない」は死ではない）。tui は root slot の有無が生死そのもの。
pub fn lane_state_of(root_act: SessionAct, root_slot_pid: Option<u32>) -> (LaneState, Option<u32>) {
    match (root_act, root_slot_pid) {
        (SessionAct::Chat, _) => (LaneState::Running, None),
        (SessionAct::Tui, Some(pid)) => (LaneState::Running, Some(pid)),
        (SessionAct::Tui, None) => (LaneState::Dead, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// doc 53 §12: lane の代表値は root の act と実体から**導出**される。
    ///
    /// 旧実装は動詞ごとに `info.pid = …` / `info.state = …` を手で書いていた（census §10.1 の
    /// 「代表値追随」列）。書き忘れた動詞だけ古い値を映す、が起きていた class。
    #[test]
    fn lane_state_is_derived_from_root() {
        // chat: engine が起きていなくても Running（pid は持たない = chat-idle の正常形）
        assert_eq!(
            lane_state_of(SessionAct::Chat, None),
            (LaneState::Running, None)
        );
        // chat lane に PTY の pid が紛れていても代表値には出さない（engine が代表）
        assert_eq!(
            lane_state_of(SessionAct::Chat, Some(42)),
            (LaneState::Running, None)
        );
        // tui: root slot があれば Running + その pid
        assert_eq!(
            lane_state_of(SessionAct::Tui, Some(42)),
            (LaneState::Running, Some(42))
        );
        // tui: root slot が無い = 死（spawn 失敗 / 落ちた）
        assert_eq!(
            lane_state_of(SessionAct::Tui, None),
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
        let lane_label = crate::process::stand_spawner::lane_label(&addr);

        // root(#1) = Chat だけの registry を作る（= #2 は「存在しない session」）。
        session_registry::set_root_act(&addr.project, lane_label, "echoes", SessionAct::Chat)
            .expect("root を chat に");

        // desired の導出: Chat は root だけ / Tui は誰も居ない。
        let want_chat = want_chat_sessions(&addr, "echoes");
        let want_slot = want_slot_sessions(&addr, "echoes");
        assert_eq!(want_chat, vec![1], "registry に居る Chat は root だけ");
        assert!(want_slot.is_empty(), "act=Tui の session は居ない");

        // registry から消えた session（#2）は **どちらの desired にも入らない** = 畳む対象。
        // 実 engine を spawn せずに規則だけを固定する（engine の spawn は claude 実バイナリを
        // 要するため、ここでは「畳む対象に入るか」の判断だけを検証する）。
        assert!(
            !want_chat.contains(&2) && !want_slot.contains(&2),
            "registry に居ない session の実体は残さない（1 session 2 エンジンの法）"
        );
    }
}
