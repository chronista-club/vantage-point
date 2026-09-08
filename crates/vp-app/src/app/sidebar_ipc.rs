//! sidebar IPC の **解釈 = state 遷移**（`IpcEnvelope` → `SidebarState` / `SessionState` の更新 + 効果要求）。
//!
//! 旧 `app/mod.rs` の `SidebarIpcOutcome` + `handle_sidebar_ipc`（棚卸し 項目 6 / 6-1 #4、2026-09-08。
//! 本文は順序付き diff で一致、差分は可視性 `pub(crate)` のみ）。webview/ ではなく app/ に置くのは
//! これが decode でなく state 遷移だから（Codex 再レビュー ⑥ / doc 60 §2）。効果の実行は `run()` の
//! `SidebarIpc` arm が `SidebarIpcOutcome` を読んで行う。
//!
//! ⚠️ 現状は純粋ではない: `ProcessToggle` / `ProcessReorder` で `session.save()`（file 書き込み）を呼ぶ。
//! 純粋化（保存要求を outcome で返す）は test を先に置いてから別 PR（doc 60 §6 A）。

use crate::daemon::pollers::ActionsPersistPayload;
use crate::pane::{ActiveComponent, SidebarState};
use crate::session_state::SessionState;

/// sidebar IPC を解釈した結果
#[derive(Debug, Default)]
pub(crate) struct SidebarIpcOutcome {
    /// SidebarState が変化したか (true なら push_sidebar_state を呼ぶ)
    pub(crate) changed: bool,
    /// active Lane/Component が変わったか (true なら push_active_view を呼ぶ)。
    /// Lane 選択の場合は `activate_lane` を使うこと（こちらは Agent 選択・Lane 削除用）。
    pub(crate) active_changed: bool,
    /// Lane activation 要求 — caller が `activate_lane()` を呼ぶ。
    /// `active_changed` とは排他（こちらが Some なら active_changed は不要）。
    pub(crate) activate_lane: Option<String>,
    /// repo auto-spawn が必要な repo (= 「Current」 になった dead な repo)。
    /// `(name, path)` を返し、 caller が `spawn_sp_start` を呼ぶ。
    /// dedup は caller の `repo_spawn_triggered: HashSet<String>` (path key) で行う。
    pub(crate) repo_spawn_request: Option<(String, String)>,
    /// Phase 3-A: Sub Lane 作成要求 `(repo_path, name, branch, agent)`。
    /// doc 24 §10 B-create: caller が daemon (:32000) の `create_sub_lane`
    /// (Unison `daemon-control.lanes/create`) を呼ぶ (repo port 解決は不要)。
    /// `agent` は doc 11 PR-C で追加 (None なら daemon-side default)。
    pub(crate) add_sub_request: Option<(String, String, Option<String>, Option<String>)>,
    /// doc 11 PR-C / F6④: 利用可能 Agent 一覧 fetch 要求 `(repo_path)`。
    /// caller が daemon repo-proxy ask (`agents_list`) を呼ぶ → `AppEvent::AgentsResult` で push back。
    pub(crate) list_stands_request: Option<String>,
    /// Phase 4-A: Sub Lane 削除要求 `(repo_path, address)`。
    /// caller が repo port を解決して `client.delete_lane` を呼ぶ。
    pub(crate) delete_lane_request: Option<(String, String)>,
    /// Lane Main Agent restart 要求 `(repo_path, address, fresh)`。
    /// caller が repo port を解決して `client.restart_lane` を呼ぶ。
    /// fresh=true は "New Main Session" (resume/continue 回避の fresh 起動)。
    pub(crate) restart_lane_request: Option<(String, String, bool)>,
    /// doc 39 §8.4 提案 2: 「New Root Conversation」要求 `(repo_path, lane_address)`。
    /// caller が repo の `conversation_session_new_root` を呼ぶ（非破壊 — 旧 root の会話は残る）。
    pub(crate) new_root_request: Option<(String, String)>,
    /// doc 44 D4: 開発起点の再指定要求 (repo_path, lane address)。
    /// 実体は Host の帳簿のポインタ更新だけで、lane は何も動かない (D5)。
    pub(crate) set_origin_request: Option<(String, String)>,
    /// doc 44 §12: lane の並び順の保存要求 (repo_path, lane address の表示順)。
    pub(crate) reorder_lanes_request: Option<(String, Vec<String>)>,
    /// Phase 5-C: Process restart 要求 `(repo_name)`。
    /// caller が daemon の Unison `daemon-control.repos/restart` を呼ぶ。
    pub(crate) restart_process_request: Option<String>,
    /// Process stop 要求 `(repo_name)`。
    /// caller が daemon の Unison `daemon-control.repos/stop` を呼ぶ。
    /// repo は registered のまま (停止しても sidebar リストに残り ▶ 起動が出る)。
    pub(crate) stop_process_request: Option<String>,
    /// Repo delete 要求 `(repo_name, repo_path)`。
    /// caller が repo を stop してから Unison `daemon-control.repos/remove` を呼ぶ。
    /// `repo_name` は stop 用、 `repo_path` は remove 用 (registry key)。
    pub(crate) delete_repo_request: Option<(String, String)>,
    /// Phase 1 (doc 24): repo 並び替えを daemon に永続化する要求 (path の順序列)。
    /// caller が `client.reorder_repos` を呼び、成功後に re-fetch → `ReposLoaded` で
    /// canonical 順を反映する。これで sidebar の D&D が daemon `repo_order` に一本化される。
    pub(crate) reorder_request: Option<Vec<String>>,
    /// Phase 5-D fix: repo auto-spawn dedup HashSet から path を release する要求。
    /// 「accordion を閉じる」 = 「ユーザが retry を望んでいる」 と解釈、 失敗ループの
    /// dedup deadlock を抜けられるようにする。 caller は `repo_spawn_triggered.remove(path)` を呼ぶ。
    pub(crate) repo_spawn_release: Option<String>,
    /// accordion の開閉が変わったので conversation 購読を張り直す要求。
    /// caller が全 lane に [`ensure_conversation_attach`] を撃ち直す（開いた repo は attach、
    /// 畳んだ repo は detach → daemon demand hook → 暇な engine が寝る）。
    /// これが無くても次の LanesLoaded（5s tick）で追随するが、toggle の手応えが遅れる。
    pub(crate) conversation_reattach: bool,
    /// Model Q: active lane を daemon canonical に永続する要求 `(repo_path, lane_address)`。
    /// caller が `client.set_active_lane` を fire-and-forget で呼ぶ (optimistic local は適用済)。
    pub(crate) set_active_lane_request: Option<(String, String)>,
    /// Wire inbox (doc 34 §4 V1): `wire:fetch` 要求 `(address)`。 caller が Daemon "wire" channel
    /// へ read-only request (wire/history + wire/unread-count) を投げ、
    /// `AppEvent::WireHistoryResult` で push back する (cursor 不触り)。
    pub(crate) wire_fetch_request: Option<String>,
    /// Wire inbox: `wire:ack` 要求 `(address, message_id)`。 lane の agent として ack した後、
    /// 再 fetch して `AppEvent::WireHistoryResult` で最新状態を push back する。
    pub(crate) wire_ack_request: Option<(String, String)>,
    /// in-app update: sidebar footer の「更新する」ボタン click 要求 `(latest_version)`。
    /// caller (event loop) が `flows::update::spawn_update_flow` を呼び、native 確認ダイアログ →
    /// self-update → `vp daemon restart` → GUI relaunch を専用スレッドで実行する。
    pub(crate) update_apply_request: Option<String>,
    /// Login ボタン click 要求。値 = token の宛先（"hub" | "creo"）。caller (event loop) が
    /// blocking pool で `flows::auth::run_login_blocking` (`vp auth login --for <target>` spawn) を
    /// 実行し、成功後に `daemon-control.hub/reconnect` で hub 接続へ即反映する。
    ///
    /// ⚠️ **identity は 1 つでも token は宛先ごと**（Auth0 の aud claim）。bool ではなく宛先を
    /// 運ぶのはそのため — 「ログインした」だけでは、どの API に対して有効かが決まらない。
    pub(crate) auth_login_request: Option<String>,
    /// ACTIONS の永続化要求（doc 57 Phase 4）。caller が coalesce channel へ流す。
    /// ⚠️ この arm は **`changed` を立てない** — DOM は既に user 入力で最新なので、
    /// 撃ち返すと編集中の行に古い値が入って caret が揺れる
    /// （`process:toggle` / `process:reorder` と同じ規律）。
    pub(crate) actions_persist_request: Option<ActionsPersistPayload>,
    /// Logout ボタン click 要求。値 = 宛先（`None` は「要求なし」、`Some("")` = 全宛先を捨てる）。
    /// caller が blocking pool で `flows::auth::run_logout_blocking` (確認ダイアログ →
    /// `vp auth logout [--for <target>]`) を実行し、成功後に `hub/reconnect` で即反映する。
    pub(crate) auth_logout_request: Option<String>,
    /// 設定 overlay の現在値要求（doc 59 P1）。caller が `settings:result` を push back する。
    pub(crate) settings_fetch_request: bool,
    /// 設定の保存要求（doc 59 P1）。**None の field は不変**（変えた分だけ送る契約）。
    /// caller が vp-app.toml へ書き、確定値を `settings:result` で push back する。
    pub(crate) settings_save_request: Option<crate::generated::sidebar_ipc::SettingsSave>,
    /// Add Repo 初期フォルダの folder picker 要求（doc 59 P1）。
    /// ⚠️ **キャンセル時は何もしない**（既存値を保持）。
    pub(crate) settings_pick_repo_root_request: bool,
    /// daemon 再起動要求（doc 59 P1）。⚠️ **全 repo = 全 lane の claude が落ちる**
    /// （doc 44 P1 fold-in）。caller が rfd 確認ダイアログ → `vp daemon restart` を
    /// 専用スレッドで実行する（`flows/update.rs` と同じ理由 = event loop を塞がない）。
    pub(crate) daemon_restart_request: bool,
}

/// sidebar webview から IPC で受け取った JSON を解釈し、`SidebarState` を mutate。
///
/// VP-208 PR-3: 旧 手 JSON parse (`parsed.get("t")` の文字列 match) を、 KDL schema
/// (`schema/vp-sidebar.kdl`) から生成した `IpcEnvelope` enum での typed dispatch に
/// 置き換えた。 wire ↔ Rust の drift は schema を SSOT にすることで解消される。
pub(crate) fn handle_sidebar_ipc(
    msg: &str,
    state: &mut SidebarState,
    session: &mut SessionState,
) -> SidebarIpcOutcome {
    use crate::generated::sidebar_ipc::IpcEnvelope;

    let mut out = SidebarIpcOutcome::default();
    let envelope: IpcEnvelope = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("sidebar IPC のデシリアライズ失敗: {} (msg={})", e, msg);
            return out;
        }
    };

    match envelope {
        IpcEnvelope::ProcessToggle(m) => {
            // VP-101 Phase A1.b: native <details> が IPC で `expanded` の新状態を渡してくる。
            // DOM は既に user click で toggle 済なので、Rust state を silently sync するだけ。
            // `out.changed` は立てない (rebuild すると flash する)。
            //
            // auto-spawn: expand=true で state==stopped の repo は
            // 「user が current として designate した未起動 repo」 として扱い、
            // repo auto-spawn を request する (repo lifecycle は daemon 責務)。
            //
            // 条件の "stopped" は client::RepoStatus::as_str() と一致させること。
            // 旧 ProcessState の "dead" 語彙から RepoStatus の "stopped" へ移行した
            // (VP-189) 際にこの条件の追従が漏れ、 auto-spawn が発火しなくなっていた。
            if let Some(p) = state.processes.iter_mut().find(|p| p.path == m.path) {
                let new_state = m.expanded;
                if p.expanded != new_state {
                    p.expanded = new_state;
                    tracing::debug!(
                        "process:toggle {} → expanded={} (silent sync)",
                        m.path,
                        p.expanded
                    );
                    // session 永続化: vp-app 再起動時に accordion 状態を復元
                    session.set_repo_expanded(m.path.clone(), new_state);
                    session.save();
                    // 「見えている Lane だけ生きている」: 開閉が変わったら購読を張り直す
                    // （畳んだ repo は detach → demand hook → 暇な engine が寝る）。
                    out.conversation_reattach = true;
                }
                if new_state && p.state.as_deref() == Some("stopped") {
                    out.repo_spawn_request = Some((p.name.clone(), p.path.clone()));
                }
                // Phase 5-D fix: accordion を閉じた = 「retry したい」signal と解釈、
                //  repo_spawn_triggered HashSet の entry を release。 これで spawn 失敗ループから
                //  抜けられる (collapse → expand で確実に retry が走る)。
                if !new_state {
                    out.repo_spawn_release = Some(p.path.clone());
                }
            }
        }
        IpcEnvelope::LaneDelete(m) => {
            // Phase 4-A: Sub Lane 削除要求。 caller (event loop) で repo port を解決して
            // client.delete_lane を呼ぶ。 active Lane を消した場合は active_lane_address を unset。
            if !m.path.is_empty() && !m.address.is_empty() {
                // active だった Lane が消えるなら preemptively clear (UI 反映を待たず)
                if state.active_lane_address.as_deref() == Some(m.address.as_str()) {
                    state.active_lane_address = None;
                    out.changed = true;
                    out.active_changed = true;
                }
                out.delete_lane_request = Some((m.path, m.address));
            }
        }
        IpcEnvelope::LaneRestart(m) => {
            // sidebar の restart icon → confirm dialog OK の連鎖。 caller が repo port を
            // 解決して `client.restart_lane` を呼ぶ。 active Lane を restart した場合は
            // WS が onclose → reconnect で新 PtySlot に attach し直す (PR #218)。
            if !m.path.is_empty() && !m.address.is_empty() {
                out.restart_lane_request = Some((m.path, m.address, m.fresh.unwrap_or(false)));
            }
        }
        IpcEnvelope::LaneNewRoot(m) => {
            // doc 39 §8.4 提案 2: 新しい root conversation を始める（非破壊）。caller (event loop)
            // が repo の `conversation_session_new_root` を撃つ — 新 session を採番して root に
            // 向け、旧 root の会話は session として残る（Reset Lane との対比が要点）。
            if !m.path.is_empty() && !m.address.is_empty() {
                out.new_root_request = Some((m.path, m.address));
            }
        }
        IpcEnvelope::LaneSetOrigin(m) => {
            // doc 44 D4: この lane を repo の開発起点にする。caller (event loop) が
            // daemon repo-proxy ask (`lane_origin_set`) を撃つ。結果は次の lanes snapshot に
            // `origin` として載って戻ってくるので、ここで sidebar_state を先読み更新しない
            // （帳簿が真実源 — 楽観更新すると失敗時に UI だけ嘘をつく）。
            if !m.path.is_empty() && !m.address.is_empty() {
                out.set_origin_request = Some((m.path, m.address));
            }
        }
        IpcEnvelope::LaneReorder(m) => {
            // doc 44 §12: sidebar の DnD で並び替えた結果を帳簿に保存する。
            // 起点と同じく **楽観更新しない** — 反映は次の lanes snapshot（server が
            // 帳簿の順で並べる）で戻る。#835 で push の起床が直ったので即座に届く。
            if !m.path.is_empty() && !m.order.is_empty() {
                out.reorder_lanes_request = Some((m.path, m.order));
            }
        }
        IpcEnvelope::LaneAddSub(m) => {
            // Phase 3-A: sidebar から Sub Lane 作成要求。 doc 24 §10 B-create:
            // caller (event loop) が daemon (:32000) の create_sub_lane を呼ぶ。
            // doc 11 PR-C: branch / agent は optional。 空文字は None に畳んで
            // daemon-side default にフォールバックさせる。
            let branch = m.branch.filter(|s| !s.is_empty());
            let agent = m.agent.filter(|s| !s.is_empty());
            if !m.path.is_empty() && !m.name.is_empty() {
                out.add_sub_request = Some((m.path, m.name, branch, agent));
            }
        }
        IpcEnvelope::AgentsFetch(m) => {
            // doc 11 PR-C: sidebar の + Add Sub form 開閉時に利用可能 Agent 一覧を取得。
            // caller (event loop) で daemon repo-proxy ask (`agents_list`) → sidebar の agents:result で push back。
            if !m.path.is_empty() {
                out.list_stands_request = Some(m.path);
            }
        }
        IpcEnvelope::StandSelect(m) => {
            // Phase 5-A: Repo-scope Agent row click → main area に対応 pane を表示
            // (Lane と mutually exclusive、 active_lane_address は preemptively clear)
            // DeviceRegistry 🧲 は machine-scope Agent (device = daemon 共通) なので path="" で来る。
            // machine-scope agent は path 空を許可、 それ以外 (Repo-scope) は path 必須。
            if m.kind.is_empty() || (m.path.is_empty() && m.kind != "devices") {
                tracing::warn!("stand:select with empty path/kind: {}", msg);
                return out;
            }
            let new_stand = ActiveComponent {
                repo_path: m.path.clone(),
                kind: m.kind.clone(),
            };
            // 既に同じ component が active なら no-op
            if state.active_component.as_ref() == Some(&new_stand) {
                return out;
            }
            tracing::info!("stand:select repo={} kind={}", m.path, m.kind);
            state.active_component = Some(new_stand);
            // Lane を排他で clear (= main area の active 軸を Agent に切替)
            if state.active_lane_address.is_some() {
                state.active_lane_address = None;
            }
            out.changed = true;
            out.active_changed = true;
        }
        IpcEnvelope::LaneSelect(m) => {
            if m.address.is_empty() {
                tracing::warn!("lane:select with empty address: {}", msg);
                return out;
            }
            let lanes_exist = state
                .lanes_by_repo
                .get(m.path.as_str())
                .map(|lanes| lanes.iter().any(|l| l.address.key() == m.address))
                .unwrap_or(false);
            if !lanes_exist {
                tracing::warn!(
                    "lane:select 対象 lane が見つからない: path={} address={}",
                    m.path,
                    m.address
                );
                return out;
            }
            tracing::info!("lane:select {} address={}", m.path, m.address);
            out.activate_lane = Some(m.address.clone());
            // Model Q: active lane を daemon canonical に永続 (optimistic local は activate_lane で適用)。
            out.set_active_lane_request = Some((m.path.clone(), m.address.clone()));
        }
        IpcEnvelope::ProcessReorder(m) => {
            // Currents セクションを drag-and-drop で並び替えた時の通知。
            // payload: `{"t":"process:reorder","order":["/path/a","/path/b",...]}`。
            tracing::info!("process:reorder: {} entries", m.order.len());
            // optimistic 反映: session 保存 + SidebarState（次回 push で JS 側 sort に使う）。
            // changed フラグは立てない (DOM 順は user 操作で既に変わっている、re-push で flash を避ける)。
            session.currents_order = Some(m.order.clone());
            session.save();
            state.currents_order = Some(m.order.clone());
            // Phase 1 (doc 24): daemon の repo_order にも永続化する。
            // caller が client.reorder_repos → re-fetch → ReposLoaded で canonical を反映し、
            // sidebar / ROTO / CLI vp repos を 1 つの順序源に揃える。
            out.reorder_request = Some(m.order);
        }
        IpcEnvelope::ProcessRestart(m) => {
            // Phase 5-C: repo name (from p.path → leaf name) を抽出して async restart に投げる。
            // path は normalized full path、 repo の API は repo name で識別する。
            if m.path.is_empty() {
                tracing::warn!("process:restart with empty path: {}", msg);
                return out;
            }
            let repo_name = std::path::Path::new(&m.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(m.path.as_str())
                .to_string();
            tracing::info!("process:restart {} (repo_name={})", m.path, repo_name);
            out.restart_process_request = Some(repo_name);
        }
        IpcEnvelope::ProcessStop(m) => {
            // repo を停止する (repo は registered のまま sidebar リストに残る)。
            // restart と同様 path の leaf name を repo name として扱う。
            if m.path.is_empty() {
                tracing::warn!("process:stop with empty path: {}", msg);
                return out;
            }
            let repo_name = std::path::Path::new(&m.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(m.path.as_str())
                .to_string();
            tracing::info!("process:stop {} (repo_name={})", m.path, repo_name);
            out.stop_process_request = Some(repo_name);
        }
        IpcEnvelope::RepoDelete(m) => {
            // repo を完全に削除 (repo 停止 + repos.kdl から unregister)。
            // UI 側で 2-click 確認済。 stop 用に repo_name、 remove 用に path を渡す。
            if m.path.is_empty() {
                tracing::warn!("repo:delete with empty path: {}", msg);
                return out;
            }
            let repo_name = std::path::Path::new(&m.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(m.path.as_str())
                .to_string();
            tracing::info!("repo:delete {} (repo_name={})", m.path, repo_name);
            out.delete_repo_request = Some((repo_name, m.path));
        }
        // repo:add は `AppEvent::SidebarIpc` の
        // dispatch 段で picker ルートに分岐済 (handle_sidebar_ipc には到達しない)。
        IpcEnvelope::RepoAdd => {
            tracing::debug!("sidebar IPC: picker 経路の message が handle_sidebar_ipc に到達");
        }
        IpcEnvelope::WireFetch(m) => {
            // Wire inbox (doc 34 §4 V1): 選択 lane の wire 履歴 fetch 要求。
            if !m.address.is_empty() {
                out.wire_fetch_request = Some(m.address);
            }
        }
        IpcEnvelope::WireAck(m) => {
            // Wire inbox: lane の agent としての ack 要求 (ack 後に再 fetch)。
            if !m.address.is_empty() && !m.message_id.is_empty() {
                out.wire_ack_request = Some((m.address, m.message_id));
            }
        }
        IpcEnvelope::UpdateApply(m) => {
            // in-app update: sidebar footer の「更新する」ボタン click。version は
            // ダイアログ文言用の latest version。caller (event loop) が native 確認ダイアログ →
            // self-update → daemon restart → relaunch の破壊的フローを専用スレッドで起動する。
            if !m.version.is_empty() {
                out.update_apply_request = Some(m.version);
            }
        }
        IpcEnvelope::AuthLogin(m) => {
            // Login ボタン。caller が `vp auth login --for <target>` (browser OAuth) を blocking
            // pool で実行し、成功後に hub/reconnect で接続へ即反映する。
            // 省略 = "hub"（従来の Hub 行の挙動）。
            out.auth_login_request = Some(m.target.unwrap_or_else(|| "hub".to_string()));
        }
        IpcEnvelope::AuthLogout(m) => {
            // Logout ボタン。caller が確認ダイアログ → `vp auth logout [--for …]` → hub/reconnect。
            // 空文字 = 宛先指定なし = 全部捨てる（CLI の `--for` 省略と同じ意味）。
            out.auth_logout_request = Some(m.target.unwrap_or_default());
        }
        IpcEnvelope::SettingsFetch => {
            // 設定 overlay を開いた。現在値を引き直して push back する（開くたびに引くので、
            // 手で vp-app.toml を編集した後でも現実に追いつく）。
            out.settings_fetch_request = true;
        }
        IpcEnvelope::SettingsSave(m) => {
            out.settings_save_request = Some(m);
        }
        IpcEnvelope::SettingsPickRepoRoot => {
            out.settings_pick_repo_root_request = true;
        }
        IpcEnvelope::DaemonRestart => {
            out.daemon_restart_request = true;
        }
        IpcEnvelope::ActionsPersist(m) => {
            // ACTIONS の編集を creo へ。caller が 400ms coalesce channel に流す。
            // ⚠️ **`out.changed` を立てない**（上の field の注記どおり）。
            out.actions_persist_request = Some(ActionsPersistPayload {
                items: m.items,
                removed: m.removed,
            });
        }
    }
    out
}

/// 現行の挙動を固定する characterization test（doc 60 §6 A、純粋化の前に置く）。
///
/// 観測は 3 面: (1) `SidebarState` / `SessionState` の in-memory 変化、(2) `SidebarIpcOutcome` の
/// field、(3) `session.save()` の **file 書き込み**（`$XDG_STATE_HOME` を tempdir に向けて
/// `SessionState::path(0)` を読む）。純粋化後は (3) が「outcome の保存要求 + 呼び手の実行」に
/// 変わるが、`apply` helper が呼び手を模すので test 本体の観測は変えない。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_wire::LaneInfo;
    use crate::pane::RepoPaneState;

    /// 呼び手（`run()` の `SidebarIpc` arm）の模型。現状は解釈だけで効果は持たない。
    fn apply(msg: &str, state: &mut SidebarState, session: &mut SessionState) -> SidebarIpcOutcome {
        handle_sidebar_ipc(msg, state, session)
    }

    fn repo(path: &str, expanded: bool, status: Option<&str>) -> RepoPaneState {
        let mut p = RepoPaneState::new(path, path.rsplit('/').next().unwrap_or(path));
        p.expanded = expanded;
        p.state = status.map(str::to_string);
        p
    }

    fn lane(repo: &str, name: &str) -> LaneInfo {
        serde_json::from_value(serde_json::json!({"address": {"repo": repo, "name": name}}))
            .expect("LaneInfo deserialize")
    }

    /// primary instance の session file を読む（無ければ None = save されていない）。
    fn saved_session() -> Option<SessionState> {
        let p = SessionState::path(0).expect("state dir");
        let s = std::fs::read_to_string(p).ok()?;
        Some(serde_json::from_str(&s).expect("session json"))
    }

    const REPO: &str = "/w/vp";

    #[test]
    fn malformed_json_is_ignored() {
        let mut state = SidebarState::default();
        let mut session = SessionState::default();
        let out = apply("{not json", &mut state, &mut session);
        assert!(!out.changed && !out.active_changed && out.activate_lane.is_none());
        assert!(state.processes.is_empty() && session.repos.is_empty());
    }

    // ===== process:toggle =====

    #[test]
    fn process_toggle_expand_syncs_state_saves_session_and_reattaches() {
        let _env = crate::test_env::state_dir();
        let mut state = SidebarState::default();
        state.processes.push(repo(REPO, false, Some("running")));
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"process:toggle","path":"{REPO}","expanded":true}}"#),
            &mut state,
            &mut session,
        );
        assert!(state.processes[0].expanded);
        assert_eq!(session.repo_expanded(REPO), Some(true));
        // file にも書かれる（vp-app 再起動時の accordion 復元）
        assert_eq!(
            saved_session().expect("saved").repo_expanded(REPO),
            Some(true)
        );
        // DOM は user click で toggle 済なので再 push しない（flash 回避）
        assert!(!out.changed && !out.active_changed);
        assert!(out.conversation_reattach);
        assert_eq!(out.repo_spawn_request, None);
        assert_eq!(out.repo_spawn_release, None);
    }

    #[test]
    fn process_toggle_expand_of_stopped_repo_requests_spawn() {
        let _env = crate::test_env::state_dir();
        let mut state = SidebarState::default();
        state.processes.push(repo(REPO, false, Some("stopped")));
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"process:toggle","path":"{REPO}","expanded":true}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.repo_spawn_request,
            Some(("vp".to_string(), REPO.to_string()))
        );
        assert!(out.conversation_reattach);
    }

    #[test]
    fn process_toggle_without_change_does_not_save_but_still_requests_spawn() {
        let _env = crate::test_env::state_dir();
        let mut state = SidebarState::default();
        state.processes.push(repo(REPO, true, Some("stopped")));
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"process:toggle","path":"{REPO}","expanded":true}}"#),
            &mut state,
            &mut session,
        );
        // 既に expanded=true → sync も save も無し
        assert_eq!(session.repo_expanded(REPO), None);
        assert!(saved_session().is_none(), "変化が無いのに save された");
        assert!(!out.conversation_reattach);
        // ただし「expand かつ stopped」の auto-spawn 判定は変化の有無と独立
        assert_eq!(
            out.repo_spawn_request,
            Some(("vp".to_string(), REPO.to_string()))
        );
    }

    #[test]
    fn process_toggle_collapse_saves_and_releases_spawn_dedup() {
        let _env = crate::test_env::state_dir();
        let mut state = SidebarState::default();
        state.processes.push(repo(REPO, true, Some("stopped")));
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"process:toggle","path":"{REPO}","expanded":false}}"#),
            &mut state,
            &mut session,
        );
        assert!(!state.processes[0].expanded);
        assert_eq!(
            saved_session().expect("saved").repo_expanded(REPO),
            Some(false)
        );
        assert!(out.conversation_reattach);
        assert_eq!(out.repo_spawn_request, None);
        assert_eq!(out.repo_spawn_release, Some(REPO.to_string()));
    }

    #[test]
    fn process_toggle_unknown_path_is_noop() {
        let _env = crate::test_env::state_dir();
        let mut state = SidebarState::default();
        state.processes.push(repo(REPO, false, None));
        let mut session = SessionState::default();
        let out = apply(
            r#"{"t":"process:toggle","path":"/w/other","expanded":true}"#,
            &mut state,
            &mut session,
        );
        assert!(!state.processes[0].expanded);
        assert!(saved_session().is_none());
        assert!(!out.conversation_reattach && out.repo_spawn_request.is_none());
    }

    // ===== process:reorder =====

    #[test]
    fn process_reorder_saves_session_updates_state_and_requests_daemon_persist() {
        let _env = crate::test_env::state_dir();
        let mut state = SidebarState::default();
        let mut session = SessionState::default();
        let out = apply(
            r#"{"t":"process:reorder","order":["/w/b","/w/a"]}"#,
            &mut state,
            &mut session,
        );
        let order = vec!["/w/b".to_string(), "/w/a".to_string()];
        assert_eq!(session.currents_order, Some(order.clone()));
        assert_eq!(state.currents_order, Some(order.clone()));
        assert_eq!(
            saved_session().expect("saved").currents_order,
            Some(order.clone())
        );
        assert_eq!(out.reorder_request, Some(order));
        // DOM 順は user 操作で既に変わっている → 再 push しない
        assert!(!out.changed);
    }

    // ===== lane:select =====

    #[test]
    fn lane_select_existing_lane_requests_activation_and_canonical_persist() {
        let mut state = SidebarState::default();
        state.lanes_by_repo.insert(
            REPO.to_string(),
            vec![lane("vp", "root"), lane("vp", "sub-a")],
        );
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"lane:select","path":"{REPO}","address":"vp/sub-a"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(out.activate_lane.as_deref(), Some("vp/sub-a"));
        assert_eq!(
            out.set_active_lane_request,
            Some((REPO.to_string(), "vp/sub-a".to_string()))
        );
        // 楽観反映は caller の activate_lane() が行う — ここでは state を触らない
        assert_eq!(state.active_lane_address, None);
        assert!(!out.changed && !out.active_changed);
    }

    #[test]
    fn lane_select_unknown_or_empty_address_is_noop() {
        let mut state = SidebarState::default();
        state
            .lanes_by_repo
            .insert(REPO.to_string(), vec![lane("vp", "root")]);
        let mut session = SessionState::default();
        for msg in [
            format!(r#"{{"t":"lane:select","path":"{REPO}","address":"vp/ghost"}}"#),
            r#"{"t":"lane:select","path":"/w/none","address":"vp/root"}"#.to_string(),
            format!(r#"{{"t":"lane:select","path":"{REPO}","address":""}}"#),
        ] {
            let out = apply(&msg, &mut state, &mut session);
            assert!(out.activate_lane.is_none(), "{msg}");
            assert!(out.set_active_lane_request.is_none(), "{msg}");
        }
    }

    // ===== lane:delete =====

    #[test]
    fn lane_delete_of_active_lane_clears_active_and_requests_delete() {
        let mut state = SidebarState {
            active_lane_address: Some("vp/sub-a".to_string()),
            ..Default::default()
        };
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"lane:delete","path":"{REPO}","address":"vp/sub-a"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(state.active_lane_address, None);
        assert!(out.changed && out.active_changed);
        assert_eq!(
            out.delete_lane_request,
            Some((REPO.to_string(), "vp/sub-a".to_string()))
        );
    }

    #[test]
    fn lane_delete_of_inactive_lane_only_requests() {
        let mut state = SidebarState {
            active_lane_address: Some("vp/root".to_string()),
            ..Default::default()
        };
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"lane:delete","path":"{REPO}","address":"vp/sub-a"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(state.active_lane_address.as_deref(), Some("vp/root"));
        assert!(!out.changed && !out.active_changed);
        assert!(out.delete_lane_request.is_some());
        // 空 path / address は無視
        let out = apply(
            r#"{"t":"lane:delete","path":"","address":"vp/sub-a"}"#,
            &mut state,
            &mut session,
        );
        assert!(out.delete_lane_request.is_none());
    }

    // ===== stand:select =====

    #[test]
    fn stand_select_sets_component_and_clears_lane_exclusively() {
        let mut state = SidebarState {
            active_lane_address: Some("vp/root".to_string()),
            ..Default::default()
        };
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"stand:select","path":"{REPO}","kind":"board"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            state.active_component,
            Some(ActiveComponent {
                repo_path: REPO.to_string(),
                kind: "board".to_string()
            })
        );
        assert_eq!(state.active_lane_address, None);
        assert!(out.changed && out.active_changed);
        // 同じ component をもう一度 → no-op
        let out = apply(
            &format!(r#"{{"t":"stand:select","path":"{REPO}","kind":"board"}}"#),
            &mut state,
            &mut session,
        );
        assert!(!out.changed && !out.active_changed);
    }

    #[test]
    fn stand_select_devices_allows_empty_path_but_others_do_not() {
        let mut state = SidebarState::default();
        let mut session = SessionState::default();
        let out = apply(
            r#"{"t":"stand:select","path":"","kind":"devices"}"#,
            &mut state,
            &mut session,
        );
        assert!(out.changed);
        assert_eq!(
            state.active_component.as_ref().map(|c| c.kind.as_str()),
            Some("devices")
        );
        let out = apply(
            r#"{"t":"stand:select","path":"","kind":"board"}"#,
            &mut state,
            &mut session,
        );
        assert!(!out.changed);
        assert_eq!(
            state.active_component.as_ref().map(|c| c.kind.as_str()),
            Some("devices")
        );
    }

    // ===== lane 操作の要求（state は触らない） =====

    #[test]
    fn lane_requests_do_not_touch_state() {
        let mut state = SidebarState::default();
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"lane:restart","path":"{REPO}","address":"vp/root"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.restart_lane_request,
            Some((REPO.to_string(), "vp/root".to_string(), false))
        );
        let out = apply(
            &format!(r#"{{"t":"lane:restart","path":"{REPO}","address":"vp/root","fresh":true}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(out.restart_lane_request.map(|r| r.2), Some(true));

        let out = apply(
            &format!(r#"{{"t":"lane:new_root","path":"{REPO}","address":"vp/root"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.new_root_request,
            Some((REPO.to_string(), "vp/root".to_string()))
        );

        // 起点 / 並び順は帳簿が真実源 — 楽観更新しない
        let out = apply(
            &format!(r#"{{"t":"lane:set_origin","path":"{REPO}","address":"vp/sub-a"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.set_origin_request,
            Some((REPO.to_string(), "vp/sub-a".to_string()))
        );
        assert!(state.origin_by_repo.is_empty());
        let out = apply(
            &format!(r#"{{"t":"lane:reorder","path":"{REPO}","order":["vp/sub-a","vp/root"]}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.reorder_lanes_request,
            Some((
                REPO.to_string(),
                vec!["vp/sub-a".to_string(), "vp/root".to_string()]
            ))
        );
        let out = apply(
            &format!(r#"{{"t":"lane:reorder","path":"{REPO}","order":[]}}"#),
            &mut state,
            &mut session,
        );
        assert!(out.reorder_lanes_request.is_none());

        assert!(!out.changed && state.active_lane_address.is_none() && session.repos.is_empty());
    }

    #[test]
    fn lane_add_sub_folds_empty_branch_and_agent_to_none() {
        let mut state = SidebarState::default();
        let mut session = SessionState::default();
        let out = apply(
            &format!(
                r#"{{"t":"lane:add_sub","path":"{REPO}","name":"feat","branch":"","agent":""}}"#
            ),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.add_sub_request,
            Some((REPO.to_string(), "feat".to_string(), None, None))
        );
        let out = apply(
            &format!(
                r#"{{"t":"lane:add_sub","path":"{REPO}","name":"feat","branch":"b","agent":"codex"}}"#
            ),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.add_sub_request,
            Some((
                REPO.to_string(),
                "feat".to_string(),
                Some("b".to_string()),
                Some("codex".to_string())
            ))
        );
        let out = apply(
            &format!(r#"{{"t":"lane:add_sub","path":"{REPO}","name":""}}"#),
            &mut state,
            &mut session,
        );
        assert!(out.add_sub_request.is_none());
        let out = apply(
            &format!(r#"{{"t":"agents:fetch","path":"{REPO}"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(out.list_stands_request.as_deref(), Some(REPO));
    }

    // ===== process / repo 操作は path の leaf を repo name に =====

    #[test]
    fn process_and_repo_requests_use_leaf_name() {
        let mut state = SidebarState::default();
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"process:restart","path":"{REPO}"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(out.restart_process_request.as_deref(), Some("vp"));
        let out = apply(
            &format!(r#"{{"t":"process:stop","path":"{REPO}"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(out.stop_process_request.as_deref(), Some("vp"));
        let out = apply(
            &format!(r#"{{"t":"repo:delete","path":"{REPO}"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.delete_repo_request,
            Some(("vp".to_string(), REPO.to_string()))
        );
        for t in ["process:restart", "process:stop", "repo:delete"] {
            let out = apply(
                &format!(r#"{{"t":"{t}","path":""}}"#),
                &mut state,
                &mut session,
            );
            assert!(
                out.restart_process_request.is_none()
                    && out.stop_process_request.is_none()
                    && out.delete_repo_request.is_none(),
                "{t} with empty path"
            );
        }
        // repo:add は dispatch 段で picker に分岐済 — ここに来ても何もしない
        let out = apply(r#"{"t":"repo:add"}"#, &mut state, &mut session);
        assert!(!out.changed && out.repo_spawn_request.is_none());
    }

    // ===== wire / update / auth / settings / daemon / actions =====

    #[test]
    fn effect_only_arms_map_to_outcome_fields() {
        let mut state = SidebarState::default();
        let mut session = SessionState::default();
        let out = apply(
            &format!(r#"{{"t":"wire:fetch","path":"{REPO}","address":"vp/root"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(out.wire_fetch_request.as_deref(), Some("vp/root"));
        let out = apply(
            &format!(r#"{{"t":"wire:ack","path":"{REPO}","address":"vp/root","message_id":"m1"}}"#),
            &mut state,
            &mut session,
        );
        assert_eq!(
            out.wire_ack_request,
            Some(("vp/root".to_string(), "m1".to_string()))
        );
        let out = apply(
            &format!(r#"{{"t":"wire:ack","path":"{REPO}","address":"vp/root","message_id":""}}"#),
            &mut state,
            &mut session,
        );
        assert!(out.wire_ack_request.is_none());

        let out = apply(
            r#"{"t":"update:apply","version":"0.68.0"}"#,
            &mut state,
            &mut session,
        );
        assert_eq!(out.update_apply_request.as_deref(), Some("0.68.0"));
        let out = apply(
            r#"{"t":"update:apply","version":""}"#,
            &mut state,
            &mut session,
        );
        assert!(out.update_apply_request.is_none());

        // login の宛先省略 = "hub"、logout の宛先省略 = ""（全宛先）
        let out = apply(r#"{"t":"auth:login"}"#, &mut state, &mut session);
        assert_eq!(out.auth_login_request.as_deref(), Some("hub"));
        let out = apply(
            r#"{"t":"auth:login","target":"creo"}"#,
            &mut state,
            &mut session,
        );
        assert_eq!(out.auth_login_request.as_deref(), Some("creo"));
        let out = apply(r#"{"t":"auth:logout"}"#, &mut state, &mut session);
        assert_eq!(out.auth_logout_request.as_deref(), Some(""));

        let out = apply(r#"{"t":"settings:fetch"}"#, &mut state, &mut session);
        assert!(out.settings_fetch_request);
        let out = apply(
            r#"{"t":"settings:save","developer_mode":true}"#,
            &mut state,
            &mut session,
        );
        let save = out.settings_save_request.expect("settings_save");
        assert_eq!(save.developer_mode, Some(true));
        assert_eq!(
            save.log_level, None,
            "None の field は不変（変えた分だけ送る契約）"
        );
        let out = apply(
            r#"{"t":"settings:pick_repo_root"}"#,
            &mut state,
            &mut session,
        );
        assert!(out.settings_pick_repo_root_request);
        let out = apply(r#"{"t":"daemon:restart"}"#, &mut state, &mut session);
        assert!(out.daemon_restart_request);

        let out = apply(
            r#"{"t":"actions:persist","items":[{"id":"a"}],"removed":["b"]}"#,
            &mut state,
            &mut session,
        );
        let payload = out.actions_persist_request.expect("actions_persist");
        assert_eq!(payload.items.len(), 1);
        assert_eq!(payload.removed, vec!["b".to_string()]);
        // DOM は user 入力で最新 → 撃ち返さない
        assert!(!out.changed);

        assert!(state.active_lane_address.is_none() && session.repos.is_empty());
    }
}
