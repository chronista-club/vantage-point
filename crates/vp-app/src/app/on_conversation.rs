//! event handler: **conversation**（gui mode の chat = submit / respond / interrupt / permission /
//! model、session の mode 切替と slot 操作、console の new session / switch root、agents 一覧、
//! tui の OSC 通知）。
//!
//! 旧 `run()` の match arm を fn ごとに移したもの（doc 60 §6 6-2 PR-6、2026-09-08）。本体は arm の
//! 中身を 12 空白 dedent しただけ、arm の前のコメントは fn の doc へ移動。
//!
//! 触る state: `ui.sidebar_state`、`ui.sessions.conversation_sessions`（lazy spawn）、
//! `ui.sessions.terminal_sessions`（`session_mode_applied` が tui 化で spawn）。
//! resource: `boot.webview` / `boot.rt_handle` / `boot.daemon_conn`。
//! ⚠️ `session_mode_applied` は `ui.sidebar_state.lanes_by_repo` の session.mode を書く
//! （lanes snapshot の所有者は LanesLoaded の arm。6-2 PR-7 で `on_lanes` へ）。

use tao::event_loop::EventLoopProxy;

use super::boot::Boot;
use super::lane_view::{
    ensure_conversation_attach, focused_session_agent, lane_is_chat, mark_lane_awaiting_input,
    resolve_repo_path_for_lane, root_session_of,
};
use super::state::UiState;
use crate::daemon::conn::daemon_repo_request;
use crate::daemon::pollers::resolve_active_repo_path;
use crate::events::AppEvent;
use crate::lane::conversation::{ConversationCmd, spawn_conversation_session};
use crate::lane::terminal::spawn_terminal_session;
use crate::webview::push_main;
use crate::webview::push_sidebar::{self, push_sidebar_state};

pub(super) fn osc_notification(ui: &mut UiState, boot: &Boot, lane: String) {
    // Phase 5-D Sprint C P2.1: per-Lane HD notification（tui / OSC 由来）。
    // active lane は即読 skip。共通 sink（gui の turn_completed と合流）。
    mark_lane_awaiting_input(
        &lane,
        "osc:notification",
        &mut ui.sidebar_state,
        &boot.webview,
    );
}

/// Conversation gui (doc 32): repo から受信した構造化イベントを当該 lane の Console pane に渡す。
pub(super) fn conversation_event(
    ui: &mut UiState,
    boot: &Boot,
    lane: String,
    event: serde_json::Value,
    session: u32,
) {
    // doc 38 Phase 2: 第 3 引数 session（VP 採番 key）を渡す。console.ts が focused
    // 判定に使い、chatview が背景 session の stream を焦点会話へ混ぜないよう filter する。
    push_main::console_event(&boot.webview, &lane, event.clone(), session);
    // 路 A（memory echoes-act2-notification-signal）: gui の完了/エラーを tui の
    // OSC 通知と同じ sink に流す。headless stream-json は Notification hook を発火しない
    // ため、turn_completed（stream `result` 由来）が「Claude が返し終えた＝入力待ち」の
    // 唯一のシグナル。question（PR1）/ permission_request（PR3 tool 承認 + PR4 plan の
    // ExitPlanMode）は engine が turn を pause して人の判断を待つ明示的 HITL なので、同じ
    // awaiting_input 機構で conn-hitl（magenta diamond）を点灯する（doc 35 §4 の契約）。
    // active lane は helper が即読 skip し、切替（activate_lane）で reset される。
    if let Some(kind) = event.get("kind").and_then(|k| k.as_str())
        && (kind == "turn_completed"
            || kind == "error"
            || kind == "question"
            || kind == "permission_request")
    {
        mark_lane_awaiting_input(
            &lane,
            &format!("gui:{kind}"),
            &mut ui.sidebar_state,
            &boot.webview,
        );
    }
}

/// Conversation gui: ChatPane の submit → 当該 lane の conversation session に渡す。
/// demand-driven: 未起動なら lazy spawn (subscribe → submit の順で取りこぼしなし)。
#[allow(clippy::too_many_arguments)] // payload の field をそのまま渡す（AppEvent の struct 化は別 PR）
pub(super) fn conversation_submit(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    prompt: String,
    chat_session: Option<u32>,
    images: Vec<serde_json::Value>,
    request_id: String,
) {
    let session = ui
        .sessions
        .conversation_sessions
        .entry(lane.clone())
        .or_insert_with(|| {
            // repo_path は active repo から解決 (conversation pane = active lane 前提)。
            let repo_path = resolve_active_repo_path(&ui.sidebar_state).unwrap_or_default();
            spawn_conversation_session(
                &boot.rt_handle,
                async_action_proxy.clone(),
                boot.daemon_conn.clone(),
                repo_path,
                lane.clone(),
            )
        });
    let (reply, result) = tokio::sync::oneshot::channel();
    let proxy = async_action_proxy.clone();
    let client_user_message_id = request_id.clone();
    boot.rt_handle.spawn(async move {
        let event = crate::conversation_submission::await_submit_result(&request_id, result).await;
        let _ = proxy.send_event(AppEvent::ConversationEvent {
            lane,
            session: chat_session.unwrap_or(1),
            event,
        });
    });
    let _ = session.cmd_tx.send(ConversationCmd::Submit {
        client_user_message_id,
        prompt,
        session: chat_session,
        images,
        reply,
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn conversation_codex_input(
    ui: &UiState,
    boot: &Boot,
    proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    session: u32,
    thread_id: String,
    request_id: String,
    action: serde_json::Value,
) {
    let path = resolve_repo_path_for_lane(&ui.sidebar_state, &lane);
    let conn = boot.daemon_conn.clone();
    let proxy = proxy.clone();
    boot.rt_handle.spawn(async move {
        let result = match path {
            Some(path) => daemon_repo_request(&conn, &path, "conversation_codex_input",
                serde_json::json!({"lane":lane,"session":session,"thread_id":thread_id,"action":action})).await.map(|_| ()),
            None => Err("対象の作業場所が見つかりません。入力は送信されていません。".into()),
        };
        let _ = proxy.send_event(AppEvent::ConversationEvent {
            lane, session,
            event: serde_json::json!({"kind":"codex_queue","queue":null,"request_id":request_id,"error":result.err()}),
        });
    });
}

/// Conversation gui HITL (doc 35 PR1): PromptCard の回答 → 当該 lane の conversation session へ。
/// 質問は submit 済み engine 由来なので session は既存のはずだが、防御的に lazy spawn。
#[allow(clippy::too_many_arguments)] // payload の field をそのまま渡す（AppEvent の struct 化は別 PR）
pub(super) fn conversation_respond(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    request_id: String,
    answers: Option<serde_json::Value>,
    behavior: Option<String>,
    message: Option<String>,
    chat_session: Option<u32>,
) {
    // Codex の回答は request ごとの結果を UI へ返す。旧 engine の経路は維持する。
    if request_id.starts_with("codex:") {
        let Some(chat_session) = chat_session else {
            return;
        };
        let path = resolve_repo_path_for_lane(&ui.sidebar_state, &lane);
        let conn = boot.daemon_conn.clone();
        let proxy = async_action_proxy.clone();
        boot.rt_handle.spawn(async move {
            let result = if let Some(path) = path {
                let payload = serde_json::json!({"lane":lane,"session":chat_session,"request_id":request_id,
                    "answers":answers,"behavior":behavior,"message":message});
                match tokio::time::timeout(std::time::Duration::from_secs(15), daemon_repo_request(&conn, &path, "conversation_respond", payload)).await {
                    Ok(result) => result.map(|_| ()),
                    Err(_) => Err("回答送信の結果を確認できません。再接続して要求の状態を確認してください。".into()),
                }
            } else { Err("対象の作業場所が見つかりません".into()) };
            let _ = proxy.send_event(AppEvent::ConversationEvent {
                lane, session: chat_session,
                event: serde_json::json!({"kind":"codex_interaction_result","request_id":request_id,"error":result.err()}),
            });
        });
        return;
    }
    let session = ui
        .sessions
        .conversation_sessions
        .entry(lane.clone())
        .or_insert_with(|| {
            let repo_path = resolve_active_repo_path(&ui.sidebar_state).unwrap_or_default();
            spawn_conversation_session(
                &boot.rt_handle,
                async_action_proxy.clone(),
                boot.daemon_conn.clone(),
                repo_path,
                lane.clone(),
            )
        });
    let _ = session.cmd_tx.send(ConversationCmd::Respond {
        request_id,
        answers,
        behavior,
        message,
        session: chat_session,
    });
}

/// doc 35 §5 / PR2: 実行中 turn の中断を当該 lane の conversation session に渡す。
/// interrupt は走行中 turn 前提なので session が居るはず（lazy spawn しない）。
pub(super) fn conversation_interrupt(
    ui: &mut UiState,
    _boot: &Boot,
    lane: String,
    chat_session: Option<u32>,
) {
    if let Some(session) = ui.sessions.conversation_sessions.get(&lane) {
        let _ = session.cmd_tx.send(ConversationCmd::Interrupt {
            session: chat_session,
        });
    } else {
        tracing::warn!("conversation:interrupt skip — session 未起動 (lane={lane})");
    }
}

/// doc 35 §2.5 / PR3: permission mode 切替を当該 lane の conversation session に渡す。
pub(super) fn conversation_set_permission_mode(
    ui: &mut UiState,
    _boot: &Boot,
    lane: String,
    mode: String,
    chat_session: Option<u32>,
) {
    if let Some(session) = ui.sessions.conversation_sessions.get(&lane) {
        let _ = session.cmd_tx.send(ConversationCmd::SetPermissionMode {
            mode,
            session: chat_session,
        });
    } else {
        tracing::warn!("conversation:set_permission_mode skip — session 未起動 (lane={lane})");
    }
}

/// doc 50 §4.6 A6: 名札 kind badge からの Mode 切替（session 明示）。repo の
/// `session_set_mode` に forward し、成功したら SessionModeApplied で表示を追従させる。
pub(super) fn session_set_mode(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    session: u32,
    mode: String,
) {
    // repo は対象 lane 自身から逆引き（#705 のレース教訓 — repo 応答待ちの間に
    // active lane が変わり得るため resolve_active_repo_path は使わない）。
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("session:set_mode skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    let proxy = async_action_proxy.clone();
    let (lane_for_js, mode_for_js) = (lane.clone(), mode.clone());
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        match daemon_repo_request(
            &conn,
            &path,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": session, "mode": mode }),
        )
        .await
        {
            Ok(_) => {
                tracing::info!(
                    "session_set_mode ok: lane={lane_for_js} session={session} mode={mode_for_js}"
                );
                let _ = proxy.send_event(AppEvent::SessionModeApplied {
                    lane: lane_for_js,
                    session,
                    mode: mode_for_js,
                });
            }
            Err(e) => {
                tracing::warn!("session_set_mode 失敗 (lane={lane_for_js} session={session}): {e}")
            }
        }
    });
}

/// doc 50 §4.6 A6: session_set_mode 成功後、WebView に mode を反映する。
///
/// **replay はここでは撃たない**（S2 と対）— World B が新しい kind の pane を mount し、
/// その pane が購読を張ってから demand を撃つ（購読前 replay は非 retained topic で
/// 落ちる順序 race）。ここは「mode が変わった」事実を JS に渡すだけに徹する。
pub(super) fn session_mode_applied(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    session: u32,
    mode: String,
) {
    let is_tui = mode == "tui";
    let is_root = root_session_of(&ui.sidebar_state, &lane) == session;
    // 手元 snapshot（registry の投影）を即時更新する。lanes snapshot の反映は 5s
    // periodic 頼みで stale が残るため（ConsoleModeApplied と同じ理由）、mode を
    // 読む後続（term_sessions_of / attach gate / activate_lane）が旧値を見ないようにする。
    // doc 53 R1: 更新は sessions の 1 箇所だけ — 読み手（lane_is_chat / respawn
    // gate / header 差分）は sessions から root mode を導出するので、この 1 書きで
    // 全読み手に届く（旧「root なら lane 単位 mode 投影も更新」は退役）。
    for lanes in ui.sidebar_state.lanes_by_repo.values_mut() {
        if let Some(l) = lanes.iter_mut().find(|l| l.address.key() == lane)
            && let Some(reg) = l.sessions.as_mut()
            && let Some(e) = reg.sessions.iter_mut().find(|s| s.key == session)
        {
            e.mode = mode.clone();
        }
    }
    push_sidebar_state(&boot.webview, &ui.sidebar_state);
    // xterm の起立 / 撤去（World A は instance 管理に徹し、顔ぶれの決定は上位が持つ）。
    if is_tui {
        push_main::ensure_lane(&boot.webview, &lane, session, is_root);
        // 購読が無いと新 PtySlot の出力が届かない（terminal topic は非 retained）。
        // demand 0→1 が repo の pump 張り直し + replay を撃つ。idempotent。
        match resolve_repo_path_for_lane(&ui.sidebar_state, &lane) {
            Some(path) => {
                ui.sessions
                    .terminal_sessions
                    .entry(lane.clone())
                    .or_insert_with(|| {
                        spawn_terminal_session(
                            &boot.rt_handle,
                            async_action_proxy.clone(),
                            boot.daemon_conn.clone(),
                            path,
                            lane.clone(),
                        )
                    });
            }
            None => tracing::warn!(
                "session:mode_applied — lane の repo 解決失敗、terminal session を張れず (lane={lane})"
            ),
        }
        // ⚠️ **xterm の container を active 化しないと見えない**（`.lane-pane` は
        // display:none が既定で、`.active` が付いて初めて描かれる）。chat から戻って
        // 新しく作った instance は非 active のままなので、これが無いと「名札は出るのに
        // 中身が真っ黒」になる（2026-07-25 実機で踏んだ — 旧 ConsoleModeApplied が
        // 持っていた 1 行を S6 の撤去時に移植し忘れていた）。
        // showLane は active 化に加えて rAF 2 段で fit / sendResize / focus まで行う。
        // 順序: ensure_lane より後（instance が無いと active 化できない）。
        //
        // repo 応答待ちの間に別 lane へ移っていたら表示は奪わない（mode は手元 snapshot に
        // 反映済みなので、戻った時に正しい顔ぶれで開く）。
        if ui.sidebar_state.active_lane_address.as_deref() == Some(lane.as_str()) {
            push_main::show_lane(&boot.webview, Some(&lane), false);
        }
    } else {
        // →chat: その session の xterm を畳む（PtySlot は repo 側で drop 済）。
        push_main::remove_lane_session(&boot.webview, &lane, session);
        // tui→II の対称: conversation topic への購読を確保する（初回 chat 化で張られる）。
        // 上の手元 snapshot 反映が先に要る（attach の gate が mode を読む）。
        ensure_conversation_attach(
            &lane,
            &ui.sidebar_state,
            &mut ui.sessions.conversation_sessions,
            &boot.rt_handle,
            async_action_proxy,
            &boot.daemon_conn,
        );
        // **Reborn ⊃ replay の実体**（doc 50 §4.6 ① / §4.7 逸脱②）: 切替のたび
        // transcript を読み直す。
        //
        // ⚠️ `ensure_conversation_attach` に任せてはいけない — あれは購読ハンドル
        // （`conversation_sessions`、**lane 単位**）が既にあれば no-op で、購読は lane 削除まで
        // 残る。つまり chat→tui→chat の 2 回目以降は attach が発火せず、**tui で
        // 進めた分が chat に出ない**（A6 が根治すると宣言した当の症状が別の理由で再現する。
        // team-b review 2026-07-25 の指摘で発覚）。購読を落として張り直す案は採らない —
        // 購読は lane 単位で**他の chat session の live stream も運んでいる**ため、
        // 落とすと巻き添えになる。gate を経由しない明示 demand が正しい形。
        //
        // demand は session を明示する（replay は session 単位 — `conversation_demand_start`
        // の None は focused に解決されるので、非 focused な pane を切り替えた時に
        // 別会話を読んでしまう）。
        if let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) {
            let proxy = async_action_proxy.clone();
            let lane_for_log = lane.clone();
            let conn = boot.daemon_conn.clone();
            boot.rt_handle.spawn(async move {
                if let Err(e) = daemon_repo_request(
                    &conn,
                    &path,
                    "conversation_demand_start",
                    serde_json::json!({ "lane": lane_for_log, "session": session }),
                )
                .await
                {
                    tracing::warn!(
                        "conversation_demand_start（mode 切替後）失敗 (session={session}): {e}"
                    );
                }
                let _ = &proxy; // 応答は topic 経由で届く（ここでは event を投げない）
            });
        }
    }
    push_main::console_mode_applied(&boot.webview, &lane, session, &mode);
}

/// 新セッション開始（✨ New ボタン）。doc 39 §4「New は今いる Mode に出す」で分岐する:
///  - chat lane（gui）: 「新 Draft session を作って focus」。旧会話はタブに残る
///    （タブモデルの自然形 = 前回状態キープの延長）。
///  - tui lane（tui）: lane_slot_new = slot を**足す**だけ（root 不変、A6 ③）。
///    ⚠️ 旧実装の conversation_session_new_root（root 張り替え）は A6 で撤去 — root を
///    動かすのは chip picker（console:switch_root）と「New Root Conversation」
///    （sidebar context menu、doc 39 §8.4）の明示操作のみ。
pub(super) fn console_new_session(
    ui: &mut UiState,
    boot: &Boot,
    lane: String,
    engine: Option<String>,
    mode: Option<String>,
) {
    // repo は対象 lane 自身から逆引き（#705 のレース教訓 — repo 応答待ちの間に
    // active lane が変わり得るため resolve_active_repo_path は使わない）。
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("console:new_session skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    // doc 46 P2 要件 4: Mode は**明示指定を優先**し、無ければ lane の現 Mode を継ぐ。
    // 未知の値（typo 等）は継承に倒す — 「指定したのに黙って別の Mode で作られた」より
    // 「指定が効かなかった」方が気付きやすい。
    let want_chat = match mode.as_deref() {
        Some("gui") => true,
        Some("tui") => false,
        _ => lane_is_chat(&ui.sidebar_state, &lane),
    };
    if want_chat {
        // doc 38 §4.2: chat lane は「新 Draft session を作って focus」。
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            // 1. engine を決める。doc 46 P2 要件 4 の**明示指定があればそれを使い**、
            //    無い時だけ現 focused を継ぐ（従来挙動）。指定がある場合は
            //    session_list の往復ごと省ける。
            let agent = match engine {
                Some(e) => Some(e),
                None => match daemon_repo_request(
                    &conn,
                    &path,
                    "conversation_session_list",
                    serde_json::json!({ "lane": &lane }),
                )
                .await
                {
                    Ok(payload) => focused_session_agent(&payload),
                    Err(e) => {
                        tracing::warn!(
                            "conversation_session_list（new_session 前）失敗 (lane={lane}): {e}"
                        );
                        None
                    }
                },
            };
            // 2. 新 Draft session を作って focus（focus は明示 true）。
            let mut create = serde_json::json!({ "lane": &lane, "focus": true });
            if let Some(s) = &agent {
                create["agent"] = serde_json::Value::String(s.clone());
            }
            if let Err(e) =
                daemon_repo_request(&conn, &path, "conversation_session_create", create).await
            {
                tracing::warn!("console:new_session（chat）session_create 失敗 (lane={lane}): {e}");
                return;
            }
            tracing::info!("console:new_session ok（chat, new draft）: lane={lane}");
            // 3. roster（tab strip / focusedOf）の更新は **snapshot が運ぶ**
            //    （doc 53 §11。server の `emit_lane_update` → LanesLoaded）。
            //
            //    ⚠️ 旧実装はここで一覧を取り直し「demand_start より先に送る」順序を
            //    守っていた。その理由（session filter が旧 focused のまま replay_start を
            //    落とす）は **A6 で消えている** — event は focused で捨てず session ごとの
            //    store に振り分けるようになった（doc 50 §4.3 #2 / `foldEvent`）。
            //    roster が数十 ms 遅れて届いても、表示先が切り替わるのが僅かに遅れるだけで
            //    event は落ちない。
            // 4. demand_start で新 focused（Draft）の replay を発火。no_session path でも
            //    ReplayStart/End が届いて会話がクリアされる（doc 38 §4.2）。
            if let Err(e) = daemon_repo_request(
                &conn,
                &path,
                "conversation_demand_start",
                serde_json::json!({ "lane": &lane }),
            )
            .await
            {
                tracing::warn!(
                    "conversation_demand_start（new_session 後）失敗 (lane={lane}): {e}"
                );
            }
        });
    } else {
        // tui（tui）: doc 50 §4.6 A6 ③ — chat 分岐と**対称**に「新 session を作って
        // 台に並べる」だけ。新 term pane が tiling に入場し、既存 pane は無傷。
        //
        // ⚠️ 旧実装は `conversation_session_new_root`（新 session + **root 張り替え** + slot
        // bare respawn）だった。あれは「xterm が lane に 1 枚」制約下では正しい適応
        // （新しい console を見せる唯一の方法が root の付け替えだった）が、A6 で制約が
        // 外れた今は「勝手に root を動かす副作用」に意味が反転する。root の付け替えは
        // `console:switch_root`（root picker）の明示操作に一本化した。
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            // engine の明示指定は backend まで通す（無ければ lane の agent を継ぐ）。
            let mut payload = serde_json::json!({ "lane": &lane });
            if let Some(e) = &engine {
                payload["agent"] = serde_json::Value::String(e.clone());
            }
            match daemon_repo_request(&conn, &path, "lane_slot_new", payload).await {
                Ok(res) => {
                    let session = res.get("session").and_then(serde_json::Value::as_u64);
                    tracing::info!(
                        "console:new_session ok（tui, new slot）: lane={lane} session={session:?}"
                    );
                    // roster（新 term pane が生える元）の更新は snapshot が運ぶ
                    // （doc 53 §11）。root は動いていないので会話 clear も送らない。
                }
                Err(e) => tracing::warn!("console:new_session 失敗 (lane={lane}): {e}"),
            }
        });
    }
}

/// doc 39 P3: Root 切替 picker — 既存 session へ root を向け替え（slot = Resume respawn）。
/// 後続は new_root（ConsoleSessionRenewed = clear）と違い conversation_demand_start:
/// 対象 session には既存の会話があるため、clear でなく transcript replay で追従させる
///（conversation_session_focus chain と同じ規律）。
pub(super) fn console_switch_root(ui: &mut UiState, boot: &Boot, lane: String, session: u64) {
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("console:switch_root skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        let payload = serde_json::json!({ "lane": &lane, "session": session });
        match daemon_repo_request(&conn, &path, "conversation_session_switch_root", payload).await {
            Ok(_) => {
                tracing::info!("console:switch_root ok: lane={lane} session={session}");
                // roster（tab strip / picker）の更新は snapshot が運ぶ（doc 53 §11）。
                // 旧実装が守っていた「replay より先に一覧」の順序は A6 で不要に
                // なっている（event は session ごとの store に振り分けられる —
                // `ConsoleNewSession` の chat 分岐のコメント参照）。
                if let Err(e) = daemon_repo_request(
                    &conn,
                    &path,
                    "conversation_demand_start",
                    serde_json::json!({ "lane": &lane }),
                )
                .await
                {
                    tracing::warn!(
                        "conversation_demand_start（switch_root 後）失敗 (lane={lane}): {e}"
                    );
                }
            }
            Err(e) => {
                tracing::warn!("console:switch_root 失敗 (lane={lane} session={session}): {e}")
            }
        }
    });
}

/// gui モデル切替: conversation_set_model で repo に forward（fire & forget、
/// session 単位）。適用の視覚確認は新 engine の session_init が header.model を
/// 更新することで得る。
#[allow(clippy::too_many_arguments)]
pub(super) fn conversation_set_model(
    ui: &mut UiState,
    boot: &Boot,
    proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    session: u64,
    model: Option<String>,
    effort: Option<String>,
    request_id: Option<String>,
) {
    let Ok(session) = u32::try_from(session) else {
        return;
    };
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("conversation:set_model skip — lane の repo 解決失敗 (lane={lane})");
        if let Some(request_id) = request_id {
            let _ = proxy.send_event(AppEvent::ConversationEvent {
                lane, session,
                event: serde_json::json!({"kind":"codex_config", "config":null, "request_id":request_id, "error":"対象の作業場所が見つかりません"}),
            });
        }
        return;
    };
    let conn = boot.daemon_conn.clone();
    let proxy = proxy.clone();
    boot.rt_handle.spawn(async move {
        let payload = serde_json::json!({ "lane": &lane, "session": session, "model": model, "effort":effort });
        let result = daemon_repo_request(&conn, &path, "conversation_set_model", payload).await;
        if let Some(request_id) = request_id {
            let (config, error) = match result {
                Ok(value) => (value.get("codex_config").cloned(), None),
                Err(error) => (None, Some(error)),
            };
            let _ = proxy.send_event(AppEvent::ConversationEvent {
                lane, session,
                event: serde_json::json!({"kind":"codex_config", "config":config, "request_id":request_id, "error":error}),
            });
        } else if let Err(error) = result {
            tracing::warn!("conversation:set_model 失敗 (lane={lane} session={session}): {error}");
        }
    });
}

/// doc 53 §11: 旧 `ConversationSessionsFetch`（webview → ask `conversation_session_list`）は退役。
/// roster の供給は lanes snapshot 1 本（LanesLoaded の push）— fetch は GUI 自身の
/// 動詞でしか撃たれず、CLI / MCP 由来の session 変化が pane に出なかった。
///
/// doc 38 Phase 2: 「+」からの新 session 作成。focus は送らない = backend 既定 true。
/// 作成後に一覧を取り直して tab strip に新 session を即反映（1 task で直列）。
pub(super) fn conversation_session_create(
    ui: &mut UiState,
    boot: &Boot,
    lane: String,
    agent: Option<String>,
) {
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("conversation:session_create skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        let mut create = serde_json::json!({ "lane": &lane });
        if let Some(s) = &agent {
            create["agent"] = serde_json::Value::String(s.clone());
        }
        // doc 53 §11: 動詞を撃つだけ。roster の更新は server の `emit_lane_update`
        // → lanes snapshot → LanesLoaded で届く（旧: ここで一覧を取り直していた）。
        if let Err(e) =
            daemon_repo_request(&conn, &path, "conversation_session_create", create).await
        {
            tracing::warn!("conversation_session_create 失敗 (lane={lane}): {e}");
        }
    });
}

/// doc 38 Phase 2: session tab click による focused 切替。focus → 一覧再取得 →
/// demand_start（新 focused の transcript replay 発火）の順で直列に。
pub(super) fn conversation_demand_start(ui: &mut UiState, boot: &Boot, lane: String) {
    // 消費者主導の replay demand（2026-07-24）: webview が renderer を張った直後に
    // 届く。attach 時 demand（run_conversation_session）の boot 窓取りこぼしを埋める第 2 弾
    //（冪等 — ensure_chat_engine は既起動 no-op / replay は clear-prefix で収束）。
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("conversation:demand_start skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        if let Err(e) = daemon_repo_request(
            &conn,
            &path,
            "conversation_demand_start",
            serde_json::json!({ "lane": &lane }),
        )
        .await
        {
            tracing::warn!("conversation_demand_start（webview 発）失敗 (lane={lane}): {e}");
        }
    });
}

pub(super) fn conversation_session_focus(
    ui: &mut UiState,
    boot: &Boot,
    lane: String,
    session: u32,
) {
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("conversation:session_focus skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        if let Err(e) = daemon_repo_request(
            &conn,
            &path,
            "conversation_session_focus",
            serde_json::json!({ "lane": &lane, "session": session }),
        )
        .await
        {
            tracing::warn!("conversation_session_focus 失敗 (lane={lane} session={session}): {e}");
            return;
        }
        // tab strip の focused 確定は snapshot が運ぶ（doc 53 §11 — server の
        // `conversation_session_focus` が末尾で `emit_lane_update` を撃つ）。
        // 新 focused の transcript replay を発火（session 省略 = focused に解決）。
        // 応答は使わない（replay は topic 経由で ReplayStart として届く）。エラーは warn のみ。
        if let Err(e) = daemon_repo_request(
            &conn,
            &path,
            "conversation_demand_start",
            serde_json::json!({ "lane": &lane }),
        )
        .await
        {
            tracing::warn!("conversation_demand_start（focus 後）失敗 (lane={lane}): {e}");
        }
    });
}

/// doc 38 Phase 3: session tab の × による close。remove → 一覧再取得 →
/// demand_start（除去後の新 focused の会話を replay）の順で直列に（focus 切替と同型）。
/// 最後の 1 本は backend が Err で拒否する（GUI も × は 2 本以上でしか出さない = 多重防御）。
pub(super) fn conversation_session_remove(
    ui: &mut UiState,
    boot: &Boot,
    lane: String,
    session: u32,
) {
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("conversation:session_remove skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        if let Err(e) = daemon_repo_request(
            &conn,
            &path,
            "conversation_session_remove",
            serde_json::json!({ "lane": &lane, "session": session }),
        )
        .await
        {
            // 最後の 1 本の拒否含む（Err）— 一覧はそのまま（GUI は変化なし）。
            tracing::warn!("conversation_session_remove 失敗 (lane={lane} session={session}): {e}");
            return;
        }
        // 除去後の roster / focused は snapshot が運ぶ（doc 53 §11）。
        // 除去後の新 focused の transcript replay を発火（session 省略 = focused に解決）。
        if let Err(e) = daemon_repo_request(
            &conn,
            &path,
            "conversation_demand_start",
            serde_json::json!({ "lane": &lane }),
        )
        .await
        {
            tracing::warn!("conversation_demand_start（remove 後）失敗 (lane={lane}): {e}");
        }
    });
}

/// doc 38 Phase 2: 「+」menu の engine 選択肢を埋める agents 一覧取得。
/// 既存 + Add Sub と同じ agents_list を再利用（doc 38 §3 の作成 UX）。
pub(super) fn agents_fetch(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    req: Option<String>,
) {
    let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &lane) else {
        tracing::warn!("conversation:agents_fetch skip — lane の repo 解決失敗 (lane={lane})");
        return;
    };
    let proxy = async_action_proxy.clone();
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        match daemon_repo_request(&conn, &path, "agents_list", serde_json::json!({})).await {
            Ok(payload) => {
                // doc 47 §6: 要求元の相関 id をそのまま応答へ載せ替える。
                let _ = proxy.send_event(AppEvent::Agents { lane, payload, req });
            }
            Err(e) => {
                tracing::warn!("conversation:agents_fetch の agents_list 失敗 (lane={lane}): {e}")
            }
        }
    });
}

/// doc 38 Phase 2: agents_list の結果を「+」menu へ push back。
/// doc 47 §6: 第 3 引数 = 要求元の相関 id。共有 bus の購読側はこれで振り分ける。
pub(super) fn agents(
    _ui: &mut UiState,
    boot: &Boot,
    lane: String,
    payload: serde_json::Value,
    req: Option<String>,
) {
    push_main::console_stands(&boot.webview, &lane, payload, req);
}

pub(super) fn agents_result(
    _ui: &mut UiState,
    boot: &Boot,
    repo_path: String,
    agents: Vec<crate::daemon_wire::AgentInfo>,
    error: Option<String>,
) {
    // doc 11 PR-C: + Add Sub form の dropdown を populate するための push back。
    push_sidebar::stands_result(&boot.webview, repo_path, &agents, error);
}
