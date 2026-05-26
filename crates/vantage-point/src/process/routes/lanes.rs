//! Lane REST API — GET / POST / DELETE / restart 実装済 (VP-124 Phase 1 完了時点)
//!
//! 関連 memory:
//! - `mem_1CaSsN7xj69aVQtLPQFJxQ` (SP-as-Project-Master: 9 component minimum)
//! - VP-124 Phase 1 (Lane Lifecycle delete orchestration、 `delete_lane_orchestrated`)
//!
//! ## 実装済
//!
//! - `GET /api/lanes` — `LanePool` の list (F.8 B Convergent で disk-scan merge 撤去、
//!   disk-only Lane は lane watcher / SP bootstrap で auto-spawn 経由 active 化)
//! - `POST /api/lanes` — Wing Lane create (Phase 3-A: lane clone + PtySlot spawn、
//!   F.8 B Convergent で spawn 失敗時の disk dir rollback ポリシー追加)
//! - `DELETE /api/lanes?address=<addr>&cleanup=true` — Lane destroy + cleanup
//!   (VP-124 Phase 1 で `delete_lane_orchestrated` に core 抽出、 全 trigger 共有)
//! - `POST /api/lanes/restart?address=<addr>` — Lead Stand restart (Phase A5)
//!
//! ## 未実装 (後 phase)
//!
//! - GET /api/lanes/{addr} (1 件取得、 addr URL encoding path 確定後)
//! - PUT /api/lanes/{addr}/stand (Stand 切替)
//! - WS /ws/terminal の lane param 強化 (A4-2d)

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::lanes_state::{
    Diff, LaneAddress, LaneInfo, LaneKind, LanePool, LaneState, SystemEvent,
};
use super::super::state::AppState;

// doc 11 §3.7 の `migrate_legacy_stand` shim は 2026-05-03 削除済。 PR #257 の
// stand 識別子 String 化と同タイミングで導入した「heavens_door / the_hand → echoes / shell」 (PR-pre2 で hd → echoes)
// migration shim を 1 release 期間 deprecation warn 付きで accept していたが、
// VP は user 1 人 + lane wing のみで vp-app + daemon が常に同 binary で deploy される
// 構成のため、 外部 client が旧 wire format で来る window が実質ゼロと判断、 即削除。

/// REST response: `GET /api/lanes` の JSON shape
#[derive(Debug, Serialize)]
pub struct LanesResponse {
    pub lanes: Vec<LaneInfo>,
}

/// SP の全 Lane snapshot を build する (LanePool 由来のみ、 disk-only は乗せない)。
///
/// HTTP `GET /api/lanes` と QUIC `lanes_snapshot` 両 publish 経路で **同一 logic**
/// を共有するための helper。
///
/// ## F.8 B Convergent (2026-05-26): disk-only Lane の表示廃止
///
/// 旧版は LanePool に居ない wing dir を disk-scan で `pid: None, state: Running` として
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
/// Phase 5-D: Wing Lane に対しては `cwd` から git 状態 (`WingStatus`) を populate。
/// registry には保存せず、 build 時に都度 `wing_status()` を呼ぶ (volatile + 5-7 git subprocess)。
pub async fn build_lanes_snapshot(state: &AppState) -> Vec<LaneInfo> {
    let pool = state.lane_pool.read().await;
    let mut lanes = pool.list();
    drop(pool); // git subprocess 中の lock を保たない (wing_status は数 100ms かかる事あり)

    // 既存 Wing の git status を populate
    for lane in lanes.iter_mut() {
        if matches!(lane.kind, LaneKind::Wing) {
            let path = std::path::Path::new(&lane.cwd);
            if path.exists() && path.join(".git").exists() {
                lane.wing_status = Some(crate::lane::commands::wing_status(path));
            }
        }
    }

    lanes
}

/// `GET /api/lanes` — Lane list を返す
///
/// 実 logic は [`build_lanes_snapshot`] に集約 (QUIC lanes_snapshot publish 経路と共有)。
/// CURRENTS Project (= SP 起動中) のみが /api/lanes に応答するので、 disk scan 対象が自動 enforce される。
pub async fn list_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let lanes = build_lanes_snapshot(&state).await;
    Json(LanesResponse { lanes })
}

/// `POST /api/lanes` request body (Phase 3-A: Wing Lane create + lane clone)
#[derive(Debug, Deserialize)]
pub struct CreateLaneReq {
    /// "wing" を受付。 Lead は project ごと固定。
    pub kind: String,
    /// Wing name (人間可読、 LaneAddress.name に入る)
    pub name: String,
    /// LaneStand: "echoes" (default) or "shell"
    #[serde(default)]
    pub stand: Option<String>,
    /// 既存 worktree path。 Some なら直接 cwd として使う、 None なら branch 指定で lane clone を実行する。
    #[serde(default)]
    pub cwd: Option<String>,
    /// Phase 3-A: lane clone する branch 名。 cwd が None で branch が Some の時、
    /// `vp lane new <name> <branch>` を SP 内で実行して wing dir を作成、 そこに Lane を spawn する。
    #[serde(default)]
    pub branch: Option<String>,
}

/// `POST /api/lanes` — Wing Lane create (Phase 3-A: lane clone + PtySlot spawn)
///
/// 流れ:
/// 1. 入力 validation (kind == "wing"、 name 非空)
/// 2. cwd 決定:
///    - `req.cwd` Some → そのまま使う
///    - `req.branch` Some → `vp lane new <name> <branch>` subprocess で wing dir 作成
///    - 両方 None → state.project_dir (= Lead と同じ dir) を share (legacy fallback)
/// 3. PtySlot::spawn で実 PTY 起動 (LaneStand 別 command builder 経由)
/// 4. LanePool に insert (state=Running、 pid 付き)
///
/// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane = Session Process + lane clone 連動)
pub async fn create_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateLaneReq>,
) -> Result<(StatusCode, Json<LaneInfo>), (StatusCode, Json<serde_json::Value>)> {
    // 入力 validation。 "wing" のみ受付 (Lead は project ごと固定で create 不可)。
    if req.kind != "wing" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "kind must be 'wing' (Lead is fixed per project)"
            })),
        ));
    }
    if req.name.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name is required" })),
        ));
    }

    // project_id: AppState の project_dir から basename
    let project_id = std::path::Path::new(&state.project_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let addr = LaneAddress::wing(&project_id, &req.name);
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
            return Err((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("Lane {} already exists", addr)
                })),
            ));
        }
    }

    // Phase 4-X / R5: cwd 決定 ── 優先順位 explicit cwd > lane clone (branch 明示 or auto-derive)。
    //
    // 旧 fallback (`else { state.project_dir }` で Lead と同 worktree を share) は撤廃。
    // 理由: UI から name="sub" だけ入力した場合、 silent に Lead と同 dir を共有してしまい、
    // 「Wing = 隔離 worktree」の mental model が崩れていた (race condition の温床)。
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
            crate::lane::commands::new_wing_in(
                std::path::Path::new(&project_dir),
                &name,
                &branch,
                false, // force=false
            )
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("lane task join: {}", e)})),
            )
        })?;
        let path_buf = result.map_err(|e| {
            // lane::commands::new_wing_in は wing dir 既存 + force=false の時に
            // 「ウィング '<name>' は既に存在します」を返す。 UI で input 下に表示するため
            // CONFLICT を返し、 error message をそのまま流す。
            let msg = e.to_string();
            let status = if msg.contains("既に存在") || msg.contains("already exists") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(json!({
                    "error": format!("lane clone failed (branch={}): {}", branch_for_log, msg)
                })),
            )
        })?;
        tracing::info!(
            "Wing Lane clone: name={} branch={} dir={}",
            req.name,
            branch_for_log,
            path_buf.display()
        );
        (path_buf.to_string_lossy().into_owned(), true)
    };

    // PtySlot::spawn は openpty + spawn_command の OS syscall でブロッキング。
    // Phase review fix #2: tokio worker thread (= async executor の OS thread) を占有しないよう spawn_blocking でラップ。
    // Phase 4-X の lane clone と同じ pattern。
    // Phase 1e: addr を渡して HD spec の tmux session 名導出に使う
    let cmd = crate::process::stand_spawner::build_stand_command(
        &stand,
        &addr,
        std::path::Path::new(&cwd),
    );
    // Phase 5-D: spawn_with_fallback で `claude --continue` 早期 exit 時に空 args で retry。
    // PR-D: cwd は cmd.cwd (install root) に集約、 引数からは削除。
    let spawn_result = tokio::task::spawn_blocking(move || {
        crate::process::stand_spawner::spawn_with_fallback(&cmd, 120, 48)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("PtySlot spawn task join: {}", e)})),
        )
    })?;
    let (lane_state, pid) = match spawn_result {
        Ok((slot, term_rx)) => {
            let pid = slot.pid();
            let mut pool = state.lane_pool.write().await;
            // Stage 1 (ADR-0001): TermAttach も同時 spawn (race フリー、 Lead 経路と統一)
            pool.insert_pty_slot(addr.clone(), slot, term_rx);
            tracing::info!(
                "Wing Lane spawned: addr={} stand={} cwd={} pid={}",
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
                    "Wing Lane spawn failed → rollback (lane clone で作った disk dir を削除): addr={} cwd={}: {}",
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
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": format!("Wing Lane spawn failed (rollback executed): {}", e)
                    })),
                ));
            }
            tracing::warn!(
                "Wing Lane spawn failed (explicit cwd、 Dead で record): addr={} cwd={}: {}",
                addr,
                cwd,
                e
            );
            (LaneState::Dead, None)
        }
    };

    let info = LaneInfo {
        address: addr.clone(),
        kind: LaneKind::Wing,
        name: Some(req.name.clone()),
        state: lane_state,
        stand: stand.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        pid,
        cwd,
        // create 時点では git 状態は registry に保存しない、 GET 時に都度 wing_status() で取得
        wing_status: None,
        // Phase 1e: spawn 成功時のみ tmux address を populate
        tmux: if matches!(lane_state, crate::process::lanes_state::LaneState::Running) {
            vec![crate::process::lanes_state::TmuxLaneAddress::for_spawn(
                &addr, &stand,
            )]
        } else {
            Vec::new()
        },
    };

    {
        let mut pool = state.lane_pool.write().await;
        pool.insert(info.clone());
    }

    // wiremsg Stage 0: Lane 追加を SystemEvent::Lane(Diff::Add) で発火する。
    // これを購読する producer (server.rs) が LanePool 全 snapshot を retained topic
    // (`process/star-platinum/state/lanes`) に republish し、vp-app の "lanes" 購読へ
    // push される。delete 経路 (delete_lane_orchestrated) は Diff::Remove を発火済だが、
    // create 経路はこれが欠けており add_wing 後に sidebar が追従しなかった (Stage 1
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

    Ok((StatusCode::CREATED, Json(info)))
}

/// `DELETE /api/lanes?address=<addr>` request の query
#[derive(Debug, Deserialize)]
pub struct DeleteLaneQuery {
    /// Display 形 ("<project>/lead" / "<project>/wing/<name>")
    pub address: String,
    /// Phase 4-B: lane workspace の dir も rm するか (default true)。
    /// false の場合 PtySlot だけ kill して dir 残置 (= debug / forensic 用途)。
    #[serde(default = "default_cleanup")]
    pub cleanup: bool,
}

fn default_cleanup() -> bool {
    true
}

/// VP-124 Phase 1: Lane delete orchestration の戻り値。
///
/// 全 trigger (HTTP DELETE / MCP `delete_wing` / `vp lane rm` CLI) が共有する成功 payload。
#[derive(Debug, Serialize)]
pub struct DeletedLaneInfo {
    /// Display 形 ("<project>/wing/<name>")
    pub address: String,
    /// PtySlot drop 直前の child pid (= killed)
    pub pid: Option<u32>,
    /// tmux session を kill できたか (best-effort、 不存在なら false)
    pub tmux_killed: bool,
    /// lane workspace cleanup の結果 (None = cleanup=false で skip)
    pub cleanup_status: Option<String>,
}

/// VP-124 Phase 1: Lane delete orchestration の error。
///
/// HTTP handler はこれを 4xx ステータスに mapping。 MCP / CLI も同じ enum を消費。
#[derive(Debug, thiserror::Error)]
pub enum DeleteLaneError {
    /// architecture rule: Lead Lane は project lifetime 紐付きのため削除不可。
    #[error("Lead Lane is fixed per project and cannot be deleted (use SP shutdown instead)")]
    LeadCannotBeDeleted,
    /// LanePool に該当 address の entry なし (idempotent re-call で発生)。
    #[error("Lane not found: {0}")]
    LaneNotFound(LaneAddress),
}

/// VP-124 Phase 1: Lane delete の 3-step orchestration を関数化。
///
/// 全 trigger (HTTP DELETE / MCP `delete_wing` / `vp lane rm` CLI / future Phase 3 FSEvents
/// watcher) が共有する core logic。 既存 `delete_handler` から extract、 同時に **欠落していた
/// tmux session kill + SystemEvent broadcast を補完** (= bug fix 兼 refactor)。
///
/// ## 動作
///
/// 1. **architecture rule check**: Lead は削除拒否 (`DeleteLaneError::LeadCannotBeDeleted`)
/// 2. **Phase 1 (in-memory authoritative mutation)**: `LanePool::remove` で LaneInfo + PtySlot を
///    drop (PtySlot::Drop で child kill + wait)
/// 3. **Phase 2a (tmux container cleanup)**: `tmux::kill_session` で PTY container 削除
///    (best-effort、 既存挙動では欠落していた → orphan tmux session の根本原因)
/// 4. **Phase 2b (filesystem cleanup)**: `cleanup=true` なら `lane::remove_wing_in` で workspace
///    dir 削除 (best-effort、 既存挙動踏襲)
/// 5. **Phase 3 (broadcast)**: `SystemEvent::Lane(Diff::Remove)` を broadcast → sidebar 即時反映
///    (既存挙動では欠落していた → sidebar refresh 不全の根本原因)
///
/// ## 契約
///
/// - **idempotent**: 二度呼ばれても 2 回目は `LaneNotFound` を返す、 sidebar 状態に矛盾なし
/// - **best-effort cleanup**: tmux / lane 失敗は warn log のみ、 LanePool 削除は authoritative success
/// - **失敗時**: Phase 1 で fail (Lead / NotFound) なら early return、 Phase 2 以降の partial failure
///   は `DeletedLaneInfo` の field で結果を伝える
///
/// 関連: VP-124 (PR-Phase 1 設計)、 mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane lifecycle)
pub async fn delete_lane_orchestrated(
    state: &Arc<AppState>,
    addr: LaneAddress,
    cleanup: bool,
) -> Result<DeletedLaneInfo, DeleteLaneError> {
    // architecture rule: Lead Lane は project lifetime 紐付きのため削除不可
    if matches!(addr.kind, LaneKind::Lead) {
        return Err(DeleteLaneError::LeadCannotBeDeleted);
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

    // Phase 2a: tmux session cleanup (best-effort)。
    // info.tmux は spawn 成功時に populate された session 情報、 各 entry の `session` 名で
    // `tmux kill-session -t <name>` を実行。 既存挙動では欠落していた → keystage 削除時の
    // orphan tmux session bug の根本原因 (= 本 PR で fix)。
    // for ループで全 entry に kill_session 副作用を実行 (= 短絡しない)、 結果を OR 合成。
    // `.any()` だと short-circuit で副作用 path が変わるため避ける。
    let mut tmux_killed = false;
    for t in &info.tmux {
        let killed = crate::tmux::kill_session(&t.session);
        if killed {
            tracing::info!("tmux session killed: {}", t.session);
        } else {
            tracing::warn!("tmux session kill failed (or did not exist): {}", t.session);
        }
        tmux_killed = tmux_killed || killed;
    }

    // Phase 2b: lane workspace dir cleanup (best-effort、 cleanup=true 時のみ)。
    // 既存挙動踏襲、 直 lib call (`crate::lane::commands::remove_wing_in`)。
    // 注意: `spawn_blocking` closure は `repo_name` / `name` のみ move、 `addr` は capture
    // されないので後続 match arm の `tracing` で参照可能 (= compile time 保証)。
    let cleanup_status = if cleanup && let Some(name) = info.address.name.clone() {
        // project-local lane refactor PR 1: remove_wing_in は repo_root: &Path を受け取る。
        // sidebar delete trigger は dual-read で project-local + legacy global 両 path 対応。
        let repo_root = std::path::PathBuf::from(&state.project_dir);
        let result = tokio::task::spawn_blocking(move || {
            crate::lane::commands::remove_wing_in(&repo_root, &name)
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
        tmux_killed,
        cleanup_status,
    })
}

/// `DELETE /api/lanes?address=<addr>&cleanup=true` — Lane destroy (Phase 4-A) + lane workspace cleanup (Phase 4-B)
///
/// VP-124 Phase 1 で `delete_lane_orchestrated` に core logic を抽出済み。 本 handler は
/// HTTP request parse + error → status code mapping の薄い adapter として残る。
///
/// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane lifecycle)
pub async fn delete_handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<DeleteLaneQuery>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let Some(addr) = LanePool::parse_address(&q.address) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid lane address: {}", q.address)})),
        ));
    };

    match delete_lane_orchestrated(&state, addr, q.cleanup).await {
        Ok(info) => Ok((
            StatusCode::OK,
            Json(json!({
                "deleted": info.address,
                "pid": info.pid,
                "tmux_killed": info.tmux_killed,
                "cleanup": info.cleanup_status,
            })),
        )),
        Err(e @ DeleteLaneError::LeadCannotBeDeleted) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": e.to_string()})),
        )),
        Err(e @ DeleteLaneError::LaneNotFound(_)) => {
            Err((StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()}))))
        }
    }
}

/// `POST /api/lanes/restart?address=<addr>` request の query
#[derive(Debug, Deserialize)]
pub struct RestartLaneQuery {
    /// Display 形 ("<project>/lead" / "<project>/wing/<name>")
    pub address: String,
}

/// VP-131: restart の透過 retry 設定。 tmux kill + spawn の race / transient failure を
/// 吸収するため exponential backoff で 3 attempts まで自動 retry。 user click 1 回で
/// 「auto retry」 が走り、 dogfood UX で「Restart したら確実に Echoes 復活する」 を担保。
const RESTART_MAX_ATTEMPTS: u32 = 3;
const RESTART_BACKOFF_MS: [u64; 2] = [200, 500]; // attempt 0→1: 200ms、 attempt 1→2: 500ms

/// `POST /api/lanes/restart?address=<addr>` — Lane の Lead Stand restart
///
/// 動作:
/// 1. address parse (LanePool::parse_address で逆変換)
/// 2. LanePool::restart_lane で 既存 PtySlot kill + tmux kill (VP-131) → 同 stand で respawn
/// 3. spawn 失敗時は exponential backoff で **最大 3 attempts まで透過 retry** (VP-131)
/// 4. vp-app の WS は PR #218 (auto-reconnect) で透過的に新 PtySlot に attach し直す
/// 5. 200 OK with new pid + attempts / 500 on 全 attempts 失敗 (LaneInfo は state=Dead に遷移)
pub async fn restart_handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RestartLaneQuery>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let Some(addr) = LanePool::parse_address(&q.address) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("invalid lane address: {}", q.address)})),
        ));
    };

    // VP-131: 透過 retry with exponential backoff。 各 attempt 間で write lock を release して
    // 他 handler を blocking しない設計、 tokio::time::sleep で async wait。
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..RESTART_MAX_ATTEMPTS {
        let result = {
            let mut pool = state.lane_pool.write().await;
            pool.restart_lane(&addr)
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
                tracing::info!(
                    "Lane restart OK: addr={} new_pid={} attempts={}",
                    addr,
                    pid,
                    attempt + 1
                );
                return Ok((
                    StatusCode::OK,
                    Json(json!({
                        "restarted": addr.to_string(),
                        "pid": pid,
                        "attempts": attempt + 1,
                    })),
                ));
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
    let err_msg = last_err
        .map(|e| e.to_string())
        .unwrap_or_else(|| "unknown restart failure".to_string());
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": err_msg,
            "attempts": RESTART_MAX_ATTEMPTS,
        })),
    ))
}

/// Wing name から default branch を auto-derive する。
///
/// 形式: `<git-user>/<sanitized-name>`。
///
/// - `git-user` は `git config user.name` (repo local > global の標準解決) を lowercase + sanitize したもの。
///   取得失敗・空・sanitize 後 empty なら fallback `wing` prefix を使う。
/// - `sanitized-name` は `sanitize_for_branch` で git ref 制約に合わせる。
///
/// 例: user="Mako", name="sub" → `mako/sub`
fn derive_default_branch(repo_root: &std::path::Path, name: &str) -> String {
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
        .unwrap_or_else(|| "wing".to_string());
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

    // doc 11 §3.7 の `migrate_legacy_stand` test 2 件 (translates_old_names /
    // passes_through_modern_names) は 2026-05-03 削除済 (shim 自体が削除されたため)。
}

#[cfg(test)]
mod route_smoke_tests {
    //! VP-13 sub-scope E: lanes.rs route の Axum oneshot smoke test。
    //!
    //! 既存 `mod tests` (= helpers / sanitize_for_branch test 等) とは別 mod で配線、
    //! Medium 層 route test を Section 区切りで分離。

    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt;

    #[tokio::test]
    async fn list_handler_returns_200_with_lanes_field() {
        let state = crate::process::state::build_test_app_state(None).await;
        let app = Router::new()
            .route("/api/lanes", get(list_handler))
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/lanes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(body.get("lanes").map(|v| v.is_array()).unwrap_or(false));
    }

    /// F.8 B Convergent: disk-scan merge 撤去後の `build_lanes_snapshot` は
    /// LanePool が空 → lanes 空を返す (disk dir が何があっても list に乗らない)。
    /// 旧版は disk-scan で pid: None Inactive Wing を merge していた。
    #[tokio::test]
    async fn build_lanes_snapshot_returns_only_lanepool_entries() {
        let state = crate::process::state::build_test_app_state(None).await;
        // LanePool は build_test_app_state で空 (LanePool::new()) → 0 件
        let lanes = build_lanes_snapshot(&state).await;
        assert!(
            lanes.is_empty(),
            "LanePool が空なら disk に何があっても snapshot は空 (disk-scan merge 撤去済)"
        );
    }

    /// Worker → Wing rename 完結: `kind="worker"` POST は 400 BAD_REQUEST を返す。
    /// 旧版は `req.kind != "wing" && req.kind != "worker"` で "worker" を許容していた。
    #[tokio::test]
    async fn create_handler_rejects_worker_kind() {
        let state = crate::process::state::build_test_app_state(None).await;
        let app = Router::new()
            .route("/api/lanes", post(create_handler))
            .with_state(state);
        let body = serde_json::json!({
            "kind": "worker",
            "name": "test-wing"
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/lanes")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "kind='worker' は 400 を返す (legacy alias 一掃済)"
        );
    }

    /// create_handler: name が空文字列の場合は 400 BAD_REQUEST を返す。
    #[tokio::test]
    async fn create_handler_rejects_empty_name() {
        let state = crate::process::state::build_test_app_state(None).await;
        let app = Router::new()
            .route("/api/lanes", post(create_handler))
            .with_state(state);
        let body = serde_json::json!({
            "kind": "wing",
            "name": "   "
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/lanes")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "name が空白のみの場合は 400"
        );
    }
}
