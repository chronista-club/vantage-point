//! Lane lifecycle の core 関数群 (lanes portless、 doc 27 §3.4.5)。
//!
//! SP 直結 HTTP route (`GET`/`POST`/`DELETE /api/lanes` `POST /api/lanes/restart`) は全廃。
//! create / list / delete / restart は全て World process-proxy ask の dispatch method
//! (`lane_create` / `lanes_list` / `lane_delete` / `lane_restart`) に移管し、 本 module は
//! その core 関数 (axum 非依存) のみを保持する。 全 surface (CLI flow / MCP / lane watcher) は
//! 同 dispatch method を共有する (semantics SSOT)。
//!
//! 関連 memory:
//! - `mem_1CaSsN7xj69aVQtLPQFJxQ` (SP-as-Project-Master: 9 component minimum)
//! - VP-124 Phase 1 (Lane Lifecycle delete orchestration、 `delete_lane_orchestrated`)
//!
//! ## core 関数 (dispatch_process_method が呼ぶ)
//!
//! - [`build_lanes_snapshot`] — `lanes_list` + QUIC `LanesSnapshot` publish 経路で共有する list
//!   (`LanePool` 由来のみ、 F.8 B Convergent で disk-scan merge 撤去。 disk-only Lane は lane
//!   watcher / SP bootstrap の auto-spawn 経由で active 化)
//! - [`create_performer_orchestrated`] — `lane_create` core (Phase 3-A: lane clone + PtySlot spawn、
//!   F.8 B Convergent で spawn 失敗時の disk dir rollback ポリシー)
//! - [`delete_lane_orchestrated`] / [`restart_lane_orchestrated`] — `lane_delete` / `lane_restart` core
//!   (F6②③、 PtySlot kill + tmux kill + SystemEvent broadcast)
//!
//! ## 未実装 (後 phase)
//!
//! - 1 件取得 (addr 指定) / Stand 切替 dispatch (addr encoding path 確定後)
//! - WS /ws/terminal の lane param 強化 (A4-2d)

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::super::lanes_state::{Diff, LaneAddress, LaneInfo, LaneKind, LaneState, SystemEvent};
use super::super::state::AppState;

// doc 11 §3.7 の `migrate_legacy_stand` shim は 2026-05-03 削除済。 PR #257 の
// stand 識別子 String 化と同タイミングで導入した「heavens_door / the_hand → echoes / shell」 (PR-pre2 で hd → echoes)
// migration shim を 1 release 期間 deprecation warn 付きで accept していたが、
// VP は user 1 人 + lane performer のみで vp-app + daemon が常に同 binary で deploy される
// 構成のため、 外部 client が旧 wire format で来る window が実質ゼロと判断、 即削除。

/// SP の全 Lane snapshot を build する (LanePool 由来のみ、 disk-only は乗せない)。
///
/// World process-proxy ask `lanes_list` と QUIC `lanes_snapshot` 両 publish 経路で **同一 logic**
/// を共有するための helper（旧 HTTP `GET /api/lanes` も同 logic だったが lanes portless で撤去）。
///
/// ## F.8 B Convergent (2026-05-26): disk-only Lane の表示廃止
///
/// 旧版は LanePool に居ない performer dir を disk-scan で `pid: None, state: Running` として
/// merge し sidebar に italic dim で表示していた。これは **中間状態 (= disk dir はあるが
/// LanePool に居ない)** を可視化する設計だったが、 click 不可 / Activate 経路なしで
/// 「死に体」 として user 体験を悪化させていた。
///
/// 新版では:
/// - sidebar に表示される Lane は **LanePool 由来のみ** (= 必ず active 化を試みた結果)
/// - disk dir 発見 → SP 起動時 bootstrap (server.rs) or lane watcher Create event
///   (capability/process_manager_capability.rs `handle_lane_create_event`) で
///   auto-spawn → LanePool 経由で sidebar に出る
/// - spawn 失敗時は LanePool に `LaneState::Dead` で record される (= 失敗が見える)
///
/// この変更で「disk dir はあるが sidebar に出ない」 ケースが理論上一瞬発生するが、
/// lane watcher が即座に POST /api/lanes を発火して spawn → LanePool entry が即追加
/// される (= convergence、 user 視点では遅延 ms 単位)。
///
/// Phase 5-D: Performer Lane に対しては `cwd` から git 状態 (`PerformerStatus`) を populate。
/// registry には保存せず、 build 時に都度 `performer_status()` を呼ぶ (volatile + 5-7 git subprocess)。
pub async fn build_lanes_snapshot(state: &AppState) -> Vec<LaneInfo> {
    let pool = state.lane_pool.read().await;
    let mut lanes = pool.list();
    drop(pool); // git subprocess 中の lock を保たない (performer_status は数 100ms かかる事あり)

    // 起動窓 self-heal: disk 上の intended performer で LanePool にまだ入っていない
    // (= SP 再起動直後、bootstrap の SpawnLane がまだ処理されず pool に入る前) ものを
    // Spawning(pid=null) で snapshot に merge する。
    //
    // 背景: F.8 B Convergent で disk-scan merge を撤去した結果、SP 再起動直後の初回 snapshot が
    // conductor-only になり、vp-app の LanesLoaded reconcile が performer を「消えた」と誤判定して
    // console を teardown する regression が発生していた。snapshot に常に intended performer を
    // 含めることで根治する。pid=null なので vp-app は ensureLane せず(xterm を描画しない)、spawn
    // 完了で pid が付いた次 snapshot で ensure される。steady-state では全 performer が pool 由来で
    // 既に snapshot に居るため address key で dedup し無加算。
    let project = std::path::Path::new(&state.project_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let present: std::collections::HashSet<String> =
        lanes.iter().map(|l| l.address.to_string()).collect();
    let default_stand = crate::config::Config::load()
        .unwrap_or_default()
        .default_stand_or_echoes()
        .to_string();
    for entry in
        crate::lane::commands::list_performers_for_repo(std::path::Path::new(&state.project_dir))
    {
        let address = crate::process::lanes_state::LaneAddress::performer(
            project.clone(),
            entry.name.clone(),
        );
        if present.contains(&address.to_string()) {
            continue; // 既に pool 由来 (spawn 済 or Dead) で snapshot に居る
        }
        lanes.push(LaneInfo {
            // doc 33 §2: 永続 console_mode (state file) を honor。 Default (=Tui) で埋めると
            // SP 再起動直後の boot 窓で chat lane が "tui" として snapshot に載り、 その窓で
            // vp-app が active lane を復元すると Act II の lane が xterm で開いてしまう。
            console_mode: crate::lane::console_mode::last(&project, &entry.name)
                .unwrap_or_default(),
            id: crate::lane::lane_id::load_or_create(&project, &entry.name),
            address,
            kind: LaneKind::Performer,
            name: Some(entry.name.clone()),
            state: crate::process::lanes_state::LaneState::Spawning,
            stand: default_stand.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid: None,
            cwd: entry.path,
            performer_status: None,
            cc_session_id: None,
        });
    }

    // 既存 Performer の git status を populate
    for lane in lanes.iter_mut() {
        if matches!(lane.kind, LaneKind::Performer) {
            let path = std::path::Path::new(&lane.cwd);
            if path.exists() && path.join(".git").exists() {
                lane.performer_status = Some(crate::lane::commands::performer_status(path));
            }
        }
        // R3-b: CC session id を state file から lazy read (書き手は SessionStart hook)。
        // 消費者 (echoes --resume) は conductor のみなので populate も限定し、
        // QUIC 5s tick 経路の syscall を抑える (moody 指摘 #2)。 performer の resume
        // policy 化 (設計メモ「fresh / resume が制限でなく policy になる」) の際に広げる。
        if matches!(lane.kind, LaneKind::Conductor) {
            lane.cc_session_id = crate::lane::cc_session::last(
                &lane.address.project,
                crate::process::stand_spawner::lane_label(&lane.address),
            );
        }
    }

    lanes
}

/// Performer Lane create の request body (Phase 3-A: Performer Lane create + lane clone)。
///
/// lanes portless: 旧 `POST /api/lanes` body。 World process-proxy ask `lane_create` の payload
/// として `dispatch_process_method` が serde で deserialize する。
#[derive(Debug, Deserialize)]
pub struct CreateLaneReq {
    /// "performer" を受付。 Conductor は project ごと固定。
    pub kind: String,
    /// Performer name (人間可読、 LaneAddress.name に入る)
    pub name: String,
    /// LaneStand: "echoes" (default) or "shell"
    #[serde(default)]
    pub stand: Option<String>,
    /// 既存 worktree path。 Some なら直接 cwd として使う、 None なら branch 指定で lane clone を実行する。
    #[serde(default)]
    pub cwd: Option<String>,
    /// Phase 3-A: lane clone する branch 名。 cwd が None で branch が Some の時、
    /// `vp lane new <name> <branch>` を SP 内で実行して performer dir を作成、 そこに Lane を spawn する。
    #[serde(default)]
    pub branch: Option<String>,
}

/// Performer Lane create core orchestration (Phase 3-A: lane clone + PtySlot spawn)。
///
/// lanes portless (doc 27 §3.4.5): 旧 `POST /api/lanes` の core を抽出し、 全 trigger
/// (MCP `add_performer`/`flow_handoff` / CLI `vp flow handoff` / lane watcher) が World
/// process-proxy ask `lane_create` 経由で共有する core logic に。 `delete_lane_orchestrated` /
/// `restart_lane_orchestrated` と対称 (SP HTTP route + axum handler は撤去)。
///
/// 流れ:
/// 1. 入力 validation (kind == "performer"、 name 非空)
/// 2. cwd 決定:
///    - `req.cwd` Some → そのまま使う
///    - `req.branch` Some → `vp lane new <name> <branch>` subprocess で performer dir 作成
///    - 両方 None → `<git-user>/<sanitized-name>` を auto-derive して lane clone
/// 3. PtySlot::spawn で実 PTY 起動 (LaneStand 別 command builder 経由)
/// 4. LanePool に insert (state=Running、 pid 付き)
///
/// 戻り値: 成功 `LaneInfo` / 失敗 `String`（旧 HTTP の CONFLICT="already exists" 等 error message
/// を保持。 unison error frame `{"error":..}` 経由で caller に Err 化される）。
///
/// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane = Session Process + lane clone 連動)
pub(crate) async fn create_performer_orchestrated(
    state: &Arc<AppState>,
    req: CreateLaneReq,
) -> Result<LaneInfo, String> {
    // 入力 validation。 "performer" のみ受付 (Conductor は project ごと固定で create 不可)。
    if req.kind != "performer" {
        return Err("kind must be 'performer' (Conductor is fixed per project)".to_string());
    }
    if req.name.trim().is_empty() {
        return Err("name is required".to_string());
    }

    // project_id: AppState の project_dir から basename
    let project_id = std::path::Path::new(&state.project_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let addr = LaneAddress::performer(&project_id, &req.name);
    // doc 11 PR-B: stand 識別子 String 化。 wire format は新 stand 名 (echoes / shell / tmux 等)
    // をそのまま受け取る。 未指定なら config の `default_stand` (未設定なら "echoes" fallback、
    // PR-pre2 / VP-118 で "hd" → "echoes" rename)。
    //
    // Config::load() は他 handler でも ad-hoc に呼ばれており (server.rs:49, mcp.rs:2577 等)、
    // SSOT は config.toml ファイル自体。 AppState に持たせない pattern を踏襲。
    let config = crate::config::Config::load().unwrap_or_default();
    let stand: String = req
        .stand
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| config.default_stand_or_echoes().to_string());

    // 重複チェック (早期 return)
    {
        let pool = state.lane_pool.read().await;
        if pool.get(&addr).is_some() {
            return Err(format!("Lane {} already exists", addr));
        }
    }

    // Phase 4-X / R5: cwd 決定 ── 優先順位 explicit cwd > lane clone (branch 明示 or auto-derive)。
    //
    // 旧 fallback (`else { state.project_dir }` で Conductor と同 worktree を share) は撤廃。
    // 理由: UI から name="sub" だけ入力した場合、 silent に Conductor と同 dir を共有してしまい、
    // 「Performer = 隔離 worktree」の mental model が崩れていた (race condition の温床)。
    //
    // 新規約: branch が None の時は `git config user.name` から prefix を取り、
    // `<user>/<sanitized-name>` 形式の branch を auto-derive して必ず lane clone を実行する。
    // explicit に同 dir を share したい場合は API caller が `cwd` を明示的に指定する。
    // F.8 B Convergent: cwd 決定経路を tag 付き で track。 spawn 失敗時の rollback 可否を判定する。
    // - `lane clone` 経路 (= 自分が作った disk dir): spawn 失敗時 rollback (disk dir 削除)
    // - `explicit cwd` 経路 (= user / watcher が既存 dir を渡してきた): rollback しない (= dir 保護)
    let (cwd, was_lane_cloned) = if let Some(c) = req.cwd {
        (c, false)
    } else {
        let branch = req
            .branch
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                derive_default_branch(std::path::Path::new(&state.project_dir), &req.name)
            });
        let project_dir = state.project_dir.clone();
        let name = req.name.clone();
        let branch_for_log = branch.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::lane::commands::new_performer_in(
                std::path::Path::new(&project_dir),
                &name,
                &branch,
                false,                                      // force=false
                crate::lane::commands::Isolation::Worktree, // SP は worktree default
            )
        })
        .await
        .map_err(|e| format!("lane task join: {}", e))?;
        // lane::commands::new_performer_in は performer dir 既存 + force=false の時に
        // 「パフォーマー '<name>' は既に存在します」を返す。 error message をそのまま流し、
        // caller (MCP add_performer 等) が "既に存在"/"already exists" を substring 判定可能にする。
        let path_buf =
            result.map_err(|e| format!("lane clone failed (branch={}): {}", branch_for_log, e))?;
        tracing::info!(
            "Performer Lane clone: name={} branch={} dir={}",
            req.name,
            branch_for_log,
            path_buf.display()
        );
        (path_buf.to_string_lossy().into_owned(), true)
    };

    // PtySlot::spawn は openpty + spawn_command の OS syscall でブロッキング。
    // Phase review fix #2: tokio worker thread (= async executor の OS thread) を占有しないよう spawn_blocking でラップ。
    // Phase 4-X の lane clone と同じ pattern。
    // tmux decoupling PR2: 床 (login shell) + claude 注入の Rust-native spawn (design §13)。
    let cmd = crate::process::stand_spawner::build_stand_command(
        &stand,
        &addr,
        std::path::Path::new(&cwd),
        false,
    );
    let spawn_result = tokio::task::spawn_blocking(move || {
        crate::process::stand_spawner::spawn_stand(&cmd, 120, 48)
    })
    .await
    .map_err(|e| format!("PtySlot spawn task join: {}", e))?;
    let (lane_state, pid) = match spawn_result {
        Ok((slot, term_rx)) => {
            let pid = slot.pid();
            let mut pool = state.lane_pool.write().await;
            // Stage 1 (ADR-0001): TermAttach も同時 spawn (race フリー、 Conductor 経路と統一)
            pool.insert_pty_slot(addr.clone(), slot, term_rx);
            tracing::info!(
                "Performer Lane spawned: addr={} stand={} cwd={} pid={}",
                addr,
                stand,
                cwd,
                pid
            );
            (LaneState::Running, Some(pid))
        }
        Err(e) => {
            // F.8 B Convergent: spawn 失敗時の rollback ポリシー
            // - was_lane_cloned=true (自分で disk dir を作った): rollback で dir 削除 + 500 早期 return
            //   (= 中間状態 disk-only Lane を残さない)
            // - was_lane_cloned=false (explicit cwd): dir は user / watcher 由来なので保護、
            //   Dead state で LanePool に record (= sidebar に失敗が見える、 後で手動 retry 可能)
            if was_lane_cloned {
                tracing::warn!(
                    "Performer Lane spawn failed → rollback (lane clone で作った disk dir を削除): addr={} cwd={}: {}",
                    addr,
                    cwd,
                    e
                );
                let cwd_for_rm = cwd.clone();
                let rm_result =
                    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&cwd_for_rm)).await;
                match rm_result {
                    Ok(Ok(())) => tracing::info!("rollback: disk dir 削除成功 cwd={}", cwd),
                    Ok(Err(rm_err)) => tracing::warn!(
                        "rollback: disk dir 削除失敗 (orphan dir 残置) cwd={}: {}",
                        cwd,
                        rm_err
                    ),
                    Err(join_err) => {
                        tracing::warn!("rollback: rm task join 失敗 cwd={}: {}", cwd, join_err)
                    }
                }
                return Err(format!(
                    "Performer Lane spawn failed (rollback executed): {}",
                    e
                ));
            }
            tracing::warn!(
                "Performer Lane spawn failed (explicit cwd、 Dead で record): addr={} cwd={}: {}",
                addr,
                cwd,
                e
            );
            (LaneState::Dead, None)
        }
    };

    // I1: performer の安定 id を address (project, name) で load_or_create。
    // 注: 同期 file IO だが cc_session lazy read と同様 数 ms、 spawn_blocking 隔離は省略 (pre-MVP)。
    let lane_id = crate::lane::lane_id::load_or_create(&addr.project, &req.name);
    let info = LaneInfo {
        console_mode: Default::default(),
        id: lane_id,
        address: addr.clone(),
        kind: LaneKind::Performer,
        name: Some(req.name.clone()),
        state: lane_state,
        stand: stand.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        pid,
        cwd,
        // create 時点では git 状態は registry に保存しない、 GET 時に都度 performer_status() で取得
        performer_status: None,
        cc_session_id: None,
    };

    {
        let mut pool = state.lane_pool.write().await;
        pool.insert(info.clone());
    }

    // wiremsg Stage 0: Lane 追加を SystemEvent::Lane(Diff::Add) で発火する。
    // これを購読する producer (server.rs) が LanePool 全 snapshot を retained topic
    // (`process/star-platinum/state/lanes`) に republish し、vp-app の "lanes" 購読へ
    // push される。delete 経路 (delete_lane_orchestrated) は Diff::Remove を発火済だが、
    // create 経路はこれが欠けており add_performer 後に sidebar が追従しなかった (Stage 1
    // consumer dogfood で発覚)。
    if let Err(e) = state.system_event_tx.send(SystemEvent::Lane(Diff::Add {
        payload: info.clone(),
    })) {
        tracing::warn!(
            "SystemEvent::Lane(Diff::Add) broadcast failed: addr={} err={}",
            addr,
            e
        );
    }

    Ok(info)
}

/// VP-124 Phase 1: Lane delete orchestration の戻り値。
///
/// 全 trigger (HTTP DELETE / MCP `delete_performer` / `vp lane rm` CLI) が共有する成功 payload。
#[derive(Debug, Serialize)]
pub struct DeletedLaneInfo {
    /// Display 形 ("<project>/performer/<name>")
    pub address: String,
    /// PtySlot drop 直前の child pid (= killed)
    pub pid: Option<u32>,
    /// lane workspace cleanup の結果 (None = cleanup=false で skip)
    pub cleanup_status: Option<String>,
}

/// VP-124 Phase 1: Lane delete orchestration の error。
///
/// HTTP handler はこれを 4xx ステータスに mapping。 MCP / CLI も同じ enum を消費。
#[derive(Debug, thiserror::Error)]
pub enum DeleteLaneError {
    /// architecture rule: Conductor Lane は project lifetime 紐付きのため削除不可。
    #[error("Conductor Lane is fixed per project and cannot be deleted (use SP shutdown instead)")]
    ConductorCannotBeDeleted,
    /// LanePool に該当 address の entry なし (idempotent re-call で発生)。
    #[error("Lane not found: {0}")]
    LaneNotFound(LaneAddress),
}

/// VP-124 Phase 1: Lane delete の 3-step orchestration を関数化。
///
/// 全 trigger (HTTP DELETE / MCP `delete_performer` / `vp lane rm` CLI / future Phase 3 FSEvents
/// watcher) が共有する core logic。 既存 `delete_handler` から extract、 同時に **欠落していた
/// tmux session kill + SystemEvent broadcast を補完** (= bug fix 兼 refactor)。
///
/// ## 動作
///
/// 1. **architecture rule check**: Conductor は削除拒否 (`DeleteLaneError::ConductorCannotBeDeleted`)
/// 2. **Phase 1 (in-memory authoritative mutation)**: `LanePool::remove` で LaneInfo + PtySlot を
///    drop (PtySlot::Drop で child kill + wait)
/// 3. **Phase 2a (state file GC)**: `console_mode::clear` + `cc_session::clear` で lane 単位
///    state file を削除 (best-effort。 残すと同名 lane 再作成時に旧 mode / 旧 session が蘇る)
/// 4. **Phase 2b (filesystem cleanup)**: `cleanup=true` なら `lane::remove_performer_in` で workspace
///    dir 削除 (best-effort、 既存挙動踏襲)
/// 5. **Phase 3 (broadcast)**: `SystemEvent::Lane(Diff::Remove)` を broadcast → sidebar 即時反映
///    (既存挙動では欠落していた → sidebar refresh 不全の根本原因)
///
/// ## 契約
///
/// - **idempotent**: 二度呼ばれても 2 回目は `LaneNotFound` を返す、 sidebar 状態に矛盾なし
/// - **best-effort cleanup**: tmux / lane 失敗は warn log のみ、 LanePool 削除は authoritative success
/// - **失敗時**: Phase 1 で fail (Conductor / NotFound) なら early return、 Phase 2 以降の partial failure
///   は `DeletedLaneInfo` の field で結果を伝える
///
/// 関連: VP-124 (PR-Phase 1 設計)、 mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane lifecycle)
pub async fn delete_lane_orchestrated(
    state: &Arc<AppState>,
    addr: LaneAddress,
    cleanup: bool,
) -> Result<DeletedLaneInfo, DeleteLaneError> {
    // architecture rule: Conductor Lane は project lifetime 紐付きのため削除不可
    if matches!(addr.kind, LaneKind::Conductor) {
        return Err(DeleteLaneError::ConductorCannotBeDeleted);
    }

    // Phase 1: in-memory authoritative state mutation。
    // PtySlot は LanePool 内部で保持されており、 remove() の戻り値 LaneInfo と一緒に
    // pool 外へ移動 → drop されるタイミングで child kill + wait される (PtySlot::Drop)。
    let info = {
        let mut pool = state.lane_pool.write().await;
        pool.remove(&addr)
            .ok_or(DeleteLaneError::LaneNotFound(addr.clone()))?
    };
    let pid = info.pid;

    tracing::info!(
        "Lane delete orchestrated: addr={} pid={:?} (PtySlot dropped → child killed)",
        addr,
        pid
    );

    // tmux decoupling PR2: 旧 Phase 2a (tmux session kill) は退役 — claude は PtySlot の
    // 子なので Phase 1 の remove (= PtySlot drop) で完全停止する（第 2 の生存木は無い）。

    // Phase 2a: lane 単位 state file の GC (best-effort)。 console_mode / cc_session は
    // lane 削除後に file が残ると、 同名 lane を作り直した時に旧 mode / 旧 session が
    // 蘇る (ghost file の state leak)。 lane lifecycle の終端であるここで両方消す。
    // cleanup flag には従わない — workspace dir と違い state file は lane が消えた時点で
    // 意味を失う (残す価値がない)。
    let lane_label = crate::process::stand_spawner::lane_label(&addr).to_string();
    if let Err(e) = crate::lane::console_mode::clear(&addr.project, &lane_label) {
        tracing::warn!(
            "lane delete: console_mode state の破棄に失敗 (file 残置): addr={} err={}",
            addr,
            e
        );
    }
    if let Err(e) = crate::lane::cc_session::clear(&addr.project, &lane_label) {
        tracing::warn!(
            "lane delete: cc_session state の破棄に失敗 (file 残置): addr={} err={}",
            addr,
            e
        );
    }

    // Phase 2b: lane workspace dir cleanup (best-effort、 cleanup=true 時のみ)。
    // 既存挙動踏襲、 直 lib call (`crate::lane::commands::remove_performer_in`)。
    // 注意: `spawn_blocking` closure は `repo_name` / `name` のみ move、 `addr` は capture
    // されないので後続 match arm の `tracing` で参照可能 (= compile time 保証)。
    let cleanup_status = if cleanup && let Some(name) = info.address.name.clone() {
        // project-local lane refactor PR 1: remove_performer_in は repo_root: &Path を受け取る。
        // sidebar delete trigger は dual-read で project-local + legacy global 両 path 対応。
        let repo_root = std::path::PathBuf::from(&state.project_dir);
        let result = tokio::task::spawn_blocking(move || {
            crate::lane::commands::remove_performer_in(&repo_root, &name)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                tracing::info!("lane remove 成功: {}", addr);
                Some("cleaned".to_string())
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "lane remove 失敗 (lane は削除済、 dir 残置): {}: {}",
                    addr,
                    e
                );
                Some("dir_retained_remove_failed".to_string())
            }
            Err(e) => {
                tracing::warn!("lane task join: {}", e);
                Some("dir_retained_join_failed".to_string())
            }
        }
    } else {
        None
    };

    // Phase 3: SystemEvent::Lane(Diff::Remove) broadcast → sidebar 即時反映。
    // 既存挙動では欠落していた → sidebar refresh 不全 bug の根本原因 (= 本 PR で fix)。
    // send 失敗 (= subscriber 全 drop) は warn のみ、 LanePool 削除は既に成功してるので
    // authoritative state は問題なし。
    if let Err(e) = state
        .system_event_tx
        .send(SystemEvent::Lane(Diff::Remove { id: addr.clone() }))
    {
        tracing::warn!(
            "SystemEvent::Lane(Diff::Remove) broadcast failed: addr={} err={}",
            addr,
            e
        );
    }

    Ok(DeletedLaneInfo {
        address: addr.to_string(),
        pid,
        cleanup_status,
    })
}

/// VP-131: restart の透過 retry 設定。 tmux kill + spawn の race / transient failure を
/// 吸収するため exponential backoff で 3 attempts まで自動 retry。 user click 1 回で
/// 「auto retry」 が走り、 dogfood UX で「Restart したら確実に Echoes 復活する」 を担保。
const RESTART_MAX_ATTEMPTS: u32 = 3;
const RESTART_BACKOFF_MS: [u64; 2] = [200, 500]; // attempt 0→1: 200ms、 attempt 1→2: 500ms

/// VP-131 / F6③ (doc 27 §3.4.5/§6): Lane restart の透過 retry orchestration を関数化
/// (`delete_lane_orchestrated` と対称)。 旧 `restart_handler` (HTTP) の retry loop を移植し、
/// process-proxy ask `lane_restart` が呼ぶ core logic に。 SP route + handler は撤去。
///
/// 動作:
/// 1. LanePool::restart_lane で 既存 PtySlot kill + tmux kill (VP-131) → 同 stand で respawn
/// 2. spawn 失敗時は exponential backoff で **最大 3 attempts まで透過 retry** (VP-131)
/// 3. vp-app は canvas channel demand 経由で透過的に新 PtySlot に再 attach
///
/// 戻り値: `{restarted, pid, attempts}` JSON / 全 attempts 失敗で `Err(err_msg)` (LaneInfo は
/// state=Dead に遷移済み)。
pub async fn restart_lane_orchestrated(
    state: &Arc<AppState>,
    addr: LaneAddress,
    fresh: bool,
) -> Result<serde_json::Value, String> {
    // VP-131: 透過 retry with exponential backoff。 各 attempt 間で write lock を release して
    // 他 handler を blocking しない設計、 tokio::time::sleep で async wait。
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..RESTART_MAX_ATTEMPTS {
        let result = {
            let mut pool = state.lane_pool.write().await;
            pool.restart_lane(&addr, fresh)
        };

        match result {
            Ok(()) => {
                let pid = state
                    .lane_pool
                    .read()
                    .await
                    .get(&addr)
                    .and_then(|i| i.pid)
                    .unwrap_or(0);
                // BUG#1: restart で PtySlot を差し替えたが、 World 側 subscriber は張りっぱなし
                // (count 1 のまま) で demand hook が再発火せず、 新 slot に pump が付かない
                // (= 入力は terminal_write で現 slot に届くが出力が沈黙 = 凍る console)。
                // 旧 pump handle が terminal_pumps に残っている = 購読者が居た証跡なので、
                // 新 slot に pump を張り直す (respawn_terminal_pump が旧 dead handle を abort して差替)。
                let lane_key = addr.to_string();
                let had_pump = state.terminal_pumps.read().await.contains_key(&lane_key);
                if had_pump {
                    let reattached =
                        crate::process::unison_server::respawn_terminal_pump(state, &lane_key)
                            .await;
                    tracing::info!(
                        "restart_lane: terminal pump re-attach (lane={} ok={})",
                        lane_key,
                        reattached
                    );
                }
                tracing::info!(
                    "Lane restart OK: addr={} new_pid={} attempts={}",
                    addr,
                    pid,
                    attempt + 1
                );
                return Ok(serde_json::json!({
                    "restarted": addr.to_string(),
                    "pid": pid,
                    "attempts": attempt + 1,
                }));
            }
            Err(e) => {
                tracing::warn!(
                    "Lane restart attempt {}/{} failed: addr={} err={}",
                    attempt + 1,
                    RESTART_MAX_ATTEMPTS,
                    addr,
                    e
                );
                last_err = Some(e);
                if attempt < RESTART_MAX_ATTEMPTS - 1 {
                    let backoff = RESTART_BACKOFF_MS[attempt as usize];
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
            }
        }
    }

    // 全 attempts 失敗 → LaneInfo.state は restart_lane 内で既に Dead 化済み
    Err(last_err
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unknown restart failure".to_string()))
}

/// Performer name から default branch を auto-derive する。
///
/// 形式: `<git-user>/<sanitized-name>`。
///
/// - `git-user` は `git config user.name` (repo local > global の標準解決) を lowercase + sanitize したもの。
///   取得失敗・空・sanitize 後 empty なら fallback `performer` prefix を使う。
/// - `sanitized-name` は `sanitize_for_branch` で git ref 制約に合わせる。
///
/// 例: user="Mako", name="sub" → `mako/sub`
///
/// branch 未指定時の create で使う。 doc 24 §10 B-create で daemon 側 create
/// (`routes/world.rs` の `world_create_lane`) からも sibling 呼びするため `pub(crate)`。
pub(crate) fn derive_default_branch(repo_root: &std::path::Path, name: &str) -> String {
    let prefix = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| sanitize_for_branch(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "performer".to_string());
    format!("{}/{}", prefix, sanitize_for_branch(name))
}

/// 文字列を git ref として安全な形に変換する。
///
/// 規則:
/// - lowercase
/// - ASCII alphanumeric + `-` `_` `.` 以外は `-` に置換
/// - 連続 `-` は 1 つに圧縮
/// - 先頭/末尾の `-` `.` は trim
///
/// ※ 完全な `git check-ref-format` 互換ではないが、 `~^:?*[\\` 等の禁止文字 + 制御文字を確実に除去する。
fn sanitize_for_branch(s: &str) -> String {
    let lowered: String = s
        .trim()
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| {
            // PR #228 review fix (Moody Blues #4): `.` を allowlist から外す。
            // git check-ref-format は連続 `.` (`..`) を禁止するため、 `v1.2.3` のような
            // 入力も `v1-2-3` として安全側に倒す。 末尾 `.lock` suffix も同時に予防。
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // 連続 `-` の圧縮
    let mut compact = String::with_capacity(lowered.len());
    let mut prev_dash = false;
    for c in lowered.chars() {
        if c == '-' {
            if !prev_dash {
                compact.push('-');
            }
            prev_dash = true;
        } else {
            compact.push(c);
            prev_dash = false;
        }
    }
    compact.trim_matches(|c| c == '-' || c == '.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_handles_typical_inputs() {
        assert_eq!(sanitize_for_branch("Mako"), "mako");
        assert_eq!(sanitize_for_branch("sub"), "sub");
        assert_eq!(sanitize_for_branch("Feat/API V2"), "feat-api-v2");
        assert_eq!(sanitize_for_branch("  spaces  "), "spaces");
        assert_eq!(sanitize_for_branch("multi---dash"), "multi-dash");
        assert_eq!(
            sanitize_for_branch("--leading-trailing--"),
            "leading-trailing"
        );
        assert_eq!(sanitize_for_branch("symbols!@#$%"), "symbols");
        assert_eq!(sanitize_for_branch(""), "");
        // PR #228 review fix (#4): `..` 連続が git ref として無効になるのを防ぐ
        assert_eq!(sanitize_for_branch("a..b"), "a-b");
        assert_eq!(sanitize_for_branch("v1.2.3"), "v1-2-3");
    }
}

#[cfg(test)]
mod core_tests {
    //! VP-13 sub-scope E: lanes.rs core 関数 smoke test。
    //!
    //! lanes portless (doc 27 §3.4.5): 旧 Axum oneshot route test は HTTP route 撤去に伴い
    //! `create_performer_orchestrated` / `build_lanes_snapshot` の直 call test に転換。
    //! 既存 `mod tests` (= helpers / sanitize_for_branch test 等) とは別 mod で配線。

    use super::*;

    /// テスト用の最小 `CreateLaneReq` builder（validation だけ叩く時に使う）。
    fn req(kind: &str, name: &str) -> CreateLaneReq {
        CreateLaneReq {
            kind: kind.to_string(),
            name: name.to_string(),
            stand: None,
            cwd: None,
            branch: None,
        }
    }

    /// boot-window merge 後: `build_lanes_snapshot` は LanePool 由来 + `list_performers_for_repo`
    /// の disk performer (pool 未登録分) を Spawning(pid=null) で merge する。ただし merge は
    /// **実在する intended performer 限定** (`<project_dir>/.vp/lanes/*`) なので、
    /// project_dir に performer worktree が無ければ (build_test_app_state は project_dir="" →
    /// list_performers_for_repo 空) LanePool が空 → snapshot も空。
    /// (performer 有りの merge 検証は project_dir を差せる fixture が要るため follow-up)
    #[tokio::test]
    async fn build_lanes_snapshot_empty_when_pool_and_disk_empty() {
        let state = crate::process::state::build_test_app_state(None).await;
        // LanePool 空 (LanePool::new()) + project_dir="" (performer 不在) → 0 件
        let lanes = build_lanes_snapshot(&state).await;
        assert!(
            lanes.is_empty(),
            "LanePool 空 + performer worktree 不在なら snapshot は空 (merge は intended performer 限定)"
        );
    }

    /// Worker → Performer rename 完結: `kind="worker"` は早期 Err を返す。
    /// 旧版は `req.kind != "performer" && req.kind != "worker"` で "worker" を許容していた。
    #[tokio::test]
    async fn create_rejects_worker_kind() {
        let state = crate::process::state::build_test_app_state(None).await;
        let err = create_performer_orchestrated(&state, req("worker", "test-performer"))
            .await
            .expect_err("kind='worker' は Err (legacy alias 一掃済)");
        assert!(
            err.contains("kind must be 'performer'"),
            "error message に kind 制約を含む: {}",
            err
        );
    }

    /// `create_performer_orchestrated`: name が空白のみの場合は早期 Err を返す。
    #[tokio::test]
    async fn create_rejects_empty_name() {
        let state = crate::process::state::build_test_app_state(None).await;
        let err = create_performer_orchestrated(&state, req("performer", "   "))
            .await
            .expect_err("name 空白のみは Err");
        assert!(
            err.contains("name is required"),
            "error message に name 必須を含む: {}",
            err
        );
    }
}
