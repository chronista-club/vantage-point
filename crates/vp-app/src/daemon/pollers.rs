//! daemon へ定期的に問い合わせる poller と、起動時 / 操作時に一度だけ走る spawner。
//!
//! - repo 一覧 fetch（`fetch_repos_with_ports` = `repos/list` + `registry.list` の join）
//! - activity poller（5 秒、`/api/health` + `repos/list` + `registry.list`）/ session title / lane inbox
//! - actions persist writer（coalescing channel）/ repo start（`spawn_sp_start`）/ menu event pump
//!
//! 全部 `spawn_*(&tokio::runtime::Handle, EventLoopProxy<AppEvent>, …)` の形で、結果は `AppEvent` で
//! event loop に返す（`tokio::spawn` 直書き禁止 = `rt_handle` を受ける）。
//!
//! 旧 `app/mod.rs` から移設（棚卸し 項目 6 / 6-1 #7、2026-09-08。本文は順序付き diff で一致、差分は
//! `spawn_*` の `pub(crate)` のみ。`port_merge_tests` も一緒に移設）。

use std::time::Duration;

use tao::event_loop::EventLoopProxy;

use crate::daemon::HealthProbe;
use crate::daemon::conn::{BOOT_CONTROL_WAIT, SharedDaemonConn};
use crate::daemon::control::DaemonControl;
use crate::daemon_wire::RepoInfo;
use crate::events::AppEvent;
use crate::pane::ActivitySnapshot;

/// muda の `MenuEvent::receiver()` channel を polling して `AppEvent::MenuClicked` に
/// 変換する pump スレッドを起動する。muda の menu event は global channel (single
/// receiver) なので 1 thread だけ起動する。
///
/// channel は sync (`rx.recv()` が blocking) なので shared runtime の blocking pool に逃す。
pub(crate) fn spawn_menu_event_pump(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
) {
    rt_handle.spawn_blocking(move || {
        let rx = muda::MenuEvent::receiver();
        while let Ok(ev) = rx.recv() {
            if proxy.send_event(AppEvent::MenuClicked(ev.id)).is_err() {
                tracing::debug!("EventLoop 終了、menu pump も終了");
                break;
            }
        }
    });
}

/// F6 (doc 27 §3.4): active_lane_address から対応する repo_path を引く。
///
/// active_lane_address (`<repo>/root` or `<repo>/sub/<name>`) から、 対応する
/// repo_path を引く。 daemon repo-proxy は repo port 不問・repo_path を path_key に正規化して
/// routing するので、 ask 系 (board mutate / lane ops) は port でなく path で引く。 解決失敗
/// (lane 未選択 / repo 未起動) なら `None`。 caller: `BoardMutate`（board_delete_item / board_clear）の
/// repo-proxy ask。
pub(crate) fn resolve_active_repo_path(state: &crate::pane::SidebarState) -> Option<String> {
    let active = state.active_lane_address.as_deref()?;
    for proc in &state.processes {
        if let Some(lanes) = state.lanes_by_repo.get(&proc.path)
            && lanes.iter().any(|l| l.address.key() == active)
        {
            return Some(proc.path.clone());
        }
    }
    None
}

pub(crate) fn merge_ports_from_running(
    repos: &mut [crate::daemon_wire::RepoInfo],
    running: &[crate::daemon_wire::RunningRepo],
) {
    let port_by_name: std::collections::HashMap<String, u16> = running
        .iter()
        .map(|r| (r.repo_name.clone(), r.port))
        .collect();
    for p in repos.iter_mut() {
        if let Some(&port) = port_by_name.get(&p.name) {
            p.port = Some(port);
        }
    }
}

/// 各 `RepoInfo.port` に runtime port を merge した list を返す。
///
/// `list_repos()` を直接呼んでそのまま `ReposLoaded` に乗せると、 config に port を
/// 書いていない repo (= 大多数) の port が `None` で来てしまい、 sidebar_state.processes
/// の port を全潰しする。 これが起きると以降の `LanesLoaded` で `ensureLane` が skip され
/// terminal が表示されなくなる (= restart / stop / delete 後の main console 消失 bug)。
/// **全 fetch 経路はこのヘルパ 1 本に集約**して同じ join をかける。
///
/// `list_processes` 側のみエラーなら空 map 扱い (= port は config 値のまま) で degrade、
/// `list_repos` 側エラーは bubble up する。
pub(crate) async fn fetch_repos_with_ports(
    control: &DaemonControl,
) -> anyhow::Result<Vec<RepoInfo>> {
    let (proj_res, run_res) = tokio::join!(control.list_repos(), control.list_processes());
    let mut repos = proj_res?;
    match run_res {
        Ok(runs) => merge_ports_from_running(&mut repos, &runs),
        Err(e) => {
            tracing::warn!("list_processes 失敗 (port 不明、 config 値のみ): {}", e);
        }
    };
    Ok(repos)
}

/// 起動時に daemon の Process list を別スレッドで fetch。
///
/// **Phase A4-3b bug fix (mem_1CaTpCQH8iLJ2PasRcPjHv Architecture v4)**:
/// `fetch_repos_with_ports` で registered + running を join して、各 Process に
/// `port` と `state` を解決した状態で `ReposLoaded` event に乗せる。
///
/// これにより handler 側で `if let Some(port) = p.port { spawn_lanes_subscription(...) }` が動く経路完成。
///
/// doc 45 段 3: transport は HTTP から Unison (`daemon-control` / `registry`) に移った。
/// 初回だけ `BOOT_CONTROL_WAIT` で待つ (daemon の auto-launch と競合するため)。
pub(crate) fn spawn_processes_fetch(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedDaemonConn,
) {
    rt_handle.spawn(async move {
        let result = match conn.control_within(BOOT_CONTROL_WAIT).await {
            Ok(control) => fetch_repos_with_ports(&control).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(processes) => {
                // polling 毎回発火するため log omit (= loop noise)。
                let _ = proxy.send_event(AppEvent::ReposLoaded(processes));
            }
            Err(e) => {
                tracing::warn!("daemon fetch 失敗 (daemon 未起動?): {}", e);
                let _ = proxy.send_event(AppEvent::ReposError(e.to_string()));
            }
        }
    });
}

/// 「Current repo が dead 状態」 のとき daemon に repo spawn を要求する fire-and-forget task。
///
/// State は daemon が持つ (mem_1CaTpCQH8iLJ2PasRcPjHv) ので、 vp-app は再起動しても
/// 既存 repo がいれば自動で続行 (state == running なので spawn 不要)。 dead のときだけ trigger。
///
/// 重複防止: 呼び出し側が `triggered: HashSet<String>` で path の dedup を担う。
/// (daemon 側でも `Process already running` で弾かれるが、 余計な POST を避けるため。)
pub(crate) fn spawn_sp_start(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    repo_name: String,
    repo_path: String,
    conn: SharedDaemonConn,
) {
    rt_handle.spawn(async move {
        let started = match conn.control().await {
            Ok(control) => control.start_process(&repo_name).await,
            Err(e) => Err(e),
        };
        match started {
            Ok(()) => {
                tracing::info!(
                    "repo auto-spawn 要求成功: repo={} path={}",
                    repo_name,
                    repo_path
                );
                // daemon の polling が新 repo を pick up すると、 既存の
                // spawn_processes_fetch / spawn_activity_poller が ReposLoaded を再送、
                // その流れで spawn_lanes_subscription が走って "lanes" channel を購読、
                // retained snapshot を受信して sidebar に Lane が出る。
                // ここで明示的に trigger する必要はない (polling が 5s で repo を拾う)。
                let _ = proxy; // 将来 spawn 完了通知 event を入れるなら使う
            }
            Err(e) => {
                tracing::warn!(
                    "repo auto-spawn 失敗: repo={} path={}: {}",
                    repo_name,
                    repo_path,
                    e
                );
            }
        }
    });
}

/// VP-95: Activity widget の定期更新。
///
/// 5 秒間隔で `/api/health` (HTTP) + `repos/list` + `registry.list` (Unison) を
/// fetch し、`AppEvent::ActivityUpdate` として main thread に push する。
/// daemon 未起動時は node_online=false で穏やかに通る。
///
/// VP-100 follow-up (B1 / MB1 / PH#7): daemon が **後発で online 復帰** した時、
/// `node_online: false → true` の遷移を検知して repo 一覧を
/// 再 fetch し `AppEvent::ReposLoaded` を再送する。これにより sidebar
/// repos accordion が永遠に空のまま、という UX バグを防ぐ。
/// 起動初回 (`prev_online == None`) では `spawn_processes_fetch` 側が担当するので
/// 二重 fetch を避けるため transition 検知をスキップする。
pub(crate) fn spawn_activity_poller(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedDaemonConn,
) {
    rt_handle.spawn(async move {
        let health = HealthProbe::default();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut prev_online: Option<bool> = None;
        let mut prev_running: Option<usize> = None;
        let mut prev_registered: Option<usize> = None;
        loop {
            tick.tick().await;
            // control client は tick ごとに取り直す (= 再接続後の新 client に自然に乗る)。
            let control = conn.control().await.ok();
            let snap = collect_activity(&health, control.as_ref()).await;
            let became_online = matches!(prev_online, Some(false)) && snap.node_online;
            let running_changed = prev_running.is_some_and(|p| p != snap.running_repo_count);
            // 登録数の変化（add / remove）。旧 trigger は running 数しか見ておらず、
            // 「全 repo を停止してから remove」の順で操作すると running が 0→0 のまま
            // 再 fetch が一度も走らず、sidebar が消えた repo を表示し続けた
            // （2026-07-24 実機）。数ベースなので rename / enable-flag / 並び順だけの変化は
            // 拾えない — それは daemon-repo の `ReposChanged` push（`subscriptions::spawn_repos_subscription`、
            // b-7）が担う。ここは push が届かなかった時の fallback として残す。
            let registered_changed = prev_registered.is_some_and(|p| p != snap.repo_count);
            prev_online = Some(snap.node_online);
            prev_running = Some(snap.running_repo_count);
            prev_registered = Some(snap.repo_count);
            if proxy
                .send_event(AppEvent::ActivityUpdate(snap.clone()))
                .is_err()
            {
                tracing::debug!("EventLoop 終了、activity poller も終了");
                break;
            }
            // 再 fetch trigger (Architecture v4 fix、 mem_1CaTpCQH8iLJ2PasRcPjHv):
            // - daemon online 復帰 (false → true)
            // - running 数変化 (repo 起動 / 停止)
            // どちらも port join 経由で ReposLoaded 再送 → sidebar state badge 更新
            if (became_online || running_changed || registered_changed)
                && snap.node_online
                && let Some(control) = control.as_ref()
                && let Ok(repos) = fetch_repos_with_ports(control).await
            {
                // polling tick で再 fetch → ReposLoaded を送るが、 log は omit
                // (= loop で noise)。 失敗時のみ warn にして残す。
                if proxy.send_event(AppEvent::ReposLoaded(repos)).is_err() {
                    break;
                }
            }
        }
    });
}

/// ACTIONS の永続化 1 回分（doc 57 Phase 4）。
///
/// `items` = 現在の一覧全件 / `removed` = user が明示的に消した id。
///
/// ⚠️ 名前が `generated::sidebar_ipc::ActionsPersist`（wire の request 型）と紛らわしいが
/// **別物** — あちらは受信 envelope、こちらは coalesce channel を流れる値。
#[derive(Debug, Clone)]
pub struct ActionsPersistPayload {
    pub items: Vec<serde_json::Value>,
    pub removed: Vec<String>,
}

/// ACTIONS の書きを **400ms coalesce** して daemon に流す task（doc 57 Phase 4）。
///
/// ## なぜ束ねるか
///
/// sidebar の書き換え口（`commitActions`）は**打鍵のたびに**呼ばれる。素通しすると 1 文字ごとに
/// creo へ HTTP が飛ぶ。`watch` は latest-wins なので、静まってから最後の 1 回だけを撃てば、
/// 「打ち終えた形」がちょうど 1 回書かれる。
///
/// ⚠️ **`removed` を落とさないのは webview 側の役目**。watch は途中の値を捨てるので、
/// webview は「daemon の snapshot が届くまで削除 id を持ち続けて毎回まるごと載せる」
/// （`actions-panel/store.ts` の `pendingRemovals`）。ここで蓄積すると、書きが失敗した時に
/// 消えた id の行方が 2 箇所に分かれる。
pub(crate) fn spawn_actions_persist_writer(
    rt_handle: &tokio::runtime::Handle,
    mut rx: tokio::sync::watch::Receiver<Option<ActionsPersistPayload>>,
    conn: SharedDaemonConn,
) {
    rt_handle.spawn(async move {
        loop {
            // 値が変わるまで眠る（初期値 None は読み飛ばす）。
            if rx.changed().await.is_err() {
                break; // 送り手（event loop）が落ちた
            }
            // 静まるまで待つ。待っている間に来た更新は watch が畳んでくれる。
            tokio::time::sleep(Duration::from_millis(400)).await;
            let Some(payload) = rx.borrow_and_update().clone() else {
                continue;
            };
            match conn.control().await {
                Ok(control) => {
                    if let Err(e) = control.save_actions(payload.items, payload.removed).await {
                        // 失敗しても手元の表示は消さない（次の編集 / 次の poll で再試行される）。
                        tracing::warn!("ACTIONS の永続化に失敗: {}", e);
                    }
                }
                Err(e) => tracing::warn!("ACTIONS の永続化: daemon 接続に失敗: {}", e),
            }
        }
    });
}

/// VP-143: 5s 間隔で `AppEvent::ResolveSessionTitles` を fire する background poller。
///
/// task 自体は state を持たず、 ただ tick を main thread に届ける役割。 main thread の
/// handler が `sidebar_state.lanes_by_repo` を walk して
/// `lane::title::resolve_title_for_cwd` を呼び、 結果を `session_titles` map に diff/update
/// + sidebar に push する。
///
/// `proxy.send_event` 失敗 (= EventLoop 終了) で task を終了する。 polling 周期は
/// `spawn_activity_poller` と揃えた 5s (`/rename` 反映までの max latency)。 file watch
/// (notify crate) に切り替えればリアルタイム化可能だが、 現時点は 1 lane / 1 cwd 仮定下では
/// polling で十分 (read-only mtime check + 末尾 grep のみ、 CPU 影響 minimal)。
pub(crate) fn spawn_session_title_poller(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
) {
    rt_handle.spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        // tokio::time::interval は 1 回目即発火、 起動 burst を避けるため空打ち skip
        tick.tick().await;
        loop {
            tick.tick().await;
            if proxy.send_event(AppEvent::ResolveSessionTitles).is_err() {
                tracing::debug!("EventLoop 終了、session title poller も終了");
                break;
            }
        }
    });
}

/// VP-147 PR-P2-3: 5s 間隔で `AppEvent::ResolveLaneInboxes` を fire する background poller。
///
/// `spawn_session_title_poller` と同 pattern (tokio current_thread runtime + interval tick)。
/// main thread が `sidebar_state.lanes_by_repo` を walk して各 lane の MessageState を
/// build し、 sidebar に push back する trigger となる。 Phase 2 PR-P2-3 では default 値の
/// placeholder を populate し、 sidebar UI で `.vp-message-icon` 表示の signal として動く。
/// 後続 PR で backend peek API + 永続 store query を実装して actual 値を populate する。
pub(crate) fn spawn_lane_inbox_poller(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
) {
    rt_handle.spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.tick().await;
        loop {
            tick.tick().await;
            if proxy.send_event(AppEvent::ResolveLaneInboxes).is_err() {
                tracing::debug!("EventLoop 終了、lane inbox poller も終了");
                break;
            }
        }
    });
}

/// `/api/health` (HTTP) + `repos/list` + `registry.list` (Unison) を集約して
/// `ActivitySnapshot` を組み立てる。各面の失敗時は default で穏当に通す。
///
/// doc 45 段 3 で control 面だけ Unison に移り、health は HTTP に残った (§2)。
/// `control` が `None` = 共有 QUIC connection が未確立で、この時 node_online は
/// HTTP health だけで決まる (= daemon は生きているが QUIC がまだ、を正しく表せる)。
async fn collect_activity(
    health: &HealthProbe,
    control: Option<&DaemonControl>,
) -> ActivitySnapshot {
    let mut snap = ActivitySnapshot::default();
    if let Ok(h) = health.daemon_health().await {
        snap.node_online = !h.status.is_empty();
        if !h.version.is_empty() {
            snap.daemon_version = Some(h.version);
        }
        if !h.started_at.is_empty() {
            snap.daemon_started_at = Some(h.started_at);
        }
        // hub federation 接続状態（Daemon 横の Hub インジケータ用）+ available nodes リスト
        // + 接続の auth 状態（Hub 行の Login / Logout ボタン切替用）。
        snap.hub = h.hub;
        snap.hub_nodes = h.hub_nodes;
        snap.hub_auth = h.hub_auth;
        snap.auth_targets = h.auth_targets;
        // in-app update: daemon の定期チェック結果（「更新する」ボタンの表示 gate + label）。
        snap.update_available = h.update_available;
        snap.latest_version = h.latest_version;
        // アイドル判定（doc 59 P3）: daemon の settings.kdl が唯一の真実源で、
        // GUI は表示閾値としてそれを borrow する（client 側に定数を持たない）。
        snap.idle_timeout_minutes = h.idle_timeout_minutes;
        // ACTIONS（doc 57 Phase 3）: daemon が creo から温めた一覧 + その版。
        // 版は sidebar 側が「当てるかどうか」を決めるのに使う（同じ版 = 撃ち返さない）。
        snap.actions = h.actions;
        snap.actions_rev = h.actions_rev;
        // L1 lifecycle: repo presence map（repo 行の ●◐○ dot 用、path → presence）。
        snap.presence = h
            .processes
            .into_iter()
            .map(|p| (p.path, p.presence))
            .collect();
    }
    if let Some(control) = control {
        if let Ok(repos) = control.list_repos().await {
            snap.repo_count = repos.len();
        }
        if let Ok(procs) = control.list_processes().await {
            snap.running_repo_count = procs.len();
        }
    }
    snap
}

#[cfg(test)]
mod port_merge_tests {
    //! `fetch_repos_with_ports` の core logic (= `merge_ports_from_running`) の unit test。
    //!
    //! HTTP 呼び出しを含む `fetch_repos_with_ports` 自体は integration test の領域だが、
    //! merge logic は pure calculation なので Small Test として検証する。

    use super::*;
    use crate::daemon_wire::{RepoInfo, RepoStatus, RunningRepo};

    fn make_repo(name: &str, port: Option<u16>) -> RepoInfo {
        RepoInfo {
            name: name.to_string(),
            path: format!("/repos/{name}"),
            port,
            state: RepoStatus::Running,
            ..RepoInfo::default()
        }
    }

    fn make_running(name: &str, port: u16) -> RunningRepo {
        RunningRepo {
            repo_name: name.to_string(),
            port,
        }
    }

    /// 正常系: running list の name と repo name が一致した場合に port が inject される。
    #[test]
    fn merge_injects_port_for_matched_repo() {
        let mut repos = vec![make_repo("vp", None), make_repo("creo", None)];
        let running = vec![make_running("vp", 33000), make_running("creo", 33001)];
        merge_ports_from_running(&mut repos, &running);
        assert_eq!(repos[0].port, Some(33000));
        assert_eq!(repos[1].port, Some(33001));
    }

    /// 正常系: running list に無い repo は port を変更しない (= None のまま)。
    #[test]
    fn merge_leaves_unmatched_repo_port_unchanged() {
        let mut repos = vec![make_repo("vp", None), make_repo("creo", None)];
        let running = vec![make_running("vp", 33000)]; // creo は running にない
        merge_ports_from_running(&mut repos, &running);
        assert_eq!(repos[0].port, Some(33000), "vp は inject される");
        assert_eq!(repos[1].port, None, "creo は変更されない");
    }

    /// 正常系: running list が空の場合、全 repo の port は変更されない。
    /// (= list_processes がエラーの場合の degrade path と同等)
    #[test]
    fn merge_with_empty_running_leaves_all_ports_unchanged() {
        let mut repos = vec![make_repo("vp", None), make_repo("creo", Some(33000))];
        merge_ports_from_running(&mut repos, &[]);
        assert_eq!(repos[0].port, None);
        assert_eq!(repos[1].port, Some(33000), "config の static port は維持");
    }

    /// 正常系: repo list が空の場合、panic しない。
    #[test]
    fn merge_with_empty_repos_is_noop() {
        let mut repos: Vec<RepoInfo> = vec![];
        let running = vec![make_running("vp", 33000)];
        merge_ports_from_running(&mut repos, &running);
        assert!(repos.is_empty());
    }

    /// 正常系: running に同名 repo が複数あっても最後 (HashMap 上書き) で一意に決まる。
    /// 実際の daemon は重複を持たないが、defensive に動作することを確認。
    #[test]
    fn merge_with_duplicate_running_entry_picks_one() {
        let mut repos = vec![make_repo("vp", None)];
        // HashMap なので同名は上書きされる — どちらかが選ばれれば OK
        let running = vec![make_running("vp", 33000), make_running("vp", 33001)];
        merge_ports_from_running(&mut repos, &running);
        assert!(repos[0].port.is_some(), "どちらか一方の port が入る");
    }

    /// 境界値: port が既に Some の repo も running の port で上書きされる。
    /// (= daemon の config port より runtime port が正確)
    #[test]
    fn merge_overwrites_existing_config_port_with_runtime_port() {
        let mut repos = vec![make_repo("vp", Some(9999))]; // config に static port
        let running = vec![make_running("vp", 33000)]; // runtime は別 port
        merge_ports_from_running(&mut repos, &running);
        assert_eq!(repos[0].port, Some(33000), "runtime port で上書きされる");
    }

    /// 異常系: name が大文字小文字違いの場合は match しない (= case-sensitive)。
    #[test]
    fn merge_is_case_sensitive() {
        let mut repos = vec![make_repo("VP", None)];
        let running = vec![make_running("vp", 33000)];
        merge_ports_from_running(&mut repos, &running);
        assert_eq!(repos[0].port, None, "大文字小文字違いは match しない");
    }
}
