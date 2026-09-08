//! event handler: **sidebar**（sidebar IPC の効果実行 = `handle_sidebar_ipc` の outcome を順に実行、
//! in-app update の phase、settings overlay の値の往復）。
//!
//! 旧 `run()` の match arm を fn ごとに移したもの（doc 60 §6 6-2 PR-10、2026-09-08）。本体は arm の
//! 中身を 12 空白 dedent しただけ、arm の前のコメントは fn の doc へ移動。
//!
//! `sidebar_ipc` の効果実行は **1 fn のまま** 28 段の順序で並ぶ（`session_save` が最初、`activate_lane`
//! と `changed` / `active_changed` の push は排他、`settings_save_request` は `settings_fetch` の前 …）。
//! この順序は契約（doc 60 §6 A / Codex ⑥）で、`Vec<SidebarEffect>` 化は第 2 の生成者が現れるまで延期。
//!
//! 触る state: ほぼ全部（`ui.sidebar_state` / `ui.session_state` / `ui.settings` / `ui.dev_mode` /
//! `ui.last_daemon_settings` / `ui.update_applying` / `ui.guards.*` / `ui.sessions.conversation_sessions`）
//! = `&mut UiState` が正直な signature。resource: `boot.webview` / `boot.rt_handle` / `boot.daemon_conn` /
//! `boot.actions_persist_tx` / menu item（developer mode の有効化）。

use tao::event_loop::EventLoopProxy;

use super::boot::Boot;
use super::developer_mode_env;
use super::lane_view::{activate_lane, ensure_conversation_attach, push_active_view};
use super::sidebar_ipc::handle_sidebar_ipc;
use super::state::UiState;
use crate::daemon::conn::daemon_repo_request;
use crate::daemon::pollers::{fetch_repos_with_ports, spawn_sp_start};
use crate::daemon::wire::wire_fetch_payload;
use crate::events::AppEvent;
use crate::flows::repo_dialog::{
    resolve_default_repo_root, spawn_add_repo_picker, spawn_clone_repo, spawn_repo_root_picker,
};
use crate::pane::SidebarState;
use crate::settings::Settings;
use crate::webview::push_main;
use crate::webview::push_sidebar::{self, push_sidebar_state};

/// daemon 側（settings.kdl）から取れた設定。**取れなかった場合と未設定を区別する**ため
/// `Option` を包んでいる（doc 59 P3）。
///
/// `None` = daemon に届かなかった（オフライン / 旧 binary）。この時 UI は該当区画を
/// 「daemon に接続すると編集できます」に落とす — 空欄を編集可能に見せると、押しても
/// 保存できない**行き止まり**になる。
#[derive(Debug, Clone, Default)]
pub(super) struct DaemonSettings {
    log_level: Option<String>,
    idle_timeout_minutes: Option<i64>,
    /// 既定 agent × model（doc 59 P4）。**組で保持する** — 別々に持つと
    /// 「codex なのに claude の model」を UI 側で再構成できてしまう。
    default_agent: Option<String>,
    default_model: Option<String>,
    /// 実効 agent が model 指定を受け付けるか。**daemon が判定した結果**をそのまま持つ
    /// （vp-app は engine の能力表明を知らない = 一覧を複製しない）。
    default_agent_takes_model: bool,
}

/// 設定 overlay へ返す確定値を組み立てる（doc 59 P1 + P3）。
///
/// `developer_mode` は **実効値**（env > vp-app.toml > `debug_assertions` の解決後）を渡す —
/// event loop が持っている `dev_mode` がその値なので、それをそのまま映す。
/// `resolved_repo_root` は明示値が無いときに実際に使われるパスで、入力欄の placeholder に
/// なる（「空欄だが実際はここ」を見せるため）。
///
/// ⚠️ **真実源が 2 つある**面なので、それぞれの持ち主を分けて扱う:
/// - `vp-app.toml`（GUI 固有）= developer_mode / default_repo_root — この関数が同期で読む
/// - `settings.kdl`（好み、daemon 所有）= log_level / idle_timeout — `daemon` 引数で渡る
pub(super) fn settings_snapshot(
    settings: &Settings,
    sidebar_state: &SidebarState,
    dev_mode: bool,
    daemon: Option<&DaemonSettings>,
) -> crate::generated::sidebar_ipc::SettingsResult {
    crate::generated::sidebar_ipc::SettingsResult {
        developer_mode: dev_mode,
        developer_mode_locked: developer_mode_env().is_some(),
        default_repo_root: settings.default_repo_root.clone(),
        resolved_repo_root: resolve_default_repo_root(settings, sidebar_state)
            .map(|p| p.display().to_string()),
        daemon_reachable: daemon.is_some(),
        log_level: daemon.and_then(|d| d.log_level.clone()),
        idle_timeout_minutes: daemon.and_then(|d| d.idle_timeout_minutes),
        default_agent: daemon.and_then(|d| d.default_agent.clone()),
        default_model: daemon.and_then(|d| d.default_model.clone()),
        // daemon が判定した結果をそのまま流す。UI はこれが false なら model 欄を出さない
        // （codex は VP から model を渡さない = 押しても効かない欄を並べない）。
        default_agent_takes_model: daemon.is_some_and(|d| d.default_agent_takes_model),
    }
}

pub(super) fn update_flow_phase(ui: &mut UiState, boot: &Boot, applying: bool) {
    ui.update_applying = applying;
    ui.sidebar_state.activity.update_applying = applying;
    push_sidebar_state(&boot.webview, &ui.sidebar_state);
}

pub(super) fn settings_repo_root_picked(ui: &mut UiState, boot: &Boot, path: Option<String>) {
    // キャンセル (None) は**書かない**（既存値を保持）。ただし overlay の表示は
    // 現実に合わせたいので、選ばれた / 選ばれなかったに関わらず確定値を返す。
    if let Some(p) = path {
        ui.settings.default_repo_root = Some(p);
        if let Err(e) = ui.settings.save() {
            tracing::warn!("Settings 保存失敗: {e}");
        }
    }
    // picker は vp-app.toml しか触らないので daemon 側は引き直さない
    // （`last_daemon_settings` に前回の結果が残っている）。
    push_sidebar::settings_result(
        &boot.webview,
        settings_snapshot(
            &ui.settings,
            &ui.sidebar_state,
            ui.dev_mode,
            ui.last_daemon_settings.as_ref(),
        ),
    );
}

pub(super) fn settings_daemon_fetched(
    ui: &mut UiState,
    boot: &Boot,
    fetched: Option<serde_json::Value>,
) {
    // daemon 側（settings.kdl）が揃ったので、vp-app.toml 側と合流させて
    // **1 回だけ** push する。`None` = 接続できなかった（UI は該当区画を
    // 「daemon に接続すると編集できます」に落とす）。
    ui.last_daemon_settings = fetched.map(|v| {
        let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        DaemonSettings {
            log_level: text("log_level"),
            idle_timeout_minutes: v.get("idle_timeout_minutes").and_then(|x| x.as_i64()),
            default_agent: text("default_agent"),
            default_model: text("default_model"),
            default_agent_takes_model: v
                .get("default_agent_takes_model")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        }
    });
    push_sidebar::settings_result(
        &boot.webview,
        settings_snapshot(
            &ui.settings,
            &ui.sidebar_state,
            ui.dev_mode,
            ui.last_daemon_settings.as_ref(),
        ),
    );
}

pub(super) fn sidebar_ipc(
    ui: &mut UiState,
    boot: &Boot,
    proxy: &EventLoopProxy<AppEvent>,
    respawn_proxy: &EventLoopProxy<AppEvent>,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    msg: String,
) {
    // VP-100 follow-up: repo:add / repo:clone は async picker → API → ReposLoaded ルート
    // (state 直接 mutate しないので handle_sidebar_ipc の前で分岐)
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
        match parsed.get("t").and_then(|v| v.as_str()) {
            Some("repo:add") => {
                let initial_dir = resolve_default_repo_root(&ui.settings, &ui.sidebar_state);
                spawn_add_repo_picker(
                    async_action_proxy.clone(),
                    initial_dir,
                    boot.rt_handle.clone(),
                    boot.daemon_conn.clone(),
                );
                return;
            }
            Some("process:clone") => {
                let url = parsed
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if url.is_empty() {
                    tracing::warn!("process:clone with empty url");
                    return;
                }
                let target_override = parsed
                    .get("target_dir")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(std::path::PathBuf::from);
                let default_root = resolve_default_repo_root(&ui.settings, &ui.sidebar_state);
                spawn_clone_repo(
                    async_action_proxy.clone(),
                    url,
                    default_root,
                    target_override,
                    boot.rt_handle.clone(),
                    boot.daemon_conn.clone(),
                );
                return;
            }
            _ => {}
        }
    }
    let outcome = handle_sidebar_ipc(&msg, &mut ui.sidebar_state, &mut ui.session_state);
    // 解釈は純粋（doc 60 §6 A-2）: session の file 書き込みは要求を見てここで行う。
    // in-memory の更新は handle 側で済んでいるので、他の効果より先に書いて
    // 旧実装（handle 内で save）と同じ順序を保つ。
    if outcome.session_save {
        ui.session_state.save();
    }
    // Lane activation — activate_lane() が全副作用を処理
    if let Some(addr) = outcome.activate_lane {
        activate_lane(
            &addr,
            &mut ui.sidebar_state,
            &mut ui.session_state,
            &boot.webview,
            &mut ui.guards.lane_respawn_triggered,
            &boot.rt_handle,
            respawn_proxy,
            &boot.daemon_conn,
        );
        // gui: chat lane なら conversation topic に attach（→ transcript replay）。
        ensure_conversation_attach(
            &addr,
            &ui.sidebar_state,
            &mut ui.sessions.conversation_sessions,
            &boot.rt_handle,
            async_action_proxy,
            &boot.daemon_conn,
        );
    } else {
        if outcome.changed {
            push_sidebar_state(&boot.webview, &ui.sidebar_state);
        }
        if outcome.active_changed {
            push_active_view(&boot.webview, &ui.sidebar_state);
        }
    }
    // Architecture v4: dead な repo が expand されたら repo を auto-spawn。
    // dedup: 同 session で同じ path を 2 回呼ばない (daemon 側でも弾かれるが
    // 余計な POST を避ける)。
    if let Some((name, path)) = outcome.repo_spawn_request {
        if ui.guards.repo_spawn_triggered.insert(path.clone()) {
            tracing::info!(
                "repo auto-spawn 要求 (accordion expand trigger): name={} path={}",
                name,
                path
            );
            spawn_sp_start(
                &boot.rt_handle,
                async_action_proxy.clone(),
                name,
                path,
                boot.daemon_conn.clone(),
            );
        } else {
            tracing::debug!("repo auto-spawn skip (既 trigger): {}", path);
        }
    }
    // Phase 5-D fix: accordion 閉じた → dedup HashSet から path を release。
    //  spawn 失敗で entry が居残ったまま user が collapse → expand すれば確実に retry。
    if let Some(path) = outcome.repo_spawn_release
        && ui.guards.repo_spawn_triggered.remove(&path)
    {
        tracing::info!(
            "repo auto-spawn dedup released (accordion collapse): {}",
            path
        );
    }
    // 「見えている Lane だけ生きている」: accordion の開閉で購読を張り直す。
    // 全 lane を回すのは LanesLoaded の再評価と同じ形（冪等・数十 lane 規模）。
    if outcome.conversation_reattach {
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
    }
    // Phase 5-C: Process restart 要求 (sidebar の 🔄 button から)。
    // 全 async work は shared runtime (rt_handle) 経由 — bare `tokio::spawn` は禁止
    // (.clippy.toml で compile gate)、 tao event loop closure に runtime context が
    // 無いので必ず `rt_handle.spawn` を使う。
    if let Some(repo_name) = outcome.restart_process_request {
        let proxy = async_action_proxy.clone();
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            // doc 45 段 3: 旧 `POST /api/daemon/processes/{name}/restart` を
            // Unison `daemon-control.repos/restart` に差し替え。 接続先は共有
            // QUIC connection (port 解決は conn manager が持つ)。
            let control = match conn.control().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("restart_process: {}", e);
                    return;
                }
            };
            match control.restart_process(&repo_name).await {
                Ok(()) => {
                    tracing::info!("restart_process OK: {}", repo_name);
                    // 完了 → repos 再 fetch → sidebar state badge 更新。
                    // 必ず `fetch_repos_with_ports` 経由 (= runtime port merge)
                    // で送る。 list_repos() だけだと restart 直後に全 repo の
                    // port が None で潰れ、 後続 LanesLoaded で ensureLane が
                    // 全件 skip され main terminal が消失する。
                    if let Ok(repos) = fetch_repos_with_ports(&control).await {
                        let _ = proxy.send_event(AppEvent::ReposLoaded(repos));
                    }
                }
                Err(e) => {
                    tracing::warn!("restart_process failed for {}: {}", repo_name, e);
                }
            }
        });
    }
    // Process stop 要求 (repo context menu の Stop repo から)。
    if let Some(repo_name) = outcome.stop_process_request {
        let proxy = async_action_proxy.clone();
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let control = match conn.control().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("stop_process: {}", e);
                    return;
                }
            };
            match control.stop_process(&repo_name).await {
                Ok(()) => {
                    tracing::info!("stop_process OK: {}", repo_name);
                    // 完了 → repos 再 fetch → 停止 state を sidebar に反映。
                    // restart と同じく `fetch_repos_with_ports` 経由で
                    // 他 repo の runtime port を保つ。
                    if let Ok(repos) = fetch_repos_with_ports(&control).await {
                        let _ = proxy.send_event(AppEvent::ReposLoaded(repos));
                    }
                }
                Err(e) => {
                    tracing::warn!("stop_process failed for {}: {}", repo_name, e);
                }
            }
        });
    }
    // Repo delete 要求 (repo context menu の Delete repo から、
    // UI で 2-click 確認済)。 daemon の remove_repo は稼働中 repo があると
    // エラーになるため、 先に stop → grace → remove と chain する
    // (restart_process が capability 内でやっているのと同じ順序)。
    if let Some((repo_name, repo_path)) = outcome.delete_repo_request {
        let proxy = async_action_proxy.clone();
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let control = match conn.control().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("delete_repo: {}", e);
                    return;
                }
            };
            // stop は best-effort: repo が未起動 (= 停止中) なら
            // 「No running Process」 エラーが返るが、 続行して remove する。
            match control.stop_process(&repo_name).await {
                Ok(()) => {
                    tracing::info!("delete: stop_process OK: {}", repo_name);
                    // shutdown 伝播 + port release を待つ grace period
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    tracing::info!(
                        "delete: stop_process skipped for {} (continuing): {}",
                        repo_name,
                        e
                    );
                }
            }
            match control.remove_repo(&repo_path).await {
                Ok(()) => {
                    tracing::info!("remove_repo OK: {}", repo_path);
                    // 完了 → repos 再 fetch → sidebar から除去。
                    // 削除対象以外の repo の runtime port を保つため
                    // `fetch_repos_with_ports` 経由で送る。
                    if let Ok(repos) = fetch_repos_with_ports(&control).await {
                        let _ = proxy.send_event(AppEvent::ReposLoaded(repos));
                    }
                }
                Err(e) => {
                    tracing::warn!("remove_repo failed for {}: {}", repo_path, e);
                }
            }
        });
    }
    // Phase 1 (doc 24): repo 並び替えを daemon の repo_order に永続化する。
    // restart/stop と同じ「操作 → re-fetch → ReposLoaded」パターン。成功後の
    // ReposLoaded で currents_order が canonical 順に reconcile される。
    if let Some(order) = outcome.reorder_request {
        let proxy = async_action_proxy.clone();
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let control = match conn.control().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("reorder_repos: {}", e);
                    return;
                }
            };
            match control.reorder_repos(order).await {
                Ok(()) => {
                    tracing::info!("reorder_repos OK");
                    // 完了 → repos 再 fetch → canonical 順で sidebar reconcile。
                    if let Ok(repos) = fetch_repos_with_ports(&control).await {
                        let _ = proxy.send_event(AppEvent::ReposLoaded(repos));
                    }
                }
                Err(e) => {
                    tracing::warn!("reorder_repos failed: {}", e);
                }
            }
        });
    }
    // Model Q: active lane を daemon canonical に永続 (fire-and-forget、 optimistic 適用済)。
    if let Some((repo_path, address)) = outcome.set_active_lane_request {
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let result = match conn.control().await {
                Ok(control) => control.set_active_lane(repo_path, address).await,
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                tracing::warn!("set_active_lane failed: {}", e);
            }
        });
    }
    // Phase 4-A: Sub Lane 削除要求 (sidebar の × button から)
    if let Some((repo_path, address)) = outcome.delete_lane_request {
        // F6②: 旧 DaemonRpcClient.delete_lane (repo 直結 reqwest) を daemon repo-proxy
        // ask (lane_delete) に移管。 repo port 解決は不要になり repo_path を handshake で渡す。
        // JS-side からも先 removeLane を呼ぶ (= xterm 即時 dispose、 server 反映は
        // repo の "lanes" topic snapshot 経由で sidebar に届く)。
        push_main::remove_lane(&boot.webview, &address);
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let payload = serde_json::json!({ "address": &address });
            match daemon_repo_request(&conn, &repo_path, "lane_delete", payload).await {
                Ok(_) => {
                    tracing::info!("Lane deleted: repo={} address={}", repo_path, address);
                }
                Err(e) => {
                    tracing::warn!(
                        "lane_delete failed: repo={} address={}: {}",
                        repo_path,
                        address,
                        e
                    );
                }
            }
        });
    }
    // Lane Main Agent restart 要求 (sidebar の restart icon → confirm dialog から)
    if let Some((repo_path, address, fresh)) = outcome.restart_lane_request {
        // F6③: 旧 DaemonRpcClient.restart_lane (repo 直結 reqwest) を daemon repo-proxy
        // ask (lane_restart) に移管。 repo port 解決は不要、 repo_path を handshake で渡す。
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let payload = serde_json::json!({ "address": &address, "fresh": fresh });
            match daemon_repo_request(&conn, &repo_path, "lane_restart", payload).await {
                Ok(_) => {
                    // 新 pid / state は repo の "lanes" topic snapshot で購読側に push され、
                    // 端末は canvas channel demand 経由で新 PtySlot に再 attach し直す。
                    tracing::info!("Lane restarted: repo={} address={}", repo_path, address);
                }
                Err(e) => {
                    tracing::warn!(
                        "lane_restart failed: repo={} address={}: {}",
                        repo_path,
                        address,
                        e
                    );
                }
            }
        });
    }
    // doc 39 §8.4 提案 2: 新しい root conversation（sidebar lane 行の context menu）。
    // backend が新 session を採番して root に向ける。旧 root の pane / 会話は残る
    //（= Reset Lane との違い）。反映は lanes snapshot / session list が運ぶので
    // 楽観更新しない。
    if let Some((repo_path, address)) = outcome.new_root_request {
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            match daemon_repo_request(
                &conn,
                &repo_path,
                "conversation_session_new_root",
                serde_json::json!({ "lane": &address }),
            )
            .await
            {
                Ok(res) => {
                    let session = res.get("session").and_then(serde_json::Value::as_u64);
                    tracing::info!(
                        "new root conversation: repo={repo_path} lane={address} session={session:?}"
                    );
                }
                Err(e) => tracing::warn!(
                    "conversation_session_new_root failed: repo={repo_path} lane={address}: {e}"
                ),
            }
        });
    }
    // doc 44 D4/D5: 開発起点の再指定 (sidebar lane 行の context menu から)。
    // Host の帳簿のポインタを書き換えるだけ — cwd も active lane も engine も動かない。
    // 反映は次の lanes snapshot の `origin` で戻る（楽観更新しない = 帳簿が真実源）。
    if let Some((repo_path, address)) = outcome.set_origin_request {
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            // 帳簿は lane **名**で受ける（起点は repo ごとに 1 本なので
            // address の `<repo>` 部分は冗長）。address からは末尾を取る。
            let lane_name = address.rsplit('/').next().unwrap_or("").to_string();
            if lane_name.is_empty() {
                tracing::warn!("lane_origin_set: address から lane 名を取れない: {address}");
                return;
            }
            let payload = serde_json::json!({ "lane": lane_name });
            match daemon_repo_request(&conn, &repo_path, "lane_origin_set", payload).await {
                Ok(_) => tracing::info!("開発起点を変更: repo={} lane={}", repo_path, lane_name),
                Err(e) => tracing::warn!(
                    "lane_origin_set failed: repo={} lane={}: {}",
                    repo_path,
                    lane_name,
                    e
                ),
            }
        });
    }
    // doc 44 §12: lane の並び順を帳簿に保存する（sidebar の DnD）。
    // address 列を lane 名の列に畳んでから投げる（帳簿は lane 名で受け、
    // 境界で lane_id に解決する — 起点と同じ規律）。
    if let Some((repo_path, order)) = outcome.reorder_lanes_request {
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let names: Vec<String> = order
                .iter()
                .filter_map(|a| a.rsplit('/').next())
                .filter(|n| !n.is_empty())
                .map(|n| n.to_string())
                .collect();
            if names.is_empty() {
                tracing::warn!("lane_order_set: address 列から lane 名を取れない");
                return;
            }
            let payload = serde_json::json!({ "order": names });
            match daemon_repo_request(&conn, &repo_path, "lane_order_set", payload).await {
                Ok(_) => tracing::info!(
                    "lane の並び順を保存: repo={} count={}",
                    repo_path,
                    names.len()
                ),
                Err(e) => tracing::warn!("lane_order_set failed: repo={}: {}", repo_path, e),
            }
        });
    }
    // Phase 3-A: Sub Lane 作成要求 (sidebar の + Add Sub から)
    // 投げ先は Daemon (:32000) の `daemon-control.lanes/create` 1 本 (repo port 解決は不要、
    // set_active_lane / reorder と同じ daemon-command パターン)。
    // doc 44 §9.4: daemon 側はそこで自前の provision をせず repo runtime の
    // lane 作成 core に委譲する — worktree も PtySlot も**この 1 往復で揃う**。
    // 旧構成は descriptor だけ作って PtySlot を lane_watcher の到達に賭けており、
    // 「+ で作った lane だけ engine 指定が別経路で伝わる」等の経路差が生じていた。
    // doc 11 PR-C: agent 指定 を tuple 4 番目に保持 (None なら daemon-side default)。
    if let Some((repo_path, name, branch, agent)) = outcome.add_sub_request {
        let proxy = async_action_proxy.clone();
        let name_clone = name.clone();
        let branch_clone = branch.clone();
        let stand_clone = agent.clone();
        let path_clone = repo_path.clone();
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let control = match conn.control().await {
                Ok(c) => c,
                Err(e) => {
                    let msg = e.to_string();
                    tracing::warn!("create_sub_lane: {}", msg);
                    let _ = proxy.send_event(AppEvent::SubCreateResult {
                        repo_path: path_clone,
                        name: name_clone,
                        error: Some(msg),
                    });
                    return;
                }
            };
            match control
                .create_sub_lane(
                    &path_clone,
                    &name_clone,
                    branch_clone.as_deref(),
                    stand_clone.as_deref(),
                )
                .await
            {
                Ok(()) => {
                    tracing::info!(
                        "Sub Lane created (daemon): repo={} name={} branch={:?}",
                        path_clone,
                        name_clone,
                        branch_clone
                    );
                    // 応答が返った時点で lane は既に spawn 済（doc 44 §9.4）。
                    // sidebar への反映は "lanes" topic snapshot の push を待つ
                    // （楽観更新しない = 真実源は 1 つ、doc 44 §10.3 と同じ規律）。
                    // R5: 成功通知を sidebar に push back (form を閉じる)
                    let _ = proxy.send_event(AppEvent::SubCreateResult {
                        repo_path: path_clone,
                        name: name_clone,
                        error: None,
                    });
                }
                Err(e) => {
                    // R5: 失敗通知を sidebar に push back (form 下に inline error 表示)。
                    // doc 45 段 3 以降は Unison の error 慣習 (VP-163) に従い
                    // "daemon-control.lanes/create: <daemon 側の理由>" が返る
                    // (旧 HTTP の "... HTTP 500: {json}" より読める)。 そのまま流す。
                    let msg = format!("{}", e);
                    tracing::warn!(
                        "create_sub_lane failed: repo={} name={}: {}",
                        path_clone,
                        name_clone,
                        msg
                    );
                    let _ = proxy.send_event(AppEvent::SubCreateResult {
                        repo_path: path_clone,
                        name: name_clone,
                        error: Some(msg),
                    });
                }
            }
        });
    }

    // doc 11 PR-C / F6④: 利用可能 Agent 一覧 fetch 要求 (sidebar の + Add Sub 開閉から)。
    // 旧 SP 直結 (client.list_agents) を撤去し daemon repo-proxy ask (`agents_list`) に移管。
    // repo port 解決が消滅し、 surface は Daemon :32000 だけを知れば済む (L1 portless 前進)。
    if let Some(repo_path) = outcome.list_stands_request {
        let proxy = async_action_proxy.clone();
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let (agents, error) =
                match daemon_repo_request(&conn, &repo_path, "agents_list", serde_json::json!({}))
                    .await
                {
                    // repo は {agents:[...]} を返す。 agents 配列だけ Vec<AgentInfo> に deserialize。
                    Ok(v) => {
                        let agents = v
                            .get("agents")
                            .and_then(|s| {
                                serde_json::from_value::<Vec<crate::daemon_wire::AgentInfo>>(
                                    s.clone(),
                                )
                                .ok()
                            })
                            .unwrap_or_default();
                        tracing::debug!("agents listed: repo={} count={}", repo_path, agents.len());
                        (agents, None)
                    }
                    Err(e) => {
                        tracing::warn!("agents_list failed: repo={}: {}", repo_path, e);
                        (Vec::new(), Some(e))
                    }
                };
            let _ = proxy.send_event(AppEvent::AgentsResult {
                repo_path,
                agents,
                error,
            });
        });
    }

    // Wire inbox (doc 34 §4 V1): Daemon "wire" channel への read-only fetch
    // (ack 要求は「ack → 再 fetch」に畳む)。 async I/O なので tokio task に逃し、
    // 結果は AppEvent::WireHistoryResult で event loop に戻して
    // window.vpWire.handleResult へ push back する。
    // fetch と ack は別 IPC で、 IpcEnvelope の単一 variant match により同一
    // outcome で両立しない — 単純な合成で足りる (ack は「ack → 再 fetch」に畳む)。
    let wire_req = outcome
        .wire_ack_request
        .map(|(addr, id)| (addr, Some(id)))
        .or_else(|| outcome.wire_fetch_request.map(|a| (a, None)));
    if let Some((address, ack_id)) = wire_req {
        let proxy = async_action_proxy.clone();
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let payload = wire_fetch_payload(conn, address.clone(), ack_id).await;
            let _ = proxy.send_event(AppEvent::WireHistoryResult { address, payload });
        });
    }

    // in-app update: sidebar footer の「更新する」ボタン click 要求。
    // native 確認ダイアログ → self-update → daemon restart → relaunch を
    // 専用スレッドで起動する（event loop = main thread は塞がない）。
    // on_phase は AppEvent 経由で event loop に戻し、「更新中…」表示に使う。
    if let Some(version) = outcome.update_apply_request {
        let phase_proxy = proxy.clone();
        crate::flows::update::spawn_update_flow(version, move |applying| {
            let _ = phase_proxy.send_event(AppEvent::UpdateFlowPhase(applying));
        });
    }

    // 設定 overlay（doc 59 P1）。fetch / save は最後に必ず「確定値を push back」で
    // 合流する — client が楽観更新をしないので、**真実は vp-app.toml 1 本**で決まり、
    // 保存失敗時の巻き戻しを client に持たせなくてよい。
    let settings_saved = outcome.settings_save_request.is_some();
    // daemon 側（settings.kdl）へ中継する分。**vp-app は書かない** — 書き手を
    // daemon 唯一にしてある（doc 59 §3）ので、ここは payload を組むだけ。
    let mut daemon_payload = serde_json::Map::new();
    if let Some(save) = outcome.settings_save_request {
        // ⚠️ `None` の field は**不変**（「変えた分だけ送る」契約）。
        if let Some(dev) = save.developer_mode {
            ui.dev_mode = dev;
            boot.open_devtools_item.set_enabled(dev);
            boot.reload_webview_item.set_enabled(dev);
            ui.settings.developer_mode = Some(dev);
        }
        if let Some(root) = save.default_repo_root {
            // 空文字 = **未設定に戻す**（推定へのフォールバックを復活させる）。
            // 消し方を別 UI にしないための約束 — 入力欄を空にすれば戻る。
            let trimmed = root.trim();
            ui.settings.default_repo_root = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
        if let Err(e) = ui.settings.save() {
            tracing::warn!("Settings 保存失敗: {e}");
        }
        if let Some(level) = save.log_level {
            daemon_payload.insert("log_level".into(), serde_json::json!(level));
        }
        if let Some(minutes) = save.idle_timeout_minutes {
            daemon_payload.insert("idle_timeout_minutes".into(), serde_json::json!(minutes));
        }
        if let Some(agent) = save.default_agent {
            daemon_payload.insert("default_agent".into(), serde_json::json!(agent));
        }
        if let Some(model) = save.default_model {
            daemon_payload.insert("default_model".into(), serde_json::json!(model));
        }
    }
    if outcome.settings_pick_repo_root_request {
        // rfd は blocking なので専用スレッド → 結果は
        // `AppEvent::SettingsRepoRootPicked` で戻る（そこで保存 + push back）。
        let initial = resolve_default_repo_root(&ui.settings, &ui.sidebar_state);
        spawn_repo_root_picker(async_action_proxy.clone(), initial);
    }
    if outcome.settings_fetch_request || settings_saved {
        // ⚠️ **書いてから読む**を 1 つの task に閉じ込める。2 本に分けると
        // 「保存より先に読み終えて古い値を表示する」順序が生まれる。
        // 読めたら `SettingsDaemonFetched` で戻り、そこで vp-app.toml 側と
        // 合流させて 1 回だけ push する。
        let conn = boot.daemon_conn.clone();
        let ev_proxy = proxy.clone();
        let payload =
            (!daemon_payload.is_empty()).then_some(serde_json::Value::Object(daemon_payload));
        boot.rt_handle.spawn(async move {
            let fetched = match conn.control().await {
                Ok(control) => {
                    if let Some(p) = payload
                        && let Err(e) = control.settings_set(p).await
                    {
                        tracing::warn!("settings/set 失敗: {e}");
                    }
                    control.settings_get().await.ok()
                }
                Err(e) => {
                    tracing::debug!("settings: daemon に接続できない: {e}");
                    None
                }
            };
            let _ = ev_proxy.send_event(AppEvent::SettingsDaemonFetched(fetched));
        });
    }
    if outcome.daemon_restart_request {
        // ⚠️ **全 repo = 全 lane の claude が落ちる**（doc 44 P1 fold-in）。
        // 確認ダイアログは flow 側（rfd が blocking なので専用スレッド）。
        crate::daemon::restart::spawn_daemon_restart();
    }
    // Hub 行の Login / Logout ボタン click 要求。blocking フロー（browser OAuth
    // 待ち / 確認ダイアログ / CLI spawn）を blocking pool で実行し、成功したら
    // `daemon-control.hub/reconnect` で daemon の hub 接続に credential 変化を
    // 即反映する（= 押した結果が数秒後の health poll で Hub 行に現れる）。
    // ACTIONS の永続化要求（doc 57 Phase 4）。watch は latest-wins なので、
    // 打鍵ごとに来ても debounce task が静まった 1 回だけを daemon へ撃つ。
    if let Some(payload) = outcome.actions_persist_request {
        let _ = boot.actions_persist_tx.send(Some(payload));
    }
    if outcome.auth_login_request.is_some() || outcome.auth_logout_request.is_some() {
        let login_target = outcome.auth_login_request.clone();
        let logout_target = outcome.auth_logout_request.clone();
        let conn = boot.daemon_conn.clone();
        let rt = boot.rt_handle.clone();
        boot.rt_handle.spawn(async move {
            let flow = rt.spawn_blocking(move || match login_target {
                Some(t) => crate::flows::auth::run_login_blocking(&t),
                None => crate::flows::auth::run_logout_blocking(
                    logout_target.as_deref().unwrap_or(""),
                ),
            });
            // false = キャンセル / 失敗 / 二重起動 → credentials 不変なので反映不要。
            if !matches!(flow.await, Ok(true)) {
                return;
            }
            match conn.control().await {
                Ok(control) => {
                    if let Err(e) = control.hub_reconnect().await {
                        tracing::warn!(
                            "auth flow: hub/reconnect 要求に失敗（次の自然な再接続で反映される）: {}",
                            e
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    "auth flow: daemon 接続に失敗（hub/reconnect 未送信）: {}",
                    e
                ),
            }
        });
    }
}
