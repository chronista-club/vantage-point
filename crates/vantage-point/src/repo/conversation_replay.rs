//! conversation replay — attach 時に会話を配り直す合流ロジック（doc 32 §3 / doc 40 §4、doc 61）。
//!
//! `conversation_demand_start` で transcript（commit 済）と engine host の in-flight tail を継いで
//! `ReplayStart → SessionInit → events → ReplayEnd` を per-lane topic に流す。replay 中に来た demand は
//! 合流（coalesce）して flight 完了側が直列に消化する（`RepoState::replay_flights`）。`conversation_demand_stop`
//! は購読が 0 になった時に engine を寝かせる hook。`route_conversation` は replay だけが使う配送口。
//!
//! 不変条件（verbatim に保つ）: `SessionInit` は `ReplayStart` の直後（`splice_session_init`）/
//! `replay_with_in_flight` は commit 世代 `seq` を読み前後で検算する / flight 中に来た demand は自分では配送せず rerun を予約する。
//! 受付は `unison_server::dispatch_repo_method`。

use super::state::RepoState;

/// gui replay-on-attach: conversation demand start ハンドラー。
///
/// daemon の demand hook が `repo/conversation/data/{lane}/event` の購読者 0→1 を検知し、 control
/// reverse-route で本 method を撃つ。 repo は当該 chat lane の **transcript を replay** して topic に
/// route する（`ReplayStart` + 過去会話の ConversationEvent 列）。
///
/// なぜ必要か: conversation topic は非 retained で、 会話履歴は vp-app の in-memory ring buffer に
/// しか無い。 app 再起動で ChatView が空になる（engine 側は `--resume` で会話を保持しているのに
/// 描く履歴が無い）。 唯一の履歴 SSOT である claude の transcript(jsonl) から起こし直す。
///
/// 冪等: 先頭の `ReplayStart` を見て GUI が会話表示をクリアするため、 reconnect / demand 再発火で
/// 二重化しない（terminal replay の clear-prefix と同型）。
///
/// **生成中に着地した場合**: claude は message を完了時にしか transcript へ flush しないので、
/// transcript だけでは生成中 message が欠ける（GUI は reset 済みなので、 復帰後の chunk が文の
/// 途中から新しいバブルを立ててしまう）。 そこで engine host の **in-flight tail** を transcript の
/// 後ろに継ぐ（`replay = transcript(commit 済み) ++ tail(未 commit)`、 `conversation::host` module doc）。
///
/// transcript を読んでいる最中に commit が挟まると tail と transcript が食い違う（欠落 or 二重化）。
/// commit 世代 `seq` を読み前後で検算し、 動いていたら読み直す。 収束しなければ tail を捨てて
/// commit 済み状態に収束させる（= 従来動作にフォールバック、 二重化より安全）。
///
/// chat mode でない lane / cc_session id 不明 / transcript 不在は「replay 無し」で graceful に返す
/// （console は live event を待つだけで壊れない）。
pub(crate) async fn handle_conversation_demand_start(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("conversation_demand_start: lane 未指定".to_string());
    }
    let session = super::unison_server::payload_session_key("conversation_demand_start", &payload)?;
    let Some(addr) = crate::repo::lane::parse_address(&lane) else {
        return Err(format!(
            "conversation_demand_start: lane パース失敗: {lane}"
        ));
    };

    // chat でない session は replay しない（tui の履歴は PtySlot の terminal replay が担う）。
    //
    // ⚠️ gate は **その session の mode**（doc 50 §4.6 A6）。旧実装は lane 単位
    // `console_mode`（= root cache）で弾いていたため、**root=tui のまま非 root だけ chat** という
    // A6 の正規構成で `not_chat` を返し、ReplayStart すら送らずに会話が復元されなかった
    // （team-b review 2026-07-25、score 92。§4.7「共通形は lane 単位で判断している箇所」の同型）。
    // lane 不在は graceful に not_chat（従来挙動の温存 — Err にすると boot 窓で騒がしい）。
    // doc 53 §8.4: 旧実装は `console_mode(addr).is_none()` を「lane 実在」の signal に流用
    // していた（値は不使用）。存在 check を明示形にする。
    let resolved = {
        let pool = state.lane_pool.read().await;
        if !pool.contains(&addr) {
            return Ok(serde_json::json!({"status": "not_chat", "lane": lane}));
        }
        let resolved = pool
            .resolve_chat_session(&addr, session)
            .map_err(|e| format!("conversation_demand_start: {e}"))?;
        if resolved.mode != crate::lane::session_registry::SessionMode::Gui {
            return Ok(serde_json::json!({
                "status": "not_chat", "lane": lane, "session": resolved.key
            }));
        }
        resolved
    };

    // doc 38 Phase 3（focused eager）: attach = 会話を見に来た合図。当該 session の engine を
    // ここで eager に resume spawn する（doc 33 C1 の lazy「submit まで engine-less」からの転換 —
    // repo 再起動後も uplink 再接続 → demand 再発火でこの経路に入るため「前回状態キープ」が成立）。
    // ensure は冪等（既起動なら no-op）。失敗しても replay は続行し、engine は次 submit の
    // self-heal で再試行される。shell / legacy agent 等 gui host を持たない session は skip
    //（能力表 = EngineKind が SSOT。bail を warn で騒がせない）。
    if crate::conversation::EngineKind::from_agent(&resolved.agent)
        .is_some_and(crate::conversation::EngineKind::chat_capable)
        && let Err(e) =
            state
                .lane_pool
                .write()
                .await
                .ensure_chat_engine(&addr, session, &state.topic_router)
    {
        tracing::warn!(
            "conversation_demand_start: eager engine spawn 失敗（submit で再試行）: {e}"
        );
    }

    // single-flight 化（2026-07-27）: 起動時の 3 重 demand（daemon の購読 0→1 hook /
    // vp-app の subscribe 直後 / webview showLane）が並行に replay を route すると、
    // 「二重 replay は ReplayStart の clear-prefix で収束（無害）」の前提が破れて event が
    // 混線する（詳細は [`crate::repo::state::ReplayFlights`] の doc — 2026-07-27 fleetstage で
    // 実測）。進行中なら合流して rerun 予約のみ残す。ensure_chat_engine は guard の**外**
    //（demand ごとに冪等実行 = engine 復活の即時性は従来どおり）。
    if !state.replay_flights.begin(&lane, resolved.key) {
        tracing::info!(
            "conversation replay coalesced: 進行中 replay に合流 (lane={lane}, session={})",
            resolved.key
        );
        return Ok(serde_json::json!({
            "status": "coalesced", "lane": lane, "session": resolved.key
        }));
    }
    loop {
        let result = replay_once(state, &addr, &lane, &resolved).await;
        if result.is_err() {
            // エラー中断は予約ごと破棄（次の demand が新規 flight で素直に走れるように）。
            state.replay_flights.abort(&lane, resolved.key);
            return result;
        }
        if !state.replay_flights.finish(&lane, resolved.key) {
            return result;
        }
        // 合流分の rerun: 直列にもう 1 周だけ全量を配送し直す（連続配送なら clear-prefix で
        // 収束する = 元の設計前提に戻る）。進行中 replay の途中から購読した consumer の
        // prefix 取りこぼしもこの 1 周で回復する。`resolved` は初回 resolve の snapshot
        //（rerun 窓は ms 単位なので再 resolve しない）。
        tracing::info!(
            "conversation replay rerun: 合流した demand を直列に消化 (lane={lane}, session={})",
            resolved.key
        );
    }
}

/// replay 1 本分の配送（[`handle_conversation_demand_start`] の single-flight loop の中身）。
///
/// Codex は host の一括 snapshot、Claude は transcript、他 engine は replay_log を配送する。
async fn replay_once(
    state: &RepoState,
    addr: &crate::repo::lane::LaneAddress,
    lane: &str,
    resolved: &crate::repo::lane::ResolvedSession,
) -> Result<serde_json::Value, String> {
    if crate::conversation::EngineKind::from_agent(&resolved.agent)
        == Some(crate::conversation::EngineKind::Codex)
    {
        let result = state
            .lane_pool
            .read()
            .await
            .request_codex_history(addr, resolved.key);
        if let Err(error) = result {
            let message = format!("Codex の履歴を復元できません: {error}");
            route_conversation(
                state,
                lane,
                resolved.key,
                vec![crate::conversation::ConversationEvent::Error {
                    message: message.clone(),
                }],
            )
            .await;
            return Err(message);
        }
        return Ok(serde_json::json!({"status":"ok", "lane":lane, "session":resolved.key}));
    }
    let lane_label = crate::repo::agent_spawner::lane_label(addr).to_string();
    let label = crate::lane::session_registry::session_label(&lane_label, resolved.key);
    // transcript replay は claude 専用（jsonl の SSOT を持つのは claude のみ）。会話 id は
    // registry が SSOT（doc 40 §5 reader #6 — resolve 時の registry load から持ち回った
    // `resolved.conversation`。旧 cc_session store 直読みは PR-2 で退役）。grok /
    // opencode session は claude transcript を持たないため None に倒し、必ず下の no_session
    // path（replay_log）を通す。
    let session_id = match crate::conversation::EngineKind::from_agent(&resolved.agent) {
        Some(crate::conversation::EngineKind::Claude) => resolved.conversation.clone(),
        _ => None,
    };
    let Some(session_id) = session_id else {
        // native 履歴を使わない engine は、repo が pump tap で per-session に
        // 記録した replay log を replay 源にする（engine 非依存 replay log。判定は lanes_state の
        // replay_tap と同じ判定）。それ以外（claude で会話未開始 等）は log を読まず
        // 空 chat に収束させる。
        let buffered = if matches!(
            crate::conversation::EngineKind::from_agent(&resolved.agent),
            Some(
                crate::conversation::EngineKind::Grok
                    | crate::conversation::EngineKind::OpenCode
                    | crate::conversation::EngineKind::Vpcode
            )
        ) {
            crate::conversation::replay_log::load(&addr.repo, &label)
        } else {
            Vec::new()
        };
        // ReplayStart で GUI を clear → buffered を fold → ReplayEnd で streaming を下ろす。
        // log が空なら従来と同じ「ReplayStart + ReplayEnd」= 空 chat（後方互換）。turn-scoped host
        // は attach 時点で生成中 turn を持たないため in_flight=false。
        let count = buffered.len();
        let mut events = Vec::with_capacity(count + 2);
        events.push(crate::conversation::ConversationEvent::ReplayStart);
        events.extend(buffered);
        events.push(crate::conversation::ConversationEvent::ReplayEnd { in_flight: false });
        splice_session_init(
            &mut events,
            state
                .lane_pool
                .read()
                .await
                .chat_session_init(addr, Some(resolved.key)),
        );
        route_conversation(state, lane, resolved.key, events).await;
        tracing::info!(
            "conversation replay-log: {count} events を配送 (lane={lane}, session={})",
            resolved.key
        );
        return Ok(serde_json::json!({
            "status": "no_session", "lane": lane, "session": resolved.key, "events": count
        }));
    };

    let (mut events, tail_len) =
        replay_with_in_flight(state, addr, resolved.key, &session_id).await?;
    // replay 終端で streaming の真値を宣言する。 replay は過去の assistant 発話も MessageChunk で
    // 送るため GUI 側で streaming が立つが、 replay 列は TurnCompleted を運ばない。 生成中 turn が
    // 無ければ（tail_len == 0）ここで下ろさないと、 engine が idle でも「応答中」が永久に残り、
    // turn 完了契機の処理（type-ahead flush 等）が二度と発火しなくなる。
    events.push(crate::conversation::ConversationEvent::ReplayEnd {
        in_flight: tail_len > 0,
    });
    splice_session_init(
        &mut events,
        state
            .lane_pool
            .read()
            .await
            .chat_session_init(addr, Some(resolved.key)),
    );

    let count = events.len();
    route_conversation(state, lane, resolved.key, events).await;
    tracing::info!(
        "conversation transcript replay: {count} events を配送 (lane={lane}, session={}, in-flight tail={tail_len})",
        resolved.key
    );
    Ok(serde_json::json!({
        "status": "replayed", "lane": lane, "session": resolved.key,
        "events": count, "in_flight": tail_len
    }))
}

/// 保持していた `SessionInit` を **`ReplayStart` の直後**へ差し込む。
///
/// ⚠️ `SessionInit` は engine の起動 / resume で**一度きり**流れ、transcript にも載らない。
/// GUI を開き直すと webview の state は作り直されるので、配り直さないと model / permission mode /
/// slash_commands が二度と復元されない（2026-08-07: slash command palette が空のままで発覚）。
///
/// `ReplayStart` の**後**に置くのは、GUI があれを見て会話表示をクリアするから — 前に置くと
/// 直後に消される。先頭が `ReplayStart` でない形（将来の変更）でも壊れないよう位置は実測で決める。
fn splice_session_init(
    events: &mut Vec<crate::conversation::ConversationEvent>,
    init: Option<crate::conversation::ConversationEvent>,
) {
    let Some(init) = init else { return };
    let at = usize::from(matches!(
        events.first(),
        Some(crate::conversation::ConversationEvent::ReplayStart)
    ));
    events.insert(at, init);
}

/// transcript 読み + in-flight tail の結合を、 commit 世代 `seq` で検算しながら行う。
///
/// 戻り値は `(replay 列, 継いだ tail の長さ)`。 tail 長 0 は「生成中でない」か「収束せず捨てた」。
async fn replay_with_in_flight(
    state: &RepoState,
    addr: &crate::repo::lane::LaneAddress,
    session: crate::lane::session_registry::SessionKey,
    session_id: &str,
) -> Result<(Vec<crate::conversation::ConversationEvent>, usize), String> {
    /// commit が挟まったときの読み直し回数。 commit 間隔（数百 ms 〜 秒）に対し transcript 読みは
    /// 数 ms なので、 実運用では 1 回目で収束する。
    const MAX_ATTEMPTS: usize = 3;

    for _ in 0..MAX_ATTEMPTS {
        // 先に tail を取る。 「tail → transcript」の順なら、 間に commit が挟まっても
        // transcript 側が新しい = 情報の欠落は起きない（二重化は seq 検算で弾く）。
        let before = state
            .lane_pool
            .read()
            .await
            .chat_in_flight(addr, Some(session));

        // disk read + 翻訳は同期 I/O（数 MB / 数千行）。 tokio worker を塞がないよう隔離する。
        let sid = session_id.to_string();
        let mut events = tokio::task::spawn_blocking(move || {
            crate::conversation::transcript::replay_events(&sid)
        })
        .await
        .map_err(|e| format!("conversation_demand_start: transcript 変換 join 失敗: {e}"))?;

        let after_seq = state
            .lane_pool
            .read()
            .await
            .chat_commit_seq(addr, Some(session));
        let Some(in_flight) = before else {
            // engine 未起動（chat-idle / 再起動直後）= 継ぐ tail が無い。 transcript がすべて。
            return Ok((events, 0));
        };
        if after_seq != Some(in_flight.seq) {
            // 読んでいる間に message が commit された（or engine が入れ替わった）。
            // tail が古い可能性があるので読み直す。
            continue;
        }
        let tail_len = in_flight.tail.len();
        events.extend(in_flight.tail);
        return Ok((events, tail_len));
    }

    // 収束せず（生成が極端に速い / engine が入れ替わり続ける）。 tail を捨て、 commit 済み状態に
    // 収束させる。 欠けた生成中 message は次の attach cycle で復元される。
    tracing::warn!(
        "conversation replay: commit 世代が {MAX_ATTEMPTS} 回連続で動いたため in-flight tail を破棄 (lane={addr})"
    );
    let sid = session_id.to_string();
    let events =
        tokio::task::spawn_blocking(move || crate::conversation::transcript::replay_events(&sid))
            .await
            .map_err(|e| format!("conversation_demand_start: transcript 変換 join 失敗: {e}"))?;
    Ok((events, 0))
}

/// conversation demand stop ハンドラー = **idle teardown の即時契機**。
///
/// 購読が 0 になった（= 名簿から lane タイトルが消えた）時に、その lane の**暇な** chat engine
/// を寝かせる（判定は [`LanePool::drop_idle_chat_engines`] — turn なし + N 分無活動）。
/// 畳んだ**後**に条件が揃う場合は 30s periodic の `spawn_idle_engine_sweep` が拾う
/// （demand hook は購読が切れた瞬間の 1 回きりなので、片方だけでは落ちない）。
///
/// ⚠️ 旧実装は「replay は on-attach の一度きりなので停止対象の task は無い」として noop
/// だった（#699）。当時は正しく、engine を寝かせるという発想が無かっただけ — 穴ではなく
/// 未踏の設計余地だったので、前提が変わった 2026-08-29 に埋めた。
pub(crate) async fn handle_conversation_demand_stop(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let Some(addr) = crate::repo::lane::parse_address(&lane) else {
        // 宛先が読めない = 落とす相手が決まらない。黙って何もしない（旧 noop と同じ安全側）。
        return Ok(serde_json::json!({"status": "noop", "lane": lane}));
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let dropped = state
        .lane_pool
        .write()
        .await
        .drop_idle_chat_engines(&addr, now_ms);
    if dropped.is_empty() {
        return Ok(serde_json::json!({"status": "kept", "lane": lane}));
    }
    tracing::info!(
        "idle chat engine を寝かせた（購読なし・turn なし・{}分無活動）: lane={lane} sessions={dropped:?}",
        crate::repo::lane::idle_teardown_after_minutes(),
    );
    // 実体が変わったので roster を配る（`pid` / 活動時刻が動く = 名簿の見え方が変わる）。
    crate::repo::lane::lifecycle::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "slept", "lane": lane, "sessions": dropped}))
}

/// ConversationEvent 列を per-lane conversation topic に順に route する（conversation_pump と同じ経路）。
/// `session` は発生元 session の key（doc 38 — topic は per-lane のまま、session は field で運ぶ）。
async fn route_conversation(
    state: &RepoState,
    lane: &str,
    session: crate::lane::session_registry::SessionKey,
    events: Vec<crate::conversation::ConversationEvent>,
) {
    for event in events {
        state
            .topic_router
            .route(crate::protocol::RepoMessage::ConversationEvent {
                lane: lane.to_string(),
                session,
                event,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use crate::conversation::ConversationEvent;
    use crate::repo::state::insert_test_lane;

    /// replay log の検証で外部 CLI を起動しない。存在しない cwd で eager spawn を止める。
    async fn prevent_engine_spawn(
        state: &super::RepoState,
        addr: &crate::repo::lane::LaneAddress,
        root: &std::path::Path,
    ) {
        let mut pool = state.lane_pool.write().await;
        let mut lane = pool.get(addr).unwrap().clone();
        lane.cwd = root
            .join("absent-engine-cwd")
            .to_string_lossy()
            .into_owned();
        pool.insert(lane);
    }

    #[tokio::test]
    async fn missing_codex_host_reports_history_failure_without_clearing_display() {
        use crate::lane::session_registry::{self, SessionMode};
        use crate::protocol::RepoMessage;
        let _isolated = crate::test_env::state_dir_async().await;
        let state = crate::repo::state::build_test_app_state().await;
        let addr = insert_test_lane(&state, "history-failure", SessionMode::Tui).await;
        let key = session_registry::create(
            &addr.repo,
            "main",
            "claude",
            "codex",
            SessionMode::Gui,
            false,
        )
        .unwrap();
        let resolved = state
            .lane_pool
            .read()
            .await
            .resolve_chat_session(&addr, Some(key))
            .unwrap();
        let (_, mut events) = state
            .topic_router
            .subscribe("repo/conversation/data/history-failure~lane~main/event")
            .await;
        assert!(
            super::replay_once(&state, &addr, &addr.to_string(), &resolved)
                .await
                .is_err()
        );
        assert!(
            matches!(events.try_recv(), Ok((_, RepoMessage::ConversationEvent { session, event:ConversationEvent::Error { .. }, .. })) if session == key)
        );
        assert!(
            events.try_recv().is_err(),
            "失敗で ReplayStart を送って表示を消さない"
        );
    }

    fn init_ev() -> ConversationEvent {
        ConversationEvent::SessionInit {
            session_id: "sid".into(),
            model: None,
            permission_mode: None,
            cwd: None,
            tools: vec![],
            mcp_servers: vec![],
            slash_commands: vec!["compact".into()],
            command_docs: Default::default(),
        }
    }

    /// ⚠️ **`ReplayStart` の後**に置く。GUI はあれを見て会話表示をクリアするので、
    /// 前に置くと配り直した session 状態が直後に消える。
    #[test]
    fn session_init_goes_after_replay_start() {
        let mut ev = vec![
            ConversationEvent::ReplayStart,
            ConversationEvent::ReplayEnd { in_flight: false },
        ];
        super::splice_session_init(&mut ev, Some(init_ev()));
        assert!(matches!(ev[0], ConversationEvent::ReplayStart));
        assert!(matches!(ev[1], ConversationEvent::SessionInit { .. }));
    }

    /// engine 未起動（chat-idle / tui）は配り直すものが無い = 列を触らない。
    #[test]
    fn no_retained_init_leaves_events_untouched() {
        let mut ev = vec![ConversationEvent::ReplayStart];
        super::splice_session_init(&mut ev, None);
        assert_eq!(ev.len(), 1);
    }

    /// 先頭が `ReplayStart` でない形（将来の変更）でも落とさず先頭に置く。
    #[test]
    fn without_replay_start_it_goes_first() {
        let mut ev = vec![ConversationEvent::ReplayEnd { in_flight: false }];
        super::splice_session_init(&mut ev, Some(init_ev()));
        assert!(matches!(ev[0], ConversationEvent::SessionInit { .. }));
    }

    /// engine 非依存 replay log: Grok session に会話を仕込むと、demand_start が replay_log を
    /// 読み `ReplayStart → 記録 events → ReplayEnd` を配送する（transcript を持たない engine の
    /// replay 源）。外部 engine は存在しない cwd により起動させない。
    #[tokio::test]
    async fn conversation_demand_start_replays_buffered_log_for_grok_session() {
        use crate::conversation::ConversationEvent;
        use crate::lane::session_registry::SessionMode;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::time::Duration;

        // replay_log / session_registry は vp_state_dir() を読む → tempdir に隔離。
        let _state_guard = crate::test_env::state_dir_async().await;
        let state = build_test_app_state().await;
        let addr = insert_test_lane(&state, "vptest-replaylog", SessionMode::Gui).await;
        prevent_engine_spawn(&state, &addr, _state_guard.path()).await;

        // focused な Grok session #2 を作る（session=None がこれに解決される）。
        let k2 = state
            .lane_pool
            .write()
            .await
            .create_chat_session(&addr, Some("grok"), true)
            .expect("create Grok session");
        assert_eq!(k2, 2);

        // #2 の replay 源に会話を仕込む（session label = "main#2"）。
        for ev in [
            ConversationEvent::MessageChunk {
                text: "Grok says hi".to_string(),
            },
            ConversationEvent::TurnCompleted {
                session_id: "s".to_string(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            },
        ] {
            crate::conversation::replay_log::append("vptest-replaylog", "main#2", &ev)
                .expect("replay log append");
        }

        // conversation topic を購読（非 retained なので dispatch 前に張る）。
        let topic = "repo/conversation/data/vptest-replaylog~main/event";
        let (_id, mut srx) = state.topic_router.subscribe(topic).await;

        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-replaylog/main" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "no_session");
        assert_eq!(res["events"], 2, "仕込んだ 2 event が replay される");

        // 配送列: ReplayStart → MessageChunk → TurnCompleted → ReplayEnd。
        let mut got = Vec::new();
        for _ in 0..4 {
            let (_t, msg) = tokio::time::timeout(Duration::from_secs(2), srx.recv())
                .await
                .expect("replay event timeout")
                .expect("topic closed");
            match msg {
                RepoMessage::ConversationEvent { session, event, .. } => {
                    assert_eq!(session, 2, "session field で #2 を運ぶ");
                    got.push(event);
                }
                other => panic!("想定外の message: {other:?}"),
            }
        }
        assert_eq!(got[0], ConversationEvent::ReplayStart);
        assert_eq!(
            got[1],
            ConversationEvent::MessageChunk {
                text: "Grok says hi".to_string()
            }
        );
        assert!(matches!(got[2], ConversationEvent::TurnCompleted { .. }));
        assert_eq!(got[3], ConversationEvent::ReplayEnd { in_flight: false });
    }

    /// 起動時 3 重 demand の single-flight 化（2026-07-27 fleetstage 実測の根治）: 進行中
    /// flight がある間の demand は**配送せず**合流し（status=coalesced）、rerun 予約だけ残す。
    /// 予約は flight 完了側が直列に消化する — 並行 route による event 混線（clear-prefix
    /// 収束論の破れ = 孤児 ToolCallUpdate / ReplayEnd 欠落）を構造的に排除する。
    #[tokio::test]
    async fn conversation_demand_start_coalesces_while_replay_in_flight() {
        use crate::lane::session_registry::SessionMode;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::time::Duration;

        let _state_guard = crate::test_env::state_dir_async().await;
        let state = build_test_app_state().await;
        let addr = insert_test_lane(&state, "vptest-coalesce", SessionMode::Gui).await;
        prevent_engine_spawn(&state, &addr, _state_guard.path()).await;

        // focused な Grok session #2（session 省略の demand がこれに解決される）。
        let k2 = state
            .lane_pool
            .write()
            .await
            .create_chat_session(&addr, Some("grok"), true)
            .expect("create Grok session");
        assert_eq!(k2, 2);

        // 進行中 flight を模擬（handler と同じ key = lane display 形 + session key）。
        assert!(state.replay_flights.begin("vptest-coalesce/main", 2));

        let topic = "repo/conversation/data/vptest-coalesce~main/event";
        let (_id, mut srx) = state.topic_router.subscribe(topic).await;

        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-coalesce/main" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "coalesced", "進行中は合流して配送しない");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), srx.recv())
                .await
                .is_err(),
            "coalesced の demand は event を 1 つも route しない"
        );
        assert!(
            state.replay_flights.finish("vptest-coalesce/main", 2),
            "合流は rerun 予約として残る（flight 完了側が直列に消化する契約）"
        );
        assert!(!state.replay_flights.finish("vptest-coalesce/main", 2));

        // flight 終了後の demand は通常配送に戻る（begin → replay → finish で entry が残らない）。
        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-coalesce/main" }),
        )
        .await
        .expect("demand_start after flight");
        assert_eq!(res["status"], "no_session", "Grok は replay_log path");
        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-coalesce/main" }),
        )
        .await
        .expect("demand_start twice");
        assert_eq!(
            res["status"], "no_session",
            "連続 demand も通る = flight entry が leak していない"
        );
    }

    /// doc 50 §4.6 A6: **root=tui のまま非 root だけ chat** の構成で replay が届く。
    ///
    /// team-b review 2026-07-25（score 92）: `handle_conversation_demand_start` の gate が lane 単位
    /// `console_mode`（= root cache）だったため、A6 が正規にサポートする構成で `not_chat` を返し、
    /// **ReplayStart すら送らずに会話が復元されなかった**。gate を「その session の mode」に直した
    /// ことを、root が tui のままである状態で固定する（root=chat の既存テストでは検出できない）。
    #[tokio::test]
    async fn conversation_demand_start_replays_non_root_chat_while_root_is_tui() {
        use crate::conversation::ConversationEvent;
        use crate::lane::session_registry::SessionMode;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::time::Duration;

        let _state_guard = crate::test_env::state_dir_async().await;
        let state = build_test_app_state().await;
        // **root は tui**（= 旧 gate ならここで not_chat に落ちる）。
        let addr = insert_test_lane(&state, "vptest-nonroot-chat", SessionMode::Tui).await;
        prevent_engine_spawn(&state, &addr, _state_guard.path()).await;

        // 非 root の chat session を作る（create_chat_session は mode=Chat で作る）。
        let k2 = state
            .lane_pool
            .write()
            .await
            .create_chat_session(&addr, Some("grok"), true)
            .expect("create chat session");

        // その session の replay 源に会話を仕込む。
        for ev in [
            ConversationEvent::MessageChunk {
                text: "non-root chat lives".to_string(),
            },
            ConversationEvent::TurnCompleted {
                session_id: "s".to_string(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            },
        ] {
            crate::conversation::replay_log::append(
                "vptest-nonroot-chat",
                &format!("main#{k2}"),
                &ev,
            )
            .expect("replay log append");
        }

        let topic = "repo/conversation/data/vptest-nonroot-chat~main/event";
        let (_id, mut srx) = state.topic_router.subscribe(topic).await;

        // session を明示して demand（client の mode 切替後の明示 demand と同じ形）。
        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-nonroot-chat/main", "session": k2 }),
        )
        .await
        .expect("demand_start");
        assert_ne!(
            res["status"], "not_chat",
            "root が tui でも非 root の chat には replay が届く（旧 gate はここで落ちていた）"
        );
        assert_eq!(res["events"], 2, "仕込んだ 2 event が replay される");

        // 配送列の先頭が ReplayStart（= GUI の会話 clear 契機）で、session field が非 root を運ぶ。
        let (_t, msg) = tokio::time::timeout(Duration::from_secs(2), srx.recv())
            .await
            .expect("replay event timeout")
            .expect("topic closed");
        match msg {
            RepoMessage::ConversationEvent { session, event, .. } => {
                assert_eq!(session, k2, "session field で非 root を運ぶ");
                assert_eq!(event, ConversationEvent::ReplayStart);
            }
            other => panic!("想定外の message: {other:?}"),
        }
    }
}
