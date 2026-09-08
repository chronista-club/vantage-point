//! **boot 窓の catch-up** = `AppEvent::WebviewReady`（webview が受け口を全部生やした合図）で、
//! それまでに届いていた state を **1 つの list** で撃ち直す（doc 60 §3 の不変条件）。
//!
//! 新しい面（pane / push）を足したら、その replay は **ここに足す**（module を跨いで散らさない）。
//! 旧 `run()` の match arm を移したもの（doc 60 §6 6-2 PR-9、2026-09-08）。本体は arm の中身を
//! 12 空白 dedent しただけ、arm の前のコメントは fn の doc へ移動。
//!
//! 触る state（read）: `ui.sidebar_state` / `ui.persist.session` / `ui.board_snapshots`。
//! resource: `boot.webview` / `boot.rt_handle` / `boot.daemon_conn`。
//! ⚠️ 現状 `push_sidebar_state` を撃たない（後続 tick 頼み）— 6-2b b-5 で足す。

use super::boot::Boot;
use super::lane_view::{
    lane_is_chat, push_active_view, push_session_list, resolve_repo_path_for_lane,
    session_list_payload, term_sessions_of,
};
use super::state::UiState;
use crate::daemon::conn::daemon_repo_request;
use crate::webview::push_main;

/// webview が「受け口を全部生やした」と名乗った（`entry.tsx` の `t:"ready"`）。
///
/// ## これは catch-up ではなく **replay**
///
/// bundle 評価前に Rust が撃った押し込みは、受け口 (`window.vpDispatch`) が居ないので
/// 届かない。ここで**現在の状態を丸ごと撃ち直す**ことでそれを埋める。全部 idempotent /
/// 全量置き換えなので、二重に撃っても壊れない（level 駆動）。
///
/// ⚠️ 以前は同じことを **feature ごとの pull 3 本**（`lanes:ensure-all` /
/// `bastet:devices_fetch` / `board:demand`）でやっていた。面を足すたびに pull を 1 本
/// 足す形で、しかも**その面が install された後**に撃つ順序制約が JS 側に散っていた。
/// 「webview が生まれた」という事実は 1 つなので、signal も 1 本に畳んである。
/// 新しい面を足したら **ここに replay を 1 行足す**（新しい IPC tag は要らない）。
pub(super) fn webview_ready(ui: &mut UiState, boot: &Boot) {
    // terminal S4: JS xterm instance の catch-up 再発行のみ (repo port 不要)。
    // terminal session 自体は LanesLoaded reconcile が管理するのでここでは触らない。
    for (_repo_path, lanes) in ui.sidebar_state.lanes_by_repo.clone().iter() {
        for lane in lanes {
            // doc 50 §4.6 A6: gate は term session の有無（LanesLoaded と同じ規則 —
            // lane 単位の pid / mode で切ると root=chat の lane の非 root term が
            // 落ちる）。ensureLane は idempotent なので catch-up で撃ち直してよい。
            let addr_str = lane.address.key();
            for (session, is_root) in term_sessions_of(lane) {
                push_main::ensure_lane(&boot.webview, &addr_str, session, is_root);
            }
            // doc 53 §11: **roster も同じ窓で落ちる**（team-b 指摘 2026-07-25）。
            //
            // roster が push 型になった以上、`ensure_lane` / `push_active_view` /
            // device 一覧と同じ boot race を持つ: bundle **評価前**の押し込みは
            // 受け口（`window.vpDispatch`）が居ないので届かないのに、Rust 側は
            // 「送った」として指紋を残す → その lane の roster が**実際に変わるまで
            // 二度と push されない**（tab strip / pane grid / picker が空のまま）。
            // 供給が fetch だった頃は「JS が能動的に取りに行く」ので原理的に
            // 起きなかった窓 — 供給路を変えたことの随伴。
            //
            // ここは JS が ready を名乗った後なので、**指紋を無視して撃ち直す**
            // （送った値は同じなので指紋の更新は不要）。
            if let Some(sessions) = lane.sessions.as_ref() {
                push_session_list(
                    &boot.webview,
                    &addr_str,
                    &session_list_payload(&addr_str, sessions),
                );
            }
            // **terminal の replay も同じ窓で落ちる**（2026-07-26 実測）。
            //
            // 上のコメントが列挙する「同じ boot race を持つもの」に terminal が
            // 並んでいなかった。実測した時刻:
            //   02:11:42.886  replay が client に到着（= evaluate_script が撃たれる）
            //   02:11:43.284  bundle init complete（**0.4 秒後**）
            // `window.vpTerminal` が未定義の間の `evaluate_script` は silent no-op で、
            // terminal の replay は**一度きり**なので二度と来ない → console が黒いまま。
            //
            // ここは JS が ready を名乗った後なので、demand を撃ち直して replay を
            // 取り直す（server 側は `terminal_demand_start` → `reconcile_lane` で
            // 冪等 — 既に張られていれば pump は kept、replay だけが流れ直す）。
            if !term_sessions_of(lane).is_empty()
                && let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &addr_str)
            {
                let lane_for_req = addr_str.clone();
                let conn = boot.daemon_conn.clone();
                boot.rt_handle.spawn(async move {
                    if let Err(e) = daemon_repo_request(
                        &conn,
                        &path,
                        "terminal_demand_start",
                        // `replay: true` = 「画面を持っていないので流し直して」。
                        // 準備前に届いた replay を捨てているので、server 側の
                        // 「変化なし」判定を明示要求で越える（doc 53 §6.5.0）。
                        serde_json::json!({ "lane": lane_for_req, "replay": true }),
                    )
                    .await
                    {
                        tracing::debug!(
                            "WebviewReady: terminal demand 再要求に失敗（次の契機で再試行）: {e}"
                        );
                    }
                });
            }
        }
    }
    // 現在 active な Lane を再度 show する (lane-empty placeholder を解除する保険)
    if let Some(addr) = ui.sidebar_state.active_lane_address.clone() {
        let is_chat = lane_is_chat(&ui.sidebar_state, &addr);
        push_main::show_lane(&boot.webview, Some(&addr), is_chat);
        // 起動 race で silent drop されるのは ensureLane だけではない。 auto-select の
        // activate_lane が撃つ setActivePane も同じ窓で落ちるが、これが JS 側の
        // 「active lane」を埋める唯一の経路 — showLane だけ再発行しても JS の active
        // lane は null のままなので、lane 文脈を要する操作が「active lane 不明」で
        // 早期 return する。冪等なので毎回再発行して JS 側 state を確定させる。
        //
        // doc 50 §4.6 A6: lane 単位 mode の catch-up は退役（lane 単位 mode が
        // 消滅）。roster の catch-up は上の lane ループが撃つ（doc 53 §11 — push 型に
        // なって以降、この経路にも roster が要る）。
        push_active_view(&boot.webview, &ui.sidebar_state);
    }
    // 計器盤: daemon-device の接続時 snapshot は bundle ロード前に届いて落ちている
    // （sidebar の Devices badge は state 再 push で生きるが pane だけ空、2026-07-23
    // 実機で確認）。保持済み state から全量で撃ち直す。
    push_main::render_devices(&boot.webview, &ui.sidebar_state.devices);
    // shell (L|main|R) の形: 保存があれば復元する。無ければ撃たない
    // （webview の既定値が残る = 既定を 2 箇所に書かない）。
    // ⚠️ 撃った/撃たなかったを**両方**残す。「行が無い」は「保存が無かった」とも
    // 「ここに来ていない」とも読めてしまい、実機の切り分けで 1 往復損する
    // （2026-08-06 に実際に損した）。
    match ui.persist.session.shell_layout().cloned() {
        Some(layout) => {
            tracing::info!("shell layout 復元: {layout:?}");
            push_main::shell_layout(&boot.webview, &layout);
        }
        None => tracing::info!("shell layout 復元: 保存なし（既定のまま）"),
    }
    // 掲示板: retained BoardUpdated も同じ窓で落ちる（doc 52 §10 wave 0）。
    // 保持分を撃ち直す（落ちたままだと reopen で board pane が出ず、次の live show
    // まで空のまま）。
    //
    // ⚠️ **全 repo 分を撃つ**。active repo だけに絞っていた旧実装は、bundle 再評価後に
    // 別 repo へ切り替えると board が空のままだった（webview は `(repo, lane)` で
    // 箱を持つので、届いていない repo の箱は作られない）。message には repo が
    // stamp 済なので、まとめて配っても混ざらない。
    for boards in ui.board_snapshots.values() {
        for message in boards.values() {
            push_main::board_message(&boot.webview, message.clone());
        }
    }
    // LanesLoaded のたびに follow up 発火する loop event のため log omit。
}
