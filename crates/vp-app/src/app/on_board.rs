//! event handler: **board / canvas / editor**（gui channel の canvas message = board snapshot の保持と
//! 投影、ROTO 等からの switch_lane、editor bridge の JS 評価、board の mutate ask）。
//!
//! 旧 `run()` の match arm を fn ごとに移したもの（doc 60 §6 6-2 PR-8、2026-09-08）。本体は arm の
//! 中身を 12 空白 dedent しただけ、arm の前のコメントは fn の doc へ移動。
//!
//! 触る state: `ui.board_snapshots`（所有者）、`ui.sidebar_state`（canvas 未読 / active lane）、
//! `ui.session_state` / `ui.guards.lane_respawn_triggered` / `ui.sessions.conversation_sessions`
//! （`activate_lane` + `ensure_conversation_attach` 経由 = 群を跨ぐ）、`ui.win.is_focused`（read、
//! Model B の self-filter）。resource: `boot.webview` / `boot.rt_handle` / `boot.daemon_conn`。

use tao::event_loop::EventLoopProxy;

use super::boot::Boot;
use super::lane_view::{activate_lane, ensure_conversation_attach, mark_lane_canvas_unread};
use super::state::UiState;
use crate::daemon::conn::daemon_repo_request;
use crate::daemon::pollers::resolve_active_repo_path;
use crate::events::AppEvent;
use crate::webview::push_main;

pub(super) fn editor_eval(
    _ui: &mut UiState,
    boot: &Boot,
    js: String,
    resp: tokio::sync::mpsc::UnboundedSender<String>,
) {
    // doc 48 Phase 2: editor bridge の webview 評価。結果 (wry が JSON 文字列化
    // した評価値) を canvas session 側へ返す。受信側は timeout で打ち切るので
    // callback が遅れて発火しても送信は無害 (受け手 drop 済なら send Err → 無視)。
    if let Err(e) = boot
        .webview
        .evaluate_script_with_callback(&js, move |result| {
            let _ = resp.send(result);
        })
    {
        tracing::warn!("editor bridge: evaluate_script 失敗: {}", e);
    }
}

pub(super) fn canvas_message(
    ui: &mut UiState,
    boot: &Boot,
    respawn_proxy: &EventLoopProxy<AppEvent>,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    repo_path: String,
    message: serde_json::Value,
) {
    // wiremsg Stage 2: repo の "canvas" channel から受信した RepoMessage。
    // active repo の分のみ main area の Board body に転送する
    // （**board_updated だけは例外** — 下記）。
    // active 判定: active_lane_address の repo segment == repo_path の basename。
    let active_repo = ui
        .sidebar_state
        .active_lane_address
        .as_deref()
        .and_then(|addr| addr.split('/').next());
    let msg_repo = std::path::Path::new(&repo_path)
        .file_name()
        .and_then(|s| s.to_str());
    // ⚠️ **board_updated には送信元 repo を stamp する**（repo 側の BoardUpdated は
    // 持っていない）。board の同一性は `(repo, lane)` の対で、全 repo の root lane が
    // 同じ `'main'` を名乗る。repo を落として webview に渡していた旧実装は
    // 13 repo が 1 つの箱を奪い合い、「board 行を持たない repo に切り替えると前の
    // repo の board が出たまま」になっていた（2026-08-04 根治）。
    let mut message = message;
    let is_board_update = message.get("type").and_then(|t| t.as_str()) == Some("board_updated")
        && message.get("scope").and_then(|s| s.as_str()) == Some("lane");
    if is_board_update
        && let Some(proj) = msg_repo
        && let Some(obj) = message.as_object_mut()
    {
        obj.insert(
            "repo".to_string(),
            serde_json::Value::String(proj.to_string()),
        );
    }
    // board pane の boot 窓救済（doc 52 §10 wave 0）: BoardUpdated を repo × lane で
    // 保持する。`AppEvent::WebviewReady` の replay で再配信し、retained が bundle 評価前に
    // 落ちた分を埋める。lane 欠落 = main（board-handler の flat key と一致）。
    //
    // ⚠️ scope=="lane" のみ buffer する（消費側 board-handler.ts `applyBoardUpdated` の
    //   `if (msg.scope !== 'lane') return` と対称にする）。退役済み scope="proj" の孤児行も
    //   seed_boards が無条件 broadcast し、board_key() で proj も main lane も
    //   broadcast_lane=None → lane_key="main" に衝突する。scope guard が無いと、行順
    //   次第で proj 孤児が本物の lane board を上書きし、replay が「JS が捨てる死んだ
    //   message」を配って boot 窓 regression が再発する（team-b review 2026-07-24）。
    if is_board_update && let Some(proj) = msg_repo {
        let lane_key = message
            .get("lane")
            .and_then(|l| l.as_str())
            .unwrap_or("main")
            .to_string();
        ui.board_snapshots
            .entry(proj.to_string())
            .or_default()
            .insert(lane_key, message.clone());
    }
    // B1 + cross-project: switch_lane は board content ではなく active Lane 切替コマンド。
    // active を「変える」コマンドなので、active repo guard の **外**で処理する
    // （別 repo の repo から来た switch_lane こそ通す）。送信元 repo の repo
    // (= msg_repo) の lane を activate し、sidebar / main area を追随させる。
    if message.get("type").and_then(|t| t.as_str()) == Some("switch_lane") {
        if let (Some(repo), Some(token)) = (msg_repo, message.get("lane").and_then(|l| l.as_str()))
        {
            // token → lane address（形式は `address_from_lane_token` の 1 箇所）
            let address = crate::lane_address::address_from_lane_token(repo, token);
            // Model B (focus = 操舵ポインタ): switch_lane は全 instance に broadcast される
            // が、適用するのは **focused instance だけ**。非 focus の window はこの event を
            // 無視し、自分の lane に park されたまま (= 2 window が別々の lane を同時に見られる)。
            if ui.win.is_focused {
                activate_lane(
                    &address,
                    &mut ui.sidebar_state,
                    &mut ui.persist,
                    &boot.webview,
                    &mut ui.guards.lane_respawn_triggered,
                    &boot.rt_handle,
                    respawn_proxy,
                    &boot.daemon_conn,
                );
                // gui: chat lane なら conversation topic に attach（→ transcript replay）。
                ensure_conversation_attach(
                    &address,
                    &ui.sidebar_state,
                    &mut ui.sessions.conversation_sessions,
                    &boot.rt_handle,
                    async_action_proxy,
                    &boot.daemon_conn,
                );
            } else {
                tracing::debug!("switch_lane skip (not focused): address={}", address);
            }
        }
    } else if is_board_update || (active_repo.is_some() && active_repo == msg_repo) {
        // ⚠️ **board_updated は active repo でなくても流す**。webview が `(repo, lane)` で
        // 箱を分けるようになったので、全 repo 分を持たせておけば repo 切替が
        // 「キーを差し替えるだけ」で済む（撃ち直しの経路が要らない）。
        // active repo に絞っていた旧実装は、切替先の board を **一度も届けない**まま
        // 前の repo の箱を見せていた（bug の後半）。
        // board 以外（switch_lane を除く content）は従来どおり active repo のみ。
        match serde_json::to_value(&message) {
            Ok(json) => push_main::board_message(&boot.webview, json),
            Err(e) => {
                tracing::warn!("CanvasMessage serialize 失敗: {}", e);
            }
        }
    }

    // board 着信 badge: show が現在 active でない lane に着いたら sidebar に
    // canvas_unread を計上する。別 repo / 別 lane（同 repo だが別 lane）の両ケースを
    // 1 箇所で拾う（上の forward guard とは独立）。active lane 宛の show は board pane 側
    // （board-handler.ts の presence → lane-panes、doc 52 §10 wave 0）で解決する。
    if message.get("type").and_then(|t| t.as_str()) == Some("show")
        && let Some(repo) = msg_repo
    {
        let token = message
            .get("lane")
            .and_then(|l| l.as_str())
            .unwrap_or(crate::lane_address::ROOT_LANE_NAME);
        // token → lane address（switch_lane と同じ helper を通す）。
        let address = crate::lane_address::address_from_lane_token(repo, token);
        if ui.sidebar_state.active_lane_address.as_deref() != Some(address.as_str()) {
            mark_lane_canvas_unread(&address, &mut ui.sidebar_state, &boot.webview);
        }
    }
}

pub(super) fn board_mutate(ui: &mut UiState, boot: &Boot, method: String, body: serde_json::Value) {
    // board モデル (2026-07-15): WebView の board mutate（thumbnail ✕ / Clear ボタン）を
    // daemon repo-proxy ask で active repo の repo に forward する。 repo が DB を更新して
    // BoardUpdated(retained) を broadcast し、 canvas channel 経由で webview の board が
    // 更新される（webview は truth を持たず repo の反映を待つ view）。 active repo 解決
    // 失敗は silent skip。
    let Some(path) = resolve_active_repo_path(&ui.sidebar_state) else {
        tracing::debug!("board mutate skip — active repo 解決失敗");
        return;
    };
    let conn = boot.daemon_conn.clone();
    boot.rt_handle.spawn(async move {
        match daemon_repo_request(&conn, &path, &method, body).await {
            Ok(_) => tracing::debug!("board mutate ({}) → Daemon OK", method),
            Err(e) => tracing::warn!("board mutate ({}) 失敗: {}", method, e),
        }
    });
}
