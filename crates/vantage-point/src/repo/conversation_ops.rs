//! conversation ops — Unison method の handler で、owner は `lanes_state` の facade と `conversation::engine`（doc 61）。
//!
//! submit / nudge / respond / interrupt / set_permission_mode / session_*（list / create / focus / remove /
//! new_root / switch_root）/ session_set_mode / session_now / conversation_set_model。payload を剥がして
//! owner を呼び JSON を返すだけで、engine の起動と投入（`ensure_and_submit_chat`）と mode 切替
//! （`apply_session_mode`）はここで束ねる。書き込みは漏斗（doc 40 §4）。受付は `unison_server::dispatch_repo_method`。

use std::sync::Arc;

use super::state::AppState;

/// gui (doc 33): conversation プロンプト投入。
///
/// surface (vp-app) → daemon canvas channel → repo control → 本 dispatch。
/// **mode=chat が前提**（法: 1 lane 高々 1 エンジン。tui のまま submit は Err で弾き、
/// 生きた TUI を暗黙に殺さない）。engine は LanePool が lazy spawn（初回のみ）し、
/// ConversationEvent は conversation_pump 経由で `repo/conversation/data/{lane}/event` に流れる。
pub(crate) async fn handle_conversation_submit(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_submit: lane 未指定".to_string());
    }
    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if prompt.is_empty() {
        return Err("conversation_submit: prompt 未指定".to_string());
    }
    let session = super::unison_server::payload_session_key("conversation_submit", &payload)?;
    // 添付画像（chat 入力欄への貼り付け、2026-08-30）。省略・空は従来どおり text だけ。
    // ⚠️ VP は保存しない — engine に渡すだけで transcript / replay にも残さない（mako 裁定）。
    let images = parse_image_inputs(payload.get("images"));
    ensure_and_submit_chat(state, "conversation_submit", lane, session, prompt, &images).await?;
    // user 発話は pump に流れない（GUI が optimistic bubble を出す設計）ので、transcript を持たない
    // engine の session は replay 源に user turn が残らない。submit 成功後にここで記録する。
    // ⚠️ nudge（下）では書かない — claude の transcript replay が origin.kind=="human" で VP 注入を
    // 間引くのと同じ規律。harness 注入（wire delivery / delegation）は会話として再生しない対称性。
    record_user_message_if_transcriptless(state, lane, session, prompt).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// transcript を持たない engine（codex / grok / opencode）の session に、user 発話を replay log へ記録する。
///
/// claude は transcript が SSOT なので記録しない（二重化回避）。engine 解決に失敗しても submit は
/// 既に成立済みなので warn に留める（配送と replay 記録は独立系統）。tap（pump）が assistant 側を
/// 書くのと対になり、replay で user ⇄ assistant のターンが揃う。
async fn record_user_message_if_transcriptless(
    state: &AppState,
    lane: &str,
    session: Option<crate::lane::session_registry::SessionKey>,
    prompt: &str,
) {
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return;
    };
    let resolved = {
        let pool = state.lane_pool.read().await;
        pool.resolve_chat_session(&addr, session)
    };
    let Ok(resolved) = resolved else {
        return;
    };
    // 記録対象は transcript を持たない engine のみ（tap と同じ Codex|Grok|OpenCode 判定）。
    if !matches!(
        crate::conversation::EngineKind::from_agent(&resolved.agent),
        Some(
            crate::conversation::EngineKind::Codex
                | crate::conversation::EngineKind::Grok
                | crate::conversation::EngineKind::OpenCode
                | crate::conversation::EngineKind::Vpcode
        )
    ) {
        return;
    }
    let lane_label = crate::repo::agent_spawner::lane_label(&addr).to_string();
    let label = crate::lane::session_registry::session_label(&lane_label, resolved.key);
    let event = crate::conversation::ConversationEvent::UserMessage {
        text: prompt.to_string(),
    };
    if let Err(e) = crate::conversation::replay_log::append(&addr.repo, &label, &event) {
        tracing::warn!(
            "conversation replay-log: user 発話の記録に失敗（lane={lane}, session={}）: {e}",
            resolved.key
        );
    }
}

/// channel E（doc 34 §3）: wire delivery / delegation reconcile からの engine 直接注入。
///
/// `{lane, text}` — Tui の `lane_nudge`（PtySlot 直書き）の Chat 対応物。nudge 文言を 1 ターン
/// として submit する。turn 実行中でも engine 側が queue するため任意時点で呼べる
/// （doc 34 Step 0 spike ①実測）。
pub(crate) async fn handle_conversation_nudge(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_nudge: lane 未指定".to_string());
    }
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return Err("conversation_nudge: text 未指定".to_string());
    }
    // doc 39 §3-1: wire 配送は常に **root**（lane の人格）に解決する。lane 宛の nudge を
    // focused に注入すると「gui で別タブを見ている」だけで配送先が変わる誤配送になる
    // （N=1 では root=focused=1 で従来と同一挙動）。lane パース失敗は session=None のまま
    // ensure_and_submit_chat 側の同じパースが報告する（エラー文言の一元化）。
    let session = crate::repo::lanes_state::LanePool::parse_address(lane).map(|addr| {
        crate::lane::session_registry::root(
            &addr.repo,
            crate::repo::agent_spawner::lane_label(&addr),
        )
    });
    ensure_and_submit_chat(state, "conversation_nudge", lane, session, text, &[]).await?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// gui HITL (doc 35 PR1): PromptCard の回答を逆方向 `can_use_tool` へ書き戻す。
///
/// surface (vp-app) → daemon canvas channel → repo control → 本 dispatch。`request_id` は Question
/// event 由来の control_response マッチング用。allow は `{lane, request_id, answers}`、deny は
/// `{lane, request_id, behavior:"deny", message?}`。**ensure しない**（応答対象 engine 不在は Err —
/// 質問した engine が死んでいたら応答先が無い、doc §2.3）。
pub(crate) async fn handle_conversation_respond(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_respond: lane 未指定".to_string());
    }
    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err("conversation_respond: request_id 未指定".to_string());
    }
    // behavior=="deny" のみ拒否、それ以外（既定 / "allow"）は許可 + answers を運ぶ。
    let decision = if payload.get("behavior").and_then(|v| v.as_str()) == Some("deny") {
        crate::conversation::PermissionDecision::Deny {
            message: payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }
    } else {
        crate::conversation::PermissionDecision::Allow {
            answers: payload.get("answers").cloned(),
        }
    };

    let session = super::unison_server::payload_session_key("conversation_respond", &payload)?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_respond: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .respond_permission_chat(&addr, session, request_id, decision)
        .await
        .map_err(|e| format!("conversation_respond: {e}"))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// doc 35 §5: 実行中 turn の中断（stop ボタン / Esc）。`{lane}` → `LanePool::interrupt_chat`。
/// engine は turn を止めるだけでプロセスは生存し、次の submit を受けられる。
pub(crate) async fn handle_conversation_interrupt(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_interrupt: lane 未指定".to_string());
    }
    let session = super::unison_server::payload_session_key("conversation_interrupt", &payload)?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_interrupt: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .interrupt_chat(&addr, session)
        .await
        .map_err(|e| format!("conversation_interrupt: {e}"))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// doc 35 §2.5 / PR3: permission mode の動的切替。`{lane, mode}` → LanePool::set_permission_mode_chat。
pub(crate) async fn handle_conversation_set_permission_mode(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_set_permission_mode: lane 未指定".to_string());
    }
    let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    if mode.is_empty() {
        return Err("conversation_set_permission_mode: mode 未指定".to_string());
    }
    let session =
        super::unison_server::payload_session_key("conversation_set_permission_mode", &payload)?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_set_permission_mode: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .set_permission_mode_chat(&addr, session, mode)
        .await
        .map_err(|e| format!("conversation_set_permission_mode: {e}"))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// doc 38: lane の session 一覧（registry + engine 生死 + 会話 id の view）。
/// `{lane}` → `{lane, focused, sessions: [{key, agent, engine_session_id?, live, focused}]}`。
/// Phase 2 の tab strip はこれを描くだけ（UI は state を持たない）。
pub(crate) async fn handle_conversation_session_list(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_list: lane 未指定".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_list: lane パース失敗: {lane}"))?;
    let sessions = state
        .lane_pool
        .read()
        .await
        .list_chat_sessions(&addr)
        .map_err(|e| format!("conversation_session_list: {e}"))?;
    let focused = sessions.iter().find(|s| s.focused).map(|s| s.key);
    Ok(serde_json::json!({"lane": lane, "focused": focused, "sessions": sessions}))
}

/// doc 38: session を追加する（Phase 2 の chat header「+」の backend）。
/// `{lane, agent?, focus?}` → `{lane, session}`。agent 省略 = lane の agent、focus 省略 = true
/// （「+」で作った session にそのまま話しかける UX が既定）。engine は spawn しない（Draft）。
pub(crate) async fn handle_conversation_session_create(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_create: lane 未指定".to_string());
    }
    let agent = payload.get("agent").and_then(|v| v.as_str());
    let focus = payload
        .get("focus")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_create: lane パース失敗: {lane}"))?;
    let key = state
        .lane_pool
        .read()
        .await
        .create_chat_session(&addr, agent, focus)
        .map_err(|e| format!("conversation_session_create: {e}"))?;
    // doc 53 §12.4 R3c: 動詞は registry に書いた。実体を合わせるのは reconcile。
    // Chat の engine は lazy なので普通は no-op — それでも呼ぶのは「**契機は判断を持たない**」
    // ため（「今回は要らない」を動詞ごとに判断し始めると、要る場合を 1 つ取りこぼす）。
    state.reconcile_lane(&addr).await;
    // doc 53 §11: roster が変わったので知らせる（GUI の pane 一覧は snapshot 1 本で供給される）。
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": key}))
}

/// doc 38: focused session の切替。`{lane, session}`。registry 永続のみ（slot への注入 /
/// eager resume spawn は Phase 3 の attach 状態機械で束ねて実装）。
pub(crate) async fn handle_conversation_session_focus(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_focus: lane 未指定".to_string());
    }
    let session =
        super::unison_server::payload_session_key("conversation_session_focus", &payload)?
            .ok_or_else(|| "conversation_session_focus: session 未指定".to_string())?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_focus: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .focus_chat_session(&addr, session)
        .map_err(|e| format!("conversation_session_focus: {e}"))?;
    // doc 53 §12.4 R3c: focused は intent の一部（registry）。実体側は reconcile。
    state.reconcile_lane(&addr).await;
    {
        // doc 38 Phase 3（focused eager）: tab 切替 = その会話を見る宣言。新 focused の engine を
        // eager に resume spawn する（切替後の初 submit を待たない）。mode=Tui の session（registry のみの
        // 切替 = 正当）/ shell・legacy agent session（gui host なし）等は debug で飲む — 切替自体は成功。
        //
        // reconcile の**後**に置く: reconcile は「mode=Chat でない session の engine」を畳むので、
        // 先に起こすと同じ lock 区間の外で畳まれ得る。順序は「intent を合わせる → 注視に応じて
        // 起こす」（engine の eager は demand 側の判断 = reconcile の仕事ではない）。
        let mut pool = state.lane_pool.write().await;
        if let Err(e) = pool.ensure_chat_engine(&addr, Some(session), &state.topic_router) {
            tracing::debug!("conversation_session_focus: eager spawn せず（{e}）");
        }
    }
    // doc 53 §11: focused も roster の一部（chip / tab の点灯先）なので知らせる。
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session}))
}

/// doc 38 Phase 3: session を取り除く（tab を閉じる）。`{lane, session}` →
/// `{lane, session, focused}`（focused = 除去後の focus 先。GUI は list 再取得で追随）。
/// root は registry が拒否（doc 39 §6 — 最後の 1 本の拒否を包含。GUI も root タブの × を
/// 隠す = 多重防御）。lane を素に戻すのは Reset lane（fresh restart）の役目。
pub(crate) async fn handle_conversation_session_remove(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_remove: lane 未指定".to_string());
    }
    let session =
        super::unison_server::payload_session_key("conversation_session_remove", &payload)?
            .ok_or_else(|| "conversation_session_remove: session 未指定".to_string())?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_remove: lane パース失敗: {lane}"))?;
    let focused = state
        .lane_pool
        .read()
        .await
        .remove_session(&addr, session)
        .map_err(|e| format!("conversation_session_remove: {e}"))?;
    // doc 53 §12.4 R3c: registry から消えた session の実体（PtySlot / chat engine）は
    // reconcile が畳む。旧実装は動詞が種類ごとに手で畳んでいて、A6 で term pane に ✕ が
    // 出たとき **chat 側だけ畳んで PTY が孤児**になるバグを出した（doc 50 §4.6）。
    state.reconcile_lane(&addr).await;
    // ⚠️ replay の破棄は reconcile の**後**。slot が生きている間に消すと `PtySlot::drop` の
    // 最終 flush が書き戻して復活する（`restart_lane` の Reset 分岐が踏んだのと同じ罠）。
    state
        .lane_pool
        .read()
        .await
        .discard_session_traces(&addr, session);
    // doc 53 §11: session が 1 本消えた = roster の変化。
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session, "focused": focused}))
}

/// doc 39 §4: tui の ✨ New — 新 session を作って root をそれへ向ける。
/// `{lane}` → `{lane, session}`。
///
/// R3c-2: 動詞は registry に書くだけで、新 root の実体は reconcile が立てる。
/// **旧 root の pane はそのまま残る** — 旧実装は `restart_lane_orchestrated` で root slot を
/// 張り替えていたので、代表が変わるたびに前の console が消えていた（doc 53 §12.4）。
///
/// ⚠️ **現在 client からの呼び手は無い**（doc 50 §4.6 A6）。picker の「✨ 新 ID から」は
/// 「Add（Conversation を足す）+ Reborn（その場で始め直す）」の合成でしかないため撤去した。
/// この verb は **Reborn の server 側の種**として残す — Reborn は「今の session を終えて
/// 新しい session を同じ場所（Pane）で始める」操作で、root pane に適用したときの挙動が
/// ちょうどこれ（旧 root を残すか閉じるかは Reborn の設計で決める）。
/// A6 で lane 単位 mode の gate（旧「mode=Tui 限定」）は撤去済。
pub(crate) async fn handle_conversation_session_new_root(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_new_root: lane 未指定".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_new_root: lane パース失敗: {lane}"))?;
    let key = state
        .lane_pool
        .read()
        .await
        .create_root_session(&addr, payload.get("agent").and_then(|v| v.as_str()))
        .map_err(|e| format!("conversation_session_new_root: {e}"))?;
    // doc 53 §12.4 R3c-2: **旧 root の console は残る**。旧実装は restart_lane_orchestrated で
    // root slot を張り替えていたので、代表が変わるたびに前の pane が消えていた — session =
    // Pane（doc 50）の今、代表の変更は pane の破棄ではない。reconcile は新 root の実体を
    // 足すだけ（新 root は会話 id を持たないので bare で立つ）。
    state.reconcile_lane(&addr).await;
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": key}))
}

/// doc 39 P3: Root 切替 picker — root（誰が lane の代表か）を既存 session へ向け替える
/// （`{lane, session}` → `{lane, session}`）。
///
/// R3c-2: **実体には何も起きない**のが正しい — 対象 session の pane は既に在るので、
/// reconcile から見て desired は変わらない（doc 53 §12.4）。旧実装は
/// `restart_lane_orchestrated` で root slot を対象 session の会話に張り替えていた =
/// 代表の変更を化身の置き換えと混同していた。
///
/// lane 単位 mode の gate は A6 で撤去（残る制限は既知 engine のみ — `switch_root_session` 参照）。
pub(crate) async fn handle_conversation_session_switch_root(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_switch_root: lane 未指定".to_string());
    }
    let key = payload
        .get("session")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "conversation_session_switch_root: session 未指定".to_string())?
        as crate::lane::session_registry::SessionKey;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_switch_root: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .switch_root_session(&addr, key)
        .map_err(|e| format!("conversation_session_switch_root: {e}"))?;
    // doc 53 §12.4 R3c-2: 対象 session の pane は**既に在る**ので、reconcile から見て
    // desired は変わらない = 実体には何も起きないのが正しい（旧実装は root slot を対象 session の
    // 会話で張り替えていた = 代表の変更を化身の置き換えと混同していた）。呼ぶのは
    // 「契機は判断を持たない」の規律と、代表値（pid / state）の導出をやり直すため。
    state.reconcile_lane(&addr).await;
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": key}))
}

/// ensure（mode ガード + lazy spawn）→ submit（+ engine 死亡時 1 回の self-heal retry）の共通核。
///
/// `conversation_submit`（GUI 入力）と `conversation_nudge`（channel E）が共用する。`ctx` はエラー文言の
/// 前置き（呼び出し元 method 名 — 嘘ログ防止のため呼び元を正しく名乗る）。
/// payload の `images` を [`crate::conversation::ImageInput`] 列に写す（純関数）。
///
/// 形: `[{"media_type":"image/png","data":"<base64>"}, ...]`。不正な要素は**黙って落とす**
/// （1 枚の取りこぼしで投入自体を失敗させない — text は届けたい）。省略 / 非配列は空。
///
/// ⚠️ base64 の妥当性はここでは検証しない。engine（claude）が弾いた場合は Error event として
/// 会話面に出るので、VP 側で二重に検査しない（判定を 2 箇所に置かない）。
fn parse_image_inputs(raw: Option<&serde_json::Value>) -> Vec<crate::conversation::ImageInput> {
    let Some(arr) = raw.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let media_type = item.get("media_type")?.as_str()?;
            let data = item.get("data")?.as_str()?;
            if media_type.is_empty() || data.is_empty() {
                return None;
            }
            Some(crate::conversation::ImageInput {
                media_type: media_type.to_string(),
                data_base64: data.to_string(),
            })
        })
        .collect()
}

async fn ensure_and_submit_chat(
    state: &AppState,
    ctx: &str,
    lane: &str,
    session: Option<crate::lane::session_registry::SessionKey>,
    prompt: &str,
    images: &[crate::conversation::ImageInput],
) -> Result<(), String> {
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("{ctx}: lane パース失敗: {lane}"))?;

    // ensure（mode ガード + lazy spawn は LanePool = 法の番人が行う）。session=None は focused。
    state
        .lane_pool
        .write()
        .await
        .ensure_chat_engine(&addr, session, &state.topic_router)
        .map_err(|e| format!("{ctx}: {e}"))?;

    // submit（read lock — 他 lane の操作をブロックしない）。
    let submit_result = state
        .lane_pool
        .read()
        .await
        .submit_chat(&addr, session, prompt, images)
        .await;
    if let Err(e) = submit_result {
        // self-heal: engine が死んでいた場合は当該 session だけ落として 1 回だけ張り直す。
        tracing::warn!("{ctx} 失敗 → engine 再起動して retry: {e}");
        {
            let mut pool = state.lane_pool.write().await;
            pool.drop_chat_engine(&addr, session);
            pool.ensure_chat_engine(&addr, session, &state.topic_router)
                .map_err(|e| format!("{ctx}: engine 再起動失敗: {e}"))?;
        }
        state
            .lane_pool
            .read()
            .await
            .submit_chat(&addr, session, prompt, images)
            .await
            .map_err(|e| format!("{ctx} 失敗（retry 後）: {e}"))?;
    }
    Ok(())
}

/// session Mode（見え方）切替の共通実体（doc 50 §4.6 A6）。
///
/// 旧 `console_set_mode`（root 固定）と新 `session_set_mode`（session 明示）が共有する。
/// 遷移の実体（旧エンジン stop → mode 永続 → 新エンジン起立）は `LanePool::set_session_mode`。
/// 切替後の同一会話継続は cc_session `--resume` が担う。
///
/// **replay はここで撃たない** — client が新 pane を mount → topic を購読してから
/// `conversation_demand_start`（chat）/ terminal subscribe（tui）で撃つ。動詞側で撃つと購読前
/// replay の順序 race になる（非 retained topic で落ちる）。既存 `ConsoleNewSession` と同じ
/// 「購読してから demand」の規律で、Reborn ⊃ replay（更新済 transcript の再読）を保証する。
async fn apply_session_mode(
    state: &AppState,
    lane: &str,
    addr: &crate::repo::lanes_state::LaneAddress,
    session: crate::lane::session_registry::SessionKey,
    mode: crate::lane::session_registry::SessionMode,
) -> Result<serde_json::Value, String> {
    state
        .lane_pool
        .read()
        .await
        .set_session_mode(addr, session, mode)
        .map_err(|e| format!("session_set_mode 失敗: {e}"))?;
    // doc 53 §12.4 R3c: mode を書けば desired が変わる。**両方向を同じ 1 本が合わせる** —
    // tui 方向は新 PtySlot が立って pump が張られ（「vp-app が購読を跨いで維持している」lane
    // では demand が 1 のままで 0→1 hook が発火しないので、この契機が必須）、chat 方向は
    // slot が畳まれて pump 台帳 entry が撤去される。健在な兄弟 pane は pid 照合で触られない
    // （team-b 10 回目の「隣の pane の clear + 全 replay」は構造で再発しない）。
    state.reconcile_lane(addr).await;
    {
        // doc 33 §9: chat へは engine を eager spawn（切替時に resume を開始 → session_init を
        // 早く出す）。失敗しても切替自体は成功扱い（engine は次 submit で self-heal 再試行）。
        // reconcile の後（`conversation_session_focus` と同じ「intent → 注視」の順序）。
        let mut pool = state.lane_pool.write().await;
        if mode == crate::lane::session_registry::SessionMode::Gui
            && let Err(e) = pool.ensure_chat_engine(addr, Some(session), &state.topic_router)
        {
            tracing::warn!(
                "session_set_mode: eager chat engine spawn 失敗（submit で再試行）: {e}"
            );
        }
    }
    // doc 53 §11: mode は roster の一部（pane の kind を決める）ので知らせる。
    super::routes::lanes::emit_lane_update(state, addr).await;
    Ok(serde_json::json!({
        "status": "ok", "lane": lane, "session": session, "mode": mode.as_str()
    }))
}

/// doc 50 §4.6 A6: session = Pane の Mode 切替。`{lane, session, mode: "tui"|"gui"}`。
///
/// 名札の kind badge が任意 pane を切り替える経路（旧 lane 単位 `console_set_mode` の後継）。
/// session は明示必須（root 決め打ちにしない）。replay は client が新 pane 購読後に撃つ。
pub(crate) async fn handle_session_set_mode(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("session_set_mode: lane 未指定".to_string());
    }
    let session = super::unison_server::payload_session_key("session_set_mode", &payload)?
        .ok_or_else(|| "session_set_mode: session 未指定（root 決め打ちにしない）".to_string())?;
    let mode_str = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    let mode = crate::lane::session_registry::SessionMode::parse(mode_str)
        .ok_or_else(|| format!("session_set_mode: mode 不正: {mode_str:?}（tui|gui）"))?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("session_set_mode: lane パース失敗: {lane}"))?;
    apply_session_mode(state, lane, &addr, session, mode).await
}

// doc 50 §4.6 A6: 旧 `console_set_mode`（lane 単位の Mode 切替）は撤去した。見え方は session の
// 属性になり、切替は `session_set_mode {lane, session, mode}` 一本（名札 kind badge が撃つ）。
// mode / mode の二語併存を PR 後に残さないため、GUI 移行と同じ PR で消している。

/// doc 51 §1 A3b: session の「今なにを」自己申告を該当 session の conversation topic に注入する。
///
/// 発生源は AI 自身の `vp now` CLI（識別は spawn 時注入の `VP_REPO` / `VP_LANE` /
/// `VP_SESSION_KEY` env）。daemon は値を保存しない — 非 retained topic への fire-and-forget
/// （now-line は揮発。lane 行への掲揚で保持が要るのは Phase B の関心 — その時に retained 化を
/// 判断する）。session 未指定は root（lane の代表）に読み替える。
pub(crate) async fn handle_session_now(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("session_now: lane 未指定".to_string());
    }
    let text = payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err("session_now: text が空です".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("session_now: lane パース失敗: {lane}"))?;
    // ⚠️ route には **canonical を流す**（生の入力を流さない）。`vp now` は env（VP_LANE）
    // 由来の lane 文字列を運ぶため、旧世代の spawn env（`root` 等）が混ざる。parse で
    // 正規化した addr を捨てて raw を流すと、topic 上の lane が旧名のままになり、
    // 新名 key で照合する GUI の now-line と一致せず**無音で表示されない**。
    let lane = addr.canonical();
    let session = match payload.get("session").and_then(serde_json::Value::as_u64) {
        Some(s) => s as crate::lane::session_registry::SessionKey,
        None => crate::lane::session_registry::load(&addr.repo, &addr.name, "claude").root,
    };
    state
        .topic_router
        .route(crate::protocol::RepoMessage::ConversationEvent {
            lane: lane.clone(),
            session,
            event: crate::conversation::ConversationEvent::NowLine {
                text: text.to_string(),
            },
        })
        .await;
    Ok(serde_json::json!({ "ok": true, "lane": lane, "session": session }))
}

/// gui モデル切替: chat engine の `--model` を **session 単位**で切替える（mako 裁定
/// 2026-07-27 — doc 50 session=Pane で 1 lane 多 session になり、旧 `console_set_model`
/// （root slot 単位 + per-lane `engine_model` file）は旧前提として退役。記録先は registry の
/// `SessionEntry.model`）。
///
/// `{lane, session, model: string|null}`。null / 省略 = 記録を消して engine 既定に戻す。
/// spec「セッション進行中でも切り替えられる」の実体はここ — 稼働中 engine を drop して
/// `ensure_chat_engine` で即再 spawn すると、`--resume` + 新 `--model` で
/// **会話コンテキストを保ったままモデルだけ替わる**（CC の `/model` の VP 版）。
/// engine 不在（tui 中 / chat-idle）は記録のみ = 次 spawn から適用。
/// ⚠️ 進行中の turn は engine drop で切れる（UI 側は streaming 中 picker を disable して抑止）。
pub(crate) async fn handle_conversation_set_model(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_set_model: lane 未指定".to_string());
    }
    let session = super::unison_server::payload_session_key("conversation_set_model", &payload)?
        .ok_or_else(|| {
            "conversation_set_model: session 未指定（root 決め打ちにしない）".to_string()
        })?;
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(ref m) = model
        && !crate::lane::engine_model::is_valid_model(m)
    {
        return Err(format!("conversation_set_model: model 名が不正: {m:?}"));
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_set_model: lane パース失敗: {lane}"))?;

    {
        let mut pool = state.lane_pool.write().await;
        let info = pool
            .get(&addr)
            .ok_or_else(|| format!("conversation_set_model: Lane not found: {lane}"))?;
        // 切替可否は **当該 session の engine** で判定する（doc 39 P4-A の root-agent 解決の
        // session 版 — session 明示になったので root への丸めは消えた）。可否の真実は
        // `EngineKind::model_choices` の空/非空 1 本（旧 `model_switchable` 述語は catalog に
        // 畳んだ — client にも同じ catalog が LaneSessionView で届くので、UI と server の
        // 判定が同じ表から出る）。
        let lane_label = crate::repo::agent_spawner::lane_label(&addr).to_string();
        let default_agent = info.agent.clone();
        let reg = crate::lane::session_registry::load(&addr.repo, &lane_label, &default_agent);
        let entry_agent = reg
            .sessions
            .iter()
            .find(|s| s.key == session)
            .map(|s| s.agent.clone())
            .ok_or_else(|| {
                format!("conversation_set_model: session が存在しません（lane={lane}, session={session}）")
            })?;
        match crate::conversation::EngineKind::from_agent(&entry_agent) {
            Some(k) if !k.model_choices().is_empty() => {}
            Some(_) => {
                return Err(format!(
                    "{entry_agent} エンジンの model は engine 側で選択します（lane={lane}, session={session}）"
                ));
            }
            None => {
                return Err(format!(
                    "conversation_set_model は model 切替対応 engine の session のみ（lane={lane}, session={session}, agent={entry_agent}）"
                ));
            }
        }
        crate::lane::session_registry::set_model(
            &addr.repo,
            &lane_label,
            &default_agent,
            session,
            model.as_deref(),
        )
        .map_err(|e| format!("conversation_set_model: model 永続失敗: {e}"))?;
        // 稼働中 engine の入替（drop → resume 付き eager 再 spawn）。spawn 失敗しても
        // 記録は成功済みなので mode 切替と同様に成功扱い — 次 submit で self-heal される。
        // 入替は当該 session の engine のみ（chat_engines は (lane, session) の 2 段 map）。
        if pool.drop_chat_engine(&addr, Some(session))
            && let Err(e) = pool.ensure_chat_engine(&addr, Some(session), &state.topic_router)
        {
            tracing::warn!("conversation_set_model: engine 再 spawn 失敗（submit で再試行）: {e}");
        }
    }
    tracing::info!("conversation_set_model: lane={lane} session={session} model={model:?}");
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session, "model": model}))
}

#[cfg(test)]
mod tests {
    use crate::repo::state::insert_test_lane;

    /// channel E (doc 34): conversation_nudge dispatch の error 経路 4 種
    /// (lane 未指定 / text 未指定 / parse 失敗 / lane 不在)。happy path は実 engine 要のため
    /// conversation_host_roundtrip (ignored) と実機 dogfood で検証。
    #[tokio::test]
    async fn conversation_nudge_dispatch_error_paths() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // text 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        assert!(res.is_err(), "text 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "lane": "%3", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // lane 不在 (ensure_chat_engine が Lane not found)
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "lane": "vp/main", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "不在 lane への nudge は Err: {res:?}");
    }

    /// doc 35 PR1: conversation_respond dispatch の error 経路 4 種
    /// (lane 未指定 / request_id 未指定 / parse 失敗 / engine 不在)。happy path は実 engine 要のため
    /// conversation_host_question_roundtrip (ignored) と実機 dogfood で検証。
    #[tokio::test]
    async fn conversation_respond_dispatch_error_paths() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "request_id": "r1" }),
        )
        .await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // request_id 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        assert!(res.is_err(), "request_id 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "lane": "%3", "request_id": "r1" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // engine 不在 (respond_permission_chat が chat engine 未起動)。ensure しないので Err。
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "lane": "vp/main", "request_id": "r1", "answers": {} }),
        )
        .await;
        assert!(res.is_err(), "engine 不在への respond は Err: {res:?}");
    }

    /// doc 38: session registry RPC 3 本の error 経路（lane 未指定 / parse 失敗 / lane 不在 /
    /// session 未指定）。happy path は LanePool 側のテスト（lanes_state）が持つ。
    #[tokio::test]
    async fn conversation_session_rpc_dispatch_error_paths() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        for method in [
            "conversation_session_list",
            "conversation_session_create",
            "conversation_session_focus",
            "conversation_session_remove",
        ] {
            // lane 未指定
            let res = dispatch_repo_method(&state, method, serde_json::json!({})).await;
            assert!(res.is_err(), "{method}: lane 未指定は Err: {res:?}");
            // parse 失敗
            let res = dispatch_repo_method(
                &state,
                method,
                serde_json::json!({ "lane": "%3", "session": 1 }),
            )
            .await;
            assert!(res.is_err(), "{method}: parse 不能 lane は Err: {res:?}");
            // lane 不在（pool 空）
            let res = dispatch_repo_method(
                &state,
                method,
                serde_json::json!({ "lane": "vp/main", "session": 1 }),
            )
            .await;
            assert!(res.is_err(), "{method}: 不在 lane は Err: {res:?}");
        }
        // focus は session 必須。
        let res = dispatch_repo_method(
            &state,
            "conversation_session_focus",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        assert!(res.is_err(), "session 未指定の focus は Err: {res:?}");
    }

    /// **✕ の end-to-end（dispatch → 動詞 → reconcile → replay 破棄）**（doc 53 §12.4 / R3c-1）。
    ///
    /// R3c で「動詞は registry に書くだけ / 実体は reconcile が畳む」に割れたので、**配線が
    /// 繋がっているか**は handler を通してしか見えない（LanePool 単体テストは動詞と reconcile を
    /// テストが手で並べるため、本番で片方を呼び忘れても緑になる）。
    ///
    /// 併せて**順序**も固定する: `PtySlot::drop` は最終 flush で replay を disk に書き戻すので、
    /// replay 破棄が reconcile より前だと消したそばから復活する。ここでは slot に固有の目印を
    /// 出力させ、✕ の後にそれが**残っていない**ことを見る（team-b 指摘 2026-07-26）。
    #[cfg(unix)]
    #[tokio::test]
    async fn session_remove_drops_slot_and_replay_through_dispatch() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        // agent="shell" の lane + root slot（root は ✕ できないので #2 を足して閉じる）。
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "shell".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                sub_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                agent_name: None,
                flow_state: None,
            });
        }
        let res = dispatch_repo_method(
            &state,
            "lane_slot_new",
            serde_json::json!({ "lane": lane.clone() }),
        )
        .await
        .expect("lane_slot_new");
        let session = res["session"].as_u64().expect("session") as u32;

        // #2 の slot を **replay_path 付き**で立て直し、固有の目印を出力させる。
        //
        // ⚠️ ここが**順序の罠を検出できる形**にする鍵: replay を持たない test slot だと Drop が
        // 何も書かないので、`discard_session_traces` を reconcile の**前**に動かしても緑のまま
        // 通ってしまう（= 壊し方を先に決める規律。この test は逆順にすると赤くなる）。
        let replay = crate::daemon::pty_slot::replay_file_path_session(
            &addr.repo,
            crate::repo::agent_spawner::lane_label(&addr),
            session,
        );
        {
            let (slot, mut rx) = PtySlot::spawn(
                &cwd,
                "/bin/sh",
                &["-c".to_string(), "printf GHOST_SCREEN; cat".to_string()],
                &[],
                80,
                24,
                Some(replay.clone()),
            )
            .expect("PTY spawn");
            // 出力が replay buffer に入るまで待つ（空 buffer は Drop が書かない）。
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), Some(session), slot, rx);
        }

        dispatch_repo_method(
            &state,
            "conversation_session_remove",
            serde_json::json!({ "lane": lane.clone(), "session": session }),
        )
        .await
        .expect("conversation_session_remove");

        let pool = state.lane_pool.read().await;
        assert!(
            !pool.slot_sessions(&addr).contains(&session),
            "✕ で PtySlot が畳まれる（動詞 → reconcile の配線が繋がっている証拠）"
        );
        assert!(
            !crate::lane::session_registry::load("vp", "main", "shell")
                .sessions
                .iter()
                .any(|s| s.key == session),
            "registry からも消える"
        );
        // 「file が消えたか」ではなく「**旧画面が残っていないか**」を見る
        // （[[verify-the-cleanup-not-just-the-disappearance]]）。
        let ghost = std::fs::read(&replay)
            .map(|b| String::from_utf8_lossy(&b).contains("GHOST_SCREEN"))
            .unwrap_or(false);
        assert!(
            !ghost,
            "閉じた session の画面が残っている（replay 破棄が reconcile より前だと \
             PtySlot::drop の最終 flush が書き戻す）: {replay:?}"
        );
    }

    /// doc 51 §1 A3b: `session_now`（`vp now` の daemon 側）が NowLine event を該当 session の
    /// conversation topic に注入する。session は message の別 field で運ぶ（doc 38 落とし穴① —
    /// topic key は per-lane のまま）。非 retained なので subscribe が先。
    #[tokio::test]
    async fn session_now_routes_nowline_to_session_topic() {
        use crate::conversation::ConversationEvent;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let (_id, mut srx) = state
            .topic_router
            .subscribe("repo/conversation/data/vp~lane~main/event")
            .await;

        // ⚠️ 入力はあえて**旧世代の env 形**（`vp/root`）。session_now は parse で正規化した
        // canonical を route する契約なので、旧 env の agent からでも新名 topic に届くことを固定。
        let resp = dispatch_repo_method(
            &state,
            "session_now",
            serde_json::json!({ "lane": "vp/root", "session": 3, "text": "panic 箇所を特定中" }),
        )
        .await
        .expect("session_now");
        assert_eq!(resp["session"], 3);

        let (topic, msg) = tokio::time::timeout(std::time::Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(topic, "repo/conversation/data/vp~lane~main/event");
        match msg {
            RepoMessage::ConversationEvent {
                lane,
                session,
                event,
            } => {
                assert_eq!(lane, "vp/lane/main", "route には canonical が流れる");
                assert_eq!(session, 3);
                assert_eq!(
                    event,
                    ConversationEvent::NowLine {
                        text: "panic 箇所を特定中".into()
                    }
                );
            }
            other => panic!("想定外の message: {other:?}"),
        }

        // 空 text は明示エラー（無音の no-op にしない）。
        let err = dispatch_repo_method(
            &state,
            "session_now",
            serde_json::json!({ "lane": "vp/main", "text": "  " }),
        )
        .await
        .expect_err("空 text は拒否");
        assert!(err.contains("text"), "エラーが理由を運ぶ: {err}");
    }

    /// conversation_set_model の可否判定は **当該 session の agent** で決まる（旧
    /// `console_set_model` の root 決め打ちは session 明示化で退役 — doc 50 session=Pane、
    /// mako 裁定 2026-07-27）。cross-engine lane（#812）で lane agent と食い違っても、
    /// **同一 lane 内で session ごとに可否が分かれる**ことを固定する。可否の真実は
    /// `EngineKind::model_choices` の空/非空 1 本（旧 `model_switchable` 述語は catalog に畳んだ）。
    #[tokio::test]
    async fn conversation_set_model_gates_on_session_agent() {
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        // session_registry は vp_state_dir() を読む → tempdir に隔離。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;

        // sub LaneInfo を組む（chat engine 不在なので drop→ensure の engine 入替は
        // no-op — drop_chat_engine が false を返し ensure は走らない）。
        let build = |name: &str, agent: &str| LaneInfo {
            id: Default::default(),
            address: LaneAddress::sub("vp", name),
            state: LaneState::Running,
            agent: agent.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            pid: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        };

        // cross-engine lane: lane 固定 agent=codex、session 1 = codex / session 2 = claude（root）。
        crate::lane::session_registry::create_root(
            "vp",
            "mixed",
            "codex",
            "claude",
            crate::lane::session_registry::SessionMode::Tui,
        )
        .expect("claude session を root に");
        state
            .lane_pool
            .write()
            .await
            .insert(build("mixed", "codex"));
        let lane = LaneAddress::sub("vp", "mixed").to_string();

        // claude session（key=2）は catalog 非空 → 成功。永続先は **当該 session** の registry entry。
        dispatch_repo_method(
            &state,
            "conversation_set_model",
            serde_json::json!({ "lane": lane.as_str(), "session": 2, "model": "sonnet" }),
        )
        .await
        .expect("claude session は切替可（lane agent=codex に引きずられない）");
        let reg = crate::lane::session_registry::load("vp", "mixed", "codex");
        assert_eq!(
            reg.sessions
                .iter()
                .find(|s| s.key == 2)
                .and_then(|s| s.model.as_deref()),
            Some("sonnet"),
            "model が session 2 の registry entry に永続される"
        );
        assert_eq!(
            reg.sessions
                .iter()
                .find(|s| s.key == 1)
                .and_then(|s| s.model.clone()),
            None,
            "他 session は無傷（per-session — 旧 lane 単位との違いの核）"
        );

        // codex session（key=1）は catalog 空 → 拒否（同一 lane 内で session ごとに可否が分かれる）。
        let err = dispatch_repo_method(
            &state,
            "conversation_set_model",
            serde_json::json!({ "lane": lane.as_str(), "session": 1, "model": "sonnet" }),
        )
        .await
        .expect_err("codex session は拒否");
        assert!(
            err.contains("codex"),
            "拒否メッセージは session の engine(codex)を指す: {err}"
        );

        // session 未指定は Err（root 決め打ちにしない — session_set_mode と同じ規律）。
        let err = dispatch_repo_method(
            &state,
            "conversation_set_model",
            serde_json::json!({ "lane": lane.as_str(), "model": "sonnet" }),
        )
        .await
        .expect_err("session 必須");
        assert!(err.contains("session"), "{err}");
    }

    /// conversation_submit の lane / prompt 欠落は graceful Err（claude 不要）。
    #[tokio::test]
    async fn conversation_submit_missing_fields_is_graceful() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        assert!(
            dispatch_repo_method(
                &state,
                "conversation_submit",
                serde_json::json!({ "prompt": "hi" })
            )
            .await
            .is_err(),
            "lane 欠落は Err"
        );
        assert!(
            dispatch_repo_method(
                &state,
                "conversation_submit",
                serde_json::json!({ "lane": "vp/main" })
            )
            .await
            .is_err(),
            "prompt 欠落は Err"
        );
        // pool 未登録 lane への submit も graceful Err（engine spawn は起きない）。
        assert!(
            dispatch_repo_method(
                &state,
                "conversation_submit",
                serde_json::json!({ "lane": "vp/main", "prompt": "hi" })
            )
            .await
            .is_err(),
            "未登録 lane は Err"
        );
    }

    /// doc 33 の法: mode=tui の lane への conversation_submit は Err（暗黙切替しない）。
    /// claude 不要 — mode ガードは engine spawn 前に弾く。
    #[tokio::test]
    async fn conversation_submit_rejected_in_tui_mode() {
        use crate::lane::session_registry::SessionMode;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        insert_test_lane(&state, "vptest-c1-tui", SessionMode::Tui).await;
        let err = dispatch_repo_method(
            &state,
            "conversation_submit",
            serde_json::json!({ "lane": "vptest-c1-tui/main", "prompt": "hi" }),
        )
        .await
        .expect_err("tui mode は Err");
        // ⚠️ 旧 assertion は `contains("console_set_mode") || contains("mode")` で、`||` の第 2 項が
        // 何にでも当たるため **撤去済 verb を案内し続けていることを検知できなかった**（team-b review
        // 2026-07-25）。案内する verb 名そのものを固定する（動詞を rename したらここが落ちる）。
        assert!(
            err.contains("session_set_mode"),
            "現役の切替 verb を案内するメッセージであること: {err}"
        );
    }

    // doc 50 §4.6 A6: 旧 `console_set_mode_validates_and_transitions` は動詞ごと撤去した
    // （検証内容は下の `session_set_act_*` が session 単位で引き継いでいる）。

    /// doc 50 §4.6 A6: `session_set_mode` は session 明示必須で、その session の mode を切り替える。
    /// 旧 `console_set_mode`（root 固定）と同じ実体に委譲されるが、session を省略できない。
    #[tokio::test]
    async fn session_set_mode_requires_session_and_switches_that_session() {
        use crate::lane::session_registry::{self, SessionMode};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        // set_session_mode は registry（disk = vp_state_dir）を読み書きする → tempdir に隔離。
        // ⚠️ 隔離しないと実 state dir を汚染し、**2 回目以降の run で mode が既に chat のため
        // no-op 早期 return して落ちる**（= 実行順・実行回数に依存する偽の緑/赤）。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = insert_test_lane(&state, "vptest-ssa", SessionMode::Tui).await;
        let lane = "vptest-ssa/main";

        // session 省略は Err（root 決め打ちにしない = 誤配送を黙って起こさない）。
        assert!(
            dispatch_repo_method(
                &state,
                "session_set_mode",
                serde_json::json!({ "lane": lane })
            )
            .await
            .is_err(),
            "session 未指定は Err"
        );
        // mode 不正も Err。
        assert!(
            dispatch_repo_method(
                &state,
                "session_set_mode",
                serde_json::json!({ "lane": lane, "session": 1, "mode": "bogus" })
            )
            .await
            .is_err(),
            "mode 不正は Err"
        );

        // root session の tui→chat（engine-less でも registry が更新される）。
        let root = crate::repo::lanes_state::LanePool::root_session_key(&addr);
        let res = dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": root, "mode": "gui" }),
        )
        .await
        .expect("tui→chat ok");
        assert_eq!(res["mode"], "gui");
        assert_eq!(res["session"], root);
        // registry（disk SSOT）に mode が永続し、読み手（root_mode 直読）が新しい値を見る。
        // doc 53 R1: 旧 root cache は退役 — 「cache も追従する」の性質は「読み手が SSOT を
        // 直読する」に言い直された（§8.6: テストの消滅 = 性質の消滅にしない）。
        assert_eq!(
            session_registry::root_mode(&addr.repo, "main"),
            SessionMode::Gui,
            "root session の mode が registry に永続する"
        );
        assert_eq!(
            state.lane_pool.read().await.root_mode(&addr),
            SessionMode::Gui,
            "boot spawn / nudge 配送が使う読み手（pool.root_mode）が新しい mode を見る"
        );

        // 同一 mode への再切替は no-op Ok。
        dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": root, "mode": "gui" }),
        )
        .await
        .expect("chat→chat no-op ok");

        // 実在しない session は Err（registry の住人だけが切り替えられる）。
        assert!(
            dispatch_repo_method(
                &state,
                "session_set_mode",
                serde_json::json!({ "lane": lane, "session": 99, "mode": "gui" })
            )
            .await
            .is_err(),
            "実在しない session は Err"
        );
    }

    /// doc 50 §4.6 A6 ②: Chat 化の可否は **その session の agent** の能力で決まる。
    ///
    /// GUI 側 badge も同じ能力表（`chat_capable`）で gating するが、**server が最終的な門番**。
    /// root 決め打ちにしないこと（非 root は engine が違いうる — shell の console を chat に
    /// しようとしても、その session の agent で弾く）を固定する。
    #[tokio::test]
    async fn session_set_mode_gui_requires_chat_capable_agent() {
        use crate::lane::session_registry::{self, SessionMode};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = insert_test_lane(&state, "vptest-cap", SessionMode::Tui).await;
        let lane = "vptest-cap/main";

        // lane の agent は conversation（chat 可能）だが、**非 root に shell の session** を足す。
        let shell = session_registry::create(
            &addr.repo,
            "main",
            "claude",
            "shell",
            SessionMode::Tui,
            false,
        )
        .expect("shell session 作成");

        // その session を chat にしようとすると、**その session の agent（shell）**で弾かれる。
        let err = dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": shell, "mode": "gui" }),
        )
        .await
        .expect_err("shell session の chat 化は Err");
        assert!(
            err.contains("shell") || err.contains("gui"),
            "エラーは能力不足を説明する（got={err}）"
        );

        // 逆向き（chat → tui）は engine を問わず可能（tui は login shell に流し込むだけ）。
        // shell session は既に tui なので no-op Ok になることで「拒否されない」ことを示す。
        dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": shell, "mode": "tui" }),
        )
        .await
        .expect("tui 方向は engine を問わず通る");
    }

    /// 実機統合: mode=chat の lane への conversation_submit が engine を lazy spawn し、ConversationEvent が
    /// `repo/conversation/data/{lane}/event` topic に届く repo 終端 round-trip を検証する。
    /// `cargo test -p vantage-point --ignored conversation_submit_roundtrip`（要 claude CLI）。
    #[tokio::test]
    #[ignore = "requires claude CLI + subscription"]
    async fn conversation_submit_roundtrip() {
        use crate::conversation::ConversationEvent;
        use crate::lane::session_registry::SessionMode;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        // doc 33: submit には mode=chat の lane が pool に要る。
        // repo 名はテスト固有にする — 実在 repo だと registry の会話 id が本物の
        // session id を返し、temp cwd との不整合で resume が失敗する。
        insert_test_lane(&state, "vptest-c1-rt", SessionMode::Gui).await;
        // conversation data は非 retained なので submit 前に subscribe。
        let (_id, mut srx) = state
            .topic_router
            .subscribe("repo/conversation/data/vptest-c1-rt~main/event")
            .await;

        dispatch_repo_method(
            &state,
            "conversation_submit",
            serde_json::json!({ "lane": "vptest-c1-rt/main", "prompt": "Reply with exactly: PONG" }),
        )
        .await
        .expect("conversation_submit ok");

        let mut got_init = false;
        let mut text = String::new();
        let mut got_done = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(90), srx.recv()).await {
                Ok(Some((_topic, RepoMessage::ConversationEvent { event, .. }))) => match event {
                    ConversationEvent::SessionInit { .. } => got_init = true,
                    ConversationEvent::MessageChunk { text: t } => text.push_str(&t),
                    ConversationEvent::TurnCompleted { .. } => {
                        got_done = true;
                        break;
                    }
                    ConversationEvent::Error { message } => panic!("engine error: {message}"),
                    _ => {}
                },
                _ => break,
            }
        }

        assert!(got_init, "SessionInit が topic に届く");
        assert!(got_done, "TurnCompleted が topic に届く");
        assert!(text.to_uppercase().contains("PONG"), "本文 PONG: {text:?}");
    }
}
