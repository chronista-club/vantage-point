//! event handler: **misc**（群を持たない小さな面 — session title / lane inbox / ink snapshot /
//! debug log tail / device event / code pane / wire 履歴 / activity）。
//!
//! 旧 `run()` の match arm を fn ごとに移したもの（doc 60 §6 6-2 PR-5、2026-09-08）。本体は arm の
//! 中身を 12 空白 dedent しただけ、arm の前のコメントは fn の doc へ移動。
//!
//! 触る state: `ui.sidebar_state`（titles / inboxes / devices / activity）、`ui.debuglog_watch_gen`、
//! `ui.update_applying`（read）。resource: `boot.webview` / `boot.rt_handle`。
//! ⚠️ `resolve_session_titles` / `resolve_lane_inboxes` は `lanes_by_repo` を読む（on_lanes の state）。

use tao::event_loop::EventLoopProxy;

use super::boot::Boot;
use super::lane_view::lookup_lane_cwd_by_address;
use super::state::UiState;
use crate::events::AppEvent;
use crate::webview::editor_bridge::fleet_dispatch_js;
use crate::webview::push_main;
use crate::webview::push_sidebar::{self, push_sidebar_state};

pub(super) fn resolve_session_titles(ui: &mut UiState, boot: &Boot) {
    // VP-143 → doc 58 ②-c: 全 lane の **session ごと**に cc custom-title を resolve
    // → diff → sidebar に push。poller (`spawn_session_title_poller`) が 5s 間隔で
    // tick を送ってここに来る。resolve は read-only file I/O なので main thread
    // blocking は無視できる範囲。
    //
    // 鍵は `{address}#{session}`（webview 側 `sessionNowKey` と同形 — now-line と
    // 同じ語彙で session を指す）。conversation id を持つ session は jsonl を直接
    // 特定（相部屋で他人の title を拾わない）、持たない session（Draft / TUI の
    // 旧 wire）は root に限り従来の cwd 推定に fallback する（新規に嘘を増やさない
    // — 非 root の cwd 推定は「最新 mtime = root の会話」とぶつかりやすい）。
    let mut changed = false;
    let mut current_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for lanes in ui.sidebar_state.lanes_by_repo.values() {
        for lane in lanes {
            let address = lane.address.key();
            let cwd = std::path::Path::new(&lane.cwd);
            // sessions registry 欠落（旧 wire / boot 窓）は root=1 の 1 session とみなす
            let entries: Vec<(u32, Option<String>, bool)> = match &lane.sessions {
                Some(reg) if !reg.sessions.is_empty() => reg
                    .sessions
                    .iter()
                    .map(|e| (e.key, e.conversation.clone(), e.key == reg.root))
                    .collect(),
                _ => vec![(1, None, true)],
            };
            for (key, conversation, is_root) in entries {
                let map_key = format!("{address}#{key}");
                current_keys.insert(map_key.clone());
                let resolved = match conversation.as_deref() {
                    Some(conv) => crate::lane::title::resolve_title_for_conversation(cwd, conv),
                    None if is_root => crate::lane::title::resolve_title_for_cwd(cwd),
                    None => None,
                };
                let prev = ui.sidebar_state.session_titles.get(&map_key).cloned();
                match (resolved, prev) {
                    (Some(new_title), Some(old)) if old == new_title => {}
                    (None, None) => {}
                    (Some(new_title), _) => {
                        ui.sidebar_state.session_titles.insert(map_key, new_title);
                        changed = true;
                    }
                    (None, Some(_)) => {
                        ui.sidebar_state.session_titles.remove(&map_key);
                        changed = true;
                    }
                }
            }
        }
    }
    // 既に消えた lane の stale entry 掃除
    let stale: Vec<String> = ui
        .sidebar_state
        .session_titles
        .keys()
        .filter(|k| !current_keys.contains(k.as_str()))
        .cloned()
        .collect();
    for k in stale {
        ui.sidebar_state.session_titles.remove(&k);
        changed = true;
    }
    if changed {
        push_sidebar_state(&boot.webview, &ui.sidebar_state);
    }
}

pub(super) fn resolve_lane_inboxes(ui: &mut UiState, boot: &Boot) {
    // VP-147 PR-P2-3: 全 lane の mailbox inbox 状況を resolve → sidebar に push。
    //  poller (`spawn_lane_inbox_poller`) が 5s 間隔で tick を送ってここに来る。
    //  Phase 2 (icon visibility のみ) では default MessageState を populate して
    //  sidebar UI で `.vp-message-icon` 表示の signal とする。 backend peek API
    //  + 永続 store query は後続 PR で実装、 actual 値で MessageState を populate。
    use crate::pane::MessageState;
    let mut changed = false;
    let mut current_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for lanes in ui.sidebar_state.lanes_by_repo.values() {
        for lane in lanes {
            let address = lane.address.key();
            current_keys.insert(address.clone());
            // Phase 2 placeholder: default MessageState (= unread_count 0、 has_persistent false)
            // 既存 entry が無い (= 初回 tick or 新規 lane) 場合のみ insert、 上書きしない。
            // TODO 後続 PR (Phase 2.5): backend peek API を叩いて actual 値で update。
            //   `Vacant` ガードを `entry().and_modify(|s| *s = fetched).or_insert_with(...)`
            //   に書き換えて、 既存 entry の `unread_count` 等を refresh する。 現状 (Phase 2)
            //   は actual 値が無いので Vacant のみ insert で sufficient (icon visibility のみ)。
            if let std::collections::hash_map::Entry::Vacant(e) =
                ui.sidebar_state.lane_inboxes.entry(address)
            {
                e.insert(MessageState::default());
                changed = true;
            }
        }
    }
    // 既に消えた lane の stale entry 掃除
    let stale: Vec<String> = ui
        .sidebar_state
        .lane_inboxes
        .keys()
        .filter(|k| !current_keys.contains(k.as_str()))
        .cloned()
        .collect();
    for k in stale {
        ui.sidebar_state.lane_inboxes.remove(&k);
        changed = true;
    }
    if changed {
        push_sidebar_state(&boot.webview, &ui.sidebar_state);
    }
}

pub(super) fn ink_snapshot(
    ui: &mut UiState,
    boot: &Boot,
    proxy: &EventLoopProxy<AppEvent>,
    rect: crate::webview::ink_snapshot::InkRect,
) {
    // ink（対話面, doc 52 §3）: board pane（#ink-stage）を WKWebView.takeSnapshot で
    // PNG 化する。保存先 dir は active lane の flat key で分ける（board と同じ空間）。
    // completion（main thread）は InkSnapshotReady で event loop に戻す（proxy.clone）。
    let lane_key = ui
        .sidebar_state
        .active_lane_address
        .as_deref()
        .map(crate::webview::ink_snapshot::lane_key_from_address)
        .unwrap_or_else(|| "main".to_string());
    match crate::webview::ink_snapshot::snapshot_path(&lane_key) {
        Ok(out_path) => {
            let ready_proxy = proxy.clone();
            crate::webview::ink_snapshot::take_snapshot(
                &boot.webview,
                rect,
                out_path,
                move |path, error| {
                    let _ = ready_proxy.send_event(AppEvent::InkSnapshotReady { path, error });
                },
            );
        }
        Err(e) => {
            let _ = proxy.send_event(AppEvent::InkSnapshotReady {
                path: None,
                error: Some(format!("snapshot 保存先の作成に失敗: {e}")),
            });
        }
    }
}

pub(super) fn ink_snapshot_ready(
    _ui: &mut UiState,
    boot: &Boot,
    path: Option<String>,
    error: Option<String>,
) {
    // ink: snapshot 完了/失敗を webview に返す（ink.ts が会話へ一行 + 画像を送る）。
    // 成功と失敗で受け手の振る舞いが別なので event も 2 本（schema 参照）。
    match path {
        Some(p) => push_main::ink_snapshot(&boot.webview, p),
        None => push_main::ink_snapshot_error(&boot.webview, error.unwrap_or_default()),
    }
}

pub(super) fn debug_log_watch(
    ui: &mut UiState,
    _boot: &Boot,
    proxy: &EventLoopProxy<AppEvent>,
    source: String,
) {
    // R sidebar の debug log（sidebar view modes）: 世代を進めて旧 tail を退場させ、
    // 新しい tail thread を起こす（最後の watch が勝つ = 単一 tail）。
    use std::sync::atomic::Ordering;
    let generation = ui.debuglog_watch_gen.fetch_add(1, Ordering::Relaxed) + 1;
    match crate::debug_log::log_path(&source) {
        Some(path) => {
            crate::debug_log::spawn_tail(
                source,
                path,
                generation,
                ui.debuglog_watch_gen.clone(),
                proxy.clone(),
            );
        }
        None => tracing::warn!("debuglog:watch の未知 source: {source}"),
    }
}

pub(super) fn debug_log_unwatch(ui: &mut UiState, _boot: &Boot) {
    // 世代を進めるだけで tail は次の poll で止まる（見ていない log は読まない）。
    use std::sync::atomic::Ordering;
    ui.debuglog_watch_gen.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn debug_log_chunk(
    ui: &mut UiState,
    boot: &Boot,
    source: String,
    reset: bool,
    lines: Vec<String>,
    generation: u64,
) {
    // tail thread からの行群を R sidebar へ。退場直前の旧世代 thread が送った
    // 残 chunk はここで棄てる（新 backlog の後に旧行が 1 回混ざる race の封じ）。
    // stream なので replay は持たない（次の watch が毎回 backlog から始まる）。
    use std::sync::atomic::Ordering;
    if generation == ui.debuglog_watch_gen.load(Ordering::Relaxed) {
        push_main::debuglog_lines(&boot.webview, &source, reset, lines);
    }
}

pub(super) fn device_event(ui: &mut UiState, boot: &Boot, payload: serde_json::Value) {
    tracing::debug!("🧲 device event: {}", payload);
    // Phase 2: device 一覧を registry 更新 → sidebar (Devices badge) + main area
    // (DeviceRegistry pane の device list) の両方に push。
    if crate::pane::apply_device_event(&mut ui.sidebar_state.devices, &payload) {
        push_sidebar_state(&boot.webview, &ui.sidebar_state);
        push_main::render_devices(&boot.webview, &ui.sidebar_state.devices);
    }
    // fleet 配線 (doc 49 LE-19): 操作入力 (control_event) は webview の mapping
    // registry へ fire-and-forget 転送。受け手 (window.vpFleet) は gallery-panes.tsx。
    if let Some(js) = fleet_dispatch_js(&payload)
        && let Err(e) = boot.webview.evaluate_script(&js)
    {
        tracing::warn!("fleet dispatch: evaluate_script 失敗: {}", e);
    }
}

/// ===== code pane（コードブラウザ P1）=====
/// demand（CodeList / CodeRead）は blocking I/O を spawn_blocking に
/// 逃し、結果 event で main thread に戻して push する（旧 File Explorer と同型）。
pub(super) fn code_list(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    lane: String,
) {
    match lookup_lane_cwd_by_address(&ui.sidebar_state, &lane) {
        Some(cwd) => {
            let proxy = async_action_proxy.clone();
            boot.rt_handle.spawn_blocking(move || {
                let (entries, truncated) = crate::webview::file_explorer::list_entries(&cwd);
                let _ = proxy.send_event(AppEvent::CodeEntriesResult {
                    lane,
                    entries,
                    truncated,
                });
            });
        }
        None => {
            tracing::warn!("code:list: lane cwd unknown for address={lane} (skip)");
        }
    }
}

pub(super) fn code_read(
    ui: &mut UiState,
    boot: &Boot,
    async_action_proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    rel_path: String,
) {
    match lookup_lane_cwd_by_address(&ui.sidebar_state, &lane) {
        Some(cwd) => {
            let proxy = async_action_proxy.clone();
            boot.rt_handle.spawn_blocking(move || {
                let payload = crate::webview::file_explorer::read_file(&cwd, &rel_path);
                let _ = proxy.send_event(AppEvent::CodeFileResult {
                    lane,
                    rel_path,
                    payload,
                });
            });
        }
        None => {
            tracing::warn!("code:read: lane cwd unknown for address={lane} (skip)");
        }
    }
}

pub(super) fn code_entries_result(
    _ui: &mut UiState,
    boot: &Boot,
    lane: String,
    entries: Vec<crate::webview::file_explorer::Entry>,
    truncated: bool,
) {
    push_main::code_entries(&boot.webview, &lane, &entries, truncated);
}

pub(super) fn code_file_result(
    _ui: &mut UiState,
    boot: &Boot,
    lane: String,
    rel_path: String,
    payload: serde_json::Value,
) {
    push_main::code_file(&boot.webview, &lane, &rel_path, &payload);
}

/// Wire inbox (doc 34 §4 V1): fetch 結果を sidebar の vpWire 受け口へ push back。
pub(super) fn wire_history_result(
    _ui: &mut UiState,
    boot: &Boot,
    address: String,
    payload: serde_json::Value,
) {
    tracing::debug!("wire history 受領 (address={address})");
    push_sidebar::wire_result(&boot.webview, payload);
}

pub(super) fn activity_update(ui: &mut UiState, boot: &Boot, snap: crate::pane::ActivitySnapshot) {
    ui.sidebar_state.activity = snap;
    // 適用中フラグは GUI local（health 由来ではない）ので poll 上書きから守る。
    ui.sidebar_state.activity.update_applying = ui.update_applying;
    push_sidebar_state(&boot.webview, &ui.sidebar_state);
}
