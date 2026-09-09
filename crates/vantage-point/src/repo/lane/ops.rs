//! lane ops — lane 系 Unison method の handler（owner は `lane/lifecycle` / `host/ledger` / `lane/session_registry`、doc 61）。
//!
//! nudge / slots / slot_new / capture / delete / restart / session_changed / create / origin_get / origin_set /
//! order_set / lanes_list。payload を剥がして owner を呼び JSON を返す。`lane_origin_set` / `lane_order_set` は
//! ledger 書き → `system_event_tx` の順、`lane_session_changed` は record → emit の順を保つ（doc 61 §3）。
//! 受付は `unison_server::dispatch_repo_method`。

use std::sync::Arc;

use crate::repo::state::AppState;

/// tmux decoupling PR1: lane nudge。 論理 lane address 宛に literal text + Enter を PtySlot へ書く。
///
/// 旧制御面 (`tmux send-keys -t <session>`) の repo-proxy 置換。 daemon (delivery/reconcile
/// loop の re-nudge) / CLI (`vp flow handoff`) / MCP (`flow_handoff`) が control channel 経由で
/// この method を ask する。 repo-local な `AppState::nudge_lane` は同じ `deliver_nudge` sink を
/// in-process で呼ぶ (text→Enter の submit 意味論は `deliver_nudge` に集約)。
pub(crate) async fn handle_lane_nudge(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_nudge: lane 未指定".to_string());
    }
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let Some(addr) = crate::repo::lane::LanePool::parse_address(lane) else {
        return Err(format!("lane_nudge: lane パース失敗: {}", lane));
    };
    // doc 46 P5: `session` 省略 = root（mailbox を名乗る住人）。明示指定で同居する別 slot に届く。
    let session = crate::repo::unison_server::payload_session_key("lane_nudge", &payload)?;
    crate::repo::lane::deliver_nudge(&state.lane_pool, &addr, session, text)
        .await
        .map_err(|e| format!("lane_nudge 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session}))
}

/// doc 46 P5: lane が持つ **PTY slot の一覧**（session / pid / 生死 / root か / attach 有無）。
///
/// slot は lane に 1 枚ではなく session ごとになった。表示は当面ミニマム（1 枚ずつ）なので、
/// **UI を通さずに枚数と中身を読む口**をここに置く（doc 47 §7 成立条件② — 「読み手のない
/// 書き込み」を作らない）。CLI `vp lane slots` がこの method を ask する。
pub(crate) async fn handle_lane_slots(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_slots: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lane::LanePool::parse_address(lane) else {
        return Err(format!("lane_slots: lane パース失敗: {}", lane));
    };
    let pool = state.lane_pool.read().await;
    if pool.get(&addr).is_none() {
        return Err(format!("lane_slots: lane 不在: {lane}"));
    }
    let slots = pool.slot_inventory(&addr);
    Ok(serde_json::json!({
        "status": "ok",
        "lane": lane,
        "count": slots.len(),
        "slots": slots,
    }))
}

/// doc 46 P5 **producer**: 新しい console（slot）を 1 枚立てる。
/// `{lane, agent?}` → `{status, lane, session, pid, count}`。
///
/// - 常に **新しい session** を採番してそこに slot を立てる（doc 46 §1.5「Pane は必ず新しい
///   session id で始まる」= session ↔ Pane 1:1）。既存 session の open は持たない
/// - `agent` 省略 = 現 root の engine を引き継ぐ（doc 46 P2 の「Engine を選んで新コンソール」の
///   tui 版。`conversation_session_create` は Mode=Chat 固定なのでそちらでは作れない）
/// - **root / focused は動かさない** — mailbox も pid も Dead 判定も root のまま（doc 40 §4-1）
///
/// pump は動詞の末尾の reconcile が demand（購読者の有無）に応じて張る（doc 53 R2 —
/// 旧「GUI 配線は張らない」は A6 の pump wiring 追加以降コードと矛盾していた、moody 指摘）。
/// 購読者不在の CLI 運用では pump なしのまま `vp lane slots` / `vp lane capture --session` /
/// `vp lane nudge --session` で読み書きする（capture は TermAttach 直読で topic 非依存）。
pub(crate) async fn handle_lane_slot_new(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_slot_new: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lane::LanePool::parse_address(lane) else {
        return Err(format!("lane_slot_new: lane パース失敗: {}", lane));
    };
    let agent = payload.get("agent").and_then(|v| v.as_str());
    let session = state
        .lane_pool
        .read()
        .await
        .create_console_session(&addr, agent)
        .map_err(|e| format!("lane_slot_new: {e}"))?;
    // doc 53 §12.4 R3c: slot を立てるのは reconcile（desired = mode=Tui の session）。
    //
    // ⚠️ pump もこの中で合う。これが無いと新 session の PTY 出力が誰にも届かない: client 側の
    // terminal 購読は **lane 単位で 1 本**（`terminal_sessions` は lane key）なので、root が既に
    // tui で購読済の lane に 2 枚目を足すと購読者数は 1→1 のまま = demand hook のエッジが
    // 立たない → 入力は通る（terminal_write は直送）のに**出力だけ永久に沈黙**する。
    // 「lane 単位のハンドルが session の増加を捉えない」= 制約撤廃の随伴（doc 50 §4.7）の一族。
    state.reconcile_lane(&addr).await;
    // pid は**導出**する（動詞の戻り値ではなくなった）。spawn に失敗していれば None =
    // 「intent はあるが立っていない」の観測値そのもの（doc 53 §12.2 — 巻き戻さない）。
    let (pid, count) = {
        let pool = state.lane_pool.read().await;
        let pid = pool
            .slot_pids(&addr)
            .into_iter()
            .find(|(k, _)| *k == session)
            .map(|(_, p)| p);
        (pid, pool.slot_sessions(&addr).len())
    };
    // doc 53 §11: **本バグの当事者** — CLI / MCP から console を足しても、これが無いと
    // GUI の roster に出ない（GUI 自身の動詞しか fetch の契機にならなかった）。
    super::lifecycle::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({
        "status": "ok",
        "lane": lane,
        "session": session,
        "pid": pid,
        "count": count,
    }))
}

/// tmux decoupling: lane console capture。 lane の Term grid（TermAttach）を text で返す。
///
/// 旧 `tmux capture-pane`（`handle_tmux_capture`）の native 代替 — main が sub の
/// console を読む dev-flow 用途。 CLI `vp lane capture` / 将来の MCP がこの method を ask する。
pub(crate) async fn handle_lane_capture(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_capture: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lane::LanePool::parse_address(lane) else {
        return Err(format!("lane_capture: lane パース失敗: {}", lane));
    };
    // doc 46 P5: `session` 省略 = root（lane の代表 slot）。明示指定で同居する別 slot を読む。
    let session = crate::repo::unison_server::payload_session_key("lane_capture", &payload)?;
    let pool = state.lane_pool.read().await;
    let content = match pool.capture_lane(&addr, session) {
        Some(c) => c,
        None => {
            // capture 不能の理由を分岐して UX 混乱を減らす（dogfood 2026-07-19: chat mode lane で
            // 一律「lane 不在 or console 未配線」に混乱した）。chat lane は term_attach 無しが
            // 正常なので、pool に実在して root の mode（registry 直読、doc 53 R1）が Chat なら
            // 「tui に切り替えよ」と案内する。
            // doc 46 P5: slot が複数枚になったので「その lane には何枚あるか」も添える
            // （--session の指し先が無い時に、存在する session key が判る）。
            let available = pool.slot_sessions(&addr);
            let msg = match pool.get(&addr) {
                Some(_) if session.is_some_and(|k| !available.contains(&k)) => format!(
                    "lane_capture: 指定 session に console はありません（session={}, この lane の slot: {:?}）: {lane}",
                    session.unwrap_or(0),
                    available
                ),
                Some(_)
                    if pool.root_mode(&addr) == crate::lane::session_registry::SessionMode::Gui =>
                {
                    format!(
                        "lane_capture: chat mode の lane に console はありません（tui に切り替えると capture できます）: {lane}"
                    )
                }
                Some(_) => format!("lane_capture: console 未配線: {lane}"),
                None => format!("lane_capture: lane 不在: {lane}"),
            };
            return Err(msg);
        }
    };
    Ok(serde_json::json!({
        "status": "ok",
        "lane": lane,
        "session": session,
        "slots": pool.slot_sessions(&addr),
        "content": content,
    }))
}

/// F6② (doc 27 §3.4.5/§6): Lane delete。 旧 SP HTTP `DELETE /api/lanes` を repo-proxy ask に
/// 移管（surface→repo 直結 HTTP を撤去、 daemon 経由の ask に統一）。 logic は旧 `delete_handler`
/// から移設し、 core の `delete_lane_orchestrated` を再利用（HTTP route + handler は削除）。
pub(crate) async fn handle_lane_delete(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_delete: address 必須")?;
    // cleanup default = true (旧 DeleteLaneQuery default_cleanup と一致、 dir も rm する)。
    let cleanup = payload
        .get("cleanup")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let addr = crate::repo::lane::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_delete: invalid lane address: {}", address))?;
    match super::lifecycle::delete_lane_orchestrated(state, addr, cleanup).await {
        Ok(info) => Ok(serde_json::json!({
            "deleted": info.address,
            "pid": info.pid,
            "cleanup": info.cleanup_status,
        })),
        Err(e) => Err(e.to_string()),
    }
}

/// F6③ (doc 27 §3.4.5/§6): Lane restart。 旧 SP HTTP `POST /api/lanes/restart` を repo-proxy
/// ask に移管。 core の `restart_lane_orchestrated` (VP-131 透過 retry loop) を呼ぶ薄い adapter。
pub(crate) async fn handle_lane_restart(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_restart: address 必須")?;
    // fresh default=false (旧 RestartLaneQuery の #[serde(default)] と一致)。wire は bool のまま
    // （fresh=true = Reset lane / false = 会話を継ぐ）。
    //
    // R3c-2: server 側では `RespawnMode` ではなく**別の動詞**に分かれた（restart = 実体だけ
    // 捨てる / Reset = intent ごと素に戻す）。wire の bool は互換のため維持 — client の語彙を
    // 変える話は doc 54 の schema 束で扱う。
    let fresh = payload
        .get("fresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let addr = crate::repo::lane::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_restart: invalid lane address: {}", address))?;
    if fresh {
        super::lifecycle::reset_lane_orchestrated(state, addr).await
    } else {
        super::lifecycle::restart_lane_orchestrated(state, addr).await
    }
}

/// 供給 push 根治（session chip 凍結、2026-07-17）: engine session pointer の変化通知。
///
/// pointer（cc_sessions 等の state file）の書き手は claude の UserPromptSubmit hook で、
/// repo プロセスの外にいる — repo は file を「読みに行った時だけ」変化を知る（ask 経路は正しく、
/// push 経路に変化イベントが存在しなかった）。hook → Daemon "wire" channel
/// (`lane/session-changed`) → 本 method で repo に届き、repo が focused session 規則で真値を
/// re-enrich して `Diff::Update` を emit する（daemon は routing のみ、真実源は repo のまま）。
///
/// doc 40 §4 / doc 46 P5: payload の `session` は**報告者が名乗った session**。会話 id は
/// その session に記録される（root 固定ではない）— 同じ lane に複数の console slot が
/// 同居しても、同居人の報告が root の `--resume` を壊さない。
pub(crate) async fn handle_lane_session_changed(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_session_changed: lane 必須".to_string());
    }
    let addr = crate::repo::lane::LanePool::parse_address(lane)
        .ok_or_else(|| format!("lane_session_changed: invalid lane address: {lane}"))?;
    let Some(agent) = state
        .lane_pool
        .read()
        .await
        .get(&addr)
        .map(|l| l.agent.clone())
    else {
        return Err(format!("lane_session_changed: lane が存在しません: {lane}"));
    };
    // doc 40 §4/§6: hook の会話報告（session_id + event + 報告者が名乗る session）を
    // **報告された session** に適用する — policy（宛先解決 + F1/F2 guard）の唯一の実装点は
    // record_conversation。session_id 無し = 旧 hook / 旧 daemon からの「変化通知のみ」
    // （従来互換、enrich だけ行う）。
    if let Some(sid) = payload.get("session_id").and_then(|v| v.as_str()) {
        use crate::lane::session_registry::{ConversationReport, ReportTarget, ReportTrigger};
        let trigger = match payload.get("event").and_then(|v| v.as_str()) {
            Some("issued") => ReportTrigger::Issued,
            _ => ReportTrigger::Spoken,
        };
        // `session` 不在 = 報告者が名乗らなかった（VP_SESSION_KEY 無しで spawn 済の slot /
        // VP 外起動）→ 後方互換で root 宛。**ここで root に丸めない**（Unspecified のまま
        // 渡す）ことで、実在しない session の報告が root に化けるのを registry 側が拒める。
        let target = match crate::repo::unison_server::payload_session_key(
            "lane_session_changed",
            &payload,
        )? {
            Some(key) => ReportTarget::Session(key),
            None => ReportTarget::Unspecified,
        };
        let report = ConversationReport {
            target,
            conversation: sid,
            trigger,
        };
        let lane_label = crate::repo::agent_spawner::lane_label(&addr);
        match crate::lane::session_registry::record_conversation(
            &addr.repo, lane_label, &agent, report,
        ) {
            Ok(outcome) => {
                tracing::info!(
                    "conversation report: addr={addr} report={report:?} outcome={outcome:?}"
                );
            }
            Err(e) => {
                tracing::warn!("conversation report 適用失敗: addr={addr} err={e}");
            }
        }
    }
    super::lifecycle::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({ "status": "ok", "lane": lane }))
}

/// lanes portless (doc 27 §3.4.5): Lane create。 旧 SP HTTP `POST /api/lanes` を repo-proxy ask に
/// 移管。 core の `create_sub_orchestrated` (lane clone + PtySlot spawn) を呼ぶ薄い adapter。
/// payload は `CreateLaneReq` 互換 JSON (kind/name/agent?/cwd?/branch?/base?)。 成功は LaneInfo JSON、
/// 失敗は core が返す String error (旧 HTTP の CONFLICT="already exists" 等を保持)。
pub(crate) async fn handle_lane_create(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: super::lifecycle::CreateLaneReq = serde_json::from_value(payload)
        .map_err(|e| format!("lane_create: invalid payload: {}", e))?;
    let info = super::lifecycle::create_sub_orchestrated(state, req).await?;
    serde_json::to_value(&info).map_err(|e| format!("lane_create: LaneInfo serialize 失敗: {}", e))
}

/// 帳簿が起点を解決するための lane 一覧（id と表示名の対だけ）。
///
/// `LaneInfo` 全体ではなく [`crate::host::ledger::LaneRef`] に落とすのは、帳簿が lane の
/// 中身に依存しないため（`host::farewell` が git を知らないのと同じ切り方）。
async fn ledger_lane_refs(state: &Arc<AppState>) -> Vec<crate::host::ledger::LaneRef> {
    state
        .lane_pool
        .read()
        .await
        .list()
        .into_iter()
        .map(|l| crate::host::ledger::LaneRef::new(l.id.to_string(), l.address.name))
        .collect()
}

/// doc 44 D4: 帳簿から開発起点を読む。応答は [`crate::host::ledger::Origin`] の JSON。
///
/// 未設定 / dangling でも error にせず、**どう決まったか**を `source` で返す
/// （起点が読めないだけで呼び出し側が止まる方が困る）。
pub(crate) async fn handle_lane_origin_get(
    state: &Arc<AppState>,
) -> Result<serde_json::Value, String> {
    let lanes = ledger_lane_refs(state).await;
    let origin = crate::host::ledger::origin(state.vpdb.as_ref(), &state.repo_dir, &lanes).await;
    serde_json::to_value(&origin).map_err(|e| format!("lane_origin_get: serialize 失敗: {e}"))
}

/// doc 44 D4: 開発起点を設定する。payload = `{ "lane": "<lane 名>" }`。
///
/// 人が打つのは名前、帳簿に入るのは `lane_id` — 変換は
/// [`crate::host::ledger::set_origin`] が境界で 1 回だけ行う。
/// D5 の通り **何も動かさない**（cwd も active lane も変えない、ポインタの書き換えだけ）。
pub(crate) async fn handle_lane_origin_set(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_origin_set: lane 必須".to_string());
    }
    let lanes = ledger_lane_refs(state).await;
    crate::host::ledger::set_origin(state.vpdb.as_ref(), &state.repo_dir, lane, &lanes).await?;
    // 起点は snapshot の `origin` に載るので、投影が変わった = 即 publish する。
    // これが無いと 5s periodic tick まで sidebar の star が動かず「押しても無反応」に見える。
    let _ = state
        .system_event_tx
        .send(crate::repo::lane::SystemEvent::LanesProjectionChanged);
    let origin = crate::host::ledger::origin(state.vpdb.as_ref(), &state.repo_dir, &lanes).await;
    serde_json::to_value(&origin).map_err(|e| format!("lane_origin_set: serialize 失敗: {e}"))
}

/// doc 44 §12: lane の並び順を帳簿に保存する。payload = `{ "order": ["<lane 名>", ...] }`。
///
/// 起点と同じく、人が触るのは名前で帳簿に入るのは `lane_id`。保存後の反映は
/// 次の lanes snapshot に載って戻る（`build_lanes_snapshot` が帳簿の順で並べる）。
pub(crate) async fn handle_lane_order_set(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let order: Vec<String> = payload
        .get("order")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if order.is_empty() {
        return Err("lane_order_set: order 必須".to_string());
    }
    let lanes = ledger_lane_refs(state).await;
    crate::host::ledger::set_lane_order(state.vpdb.as_ref(), &state.repo_dir, &order, &lanes)
        .await?;
    // 並び順が変わった = snapshot が変わるので、publish して vp-app を起こす
    // （doc 44 §11 の指紋は lanes の並びも含むため、次の publish で必ず届く）。
    let _ = state
        .system_event_tx
        .send(crate::repo::lane::SystemEvent::LanesProjectionChanged);
    Ok(serde_json::json!({ "status": "ok", "count": order.len() }))
}

/// lanes portless (doc 27 §3.4.5): Lane list。 旧 SP HTTP `GET /api/lanes` を repo-proxy ask に
/// 移管。 core の `build_lanes_snapshot` を呼び `{lanes:[...]}` で wrap (旧 HTTP `LanesResponse` 互換)。
pub(crate) async fn handle_lanes_list(state: &Arc<AppState>) -> Result<serde_json::Value, String> {
    let lanes = super::lifecycle::build_lanes_snapshot(state).await;
    // doc 46 P5: slot は lane に 1 枚ではなく session ごとになった。`vp lane ls --detail` から
    // 枚数が見えるよう、lane ごとの slot session key を snapshot に添える（LaneInfo 自体には
    // 足さない — descriptor は帳簿の永続形で、slot は in-memory な runtime 事実だから。
    // 混ぜると「再起動で復元されるべき値」に見えてしまう）。
    let pool = state.lane_pool.read().await;
    let lanes: Vec<serde_json::Value> = lanes
        .iter()
        .map(|lane| {
            let mut v = serde_json::to_value(lane).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "slots".to_string(),
                    serde_json::json!(pool.slot_sessions(&lane.address)),
                );
            }
            v
        })
        .collect();
    Ok(serde_json::json!({ "lanes": lanes }))
}

#[cfg(test)]
mod tests {
    use crate::repo::state::default_test_shell;

    /// tmux decoupling PR1-2: lane_nudge dispatch の error 経路 3 種
    /// (lane 未指定 / parse 失敗 / lane 不在 = PtySlot 無)。 happy path は実機検証済 (design §13.6)。
    #[tokio::test]
    async fn lane_nudge_dispatch_error_paths() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res =
            dispatch_repo_method(&state, "lane_nudge", serde_json::json!({ "text": "x" })).await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_repo_method(
            &state,
            "lane_nudge",
            serde_json::json!({ "lane": "%3", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // lane 不在 (PtySlot 無)
        let res = dispatch_repo_method(
            &state,
            "lane_nudge",
            serde_json::json!({ "lane": "vp/main", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への nudge は Err: {res:?}");
    }

    /// tmux decoupling PR2 → capture error 明確化（2026-07-19）: lane_capture dispatch の error 経路。
    /// 未指定 / parse 不能 / pool 不在（lane 不在）/ chat mode lane（console 無しが正常）を分岐して返す。
    #[tokio::test]
    async fn lane_capture_dispatch_error_paths() {
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        // doc 53 R1: chat 案内の分岐が registry（root_mode）直読になったため、state dir を
        // 隔離して registry に書く（隔離しないと実 state を読み書きしてしまう）。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(&state, "lane_capture", serde_json::json!({})).await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": "some-label" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");

        // pool に実在しない lane = 「lane 不在」。
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        let err = res.expect_err("pool 不在 lane の capture は Err");
        assert!(
            err.contains("lane 不在"),
            "pool 不在は『lane 不在』を返す: {err}"
        );

        // pool に実在して root mode==Chat の lane = 「chat mode の lane に console はありません」。
        // chat lane は term_attach を持たないので capture_lane は None だが、これは正常状態。
        // mode は registry（SSOT）に書く — 案内分岐の読み手（root_mode 直読）と同じ経路を通す。
        let addr = LaneAddress::sub("vp", "chat-x");
        crate::lane::session_registry::set_root_mode(
            "vp",
            "chat-x",
            "claude",
            crate::lane::session_registry::SessionMode::Gui,
        )
        .expect("test registry へ root mode を書けること");
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "claude".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: std::env::temp_dir().to_string_lossy().to_string(),
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
            "lane_capture",
            serde_json::json!({ "lane": addr.to_string() }),
        )
        .await;
        let err = res.expect_err("chat mode lane の capture は Err");
        assert!(
            err.contains("chat mode"),
            "chat mode lane は専用メッセージを返す: {err}"
        );
    }

    /// doc 46 P5: slot が (lane, session) key になったので、**UI を通さずに枚数と中身を読む口**を
    /// 用意した（doc 47 §7 成立条件② — 「読み手のない書き込み」を作らない）。
    /// `lane_slots` の一覧と、`lane_capture --session` の指し先不在エラーを固定する。
    #[cfg(unix)]
    #[tokio::test]
    async fn lane_slots_lists_every_session_slot() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        // slot_inventory は root を registry から解決する → tempdir に隔離。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;

        let addr = LaneAddress::root("vp");
        let res = dispatch_repo_method(
            &state,
            "lane_slots",
            serde_json::json!({ "lane": addr.to_string() }),
        )
        .await;
        assert!(
            res.expect_err("pool 不在 lane は Err")
                .contains("lane 不在"),
            "pool に居ない lane は『lane 不在』"
        );

        let cwd = std::env::temp_dir().to_string_lossy().to_string();
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
            for key in [1u32, 2] {
                let (slot, rx) = PtySlot::spawn(
                    &cwd,
                    "/bin/sh",
                    &["-c".to_string(), "cat".to_string()],
                    &[],
                    80,
                    24,
                    None,
                )
                .expect("PTY spawn");
                pool.insert_pty_slot(addr.clone(), Some(key), slot, rx);
            }
        }

        let res = dispatch_repo_method(
            &state,
            "lane_slots",
            serde_json::json!({ "lane": addr.to_string() }),
        )
        .await
        .expect("lane_slots");
        assert_eq!(res["count"], 2, "同居する slot の枚数が読める: {res}");
        assert_eq!(res["slots"][0]["session"], 1);
        assert_eq!(
            res["slots"][0]["root"], true,
            "#1 が root（registry 既定形）"
        );
        assert_eq!(res["slots"][1]["session"], 2);
        assert_eq!(res["slots"][1]["root"], false);

        // capture は session 指定で slot を選べる。応答に slots を添えるので、
        // 「今どれを読んだか」「他に何枚あるか」が CLI から判る。
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": addr.to_string(), "session": 2 }),
        )
        .await
        .expect("capture #2");
        assert_eq!(res["session"], 2);
        assert_eq!(res["slots"], serde_json::json!([1, 2]));

        // 指し先が無い session は、存在する slot を添えて Err（探し方が判るエラー）。
        let err = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": addr.to_string(), "session": 9 }),
        )
        .await
        .expect_err("不在 session の capture は Err");
        assert!(
            err.contains("この lane の slot") && err.contains("[1, 2]"),
            "存在する slot を案内する: {err}"
        );
    }

    /// doc 46 P5 producer の end-to-end（RPC → CLI が見る形）: `lane_slot_new` で立てた console が
    /// `lane_slots` に **2 枚目として出る**こと。#854 が用意した容量に production の書き手が
    /// 付いたことの証跡（「読み手のない書き込み」の逆 — 読み手は先にあり、書き手が来た）。
    #[cfg(unix)]
    #[tokio::test]
    async fn lane_slot_new_adds_a_console_visible_in_lane_slots() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        // session registry / slot_inventory の root 解決は vp_state_dir() を読む → tempdir に隔離。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string();

        // lane 不在は Err（他の lane_* dispatch と同じ入口検査）。
        for payload in [
            serde_json::json!({}),
            serde_json::json!({ "lane": "%3" }),
            serde_json::json!({ "lane": lane.clone() }),
        ] {
            let res = dispatch_repo_method(&state, "lane_slot_new", payload.clone()).await;
            assert!(res.is_err(), "入口検査: {payload} は Err: {res:?}");
        }

        // agent="shell" の lane（console に engine を注入しない）+ 既存の root slot。
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
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
            let (slot, rx) = PtySlot::spawn(
                &cwd,
                "/bin/sh",
                &["-c".to_string(), "cat".to_string()],
                &[],
                80,
                24,
                None,
            )
            .expect("PTY spawn");
            pool.insert_pty_slot(addr.clone(), Some(1), slot, rx);
        }

        // agent 省略 = 現 root の engine を引き継ぐ（registry 不在 = lane agent の "shell"）。
        let res = dispatch_repo_method(
            &state,
            "lane_slot_new",
            serde_json::json!({ "lane": lane.clone() }),
        )
        .await
        .expect("lane_slot_new");
        assert_eq!(res["session"], 2, "新 session を採番して立てる: {res}");
        assert_eq!(res["count"], 2, "この lane の console は 2 枚に: {res}");

        let res = dispatch_repo_method(
            &state,
            "lane_slots",
            serde_json::json!({ "lane": lane.clone() }),
        )
        .await
        .expect("lane_slots");
        assert_eq!(res["count"], 2, "`vp lane slots` に 2 枚出る: {res}");
        assert_eq!(res["slots"][1]["session"], 2);
        assert_eq!(res["slots"][1]["root"], false, "同居人であって代表ではない");
        assert_eq!(res["slots"][1]["alive"], true);

        // 立てた console は `vp lane capture --session 2` で読める（UI を通さない読み手）。
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": lane, "session": 2 }),
        )
        .await
        .expect("capture #2");
        assert_eq!(res["session"], 2);
    }

    /// F6②: lane_delete dispatch e2e — sub lane を pool に作り、 lane_delete で除去できる。
    /// 二度目の delete は LaneNotFound で Err (= idempotent re-call の契約)。 Err message が
    /// "Lane not found" を含むことも固定する (MCP/CLI の idempotent 判定がこの文字列に依存)。
    #[tokio::test]
    async fn lane_delete_removes_sub_and_idempotent() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "chore");
        let address = addr.to_string();

        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            let mut pool = state.lane_pool.write().await;
            // delete は lanes map (LaneInfo) を remove するので LaneInfo + PtySlot 両方を登録する。
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "claude".to_string(),
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
            pool.insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // cleanup=false: test lane に実 workspace dir はないので Phase 2b (fs rm) をスキップ。
        let res = dispatch_repo_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect("lane_delete");
        assert_eq!(res["deleted"], address);

        // pool から PtySlot が消えている (subscribe_output が None)。
        assert!(
            state
                .lane_pool
                .read()
                .await
                .subscribe_output(&addr, None)
                .is_none(),
            "lane_delete 後も PtySlot が pool に残っている"
        );

        // 二度目の delete は LaneNotFound で Err (idempotent re-call の契約)。
        let err = dispatch_repo_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect_err("既に消えた lane の delete は Err (LaneNotFound)");
        assert!(
            err.contains("Lane not found"),
            "Err message に LaneNotFound が含まれる (MCP/CLI の idempotent 判定が依存): {err}"
        );
    }

    /// F6②: Main lane は lane_delete で拒否される (architecture rule: repo lifetime 紐付き)。
    #[tokio::test]
    async fn lane_delete_rejects_main() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        // delete_lane_orchestrated は LanePool の有無に関係なく kind=Main を最初に弾く。
        let err = dispatch_repo_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": "vp/main" }),
        )
        .await
        .expect_err("Main の delete は Err");
        assert!(
            err.contains("Main"),
            "Main delete は MainCannotBeDeleted: {err}"
        );
    }

    /// F6③: lane_restart dispatch — 存在しない lane の restart は透過 retry (3 attempts) 後 Err。
    /// dispatch 配線 + restart_lane_orchestrated 到達を確認 (respawn 成功 path は実機検証で担保)。
    #[tokio::test]
    async fn lane_restart_unknown_lane_errs() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(
            &state,
            "lane_restart",
            serde_json::json!({ "address": "vp/sub/ghost" }),
        )
        .await;
        assert!(res.is_err(), "存在しない lane の restart は Err");
    }

    /// 供給 push 根治: 存在しない lane の session 変化通知は Err（黙って成功にしない）。
    #[tokio::test]
    async fn lane_session_changed_unknown_lane_errs() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({ "lane": "vp/sub/ghost" }),
        )
        .await;
        assert!(res.is_err(), "存在しない lane の session 変化通知は Err");
    }

    /// 供給 push 根治: `lane_session_changed` が `Diff::Update` を emit し、payload の
    /// engine_session_id が state file の現値（focused session 規則の re-enrich）を映す。
    /// これが Daemon lane_registry / vp-app header を追従させる push の起点になる。
    #[tokio::test]
    async fn lane_session_changed_emits_enriched_lane_update() {
        use crate::repo::lane::state::Diff;
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState, SystemEvent};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        // refresh_engine_session_id は vp_state_dir() を読む — tempdir guard で隔離。
        let state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        state.lane_pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: LaneAddress::root("vp"),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-07-17T00:00:00Z".to_string(),
            pid: Some(1),
            cwd: state_dir.path().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        });
        // hook 相当の会話 id 記録（記録契機 UserPromptSubmit の後の状態）。doc 40: SSOT は registry。
        crate::lane::session_registry::set_conversation("vp", "main", "claude", 1, Some("sid-new"))
            .expect("record conversation");

        let mut rx = state.system_event_tx.subscribe();
        let res = dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await
        .expect("lane_session_changed ok");
        assert_eq!(res["status"], "ok");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("Diff::Update が 1s 以内に届く")
            .expect("broadcast recv");
        match event {
            SystemEvent::Lane(Diff::Update { payload }) => {
                assert_eq!(payload.address, LaneAddress::root("vp"));
                assert_eq!(
                    payload.engine_session_id.as_deref(),
                    Some("sid-new"),
                    "emit 時に state file の現値で re-enrich される"
                );
            }
            other => panic!("expected Diff::Update, got: {other:?}"),
        }
    }

    /// doc 40 §4/§6: hook の会話報告（session_id + event 付き payload）が root session の
    /// registry に記録され（旧 store への直書きは発生しない = 漏斗一本化）、Diff::Update が
    /// 新 id と sessions snapshot を運ぶ。eager（issued）でも fresh な root には即記録される
    /// = 「発行時点で chip が点く」の配線検証。
    #[tokio::test]
    async fn lane_session_changed_records_conversation_report_into_registry() {
        use crate::repo::lane::state::Diff;
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState, SystemEvent};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        state.lane_pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: LaneAddress::root("vp"),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-07-18T00:00:00Z".to_string(),
            pid: Some(1),
            cwd: state_dir.path().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        });

        let mut rx = state.system_event_tx.subscribe();
        dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({
                "lane": "vp/main",
                "session_id": "sid-issued",
                "event": "issued",
            }),
        )
        .await
        .expect("lane_session_changed ok");

        // registry（SSOT）に記録され、旧 store には書かれない
        let reg = crate::lane::session_registry::load("vp", "main", "claude");
        let root_conv = reg
            .sessions
            .iter()
            .find(|s| s.key == reg.root)
            .and_then(|s| s.conversation.as_deref().map(str::to_string));
        assert_eq!(
            root_conv.as_deref(),
            Some("sid-issued"),
            "issued 報告が fresh root に即記録される（発行時点点灯の核。書き手は registry に漏斗化 —\
             doc 40 PR-2 で旧 store への直書き経路そのものが撤去済み）"
        );

        // Diff::Update が新 id + sessions snapshot を運ぶ
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("Diff::Update が 1s 以内に届く")
            .expect("broadcast recv");
        match event {
            SystemEvent::Lane(Diff::Update { payload }) => {
                assert_eq!(payload.engine_session_id.as_deref(), Some("sid-issued"));
                assert_eq!(
                    payload.cc_session_id.as_deref(),
                    Some("sid-issued"),
                    "root=claude なので channel D 契約（cc_session_id）にも同値が載る"
                );
                let sessions = payload.sessions.expect("sessions snapshot が同梱される");
                assert_eq!(sessions.root, 1);
            }
            other => panic!("expected Diff::Update, got: {other:?}"),
        }
    }

    /// doc 40 §4 / doc 46 P5 の配線: `session` を名乗った報告は**その session** に着地し、
    /// root の会話 id を上書きしない（同じ lane に console slot が同居できる前提）。
    /// 実在しない session の報告は root に化けず、何も書かない。
    #[tokio::test]
    async fn lane_session_changed_records_into_reported_session() {
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        state.lane_pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: LaneAddress::root("vp"),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-07-22T00:00:00Z".to_string(),
            pid: Some(1),
            cwd: state_dir.path().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        });
        // root(#1) は発話済み、同居人 #2 が立っている状態。
        crate::lane::session_registry::set_conversation(
            "vp",
            "main",
            "claude",
            1,
            Some("sid-root"),
        )
        .expect("root conversation");
        let k2 = crate::lane::session_registry::create(
            "vp",
            "main",
            "claude",
            "claude",
            crate::lane::session_registry::SessionMode::Tui,
            false,
        )
        .expect("create #2");

        // 同居人（#2）の hook 報告
        dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({
                "lane": "vp/main",
                "session_id": "sid-roommate",
                "event": "spoken",
                "session": k2,
            }),
        )
        .await
        .expect("lane_session_changed ok");

        let reg = crate::lane::session_registry::load("vp", "main", "claude");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some("sid-root"),
            "同居人の報告で root の会話 id（= root の --resume 先）が化けない"
        );
        assert_eq!(
            reg.sessions[1].conversation.as_deref(),
            Some("sid-roommate"),
            "報告は名乗った session に着地する"
        );

        // 実在しない session の報告 → root に落ちない（黙って root を潰さない）
        dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({
                "lane": "vp/main",
                "session_id": "sid-ghost",
                "event": "spoken",
                "session": 99,
            }),
        )
        .await
        .expect("lane_session_changed ok（記録はしないが配線は成功）");
        let reg = crate::lane::session_registry::load("vp", "main", "claude");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some("sid-root"),
            "実在しない session の報告は root に化けない"
        );
        assert_eq!(reg.sessions.len(), 2, "session は増えない");
    }

    /// lanes portless: `lanes_list` dispatch arm が `{lanes:[...]}` 形で返る (build_lanes_snapshot 経由)。
    #[tokio::test]
    async fn lanes_list_returns_lanes_array() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(&state, "lanes_list", serde_json::json!({}))
            .await
            .expect("lanes_list dispatch");
        assert!(
            res.get("lanes").map(|s| s.is_array()).unwrap_or(false),
            "lanes_list は {{lanes:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lane_create` dispatch arm が validation error を unison error frame
    /// (= Err) として返す (core の `create_sub_orchestrated` に到達している証)。
    ///
    /// doc 44 P2: 旧版は `kind != "sub"` を叩いていたが、`kind` は撤去された
    /// （lane に種別が無くなり指定の余地が消えた）。後継の validation = 開発起点の予約名拒否。
    #[tokio::test]
    async fn lane_create_rejects_reserved_name() {
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;

        let state = build_test_app_state(None).await;
        let err = dispatch_repo_method(
            &state,
            "lane_create",
            serde_json::json!({ "name": crate::repo::lane::ROOT_LANE_NAME }),
        )
        .await
        .expect_err("予約名は Err");
        // doc 44 §9: 判定は `validate_sub_name` に一本化された（両経路で同じ gate）。
        // message は同関数のものになるので、予約名を名指ししていることだけを見る。
        assert!(
            err.contains(crate::repo::lane::ROOT_LANE_NAME) && err.contains("reserved"),
            "error は予約名である旨を含む: {err}"
        );

        // 旧 client が送る `kind` は unknown field として無視され、name だけで通ること
        // （name が空なら別の validation で弾かれる = kind に依存しない）
        let err = dispatch_repo_method(
            &state,
            "lane_create",
            serde_json::json!({ "kind": "sub", "name": "  " }),
        )
        .await
        .expect_err("空 name は Err");
        assert!(err.contains("empty"), "name 制約で弾かれる: {err}");
    }

    /// doc 50 §4.6 A6: `lane_slot_new` は **新 slot に pump を張る**。
    ///
    /// team-b review 2026-07-25 の指摘（score 87）: pump の起動契機は ①demand hook（購読者数
    /// 0→1 のエッジ）②act 切替 の 2 つしかなく、slot 追加はどちらでもなかった。しかも client の
    /// terminal 購読は lane 単位 1 本なので、root が既に tui で購読済の lane に「+ New」で 2 枚目を
    /// 足すと購読者数は 1→1 のまま = エッジが立たない → **入力は通るのに出力だけ永久に沈黙**。
    ///
    /// 「既に pump がある lane に slot を足す」順序（= 実運用で最頻の経路）で回帰を止める。
    #[tokio::test]
    async fn lane_slot_new_attaches_pump_to_the_new_slot() {
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lane::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::time::Duration;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-slotpump");
        let lane = addr.to_string();

        // lane を登録（agent=shell = console に engine を注入しない）+ root slot を立てる。
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
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn root");
            pool.insert_pty_slot(addr.clone(), None, slot, rx);
        }
        // 購読を張って demand_start → root に pump（= 「既に購読済」の状態を作る）。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, _srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        let pumps_before = state
            .terminal_pumps
            .read()
            .await
            .get(&lane)
            .map(|m| m.len())
            .unwrap_or(0);
        assert_eq!(pumps_before, 1, "root の pump が 1 本張られている");

        // ここで「+ New」相当（lane_slot_new）。**購読者数は 1→1 のままでエッジは立たない**。
        let res = dispatch_repo_method(
            &state,
            "lane_slot_new",
            serde_json::json!({ "lane": lane, "agent": "shell" }),
        )
        .await
        .expect("lane_slot_new");
        let new_session = res["session"].as_u64().expect("session") as u32;

        // それでも新 slot に pump が張られている（動詞の末尾 reconcile が不足分を検知するため）。
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pumps = state.terminal_pumps.read().await;
        let lane_pumps = pumps.get(&lane).expect("lane の pump map");
        assert_eq!(
            lane_pumps.len(),
            2,
            "root + 新 session の 2 本（got keys={:?}）",
            lane_pumps.keys().collect::<Vec<_>>()
        );
        assert!(
            lane_pumps.contains_key(&new_session),
            "新 session の pump がある（無いと出力が永久に届かない）"
        );
    }
}
