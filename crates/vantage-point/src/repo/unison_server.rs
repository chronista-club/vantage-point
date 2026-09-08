//! Unison QUIC サーバー
//!
//! MCP <-> Process 間の高速通信レイヤー。
//! Axum HTTP サーバーと並行して起動し、同じ Hub.broadcast() パターンで
//! WebSocket クライアントにメッセージを配信する。
//!
//! ポート: HTTP と同一ポート番号を使う。 HTTP は TCP・QUIC は UDP で OS レベルの
//! ポート名前空間が独立しているため衝突しない (`QUIC_PORT_OFFSET = 0`)。
//!
//! "process" チャネルですべての操作を統一:
//! - show / clear / toggle_pane / split_pane / close_pane
//! - watch_file / unwatch_file

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::board;
use super::conversation_ops;
use super::conversation_replay;
use super::editor_bridge;
use super::state::AppState;
use crate::protocol::RepoMessage;

/// QUIC ポートのオフセット（HTTP ポートからの差分）
/// TCP (HTTP) と UDP (QUIC) は OS レベルで独立 → 同一ポートで共存可能
pub const QUIC_PORT_OFFSET: u16 = 0;

/// UnwatchFile リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct UnwatchFileRequest {
    pane_id: String,
}

// =============================================================================
// Process チャネル ハンドラー
// =============================================================================

/// RepoMessage を受け取って broadcast + Msgbox 配信する汎用ハンドラー
///
/// MCP → QUIC → ここ の経路では、MCP が RepoMessage をそのままシリアライズして送る。
/// HTTP ハンドラ（health.rs の show_handler 等）と同じ RepoMessage 形式を受ける。
///
/// 配信先:
/// 1. Hub broadcast → WebSocket → Canvas（既存）
/// 2. Msgbox "protocol" → board Capability（VP-24）
fn handle_process_message(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let msg: RepoMessage = serde_json::from_value(payload.clone())
        .map_err(|e| format!("Invalid RepoMessage: {}", e))?;

    // 1. Hub broadcast → WebSocket → Canvas（既存経路）
    // TopicRouter が Hub ブリッジ経由で自動的に retained に保存するため、
    // 明示的なキャッシュは不要。Hub に broadcast するだけ。
    state.hub.broadcast(msg);

    // 現在は Hub broadcast のみで Canvas に配信。

    Ok(serde_json::json!({"status": "ok"}))
}

/// watch_file メソッドのハンドラー
async fn handle_watch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let config: crate::file_watcher::WatchConfig = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid watch_file payload: {}", e))?;

    let pane_id = config.pane_id.clone();

    state
        .file_watchers
        .lock()
        .await
        .start_watch(config, state.hub.clone())
        .map_err(|e| format!("watch_file 開始失敗: {}", e))?;

    Ok(serde_json::json!({"status": "ok", "pane_id": pane_id}))
}

/// unwatch_file メソッドのハンドラー
async fn handle_unwatch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: UnwatchFileRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid unwatch_file payload: {}", e))?;

    state.file_watchers.lock().await.stop_watch(&req.pane_id);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

// =============================================================================
// ProcessRunner ハンドラー
// =============================================================================

/// プロセス起動
async fn handle_process_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::repo::process_runner::RunParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    let process_id = crate::repo::process_runner::process_run(
        &state.process_registry,
        &params,
        &state.repo_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// プロセス停止
async fn handle_process_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::repo::process_runner::process_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// コード注入
async fn handle_process_inject(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::repo::process_runner::InjectParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    crate::repo::process_runner::process_inject(&state.process_registry, &params).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// プロセス一覧
async fn handle_process_list(state: &AppState) -> Result<serde_json::Value, String> {
    let processes = state.process_registry.lock().await.list();
    Ok(serde_json::json!({"status": "ok", "processes": processes}))
}

// L0 portless Group B-3: 旧 SP HTTP `/api/ruby/*` を repo-proxy ask に移管。 HTTP handler と同じ
// `process_runner::ruby_*` core を呼ぶ薄い adapter (payload からフィールド抽出)。 ruby_list は
// `process_registry.list()` = `handle_process_list` と同一なので dispatch 側で再利用する。

/// ruby_eval: 短命 Ruby 実行
async fn handle_ruby_eval(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let r =
        crate::repo::process_runner::ruby_eval(code, file, pane_id, &state.repo_dir, &state.hub)
            .await?;
    Ok(serde_json::json!({
        "status": "ok",
        "stdout": r.stdout,
        "stderr": r.stderr,
        "exit_code": r.exit_code,
        "elapsed_ms": r.elapsed_ms,
    }))
}

/// ruby_run: 長命 Ruby daemon 起動
async fn handle_ruby_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let name = payload.get("name").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let process_id = crate::repo::process_runner::ruby_run(
        &state.process_registry,
        code,
        file,
        name,
        pane_id,
        &state.repo_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// ruby_stop: Ruby daemon 停止
async fn handle_ruby_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::repo::process_runner::ruby_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// Terminal チャネル制御メッセージハンドラー
// =============================================================================

// =============================================================================
// サーバー起動
// =============================================================================

/// repo "process" channel の method dispatch（reverse-routing と共有する単一の入口）。
///
/// repo の "process" Unison channel handler と、 Daemon reverse-routing 経由 (repo control
/// keepalive) の **両方**がこの関数を呼ぶことで、 「MCP が repo 直結」「MCP → Daemon → repo
/// reverse」どちらの経路でも同一の dispatch ロジック・同一の AppState 操作になる
/// (L0 SP-portless: repo listen port を Daemon 単一 endpoint に寄せても挙動不変)。
/// S2 (doc 27 §4.1) → doc 53 R2: terminal demand start / stop の共通ハンドラー。
///
/// daemon の TopicRouter demand hook が `repo/terminal/data/{lane}/out` の購読者 0↔1 を
/// 検知して撃つ。旧実装は start / stop がそれぞれ「張る」「畳む」を実行していたが、
/// reconcile 化で両者は**同じ操作**になった — demand の今（購読者数の level）は
/// `TopicRouter::demand_active` が答えるので、 hook の向きは信じず契機としてだけ使う
/// （start 側は「購読者が現れたので pump を揃えろ」、 stop 側は「消えたので畳め」、
/// どちらも reconcile 1 呼びで正しい方に収束する）。
async fn handle_terminal_demand(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("terminal_demand: lane 未指定".to_string());
    }
    if crate::repo::lanes_state::LanePool::parse_address(&lane).is_none() {
        return Err(format!("terminal_demand: lane パース失敗: {}", lane));
    }
    // client が「画面を持っていない」と名乗った場合は replay を必ず流す
    // （JS ready 後の catch-up。webview 準備前に届いた replay は捨てられている）。
    let force_replay = payload
        .get("replay")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Lane 不在 / PtySlot 無でも受理（= Lane 起動後の再 demand 余地を残す）。
    let r = if force_replay {
        crate::repo::terminal_pump::reconcile_lane_pumps_forcing_replay(
            &state.lane_pool,
            &state.terminal_pumps,
            &state.topic_router,
            &lane,
        )
        .await
    } else {
        reconcile_terminal_pumps(state, &lane).await
    };
    Ok(serde_json::json!({
        "status": "reconciled", "lane": lane,
        "attached": r.attached, "removed": r.removed, "kept": r.kept,
    }))
}

/// [`crate::repo::terminal_pump::reconcile_lane_pumps`] の AppState 版（呼び手の糖衣）。
///
/// demand hook / 動詞の末尾（mode 切替・slot 追加・restart）/ boot 復元後 — pump に影響する
/// あらゆる契機がこの 1 本を呼ぶ。旧 `respawn_terminal_pump` の `only` 引数（呼び手ごとの
/// scope 判断）は廃止 — 「pid 一致は触らない」の照合が兄弟保護を構造で保証する。
pub(crate) async fn reconcile_terminal_pumps(
    state: &AppState,
    lane: &str,
) -> crate::repo::terminal_pump::PumpReconcile {
    crate::repo::terminal_pump::reconcile_lane_pumps(
        &state.lane_pool,
        &state.terminal_pumps,
        &state.topic_router,
        lane,
    )
    .await
}

/// [`crate::repo::lane_reconcile::reconcile_lane`] の AppState 版（呼び手の糖衣）。
///
/// **動詞の末尾はこれ 1 本**（doc 53 §12.4 / R3c）。registry に intent を書いた動詞は、
/// 実体（PtySlot / chat engine / 代表値 / pump）を自分で動かさずにこれを呼ぶ。
/// pump だけを合わせたい契機（demand hook）は [`reconcile_terminal_pumps`] のまま —
/// あちらは lane 全体の実体を触らない軽い経路。
pub(crate) async fn reconcile_lane(
    state: &AppState,
    addr: &crate::repo::lanes_state::LaneAddress,
) -> crate::repo::lane_reconcile::LaneReconcile {
    crate::repo::lane_reconcile::reconcile_lane(
        &state.lane_pool,
        &state.terminal_pumps,
        &state.topic_router,
        addr,
    )
    .await
}

/// payload の additive な session key（doc 38 / doc 46 P5）。省略 / null = `None`。
///
/// 型不正・0 は Err — 黙って既定に落とすと「指定したつもりの session と別の会話に届く」
/// 誤配送になるため、明示エラーで返す。
///
/// ⚠️ **`None` の解決先は経路で違う**（型が同じなので取り違えやすい）:
/// - chat 系（`conversation_*`）= **focused**（[`LanePool::resolve_chat_session`]）
/// - slot 系（`terminal_*` / `lane_capture` / `lane_nudge`）= **root**
///   （[`LanePool::slot_session`] — slot は lane の設備で、代表は root。doc 39「座と化身」）
/// - 会話報告（`lane_session_changed`）= **root だが「不明」として運ぶ**
///   （[`crate::lane::session_registry::ReportTarget::Unspecified`] — 着地先は root でも、
///   「名乗らなかった」という事実を registry まで届ける。root に丸めてから渡すと、実在しない
///   session の報告も root 宛と見分けが付かなくなる。doc 40 §4）
pub(crate) fn payload_session_key(
    ctx: &str,
    payload: &serde_json::Value,
) -> Result<Option<crate::lane::session_registry::SessionKey>, String> {
    match payload.get("session") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            let n = v
                .as_u64()
                .filter(|n| (1..=u64::from(u32::MAX)).contains(n))
                .ok_or_else(|| format!("{ctx}: session が不正（1 以上の整数）: {v}"))?;
            Ok(Some(n as u32))
        }
    }
}

/// S3 (doc 27 §4.1, 経路 B): terminal 入力。
///
/// surface (vp-app) → daemon canvas channel (upstream request) → repo control → 本 dispatch。
/// `data` は base64 (出力 pump の encoding と対称、 任意バイトを JSON で運ぶため)。 decode して
/// 当該 slot の PtySlot に書き込む (`session` 省略 = root、doc 46 P5)。
async fn handle_terminal_write(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_write: lane 未指定".to_string());
    }
    let data_b64 = payload.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| format!("terminal_write: base64 decode 失敗: {}", e))?;
    vp_paths::term_trace("B:repo-recv", lane, &bytes);
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_write: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .write_to_lane(
            &addr,
            payload_session_key("terminal_write", &payload)?,
            &bytes,
        )
        .map_err(|e| format!("terminal_write 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// tmux decoupling PR1: lane nudge。 論理 lane address 宛に literal text + Enter を PtySlot へ書く。
///
/// 旧制御面 (`tmux send-keys -t <session>`) の repo-proxy 置換。 daemon (delivery/reconcile
/// loop の re-nudge) / CLI (`vp flow handoff`) / MCP (`flow_handoff`) が control channel 経由で
/// この method を ask する。 repo-local な `AppState::nudge_lane` は同じ `deliver_nudge` sink を
/// in-process で呼ぶ (text→Enter の submit 意味論は `deliver_nudge` に集約)。
async fn handle_lane_nudge(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_nudge: lane 未指定".to_string());
    }
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_nudge: lane パース失敗: {}", lane));
    };
    // doc 46 P5: `session` 省略 = root（mailbox を名乗る住人）。明示指定で同居する別 slot に届く。
    let session = payload_session_key("lane_nudge", &payload)?;
    crate::repo::lanes_state::deliver_nudge(&state.lane_pool, &addr, session, text)
        .await
        .map_err(|e| format!("lane_nudge 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session}))
}

/// doc 46 P5: lane が持つ **PTY slot の一覧**（session / pid / 生死 / root か / attach 有無）。
///
/// slot は lane に 1 枚ではなく session ごとになった。表示は当面ミニマム（1 枚ずつ）なので、
/// **UI を通さずに枚数と中身を読む口**をここに置く（doc 47 §7 成立条件② — 「読み手のない
/// 書き込み」を作らない）。CLI `vp lane slots` がこの method を ask する。
async fn handle_lane_slots(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_slots: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
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
async fn handle_lane_slot_new(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_slot_new: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
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
    reconcile_lane(state, &addr).await;
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
    super::routes::lanes::emit_lane_update(state, &addr).await;
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
async fn handle_lane_capture(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_capture: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_capture: lane パース失敗: {}", lane));
    };
    // doc 46 P5: `session` 省略 = root（lane の代表 slot）。明示指定で同居する別 slot を読む。
    let session = payload_session_key("lane_capture", &payload)?;
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

/// S3: terminal resize。 PtySlot (+ TermAttach grid) を cols×rows に同期する。
async fn handle_terminal_resize(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_resize: lane 未指定".to_string());
    }
    // bounds check: 0 / 極大値で PTY に不正 dims を渡さない (u64→u16 silent wrap も防ぐ)。
    // 旧 daemon "terminal" channel の resize 経路と同じ範囲 (1..=1000)。
    let cols = payload.get("cols").and_then(|v| v.as_u64()).unwrap_or(80);
    let rows = payload.get("rows").and_then(|v| v.as_u64()).unwrap_or(24);
    if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
        return Err(format!(
            "terminal_resize: 不正な dims (cols={cols} rows={rows})"
        ));
    }
    let (cols, rows) = (cols as u16, rows as u16);
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_resize: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .resize_lane(
            &addr,
            payload_session_key("terminal_resize", &payload)?,
            cols,
            rows,
        )
        .map_err(|e| format!("terminal_resize 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "cols": cols, "rows": rows}))
}

/// F6② (doc 27 §3.4.5/§6): Lane delete。 旧 SP HTTP `DELETE /api/lanes` を repo-proxy ask に
/// 移管（surface→repo 直結 HTTP を撤去、 daemon 経由の ask に統一）。 logic は旧 `delete_handler`
/// から移設し、 core の `delete_lane_orchestrated` を再利用（HTTP route + handler は削除）。
async fn handle_lane_delete(
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
    let addr = crate::repo::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_delete: invalid lane address: {}", address))?;
    match super::routes::lanes::delete_lane_orchestrated(state, addr, cleanup).await {
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
async fn handle_lane_restart(
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
    let addr = crate::repo::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_restart: invalid lane address: {}", address))?;
    if fresh {
        super::routes::lanes::reset_lane_orchestrated(state, addr).await
    } else {
        super::routes::lanes::restart_lane_orchestrated(state, addr).await
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
async fn handle_lane_session_changed(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_session_changed: lane 必須".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
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
        let target = match payload_session_key("lane_session_changed", &payload)? {
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
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({ "status": "ok", "lane": lane }))
}

/// lanes portless (doc 27 §3.4.5): Lane create。 旧 SP HTTP `POST /api/lanes` を repo-proxy ask に
/// 移管。 core の `create_sub_orchestrated` (lane clone + PtySlot spawn) を呼ぶ薄い adapter。
/// payload は `CreateLaneReq` 互換 JSON (kind/name/agent?/cwd?/branch?/base?)。 成功は LaneInfo JSON、
/// 失敗は core が返す String error (旧 HTTP の CONFLICT="already exists" 等を保持)。
async fn handle_lane_create(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: super::routes::lanes::CreateLaneReq = serde_json::from_value(payload)
        .map_err(|e| format!("lane_create: invalid payload: {}", e))?;
    let info = super::routes::lanes::create_sub_orchestrated(state, req).await?;
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
async fn handle_lane_origin_get(state: &Arc<AppState>) -> Result<serde_json::Value, String> {
    let lanes = ledger_lane_refs(state).await;
    let origin = crate::host::ledger::origin(state.vpdb.as_ref(), &state.repo_dir, &lanes).await;
    serde_json::to_value(&origin).map_err(|e| format!("lane_origin_get: serialize 失敗: {e}"))
}

/// doc 44 D4: 開発起点を設定する。payload = `{ "lane": "<lane 名>" }`。
///
/// 人が打つのは名前、帳簿に入るのは `lane_id` — 変換は
/// [`crate::host::ledger::set_origin`] が境界で 1 回だけ行う。
/// D5 の通り **何も動かさない**（cwd も active lane も変えない、ポインタの書き換えだけ）。
async fn handle_lane_origin_set(
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
        .send(crate::repo::lanes_state::SystemEvent::LanesProjectionChanged);
    let origin = crate::host::ledger::origin(state.vpdb.as_ref(), &state.repo_dir, &lanes).await;
    serde_json::to_value(&origin).map_err(|e| format!("lane_origin_set: serialize 失敗: {e}"))
}

/// doc 44 §12: lane の並び順を帳簿に保存する。payload = `{ "order": ["<lane 名>", ...] }`。
///
/// 起点と同じく、人が触るのは名前で帳簿に入るのは `lane_id`。保存後の反映は
/// 次の lanes snapshot に載って戻る（`build_lanes_snapshot` が帳簿の順で並べる）。
async fn handle_lane_order_set(
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
        .send(crate::repo::lanes_state::SystemEvent::LanesProjectionChanged);
    Ok(serde_json::json!({ "status": "ok", "count": order.len() }))
}

/// lanes portless (doc 27 §3.4.5): Lane list。 旧 SP HTTP `GET /api/lanes` を repo-proxy ask に
/// 移管。 core の `build_lanes_snapshot` を呼び `{lanes:[...]}` で wrap (旧 HTTP `LanesResponse` 互換)。
async fn handle_lanes_list(state: &Arc<AppState>) -> Result<serde_json::Value, String> {
    let lanes = super::routes::lanes::build_lanes_snapshot(state).await;
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

/// F6④ (doc 27 §3.4.5/§6): Agent 一覧。 旧 SP HTTP `GET /api/agents` を repo-proxy ask に移管。
/// tmux decoupling PR2: built-in 静的テーブル (旧 mise task scan + TTL cache は廃止)。
async fn handle_stands_list() -> Result<serde_json::Value, String> {
    let agents = super::routes::agents::list_agents();
    Ok(serde_json::json!({ "agents": agents }))
}

pub(crate) async fn dispatch_repo_method(
    state: &Arc<AppState>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        // switch_lane も generic broadcast 経路に乗せる（B1: 遠隔 active Lane 制御）。
        // hub → topic `repo/board/event/switch-lane`（一時コマンド=非
        // retained）→ canvas channel → vp-app が受信して active Lane を切り替える。
        // board モデル (2026-07-15): show/clear は repo-authoritative な board 経路へ。
        // item を DB に durable append し、 更新後 board を BoardUpdated(retained) で broadcast する。
        "show" | "clear" => board::handle_canvas_command(state, payload).await,
        // doc 52 §5: id 指定 in-place 置換（read-first、id 不在は loud error）
        "board_update" => board::handle_board_update(state, payload).await,
        // doc 52 §4/§5: 呼び出し元 lane の board を id 付き全文で返す（中継台 + identity lookup）
        "read_board" => board::handle_board_read(state, payload).await,
        // doc 48 Phase 2: editor bridge (MCP → GUI request-response)
        "editor_fields" | "editor_values" | "editor_set" => {
            editor_bridge::handle_editor_command(state, method, payload).await
        }
        // doc 49 LE-P2 PR2: layout bridge (LE-15)。editor bridge と同じ配管を op を変えて共用
        // (method に editor_ prefix が無いので op = method のまま vp-app に届く)
        "layout_get" | "layout_set" | "layout_history" => {
            editor_bridge::handle_editor_command(state, method, payload).await
        }
        "editor_result" => editor_bridge::handle_editor_result(state, payload).await,
        "toggle_pane" | "split_pane" | "close_pane" | "switch_lane" => {
            handle_process_message(state, payload)
        }
        "watch_file" => handle_watch_file(state, payload).await,
        "unwatch_file" => handle_unwatch_file(state, payload).await,
        // S2: demand-driven terminal pump (Daemon demand hook → control reverse-route)
        // doc 53 R2: start / stop は同じ reconcile の契機（demand の今は level で読む）。
        "terminal_demand_start" | "terminal_demand_stop" => {
            handle_terminal_demand(state, payload).await
        }
        // gui replay-on-attach: chat lane の transcript を attach 時に replay
        "conversation_demand_start" => {
            conversation_replay::handle_conversation_demand_start(state, payload).await
        }
        "conversation_demand_stop" => {
            conversation_replay::handle_conversation_demand_stop(state, payload).await
        }
        // S3: terminal 入力/resize (surface → canvas channel upstream → control reverse-route)
        "terminal_write" => handle_terminal_write(state, payload).await,
        "conversation_submit" => conversation_ops::handle_conversation_submit(state, payload).await,
        // channel E (doc 34): wire/delegation nudge の chat-engine 注入 (lane_nudge の Chat 対応物)
        "conversation_nudge" => conversation_ops::handle_conversation_nudge(state, payload).await,
        // gui HITL (doc 35 PR1): PromptCard 回答 → 逆方向 can_use_tool へ control_response 書き戻し
        "conversation_respond" => {
            conversation_ops::handle_conversation_respond(state, payload).await
        }
        // doc 35 §5: 実行中 turn の中断（stop ボタン / Esc）。
        "conversation_interrupt" => {
            conversation_ops::handle_conversation_interrupt(state, payload).await
        }
        // doc 35 §2.5 / PR3: permission mode の動的切替（承認 opt-in）。
        "conversation_set_permission_mode" => {
            conversation_ops::handle_conversation_set_permission_mode(state, payload).await
        }
        // doc 38 (1 Lane = N session): session registry の list / create / focus。
        // Phase 2 の tab strip はこの 3 本 + 既存 RPC の additive session param だけで成立する。
        "conversation_session_list" => {
            conversation_ops::handle_conversation_session_list(state, payload).await
        }
        "conversation_session_create" => {
            conversation_ops::handle_conversation_session_create(state, payload).await
        }
        // doc 39 §4: tui の ✨ New（新 session + root 張り替え + slot の bare respawn、非破壊）
        "conversation_session_new_root" => {
            conversation_ops::handle_conversation_session_new_root(state, payload).await
        }
        // doc 39 P3: Root 切替 picker（既存 session へ root を向け替え + Resume slot 張り替え）
        "conversation_session_switch_root" => {
            conversation_ops::handle_conversation_session_switch_root(state, payload).await
        }
        "conversation_session_focus" => {
            conversation_ops::handle_conversation_session_focus(state, payload).await
        }
        // doc 38 Phase 3: tab を閉じる（session remove）。
        "conversation_session_remove" => {
            conversation_ops::handle_conversation_session_remove(state, payload).await
        }
        "session_set_mode" => conversation_ops::handle_session_set_mode(state, payload).await,
        "conversation_set_model" => {
            conversation_ops::handle_conversation_set_model(state, payload).await
        }
        // doc 51 §1 A3b: `vp now` — session の「今なにを」自己申告を now-line に注入
        "session_now" => conversation_ops::handle_session_now(state, payload).await,
        // tmux decoupling PR1: 制御面 nudge の repo-proxy 入口 (旧 tmux send-keys の置換)
        "lane_nudge" => handle_lane_nudge(state, payload).await,
        // tmux decoupling PR2: lane console capture (旧 tmux capture-pane の native 代替)
        "lane_capture" => handle_lane_capture(state, payload).await,
        // doc 46 P5: lane が持つ PTY slot の一覧（UI を通さない slot 枚数の読み手）
        "lane_slots" => handle_lane_slots(state, payload).await,
        // doc 46 P5 producer: 新 session を採番して console を 1 枚立てる（`lane_slots` の書き手）
        "lane_slot_new" => handle_lane_slot_new(state, payload).await,
        "terminal_resize" => handle_terminal_resize(state, payload).await,
        // board モデル (2026-07-15): webview からの board mutate（thumbnail ✕ / Clear ボタン）。
        // 旧 pp_state_save/load は撤去（board は repo truth、 webview は BoardUpdated 購読 + mutate へ）。
        "board_delete_item" => board::handle_board_delete_item(state, payload).await,
        "board_clear" => board::handle_board_clear(state, payload).await,
        // cursor の server 昇格（doc 52 §5 計器盤）: thumbnail click / scrollback の注視を repo truth に。
        "board_set_cursor" => board::handle_board_set_cursor(state, payload).await,
        // lanes portless: Lane create/list (旧 SP HTTP POST/GET /api/lanes を repo-proxy ask に移管)
        "lane_create" => handle_lane_create(state, payload).await,
        "lanes_list" => handle_lanes_list(state).await,
        // F6②: Lane delete (旧 SP HTTP DELETE /api/lanes を repo-proxy ask に移管)
        "lane_delete" => handle_lane_delete(state, payload).await,
        // F6③: Lane restart (旧 SP HTTP POST /api/lanes/restart を repo-proxy ask に移管)
        "lane_restart" => handle_lane_restart(state, payload).await,
        // 供給 push 根治: hook → daemon 経由の session pointer 変化通知（Diff::Update push の起点）
        "lane_session_changed" => handle_lane_session_changed(state, payload).await,
        // doc 44 D4: Repo Host の帳簿 — 開発起点ポインタの読み書き
        "lane_origin_get" => handle_lane_origin_get(state).await,
        "lane_origin_set" => handle_lane_origin_set(state, payload).await,
        "lane_order_set" => handle_lane_order_set(state, payload).await,
        // F6④: Agent 一覧 (旧 SP HTTP GET /api/agents を repo-proxy ask に移管)
        "agents_list" => handle_stands_list().await,
        // L0 finale: repo graceful shutdown を QUIC で (旧 SP HTTP POST /api/shutdown を置換、
        // Daemon stop_process / restart_process 用)。 shutdown_token.cancel() で graceful 停止
        // (DB close 等)。 repo が即 QUIC server を畳むため応答が返らない事もあるが best-effort。
        "shutdown" => {
            tracing::info!("Shutdown requested via QUIC dispatch");
            state.shutdown_token.cancel();
            Ok(serde_json::json!({"status": "shutting_down"}))
        }
        // tmux decoupling PR2: 旧 "tmux_*" dispatch (split/list/close/capture/agent_meta/
        // send_keys/resolve_pane) は退役。 後継は lane 語彙の "lane_nudge" / "lane_capture"。
        // ProcessRunner
        "process_run" => handle_process_run(state, payload).await,
        "process_stop" => handle_process_stop(state, payload).await,
        "process_inject" => handle_process_inject(state, payload).await,
        "process_list" => handle_process_list(state).await,
        // L0 portless Group B-3: Ruby VM (旧 SP HTTP /api/ruby/* を repo-proxy ask に移管)。
        // ruby_list は process_registry.list() = process_list と同一なので handle_process_list 再利用。
        "ruby_eval" => handle_ruby_eval(state, payload).await,
        "ruby_run" => handle_ruby_run(state, payload).await,
        "ruby_stop" => handle_ruby_stop(state, payload).await,
        "ruby_list" => handle_process_list(state).await,
        // wiremsg threaded inbox (Phase A ①、 R2 で wire_thread 追加)
        "wire_send" => handle_wire_send(state, payload).await,
        "wire_recv" => handle_wire_recv(state, payload).await,
        "wire_thread" => handle_wire_thread(state, payload).await,
        // flow_progress 用 read-only 未読 count (cursor 不触り)
        "wire_unread_count" => handle_wire_unread_count(state, payload).await,
        // flow_progress 5-state FSM derive 用 read-only 最新 wmsg
        "wire_latest_msg" => handle_wire_latest_msg(state, payload).await,
        // flow_progress AwaitingUser 判定用 read-only 未 ack needs_user
        "wire_needs_user_pending" => handle_wire_needs_user_pending(state, payload).await,
        "wire_ack" => handle_wire_ack(state, payload).await,
        // Agent 委譲 (doc 28 §4): delegate=B を wake / complete=A を wake /
        // respond=NeedsInput(Reborn) に A が回答して B を再 wake (Active へ loop)。
        "delegate" => super::delegation::handle_delegate(state, payload).await,
        "complete" => super::delegation::handle_complete(state, payload).await,
        "respond" => super::delegation::handle_respond(state, payload).await,
        _ => Err(format!("不明なメソッド: process.{}", method)),
    }
}

// =============================================================================
// wiremsg ハンドラー (R2-a: daemon 中央 store への proxy 層)
//
// store 直結のロジックは routes/wire.rs (daemon 側) に移設済。 repo の責務は
// 「アドレス正規化 (N1) → daemon へ HTTP relay」 のみ。 QUIC dispatch と
// HTTP wrapper (routes/health.rs) は本 proxy 群を呼ぶため signature 不変。
// =============================================================================

/// agent address を canonical (qualified) 形に正規化する (wiremsg N1、 refactor R1 PR-B)
///
/// bare `"agent"` を qualified (`agent@<repo>`) に正規化する。
///
/// 現行 MCP (`SelfLane::from_address`) は main も canonical `agent@<repo>` を
/// 自前で送るため、本関数は実質 **冪等な素通し + 後方互換 (旧 client / bare 送信者) 用の
/// 防御層**。bare を残す理由: 旧 bare 送信が来ても store 識別子を qualified 一本に揃え、
/// cross-process 返信 (`agent@<repo>` 宛 forward) が bare query と完全一致せず届かない
/// バグ (B2、 レビュー mem_1CbuxQuNRwHBiZgBVUWVfN) を防ぐため。
/// bare 以外 (qualified / board@... / runner@... 等) はそのまま返す。
///
/// ⚠️ 正規化先 `self_repo` は「繋いだ repo の repo」なので、bare のままだと誤 repo 接続で
/// identity が化ける (= 旧 main バグの根)。だから identity の SSOT は MCP 側 canonical
/// 送出に移した。本関数は qualified を受けたら何もしない (= repo 非依存) のが正常運用。
fn normalize_agent_addr(addr: &str, self_repo: &str) -> String {
    if addr == "agent" {
        format!("agent@{}", self_repo)
    } else {
        addr.to_string()
    }
}

/// wiremsg を送信する (R2-a: daemon 中央 store への proxy)
///
/// payload: `{ from, to: [..], body, reply_to? }`
///
/// repo の責務はアドレス正規化 (N1: bare `"agent"` → `"agent@<self_repo>"`) のみ。
/// 保存・notify・local_seq 採番・body coerce は全て daemon 側
/// ([`crate::repo::routes::wire`])。 cross-process forward は中央化で概念ごと消滅。
pub(crate) async fn handle_wire_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_send: 'from' required".to_string())?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| normalize_agent_addr(s, &state.repo_name))
                .collect()
        })
        .unwrap_or_default();
    let mut forwarded = serde_json::json!({
        "from": from,
        "to": to,
    });
    if let Some(body) = payload.get("body") {
        forwarded["body"] = body.clone();
    }
    if let Some(reply_to) = payload.get("reply_to") {
        forwarded["reply_to"] = reply_to.clone();
    }
    super::daemon_wire::call("/api/wire/send", forwarded).await
}

/// wiremsg を受信する (R2-a: daemon 中央 store への proxy、 long-poll は daemon 側)
///
/// payload: `{ agent, timeout? }` — timeout の clamp (default 5s / max 30s) も daemon 側。
pub(crate) async fn handle_wire_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?;
    let timeout = payload.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);
    super::daemon_wire::call(
        "/api/wire/recv",
        serde_json::json!({ "agent": agent, "timeout": timeout }),
    )
    .await
}

/// wiremsg の ancestor-chain (系譜) を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ message_id }` — agent 文脈不要のため正規化なしで relay。
pub(crate) async fn handle_wire_thread(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _ = state; // thread は repo 文脈 (正規化) 不要。 signature は他 handler と統一
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/thread",
        serde_json::json!({ "message_id": message_id }),
    )
    .await
}

/// wiremsg の agent 関与最新 message を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の 5-state FSM derive で使う。
pub(crate) async fn handle_wire_latest_msg(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_latest_msg: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/latest-msg",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の agent 発 未 ack needs_user を取得する (daemon proxy、 read-only)
///
/// payload: `{ agent }` → `{ status, message }`。 `flow_progress` の `AwaitingUser` 判定で使う。
pub(crate) async fn handle_wire_needs_user_pending(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_needs_user_pending: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/needs-user-pending",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の per-agent 未読 count を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の集約 view / `wire_inbox` MCP tool で使う。
pub(crate) async fn handle_wire_unread_count(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_unread_count: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/unread-count",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg を ack する (R2-a 新設、 決定 D3: cursor 非破壊の ack 台帳への proxy)
///
/// payload: `{ message_id, agent }` → `{ status, acked }`
pub(crate) async fn handle_wire_ack(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::normalize_agent_addr;

    #[test]
    fn normalize_bare_agent_to_qualified() {
        assert_eq!(normalize_agent_addr("agent", "vp"), "agent@vp");
    }

    #[test]
    fn normalize_keeps_qualified_and_other_addrs() {
        assert_eq!(normalize_agent_addr("agent@vp", "vp"), "agent@vp");
        assert_eq!(normalize_agent_addr("agent@other", "vp"), "agent@other");
        assert_eq!(normalize_agent_addr("agent@vp/sub", "vp"), "agent@vp/sub");
        assert_eq!(normalize_agent_addr("board@vp", "vp"), "board@vp");
    }

    /// テスト用の spawn 可能な shell。 `$SHELL` があればそれを、 無ければ OS 既定
    /// (Unix: `/bin/sh`、 Windows: `cmd.exe`) を使う。 Windows には `/bin/sh` が無いので
    /// OS 分岐が必須 (pty_slot の `default_test_shell` と同方針)。
    fn default_test_shell() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "cmd.exe".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
    }

    // =========================================================================
    // S2 (doc 27 §4.1): demand-driven terminal pump の repo 側 e2e
    // =========================================================================

    /// 実 PtySlot を lane_pool に仕込み、 demand_start → pump 起動 → PTY 出力が
    /// per-lane terminal topic に届く → demand_stop → pump 除去、 を 1 本で検証する
    /// (daemon 側 demand hook の reverse-route 先 = repo dispatch の責務範囲)。
    #[tokio::test]
    async fn terminal_demand_start_routes_pty_output_then_stop() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string(); // "vp/main"

        // 実 PtySlot を attach (subscribe_output が Some を返す前提を作る)。
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // surface 相当: repo topic_router に per-lane terminal topic を購読
        // （= demand の level が立つ。 doc 53 R2: reconcile は購読者数を直読する）。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start（= reconcile 契機）→ pump 起動。
        let started = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(started["status"], "reconciled");
        assert_eq!(started["attached"], 1);
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump が登録される"
        );

        // shell プロンプト等の PTY 出力が terminal topic に流れてくる。
        let (rtopic, msg) = tokio::time::timeout(Duration::from_secs(5), srx.recv())
            .await
            .expect("PTY 出力が terminal topic に届かない (timeout)")
            .expect("topic channel closed");
        assert_eq!(rtopic, topic);
        assert!(matches!(msg, RepoMessage::LaneTerminalOutput { .. }));

        // 購読を畳んでから demand_stop（production の hook は 1→0 遷移の後に撃つ —
        // reconcile は edge の向きを信じず level を読むので、購読が残ったままの stop は
        // no-op になるのが正。 その裏は下の keeps_pumps テストが固定する）。
        state.topic_router.unsubscribe(sub_id).await;
        let stopped = dispatch_repo_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_stop");
        assert_eq!(stopped["status"], "reconciled");
        assert_eq!(stopped["removed"], 1);
        assert!(
            !state.terminal_pumps.read().await.contains_key(&lane),
            "pump が除去される"
        );

        // 嘘の edge への耐性: 購読が生きているのに stop が届いても pump は畳まれない
        // （level が真実源）。再購読して demand を立て直し、start で 1 本張ってから検証する。
        let (sub_id2, _srx2) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start 2");
        let lying_stop = dispatch_repo_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("lying stop");
        assert_eq!(lying_stop["removed"], 0, "購読が残る限り stop は畳まない");
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump は維持される"
        );
        state.topic_router.unsubscribe(sub_id2).await;
    }

    /// PtySlot を持たない Lane への demand_start は graceful（受理して何も張らない —
    /// Lane 起動後の再 demand 余地を残す）。
    #[tokio::test]
    async fn terminal_demand_start_without_lane_is_graceful() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "reconciled");
        assert_eq!(res["attached"], 0);
        assert!(state.terminal_pumps.read().await.is_empty());
    }

    /// S3: terminal_write の base64 入力が実 PTY に届き (echo 出力で確認)、 terminal_resize が
    /// status ok を返す。 surface→Daemon→repo control の終端 = repo dispatch の責務範囲を検証する。
    #[tokio::test]
    async fn terminal_write_reaches_pty_and_resize_ok() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string();

        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // PTY 出力を write 前に購読 (echo を取りこぼさない)。
        let mut out = state
            .lane_pool
            .read()
            .await
            .subscribe_output(&addr, None)
            .expect("subscribe_output");

        // シェル初期化待ち。
        tokio::time::sleep(Duration::from_millis(500)).await;

        // terminal_write: "echo VP_S3_OK" を PtySlot に届ける。 行確定の改行は OS 依存
        // (Unix shell は LF、 cmd.exe(ConPTY) は Enter=CR、 pty_slot の write test と同方針)。
        let echo_cmd: &[u8] = if cfg!(windows) {
            b"echo VP_S3_OK\r"
        } else {
            b"echo VP_S3_OK\n"
        };
        let data = base64::engine::general_purpose::STANDARD.encode(echo_cmd);
        let res = dispatch_repo_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": lane, "data": data }),
        )
        .await
        .expect("terminal_write");
        assert_eq!(res["status"], "ok");

        // 出力に "VP_S3_OK" が現れる (= 入力が実 PTY に届いた)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), out.recv()).await {
                Ok(Ok(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    // ConPTY は DSR (`\x1b[6n` = カーソル位置問い合わせ) の応答を端末側から
                    // 受け取るまで描画を進めない。 本番は xterm.js が応答するが、 test では
                    // 端末役として terminal_write 経由で応答する (pty_slot の write test と同型)。
                    if text.contains("\u{1b}[6n") {
                        let dsr = base64::engine::general_purpose::STANDARD.encode(b"\x1b[1;1R");
                        let _ = dispatch_repo_method(
                            &state,
                            "terminal_write",
                            serde_json::json!({ "lane": lane, "data": dsr }),
                        )
                        .await;
                    }
                    if text.contains("VP_S3_OK") {
                        found = true;
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert!(found, "terminal_write の入力が PTY 出力に反映されない");

        // terminal_resize: status ok + cols/rows echo。
        let res = dispatch_repo_method(
            &state,
            "terminal_resize",
            serde_json::json!({ "lane": lane, "cols": 120, "rows": 40 }),
        )
        .await
        .expect("terminal_resize");
        assert_eq!(res["status"], "ok");
        assert_eq!(res["cols"], 120);
        assert_eq!(res["rows"], 40);
    }

    /// replay-on-attach を **sub lane** で end-to-end 検証する。
    ///
    /// 「vp-app 再起動 → 新 xterm が後発 subscribe → 前回画面が replay で戻る」を再現:
    /// PTY 出力を先に発生させ (= replay buffer に溜める)、 その **後で** topic を新規購読し、
    /// demand_start (= reconcile_terminal_pumps → attach_output) を撃つ。 購読が出力より後でも
    /// replay snapshot 経由でマーカーが届けば、 sub でも画面復元が効くことの証明になる。
    /// sub は main と別 topic key (`vp~sub~<name>`) に載るため、 main
    /// テストとは別に経路を固める価値がある。
    #[tokio::test]
    async fn replay_on_attach_restores_screen_for_sub_lane() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        // sub lane (main とは別 topic key になる)
        let addr = LaneAddress::sub("vp", "feat-replay");
        let lane = addr.to_string();
        assert_eq!(lane, "vp/lane/feat-replay"); // canonical（名前空間つき）

        // 実 PtySlot を sub address で登録
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // マーカーを PTY に出力させる (echo)。 この出力は「過去」= replay buffer に溜まる。
        // 改行は OS 依存 (S3 test と同方針)。 ConPTY の DSR gating は reader task が起動時に
        // 自己応答する (pty_slot の Windows 分岐) ため、 ここでは端末役の応答は不要。
        let marker = "VP_SUB_REPLAY_MARKER";
        let echo_cmd: Vec<u8> = if cfg!(windows) {
            format!("echo {marker}\r").into_bytes()
        } else {
            format!("echo {marker}\n").into_bytes()
        };
        {
            let pool = state.lane_pool.write().await;
            pool.write_to_lane(&addr, None, &echo_cmd)
                .expect("write to PTY");
        }

        // マーカーが replay buffer に確実に入るまで待つ (PtySlot が echo を読み終える猶予)。
        tokio::time::sleep(Duration::from_millis(800)).await;

        // ここで初めて topic を新規購読する (= 再起動後の新 xterm。 出力より後発)。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start → reconcile_terminal_pumps → attach_output → replay 先頭配送。
        let res = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "reconciled");
        assert_eq!(res["attached"], 1, "sub lane に pump が張れるはず");

        // 後発購読でも replay 経由でマーカーが届く (= 画面復元)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut seen = String::new();
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), srx.recv()).await {
                Ok(Some((
                    got_topic,
                    RepoMessage::LaneTerminalOutput {
                        lane: l,
                        session: _,
                        data,
                    },
                ))) => {
                    assert_eq!(got_topic, topic);
                    assert_eq!(l, lane, "message は full lane address を載せる");
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                    if seen.contains(marker) {
                        found = true;
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            found,
            "sub lane の後発 attach で replay されず画面が復元しない (seen={seen:?})"
        );
    }

    /// doc 50 §4.6 A6: demand_start が lane の**各 session** に pump を張り、共有 topic に
    /// session stamp 付きで route する（Design B）。2 slot を立て、両 session の出力が
    /// それぞれ正しい `session` field で届くことを検証する。
    #[tokio::test]
    async fn reconcile_covers_all_sessions() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-multi");
        let lane = addr.to_string();

        // root（None）と 2 枚目の session（Some(2)）を立てる。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn root");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
        }
        // root の実 key（fresh lane の既定）を控える。もう片方は 2。
        let sessions = state.lane_pool.read().await.slot_sessions(&addr);
        assert_eq!(sessions.len(), 2, "root + session 2 の 2 枚");
        let root_key = *sessions.iter().find(|&&k| k != 2).expect("root key");

        // 各 slot に別マーカーを echo（過去出力 = replay buffer に溜まる）。
        let nl = if cfg!(windows) { "\r" } else { "\n" };
        {
            let pool = state.lane_pool.write().await;
            pool.write_to_lane(&addr, None, format!("echo VP_ROOT_MARK{nl}").as_bytes())
                .expect("write root");
            pool.write_to_lane(&addr, Some(2), format!("echo VP_SESS2_MARK{nl}").as_bytes())
                .expect("write s2");
        }
        tokio::time::sleep(Duration::from_millis(800)).await;

        // 後発 subscribe → demand_start → 全 session に pump。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");

        // session 別に受信を畳み、両マーカーが正しい session field で届くまで待つ。
        let mut by_session: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), srx.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, data, .. }))) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    by_session
                        .entry(session)
                        .or_default()
                        .push_str(&String::from_utf8_lossy(&bytes));
                    let root_ok = by_session
                        .get(&root_key)
                        .is_some_and(|s| s.contains("VP_ROOT_MARK"));
                    let s2_ok = by_session
                        .get(&2)
                        .is_some_and(|s| s.contains("VP_SESS2_MARK"));
                    if root_ok && s2_ok {
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            by_session
                .get(&root_key)
                .is_some_and(|s| s.contains("VP_ROOT_MARK")),
            "root session の出力が root_key stamp で届く (got={by_session:?})"
        );
        assert!(
            by_session
                .get(&2)
                .is_some_and(|s| s.contains("VP_SESS2_MARK")),
            "session 2 の出力が session=2 stamp で届く (got={by_session:?})"
        );
        // マーカーが session をまたいで混ざらない（振り分けの健全性）。
        assert!(
            !by_session
                .get(&root_key)
                .is_some_and(|s| s.contains("VP_SESS2_MARK")),
            "root stream に session 2 のマーカーが混ざらない"
        );
    }

    /// reconcile が **差し替わった slot だけ**に replay を撃ち、兄弟 pane を乱さないこと
    /// （team-b 10 回目の regression の doc 53 R2 版）。
    ///
    /// pump の張り直しは client に `REPLAY_CLEAR_PREFIX` + 全 replay を送る。旧実装は呼び手が
    /// `only` 引数で範囲を判断していた（判断を間違えると**触っていない pane が clear されて
    /// scroll 位置も飛ぶ**）。reconcile では「pid 一致は触らない」の照合が同じ保護を構造で
    /// 与える — 呼び手は範囲を指定できない（引数が存在しない）。
    ///
    /// 観測は **client が見るもの**で行う: slot 差替 + reconcile 後に流れてくる出力の
    /// session stamp が、差し替わった session だけであること。
    /// **GUI が再起動したら replay を流し直す**（doc 53 §6.5.0 の最終段、2026-07-26）。
    ///
    /// GUI プロセスが入れ替わっても **slot は生きたまま**なので、pump の identity（slot pid）は
    /// 一致する。旧実装はそれを「変化なし」と判定して attach を skip し、**新しい GUI に過去の
    /// 画面が届かなかった**（console が黒いまま = 実機で観測）。
    ///
    /// pump が答えるべきは 2 つの別の問い:
    /// - **張り直すべきか**（server 側の生産）→ slot pid
    /// - **replay を流すべきか**（client が画面を持っているか）→ **購読の世代**
    ///
    /// 1 つの述語で兼ねていたのを分けた（[[one-predicate-three-properties]]）。
    ///
    /// 壊し方: attach 判定を `current.get(s) != Some(pid)`（pid だけ）に戻すと、②の
    /// 「再購読後に replay が届く」が落ちる。
    #[cfg(unix)]
    #[tokio::test]
    async fn reconnecting_client_gets_replay_even_when_slot_is_unchanged() {
        use super::reconcile_terminal_pumps;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-reconnect");
        let lane = addr.to_string();
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));

        // slot を 1 本立てて、識別できる出力を出しておく（= replay の中身になる）。
        {
            let mut pool = state.lane_pool.write().await;
            let (slot, rx) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn");
            pool.insert_pty_slot(addr.clone(), None, slot, rx);
            pool.write_to_lane(&addr, None, b"echo VP_RECONNECT_MARK\n")
                .expect("write");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;

        // ① 最初の client が購読 → pump が張られ replay が流れる。
        let (sub1, _rx1) = state.topic_router.subscribe(&topic).await;
        reconcile_terminal_pumps(&state, &lane).await;
        let pid_before = {
            let pumps = state.terminal_pumps.read().await;
            pumps
                .get(&lane)
                .and_then(|m| m.values().next())
                .map(|p| p.slot_pid)
        };
        assert!(pid_before.is_some(), "前提: pump が張られている");

        // ① の client に replay が届ききるのを待ってから捨てる（= 以降 live 出力は流れない）。
        // ⚠️ ここで待たないと ② の観測が **replay ではなく live 出力**を拾ってしまい、
        // pid だけの旧判定でも緑になる（テストが性質を守らない）。
        tokio::time::sleep(Duration::from_millis(800)).await;
        drop(_rx1);

        // ② client が入れ替わる（GUI 再起動）。**slot は触らない** = pid は変わらない。
        //    旧購読の掃除は QUIC idle timeout 待ちで遅れるので、ここでは外さない（実機と同じ形）。
        let (_sub2, mut rx2) = state.topic_router.subscribe(&topic).await;
        reconcile_terminal_pumps(&state, &lane).await;

        // 新しい購読者に **過去の画面（replay）が届く**こと。
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(400), rx2.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { data, .. }))) => {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                        seen.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    if seen.contains("VP_RECONNECT_MARK") {
                        break;
                    }
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        assert!(
            seen.contains("VP_RECONNECT_MARK"),
            "再購読した client に replay が届く（slot は同じでも購読者が変われば流し直す）: got={seen:?}"
        );

        // slot は張り替えていない（兄弟保護 = R2 の性質を壊していない）。
        let pid_after = {
            let pumps = state.terminal_pumps.read().await;
            pumps
                .get(&lane)
                .and_then(|m| m.values().next())
                .map(|p| p.slot_pid)
        };
        assert_eq!(
            pid_after, pid_before,
            "slot は差し替えていない（pump の張り直しだけ）"
        );
        state.topic_router.unsubscribe(sub1).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reconcile_touches_only_the_swapped_slot_leaving_siblings_alone() {
        use super::{dispatch_repo_method, reconcile_terminal_pumps};
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-scoped");
        let lane = addr.to_string();

        // root + 2 枚目。どちらも出力を持たせて replay buffer を非空にする。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("main");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
            pool.write_to_lane(&addr, None, b"echo VP_A\n")
                .expect("w root");
            pool.write_to_lane(&addr, Some(2), b"echo VP_B\n")
                .expect("w s2");
        }
        let sessions = state.lane_pool.read().await.slot_sessions(&addr);
        let root_key = *sessions.iter().find(|&&k| k != 2).expect("root key");
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 初回 demand で両方に pump を張る。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        // 初回 replay を吸い切る（ここまでは両 session に流れて正常）。
        //
        // ⚠️ 時間だけで待ってはいけない: login shell は rc 読み込みの分だけ起動が遅れ、root の
        // 初回出力が後段の観測窓へ漏れる。すると「触っていない root にも流れた」と誤検出して
        // 間欠的に落ちる（v0.60.0 の release ゲートで顕在化。product は正しく、測り方の race
        // だった）。まず **両 session の実出力を見る = readiness を待ち**、その後で無音まで
        // drain する。
        let mut warmed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let warm_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while warmed.len() < 2 && tokio::time::Instant::now() < warm_deadline {
            if let Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, .. }))) =
                tokio::time::timeout(Duration::from_millis(500), srx.recv()).await
            {
                warmed.insert(session);
            }
        }
        assert_eq!(
            warmed.len(),
            2,
            "両 session の初回出力が揃ってから観測に入る (warmed={warmed:?})"
        );
        while tokio::time::timeout(Duration::from_millis(400), srx.recv())
            .await
            .is_ok()
        {}

        // 変化が無ければ reconcile は何もしない（= 契機が重なっても pane は無傷）。
        let idle = reconcile_terminal_pumps(&state, &lane).await;
        assert_eq!(
            (idle.attached, idle.removed, idle.kept),
            (0, 0, 2),
            "無変化の reconcile は全 pump を keep する"
        );

        // session 2 の slot を差し替える（= restart / mode 切替で実際に起きること）。
        {
            let mut pool = state.lane_pool.write().await;
            let (s2b, rx2b) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2b");
            pool.insert_pty_slot(addr.clone(), Some(2), s2b, rx2b);
            pool.write_to_lane(&addr, Some(2), b"echo VP_B2\n")
                .expect("w s2b");
        }
        let swapped = reconcile_terminal_pumps(&state, &lane).await;
        assert_eq!(
            (swapped.attached, swapped.kept),
            (1, 1),
            "pid 照合で差し替わった session 2 だけ attach、root は keep"
        );

        // 以後届く出力の session stamp を集める。root が混ざったら兄弟を乱している。
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(300), srx.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, .. }))) => {
                    seen.insert(session);
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            seen.contains(&2),
            "差し替わった session 2 には replay が流れる (seen={seen:?})"
        );
        assert!(
            !seen.contains(&root_key),
            "**触っていない root には何も流れない**（lane 全体を張り直すと clear + 全 replay が \
             飛んで scroll 位置が消える）(seen={seen:?})"
        );

        // 両方の pump が生きている（片方を差し替えても他方を落とさない）。
        let pumps = state.terminal_pumps.read().await;
        let lane_pumps = pumps.get(&lane).expect("lane の pump 集合");
        assert!(
            lane_pumps.contains_key(&2) && lane_pumps.contains_key(&root_key),
            "session ごとに 1 本ずつ残る (keys={:?})",
            lane_pumps.keys().collect::<Vec<_>>()
        );
    }

    /// doc 53 R2 受け入れ条件①（doc 50 §4.7「直さないと決めた 1 件」の根治）:
    /// **demand が立った後から現れた slot** にも、次の reconcile 契機で pump が張られる。
    ///
    /// 実機の形: daemon 再起動 → GUI が購読（demand edge）→ boot 復元が 800ms×N で遅れて
    /// slot を立てる → 旧実装ではその slot に pump が張られず**永久に沈黙**した。
    /// reconcile は「復元完了 = 動詞の末尾」（lane_spawn_actor / server boot）で呼ばれ、
    /// 現在の demand（level）× 現在の slot で収束する — edge の順序に依存しない。
    #[cfg(unix)]
    #[tokio::test]
    async fn late_restored_slot_gets_pump_on_next_reconcile() {
        use super::{dispatch_repo_method, reconcile_terminal_pumps};
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-late");
        let lane = addr.to_string();

        // boot 途中の姿: root slot だけが立った時点で GUI が購読 → demand edge が先に立つ。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("main");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
        }
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(
            state
                .terminal_pumps
                .read()
                .await
                .get(&lane)
                .map(|m| m.len()),
            Some(1),
            "edge 時点では root の 1 本だけ"
        );

        // 復元の続き: session 2 の slot が後から立つ（edge はもう来ない）。
        {
            let mut pool = state.lane_pool.write().await;
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
            pool.write_to_lane(&addr, Some(2), b"echo VP_LATE_MARK\n")
                .expect("w s2");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 復元完了の契機（lane_spawn_actor / server boot 相当）→ 不足分だけ attach。
        let r = reconcile_terminal_pumps(&state, &lane).await;
        assert_eq!(
            (r.attached, r.removed, r.kept),
            (1, 0, 1),
            "後から現れた slot だけ attach、既存 root pump は無傷"
        );

        // session 2 の出力（replay 込み）が届く = 沈黙しない。
        let mut seen2 = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(400), srx.recv()).await {
                Ok(Some((
                    _t,
                    RepoMessage::LaneTerminalOutput {
                        session: 2, data, ..
                    },
                ))) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    seen2.push_str(&String::from_utf8_lossy(&bytes));
                    if seen2.contains("VP_LATE_MARK") {
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            seen2.contains("VP_LATE_MARK"),
            "後から復元された slot の出力が届く（旧実装は永久に沈黙）(seen={seen2:?})"
        );
    }

    /// PtySlot を持たない Lane への terminal_write は Err (lane 不在を上位に伝える)。
    #[tokio::test]
    async fn terminal_write_unknown_lane_errs() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;

        let state = build_test_app_state(None).await;
        let data = base64::engine::general_purpose::STANDARD.encode(b"x");
        let res = dispatch_repo_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": "vp/main", "data": data }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への write は Err");
    }

    /// tmux decoupling PR1-2: lane_nudge dispatch の error 経路 3 種
    /// (lane 未指定 / parse 失敗 / lane 不在 = PtySlot 無)。 happy path は実機検証済 (design §13.6)。
    #[tokio::test]
    async fn lane_nudge_dispatch_error_paths() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

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

    /// doc 38: session param（additive）の入口検証。省略/null は OK（focused に解決）、
    /// 型不正・0 は Err — 黙って focused に落とすと誤配送になる。
    #[test]
    fn payload_session_key_validates_additive_param() {
        use super::payload_session_key;
        // 省略 / null = None（後方互換の要）。
        assert_eq!(payload_session_key("t", &serde_json::json!({})), Ok(None));
        assert_eq!(
            payload_session_key("t", &serde_json::json!({"session": null})),
            Ok(None)
        );
        assert_eq!(
            payload_session_key("t", &serde_json::json!({"session": 2})),
            Ok(Some(2))
        );
        // 0 / 負数 / 文字列 / 小数は Err。
        for bad in [
            serde_json::json!({"session": 0}),
            serde_json::json!({"session": -1}),
            serde_json::json!({"session": "2"}),
            serde_json::json!({"session": 1.5}),
        ] {
            assert!(
                payload_session_key("t", &bad).is_err(),
                "不正な session は Err: {bad}"
            );
        }
    }

    /// tmux decoupling PR2 → capture error 明確化（2026-07-19）: lane_capture dispatch の error 経路。
    /// 未指定 / parse 不能 / pool 不在（lane 不在）/ chat mode lane（console 無しが正常）を分岐して返す。
    #[tokio::test]
    async fn lane_capture_dispatch_error_paths() {
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{Diff, LaneAddress, LaneInfo, LaneState, SystemEvent};
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{Diff, LaneAddress, LaneInfo, LaneState, SystemEvent};
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

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

    /// F6④: agents_list dispatch — repo-proxy ask が `{agents:[...]}` 形で返る。
    /// list_stands_cached は mise 不在 (CI) でも空 Vec に graceful degrade するので、 配線 +
    /// wire shape (agents array 常在) を CI でも固定できる (実 agent 内容は stands.rs の
    /// mise-gated test が担保)。
    #[tokio::test]
    async fn stands_list_returns_stands_array() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(&state, "agents_list", serde_json::json!({}))
            .await
            .expect("agents_list dispatch");
        assert!(
            res.get("agents").map(|s| s.is_array()).unwrap_or(false),
            "agents_list は {{agents:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lanes_list` dispatch arm が `{lanes:[...]}` 形で返る (build_lanes_snapshot 経由)。
    #[tokio::test]
    async fn lanes_list_returns_lanes_array() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

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
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let err = dispatch_repo_method(
            &state,
            "lane_create",
            serde_json::json!({ "name": crate::repo::lanes_state::ROOT_LANE_NAME }),
        )
        .await
        .expect_err("予約名は Err");
        // doc 44 §9: 判定は `validate_sub_name` に一本化された（両経路で同じ gate）。
        // message は同関数のものになるので、予約名を名指ししていることだけを見る。
        assert!(
            err.contains(crate::repo::lanes_state::ROOT_LANE_NAME) && err.contains("reserved"),
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

    // =========================================================================
    // Agent 委譲 (doc 28 §4) の repo dispatch — early validation のみ。
    // 状態遷移ロジックは daemon 中央 store に移管したため (doc 28 §6)、その単体 test は
    // `capability::delegation_store` が担う。repo handler は必須 field 検証後に daemon へ proxy
    // する (daemon_wire::call) ので、ここでは Daemon 不要な早期 Err 経路だけを固定する。
    // =========================================================================

    /// delegate/complete/respond の必須 field 欠落 / 不正 outcome は Daemon 到達前に Err。
    #[tokio::test]
    async fn delegation_dispatch_validates_before_proxy() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // delegate: doer 欠落 → Err (proxy 前)。
        assert!(
            dispatch_repo_method(
                &state,
                "delegate",
                serde_json::json!({ "task": "x", "requester": "agent@vp" }),
            )
            .await
            .is_err(),
            "delegate doer 欠落は Err"
        );
        // complete: id 欠落 → Err。
        assert!(
            dispatch_repo_method(
                &state,
                "complete",
                serde_json::json!({ "outcome": { "kind": "done", "result": "x" } }),
            )
            .await
            .is_err(),
            "complete id 欠落は Err"
        );
        // complete: outcome の kind が未知 → from_value で Err (proxy 前)。
        assert!(
            dispatch_repo_method(
                &state,
                "complete",
                serde_json::json!({ "id": "dlg-x", "outcome": { "kind": "weird" } }),
            )
            .await
            .is_err(),
            "complete 不正 outcome は Err"
        );
        // respond: answer 欠落 → Err。
        assert!(
            dispatch_repo_method(&state, "respond", serde_json::json!({ "id": "dlg-x" }))
                .await
                .is_err(),
            "respond answer 欠落は Err"
        );
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
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
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
