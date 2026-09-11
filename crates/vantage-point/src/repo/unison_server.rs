//! repo "process" channel の受付（doc 61 §1）。
//!
//! `dispatch_repo_method` が 72 method の match 1 枚で、各 arm は owner / `*_ops` module の handler を
//! 呼ぶだけ（`board` / `editor_bridge` / `conversation_replay` / `conversation_ops` / `terminal_ops` /
//! `lane/ops` / `process_ops` / `wire_relay` / `agents` / `delegation`）。ここに残るのは
//! 受付の続き（`handle_process_message` = pane ops の generic relay）と、群ごとに `None` の意味が違う
//! `payload_session_key`、および `QUIC_PORT_OFFSET`。
//!
//! 唯一の呼び手は `repo_registry.rs`（daemon の repo-proxy → in-process dispatch、doc 45 §5.2 の
//! 単一 stream 逐次）。arm の中で spawn しない。
//!
//! ポート: HTTP と同一ポート番号を使う。 HTTP は TCP・QUIC は UDP で OS レベルの
//! ポート名前空間が独立しているため衝突しない (`QUIC_PORT_OFFSET = 0`)。

use std::sync::Arc;

use super::board;
use super::conversation_ops;
use super::conversation_replay;
use super::editor_bridge;
use super::lane;
use super::process_ops;
use super::state::RepoState;
use super::terminal_ops;
use super::wire_relay;
use crate::protocol::RepoMessage;

/// QUIC ポートのオフセット（HTTP ポートからの差分）
/// TCP (HTTP) と UDP (QUIC) は OS レベルで独立 → 同一ポートで共存可能
pub const QUIC_PORT_OFFSET: u16 = 0;

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
    state: &RepoState,
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

/// repo "process" channel の method dispatch（単一の入口）。
///
/// 経路は 1 本: MCP / GUI → daemon の repo-proxy → `RepoRuntimes::dispatch`（`repo_registry.rs`）
/// → 本 fn（in-process 直呼び）。doc 44 P1 fold-in で repo は listener を持たなくなったので、
/// 旧「repo 直結の channel handler」経路は無い。
pub(crate) async fn dispatch_repo_method(
    state: &Arc<RepoState>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        // switch_lane も generic broadcast 経路に乗せる（B1: 遠隔 active Lane 制御）。
        // hub → topic `repo/board/event/switch-lane`（一時コマンド=非
        // retained）→ canvas channel → vp-app が受信して active Lane を切り替える。
        // board モデル (2026-07-15): show/clear は repo-authoritative な board 経路へ。
        // item を DB に durable append し、 更新後 board を BoardUpdated(retained) で broadcast する。
        "show" | "clear" => board::handle_canvas_command(state.board(), payload).await,
        // doc 52 §5: id 指定 in-place 置換（read-first、id 不在は loud error）
        "board_update" => board::handle_board_update(state.board(), payload).await,
        // doc 52 §4/§5: 呼び出し元 lane の board を id 付き全文で返す（中継台 + identity lookup）
        "read_board" => board::handle_board_read(state.board(), payload).await,
        // doc 48 Phase 2: editor bridge (MCP → GUI request-response)
        "editor_fields" | "editor_values" | "editor_set" => {
            editor_bridge::handle_editor_command(state.editor(), method, payload).await
        }
        // doc 49 LE-P2 PR2: layout bridge (LE-15)。editor bridge と同じ配管を op を変えて共用
        // (method に editor_ prefix が無いので op = method のまま vp-app に届く)
        "layout_get" | "layout_set" | "layout_history" => {
            editor_bridge::handle_editor_command(state.editor(), method, payload).await
        }
        "editor_result" => editor_bridge::handle_editor_result(state.editor(), payload).await,
        "toggle_pane" | "split_pane" | "close_pane" | "switch_lane" => {
            handle_process_message(state, payload)
        }
        "watch_file" => process_ops::handle_watch_file(state, payload).await,
        "unwatch_file" => process_ops::handle_unwatch_file(state, payload).await,
        // S2: demand-driven terminal pump (Daemon demand hook → control reverse-route)
        // doc 53 R2: start / stop は同じ reconcile の契機（demand の今は level で読む）。
        "terminal_demand_start" | "terminal_demand_stop" => {
            terminal_ops::handle_terminal_demand(state, payload).await
        }
        // gui replay-on-attach: chat lane の transcript を attach 時に replay
        "conversation_demand_start" => {
            conversation_replay::handle_conversation_demand_start(state, payload).await
        }
        "conversation_demand_stop" => {
            conversation_replay::handle_conversation_demand_stop(state, payload).await
        }
        // S3: terminal 入力/resize (surface → canvas channel upstream → control reverse-route)
        "terminal_write" => terminal_ops::handle_terminal_write(state, payload).await,
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
        "lane_nudge" => lane::ops::handle_lane_nudge(state, payload).await,
        // tmux decoupling PR2: lane console capture (旧 tmux capture-pane の native 代替)
        "lane_capture" => lane::ops::handle_lane_capture(state, payload).await,
        // doc 46 P5: lane が持つ PTY slot の一覧（UI を通さない slot 枚数の読み手）
        "lane_slots" => lane::ops::handle_lane_slots(state, payload).await,
        // doc 46 P5 producer: 新 session を採番して console を 1 枚立てる（`lane_slots` の書き手）
        "lane_slot_new" => lane::ops::handle_lane_slot_new(state, payload).await,
        "terminal_resize" => terminal_ops::handle_terminal_resize(state, payload).await,
        // board モデル (2026-07-15): webview からの board mutate（thumbnail ✕ / Clear ボタン）。
        // 旧 pp_state_save/load は撤去（board は repo truth、 webview は BoardUpdated 購読 + mutate へ）。
        "board_delete_item" => board::handle_board_delete_item(state.board(), payload).await,
        "board_clear" => board::handle_board_clear(state.board(), payload).await,
        // cursor の server 昇格（doc 52 §5 計器盤）: thumbnail click / scrollback の注視を repo truth に。
        "board_set_cursor" => board::handle_board_set_cursor(state.board(), payload).await,
        // lanes portless: Lane create/list (旧 SP HTTP POST/GET /api/lanes を repo-proxy ask に移管)
        "lane_create" => lane::ops::handle_lane_create(state, payload).await,
        "lanes_list" => lane::ops::handle_lanes_list(state).await,
        // F6②: Lane delete (旧 SP HTTP DELETE /api/lanes を repo-proxy ask に移管)
        "lane_delete" => lane::ops::handle_lane_delete(state, payload).await,
        // F6③: Lane restart (旧 SP HTTP POST /api/lanes/restart を repo-proxy ask に移管)
        "lane_restart" => lane::ops::handle_lane_restart(state, payload).await,
        // 供給 push 根治: hook → daemon 経由の session pointer 変化通知（Diff::Update push の起点）
        "lane_session_changed" => lane::ops::handle_lane_session_changed(state, payload).await,
        // doc 44 D4: Repo Host の帳簿 — 開発起点ポインタの読み書き
        "lane_origin_get" => lane::ops::handle_lane_origin_get(state).await,
        "lane_origin_set" => lane::ops::handle_lane_origin_set(state, payload).await,
        "lane_order_set" => lane::ops::handle_lane_order_set(state, payload).await,
        // F6④: Agent 一覧 (旧 SP HTTP GET /api/agents を repo-proxy ask に移管)
        "agents_list" => super::agents::handle_stands_list().await,
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
        "process_run" => process_ops::handle_process_run(state, payload).await,
        "process_stop" => process_ops::handle_process_stop(state, payload).await,
        "process_inject" => process_ops::handle_process_inject(state, payload).await,
        "process_list" => process_ops::handle_process_list(state).await,
        // L0 portless Group B-3: Ruby VM (旧 SP HTTP /api/ruby/* を repo-proxy ask に移管)。
        // ruby_list は process_registry.list() = process_list と同一なので handle_process_list 再利用。
        "ruby_eval" => process_ops::handle_ruby_eval(state, payload).await,
        "ruby_run" => process_ops::handle_ruby_run(state, payload).await,
        "ruby_stop" => process_ops::handle_ruby_stop(state, payload).await,
        "ruby_list" => process_ops::handle_process_list(state).await,
        // wiremsg threaded inbox (Phase A ①、 R2 で wire_thread 追加)
        "wire_send" => wire_relay::handle_wire_send(state, payload).await,
        "wire_recv" => wire_relay::handle_wire_recv(state, payload).await,
        "wire_thread" => wire_relay::handle_wire_thread(state, payload).await,
        // flow_progress 用 read-only 未読 count (cursor 不触り)
        "wire_unread_count" => wire_relay::handle_wire_unread_count(state, payload).await,
        // flow_progress 5-state FSM derive 用 read-only 最新 wmsg
        "wire_latest_msg" => wire_relay::handle_wire_latest_msg(state, payload).await,
        // flow_progress AwaitingUser 判定用 read-only 未 ack needs_user
        "wire_needs_user_pending" => {
            wire_relay::handle_wire_needs_user_pending(state, payload).await
        }
        "wire_ack" => wire_relay::handle_wire_ack(state, payload).await,
        // Agent 委譲 (doc 28 §4): delegate=B を wake / complete=A を wake /
        // respond=NeedsInput(Reborn) に A が回答して B を再 wake (Active へ loop)。
        "delegate" => super::delegation::handle_delegate(state, payload).await,
        "complete" => super::delegation::handle_complete(state, payload).await,
        "respond" => super::delegation::handle_respond(state, payload).await,
        _ => Err(format!("不明なメソッド: process.{}", method)),
    }
}

#[cfg(test)]
mod tests {
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

        let state = build_test_app_state().await;
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
}
