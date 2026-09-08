//! event handler: **lanes**（repo 一覧と lane snapshot の到着 = sidebar の model 更新、購読の起床、
//! terminal / conversation session の reconcile、boot 窓の active lane 復元、sub lane 作成結果）。
//!
//! 旧 `run()` の match arm を fn ごとに移したもの（doc 60 §6 6-2 PR-7、2026-09-08）。本体は arm の
//! 中身を 12 空白 dedent しただけ、arm の前のコメントは fn の doc へ移動。
//!
//! 触る state（この module が **所有者**）: `ui.sidebar_state`（processes / lanes_by_repo / origin /
//! lane_sub_state / currents_order / active_lane）、`ui.guards.*`（購読 guard / spawn dedup / roster 指紋）、
//! `ui.sessions.*`（reconcile）。session file と復元 cursor は `ui.persist` が所有（doc 60 §8）— ここは
//! `restore_active_lane` / `observe_daemon_active_lane` / `activate` で頼むだけ。
//! resource: `boot.webview` / `boot.rt_handle` / `boot.daemon_conn` / `boot.instance_index`。

use tao::event_loop::EventLoopProxy;

use super::boot::Boot;
use super::lane_view::{
    activate_lane, ensure_conversation_attach, forget_roster_push, header_lane_fields_changed,
    push_active_view, push_session_list, remember_roster_push, roster_push_needed,
    session_list_payload, term_sessions_of,
};
use super::state::UiState;
use crate::daemon::subscriptions::{spawn_canvas_subscription, spawn_lanes_subscription};
use crate::events::AppEvent;
use crate::lane::terminal::spawn_terminal_session;
use crate::pane::RepoPaneState;
use crate::webview::push_main;
use crate::webview::push_sidebar::{self, push_sidebar_state};

pub(super) fn repos_loaded(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    repos: Vec<crate::daemon_wire::RepoInfo>,
) {
    // 既存 SidebarState とマージ:
    //  - 同じ path があれば既存 state を維持 (expanded / panes / active 保持)
    //  - 新規は RepoPaneState::new (Main Agent 1 つ)
    //  - サーバから消えた repo は除外
    //
    // VP-101 follow-up: register 後の auto-expand。
    // auto-select は LanesLoaded 側で扱う (Architecture v4: 真の selection unit は Lane)。
    // 「prev (旧 sidebar_state.processes) には port があった、 新 repos には port が無い」
    // 形の merge は port を不用意に消すので、 sidebar_state の port は新側 (port_by_name 反映済)
    // で上書きされる。 retroactive ensureLane (= 後段) で None→Some 遷移を補う。
    let prev: std::collections::HashMap<String, RepoPaneState> = ui
        .sidebar_state
        .processes
        .drain(..)
        .map(|p| (p.path.clone(), p))
        .collect();
    let is_initial_load = prev.is_empty();
    // Phase A4-3b: drain 前に (path → port) を retain して fetch task に渡す
    let repo_ports: Vec<(String, Option<u16>)> =
        repos.iter().map(|p| (p.path.clone(), p.port)).collect();
    // Model Q: daemon canonical の active lane (presence、 boot 復元用)。
    // 注: app の active_lane_address は単一 global (pane.rs) なので、 daemon の
    // per-repo active のうち **order 先頭の 1 つ**を採用する (意図的な単純化、
    // doc 24 §12-H)。 repo ごとに最後の active を復元する per-repo 化は
    // Phase 3 (app 側を per-repo active に拡張、 daemon は既に per-repo 保持)。
    let daemon_active_lane: Option<String> = repos.iter().find_map(|p| p.active_lane.clone());
    ui.sidebar_state.processes = repos
        .into_iter()
        .map(|p| {
            // RepoInfo.state / .port を RepoPaneState に merge
            // (sidebar JS が processStateMark で 🟢/🔴 badge 表示に使う、
            //  port は Phase 2 で lane:select 時の WS 接続先決定に使う)
            let state_str = p.state.as_str().to_string();
            let port = p.port;
            let mut pane_state = if let Some(existing) = prev.get(&p.path) {
                existing.clone()
            } else {
                // 新規 repo の expanded 解決:
                //   1. session file に saved 値があれば最優先 (vp-app 再起動の復元)
                //   2. 上記 None かつ session 中の追加 (= 初回 fetch ではない) なら auto-expand
                //   3. 初回 fetch の新規は閉じた状態
                let mut s = RepoPaneState::new(p.path.clone(), p.name.clone());
                s.expanded = ui
                    .persist
                    .session
                    .repo_expanded(&p.path)
                    .unwrap_or(!is_initial_load);
                s
            };
            pane_state.state = Some(state_str);
            pane_state.port = port;
            pane_state
        })
        .collect();
    // Phase 1 (doc 24): currents_order を daemon の repo_order (= fetch 順) の
    // mirror にする。これで currents_order は独立 SSOT ではなく canonical の派生となり、
    // JS resolveRepoOrder は実質 passthrough（sidebar = daemon = ROTO = CLI で一致）。
    ui.sidebar_state.currents_order =
        Some(repo_ports.iter().map(|(path, _)| path.clone()).collect());
    // Model Q: 初回 load で active lane を daemon canonical から復元 (session.json でなく daemon が源)。
    if is_initial_load && let Some(addr) = daemon_active_lane {
        ui.sidebar_state.active_lane_address = Some(addr.clone());
        ui.persist.observe_daemon_active_lane(addr);
    }
    // wiremsg: 各 repo の repo の Unison channel を購読する (per-repo 1 本ずつ)。
    // - Stage 1: "lanes" channel → sidebar Lane ツリー
    // - Stage 2: "canvas" channel → main area の Board body
    // retained topic なので接続直後に現スナップショットが届き、以降変化のたび
    // push される。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
    for (path, _port) in &repo_ports {
        // L0 SP-portless: lanes / canvas とも Daemon :32000 の集約 channel から購読する
        // (repo 直結を剥がす)。 どちらも daemon 側で per-repo に集約済
        // (lanes=lane_registry / canvas=TopicRouter) なので repo port 不問 = repo が down
        // (port=None) でも「前回の続き」を表示でき、 port None→Some race で購読が始まらない
        // 旧 gating の穴も解消する。 repo 復帰時は register / canvas push で各 channel が更新。
        if ui.guards.lanes_sub_active.insert(path.clone()) {
            spawn_lanes_subscription(
                &boot.rt_handle,
                async_action_proxy.clone(),
                path.clone(),
                boot.daemon_conn.clone(),
            );
        }
        if ui.guards.canvas_sub_active.insert(path.clone()) {
            spawn_canvas_subscription(
                &boot.rt_handle,
                async_action_proxy.clone(),
                path.clone(),
                boot.daemon_conn.clone(),
            );
        }
    }
    // terminal S4: ensureLane / terminal session は repo port に依存しなくなった
    // (xterm transport は Daemon "canvas" channel)。 port None→Some race のための
    // retroactive ensureLane block は撤去 — lane の出現/消滅は LanesLoaded reconcile
    // が SSOT として扱う (= ensureLane + terminal session start/stop)。
    // Phase 2.x-b: dead-respawn fix — repo が "running" になった時点で
    // repo_spawn_triggered から path を外す。 これで次に dead に落ちた時、
    // user が re-expand すれば再度 spawn が trigger される。
    // 注意: spawn 進行中 (state=="spawning") は外さない、 一連の spawn cycle が完了
    // (= "running") した時のみ。 こうすれば spawn 中の重複 POST も防げる。
    for proc in &ui.sidebar_state.processes {
        if proc.state.as_deref() == Some("running")
            && ui.guards.repo_spawn_triggered.remove(&proc.path)
        {
            tracing::debug!("sp_spawn_triggered cleared (running): {}", proc.path);
        }
    }
    push_sidebar_state(&boot.webview, &ui.sidebar_state);
}

/// Phase A4-3b: repo の Lane fetch 結果を sidebar_state に反映
pub(super) fn lanes_loaded(
    ui: &mut UiState,
    boot: &Boot,
    respawn_proxy: &EventLoopProxy<AppEvent>,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    repo_path: String,
    lanes: Vec<crate::daemon_wire::LaneInfo>,
    origin: Option<String>,
) {
    // doc 44 D4: 開発起点を反映する。**`None` は上書きしない** — snapshot に
    // 起点が載っていなかっただけで「起点が無い」ではないので、前回値を保つ
    // （既定値に落とすと ⭐ が明滅する）。
    if let Some(origin) = origin {
        ui.sidebar_state
            .origin_by_repo
            .insert(repo_path.clone(), origin);
    }
    // ループする event なので log omit (= LanesLoaded push と pair で noise 源)。
    // Architecture v4: active_lane_address が未設定なら最初の Lane を auto-select。
    // 「初回起動 → Main Lane が main area に出る」UX を Lane SSOT で保つ。
    //
    // 例外: secondary instance (Cmd+N で spawn = `instance_index != 0`) の場合は
    // auto-select を skip。 元 vp-app が既に同 lane の terminal WS を持ってる事が多く、
    // 衝突して両方の console が壊れるため。 Secondary は user が手動 lane 選択する前提。
    let is_secondary = boot.instance_index != 0;
    // session 復元優先: persist の pending（前回の active lane）が今回の lanes に含まれれば、
    // auto-select-first より先にそれを採用 (vp-app 再起動時に直前 active を維持)。
    let session_match: Option<String> = ui.persist.restore_active_lane(&lanes);
    // F.8 B Convergent: auto-select は pid あり (= Active = Pane 起動済) な Lane のみ対象。
    //  Dead Lane (pid:null、 spawn 失敗) を選ぶと WS 確立先が無く「lane not found」 reconnect ループに陥る。
    //  Active Lane が 1 件も無ければ auto-select はスキップ (user 明示選択を待つ)。
    let first_active = lanes.iter().find(|l| l.pid.is_some());
    let auto_select = !is_secondary
        && ui.sidebar_state.active_lane_address.is_none()
        && session_match.is_none()
        && first_active.is_some();
    let first_addr = if let Some(saved) = session_match {
        // session 復元: pending は `restore_active_lane` が 1 度限りで消費済
        Some(saved)
    } else if auto_select {
        first_active.map(|l| l.address.key())
    } else {
        None
    };
    let path_key = repo_path.clone();
    // Phase 2.5: prev lanes との diff で「消えた Lane」 を判定 → removeLane 発行
    let removed_addrs: Vec<String> = ui
        .sidebar_state
        .lanes_by_repo
        .get(&path_key)
        .map(|prev| {
            let new_set: std::collections::HashSet<String> =
                lanes.iter().map(|l| l.address.key()).collect();
            prev.iter()
                .map(|l| l.address.key())
                .filter(|addr| !new_set.contains(addr))
                .collect()
        })
        .unwrap_or_default();
    for addr in &removed_addrs {
        tracing::info!("Lane removed (LanesLoaded diff): {}", addr);
        push_main::remove_lane(&boot.webview, addr);
        // terminal S4: 消えた lane の terminal session を停止 (= map から remove で
        // cmd_tx drop → canvas channel close → Daemon demand stop → repo pump stop)。
        ui.sessions.terminal_sessions.remove(addr);
        // conversation session も対で停止（terminal_sessions と同寿命）。remove が無いと
        // 削除済 lane の購読 task が demand を立てたまま永久残留する。
        ui.sessions.conversation_sessions.remove(addr);
        // VP-147 PR-P2-3 Moody Blues fix #1: lane delete 検出時に lane_inboxes
        // も即時 cleanup (= 5s polling tick 待たずに stale state 解消)。
        ui.sidebar_state.lane_inboxes.remove(addr);
    }
    // 供給 push 根治（session chip 凍結、2026-07-17）: この snapshot で active lane の
    // header 相当 field（engine_session_id 等）が変わったかを差し替え前に判定して
    // おく。従来は cache 更新のみで setActivePane を撃ち直さず、lane を選び直すまで
    // Conversation ヘッダが旧値で凍結した。LanesLoaded は高頻度 loop event なので、
    // 変化時のみ（下の push）に絞る。
    let active_header_refresh = ui
        .sidebar_state
        .active_lane_address
        .as_deref()
        .and_then(|addr| {
            let prev = ui
                .sidebar_state
                .lanes_by_repo
                .get(&path_key)?
                .iter()
                .find(|l| l.address.key() == addr)?;
            let next = lanes.iter().find(|l| l.address.key() == addr)?;
            Some(header_lane_fields_changed(prev, next))
        })
        .unwrap_or(false);
    ui.sidebar_state.lanes_by_repo.insert(repo_path, lanes);
    // 購読フェーズを "ready" に (= snapshot を 1 度でも受けた)。 stalled から復帰した場合も
    // ここで解消。 absent(初期 loading) / stalled と区別して hintFor が lane 0本 を
    // 「📡 lane なし」 と正しく出せる (doc 30 §5-3)。
    ui.sidebar_state
        .lane_sub_state
        .insert(path_key.clone(), "ready".to_string());
    // terminal S4: per-lane instance — repo port には依存しない (xterm transport は
    // Daemon "canvas" channel)。 live lane (pid あり) ごとに ensureLane (JS xterm 作成) +
    // terminal session start (Daemon 購読 → demand → repo pump)。 どちらも idempotent。
    if let Some(lanes_for_proj) = ui.sidebar_state.lanes_by_repo.get(&path_key) {
        for lane in lanes_for_proj {
            // doc 50 §4.6 A6: gate は **term session が 1 つでもあるか**。
            //
            // ⚠️ 旧 gate は `pid.is_none() || console_mode == "gui"` だった。あれは
            //    「term になれるのは root だけ」という制約下では正しかった（root が
            //    chat なら lane に xterm は要らない）。A6 で非 root も term になれる
            //    ので、**root が chat でも非 root の term** が居うる — lane ごと skip
            //    すると、その term に xterm も購読も作られず「pane は並ぶが真っ黒」に
            //    なる（2026-07-25 実機 dogfood で観測。pid も root slot の pid なので
            //    root=chat では None に見え、二重に間違う）。
            //    lane 単位の判断はやめ、registry の mode から導出する。
            let terms = term_sessions_of(lane);
            if terms.is_empty() {
                continue;
            }
            // Running に戻った lane は respawn guard を解除 (再 Dead 化時に再 respawn 可能に)。
            let addr_str = lane.address.key();
            ui.guards.lane_respawn_triggered.remove(&addr_str);
            // term session ごとに xterm を用意する（PtySlot 不在なら pump が張れない
            // だけで graceful — Dead lane は別途 on-demand respawn が拾う）。
            for (session, is_root) in terms {
                push_main::ensure_lane(&boot.webview, &addr_str, session, is_root);
            }
            // terminal session 未起動なら start (idempotent)。
            ui.sessions
                .terminal_sessions
                .entry(addr_str.clone())
                .or_insert_with(|| {
                    spawn_terminal_session(
                        &boot.rt_handle,
                        async_action_proxy.clone(),
                        boot.daemon_conn.clone(),
                        path_key.clone(),
                        addr_str.clone(),
                    )
                });
        }
    }
    if let Some(addr) = first_addr {
        tracing::info!("auto-select first lane: {}", addr);
        activate_lane(
            &addr,
            &mut ui.sidebar_state,
            &mut ui.persist,
            &boot.webview,
            &mut ui.guards.lane_respawn_triggered,
            &boot.rt_handle,
            respawn_proxy,
            &boot.daemon_conn,
        );
    } else {
        push_sidebar_state(&boot.webview, &ui.sidebar_state);
    }
    // 供給 push 根治: active lane の header field が変わった snapshot でだけ
    // setActivePane を再発行（webview の LaneHeader ctx 層が新値に追従する）。
    if active_header_refresh {
        push_active_view(&boot.webview, &ui.sidebar_state);
    }
    // conversation topic への attach（chat は → demand → transcript replay、TUI は
    // now-line のみ流れる軽い購読）。doc 58 ②-a で active 限定 → **全 lane** に拡大 —
    // 名簿は背景 lane の「今なにを」も見せるため。LanesLoaded は lane snapshot 到着の
    // たび走るので、新 lane / 起動直後の session 復元もここで確実に拾える（冪等）。
    let all_addrs: Vec<String> = ui
        .sidebar_state
        .lanes_by_repo
        .values()
        .flatten()
        .map(|l| l.address.key().to_string())
        .collect();
    for addr in all_addrs {
        ensure_conversation_attach(
            &addr,
            &ui.sidebar_state,
            &mut ui.sessions.conversation_sessions,
            &boot.rt_handle,
            async_action_proxy,
            &boot.daemon_conn,
        );
    }
    // doc 53 §11: **roster の供給点はここ 1 本**（旧 `conversation_session_list` fetch は
    // 退役）。snapshot は server が動詞の末尾で push する（`emit_lane_update`）ので、
    // GUI 自身が起こした変化も CLI / MCP 由来の変化も同じ道で届く。
    //
    // 変化した lane だけ push する（LanesLoaded は定期 snapshot でも走る高頻度 event。
    // 毎回撃つと webview が roster を作り直して pane が無用に再配置される）。
    // 判定の規律は上の `active_header_refresh` と同型 = 「変化時のみ push」。
    if let Some(lanes_for_proj) = ui.sidebar_state.lanes_by_repo.get(&path_key) {
        for lane in lanes_for_proj {
            let Some(sessions) = lane.sessions.as_ref() else {
                continue;
            };
            let addr = lane.address.key();
            let payload = session_list_payload(&addr, sessions);
            if !roster_push_needed(&ui.guards.last_roster_push, &addr, &payload) {
                continue;
            }
            remember_roster_push(&mut ui.guards.last_roster_push, &addr, &payload);
            push_session_list(&boot.webview, &addr, &payload);
        }
    }
    // 消えた lane の指紋も落とす（同名再作成で「変化なし」と誤判定しないため）。
    for addr in &removed_addrs {
        forget_roster_push(&mut ui.guards.last_roster_push, addr);
    }
}

pub(super) fn lanes_error(ui: &mut UiState, boot: &Boot, repo_path: String, message: String) {
    tracing::warn!(
        "AppEvent::LanesError: repo={} message={}",
        repo_path,
        message
    );
    // repo 接続失敗 / lanes channel stall — lanes_by_repo は更新しない (前回値を保持) が、
    // 購読フェーズを "stalled" に倒して UI に surface する (doc 30 §5-3)。 hintFor が
    // `📡 loading lanes…` ではなく「⚠️ lane 接続が停滞 — restart で復帰」を出す。 復帰時の
    // snapshot 受信 (LanesLoaded) で "ready" に上書きされて自動解消する (self-heal と連動)。
    ui.sidebar_state
        .lane_sub_state
        .insert(repo_path, "stalled".to_string());
    push_sidebar_state(&boot.webview, &ui.sidebar_state);
}

/// オンデマンド respawn の restart_lane が失敗した lane を guard から解除する。
/// 解除しておくと、 次に同 lane を active にした (or LanesLoaded for Dead の) 時点で
/// 再 respawn を試行できる (= repo クラッシュ後の復帰でも auto-respawn が効く)。
/// 即ループにはならない: クリック起点は user 操作、 起動時 first_addr は active 設定後
/// None になるため LanesLoaded loop event での連続発火は起きない。
pub(super) fn lane_respawn_failed(ui: &mut UiState, _boot: &Boot, address: String) {
    if ui.guards.lane_respawn_triggered.remove(&address) {
        tracing::info!("auto-respawn guard 解除 (restart 失敗): {}", address);
    }
}

pub(super) fn repos_error(_ui: &mut UiState, boot: &Boot, msg: String) {
    push_sidebar::error(&boot.webview, &msg);
}

/// R5 Sub create flow: spawn_blocking thread からの結果を sidebar に push back。
/// success → form を閉じる + addSubOpen から削除。
/// error → form 下に inline error 表示 + form は開いたまま (再 submit 可能)。
pub(super) fn sub_create_result(
    _ui: &mut UiState,
    boot: &Boot,
    repo_path: String,
    name: String,
    error: Option<String>,
) {
    push_sidebar::sub_create_result(&boot.webview, repo_path, name, error);
}
