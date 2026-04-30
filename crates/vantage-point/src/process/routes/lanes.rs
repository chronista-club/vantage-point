//! Lane REST API — Phase A4-2b minimum (read-only)
//!
//! 関連 memory:
//! - `mem_1CaSsN7xj69aVQtLPQFJxQ` (SP-as-Project-Master: 9 component minimum)
//!
//! ## Phase A4-2b 実装
//!
//! - `GET /api/lanes` — `LanePool` の list を JSON 返却
//!
//! ## 後 phase
//!
//! - POST /api/lanes (A4-4: Worker Lane create)
//! - DELETE /api/lanes/{addr} (A4-4: destroy)
//! - GET /api/lanes/{addr} (A4-4: 1 件取得、addr の URL encoding 確定後)
//! - PUT /api/lanes/{addr}/stand (A5: Stand 切替)
//! - WS /ws/terminal の lane param 強化 (A4-2d)

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::lanes_state::{LaneAddress, LaneInfo, LaneKind, LanePool, LaneStand, LaneState};
use super::super::state::AppState;

/// REST response: `GET /api/lanes` の JSON shape
#[derive(Debug, Serialize)]
pub struct LanesResponse {
    pub lanes: Vec<LaneInfo>,
}

/// `GET /api/lanes` — Lane list を返す
///
/// Phase A4-2b: Lead Lane が 1 つ pre-populate されてる状態を返却。
/// Phase 5-D: Worker Lane に対しては `cwd` から git 状態 (`WorkerStatus`) を populate。
/// registry には保存せず、 GET 時に都度 `worker_status()` を呼ぶ (volatile + 5-7 git subprocess)。
///
/// Phase 5-E: in-memory LanePool に居ない ccws Worker dir も disk scan で merge して `pid: None` で
/// emit (Pane 不在を pid で表現、 LaneState には変更を加えない)。 防御パスのため fail-soft。
/// Active/Inactive は Project 集約として client (sidebar) 側で `lanes.every(pid != null)` で判定する設計。
/// CURRENTS Project (= SP 起動中) のみが /api/lanes に応答するので、 disk scan 対象が自動 enforce される。
pub async fn list_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pool = state.lane_pool.read().await;
    let mut lanes = pool.list();
    drop(pool); // git subprocess 中の lock を保たない (worker_status は数 100ms かかる事あり)

    // 既存 Worker の git status を populate
    for lane in lanes.iter_mut() {
        if matches!(lane.kind, crate::process::lanes_state::LaneKind::Worker) {
            let path = std::path::Path::new(&lane.cwd);
            if path.exists() && path.join(".git").exists() {
                lane.worker_status = Some(crate::ccws::commands::worker_status(path));
            }
        }
    }

    // Phase 5-E: ccws workers_dir を disk scan して、 LanePool に居ない Worker を pid: None で merge
    let project_id = std::path::Path::new(&state.project_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if !project_id.is_empty() {
        let existing_names: std::collections::HashSet<String> = lanes
            .iter()
            .filter_map(|l| {
                if matches!(l.kind, crate::process::lanes_state::LaneKind::Worker) {
                    l.name.clone()
                } else {
                    None
                }
            })
            .collect();
        let inactive = crate::ccws::commands::list_workers_for_repo(&project_id);
        for entry in inactive {
            if existing_names.contains(&entry.name) {
                continue; // in-memory 優先、 disk 側 skip
            }
            let addr = LaneAddress::worker(&project_id, &entry.name);
            let mut info = LaneInfo {
                address: addr,
                kind: LaneKind::Worker,
                name: Some(entry.name.clone()),
                state: LaneState::default(), // Pane 不在の表現は pid: None に集約 (state は default Running)
                stand: LaneStand::HeavensDoor, // default、 activate 時に上書き可
                created_at: chrono::Utc::now().to_rfc3339(),
                pid: None, // Pane (HD) 不在 = client 側で Inactive 判定の signal
                cwd: entry.path.clone(),
                worker_status: None,
            };
            // git status を best-effort で populate (branch 表示の連動)
            let path = std::path::Path::new(&entry.path);
            if path.exists() && path.join(".git").exists() {
                info.worker_status = Some(crate::ccws::commands::worker_status(path));
            }
            lanes.push(info);
        }
    }

    Json(LanesResponse { lanes })
}

/// `POST /api/lanes` request body (Phase 3-A: Worker Lane create + ccws clone)
#[derive(Debug, Deserialize)]
pub struct CreateLaneReq {
    /// "worker" のみ受付 (Lead は project ごと固定)
    pub kind: String,
    /// Worker name (人間可読、 LaneAddress.name に入る)
    pub name: String,
    /// LaneStand: "heavens_door" (default) or "the_hand"
    #[serde(default)]
    pub stand: Option<String>,
    /// 既存 worktree path。 Some なら直接 cwd として使う、 None なら branch 指定で ccws clone を実行する。
    #[serde(default)]
    pub cwd: Option<String>,
    /// Phase 3-A: ccws clone する branch 名。 cwd が None で branch が Some の時、
    /// `ccws new <name> <branch>` を SP 内で実行して worker dir を作成、 そこに Lane を spawn する。
    #[serde(default)]
    pub branch: Option<String>,
}

/// `POST /api/lanes` — Worker Lane create (Phase 3-A: ccws clone + PtySlot spawn)
///
/// 流れ:
/// 1. 入力 validation (kind == "worker", name 非空)
/// 2. cwd 決定:
///    - `req.cwd` Some → そのまま使う
///    - `req.branch` Some → `ccws new <name> <branch>` subprocess で worker dir 作成
///    - 両方 None → state.project_dir (= Lead と同じ dir) を share (legacy fallback)
/// 3. PtySlot::spawn で実 PTY 起動 (LaneStand 別 command builder 経由)
/// 4. LanePool に insert (state=Running、 pid 付き)
///
/// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane = Session Process + ccws clone 連動)
pub async fn create_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateLaneReq>,
) -> Result<(StatusCode, Json<LaneInfo>), (StatusCode, Json<serde_json::Value>)> {
    // 入力 validation
    if req.kind != "worker" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "kind must be 'worker' (Lead is fixed per project)"
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
    let addr = LaneAddress::worker(&project_id, &req.name);
    let stand = match req.stand.as_deref() {
        Some("the_hand") | Some("th") => LaneStand::TheHand,
        _ => LaneStand::HeavensDoor, // default HD
    };

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

    // Phase 4-X / R5: cwd 決定 ── 優先順位 explicit cwd > ccws clone (branch 明示 or auto-derive)。
    //
    // 旧 fallback (`else { state.project_dir }` で Lead と同 worktree を share) は撤廃。
    // 理由: UI から name="sub" だけ入力した場合、 silent に Lead と同 dir を共有してしまい、
    // 「Worker = 隔離 worktree」の mental model が崩れていた (race condition の温床)。
    //
    // 新規約: branch が None の時は `git config user.name` から prefix を取り、
    // `<user>/<sanitized-name>` 形式の branch を auto-derive して必ず ccws clone を実行する。
    // explicit に同 dir を share したい場合は API caller が `cwd` を明示的に指定する。
    let cwd = if let Some(c) = req.cwd {
        c
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
            crate::ccws::commands::new_worker_in(
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
                Json(json!({"error": format!("ccws task join: {}", e)})),
            )
        })?;
        let path_buf = result.map_err(|e| {
            // ccws::commands::new_worker_in は worker dir 既存 + force=false の時に
            // 「ワーカー '<name>' は既に存在します」を返す。 UI で input 下に表示するため
            // CONFLICT を返し、 error message をそのまま流す。
            let msg = format!("{}", e);
            let status = if msg.contains("既に存在") || msg.contains("already exists") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(json!({
                    "error": format!("ccws clone failed (branch={}): {}", branch_for_log, msg)
                })),
            )
        })?;
        tracing::info!(
            "Worker Lane ccws clone: name={} branch={} dir={}",
            req.name,
            branch_for_log,
            path_buf.display()
        );
        path_buf.to_string_lossy().into_owned()
    };

    // PtySlot::spawn は openpty + spawn_command の OS syscall でブロッキング。
    // Phase review fix #2: tokio worker thread を占有しないよう spawn_blocking でラップ。
    // Phase 4-X の ccws clone と同じ pattern。
    let cmd = crate::process::stand_spawner::build_stand_command(stand, std::path::Path::new(&cwd));
    let cwd_for_spawn = cwd.clone();
    // Phase 5-D: spawn_with_fallback で `claude --continue` 早期 exit 時に空 args で retry。
    let spawn_result = tokio::task::spawn_blocking(move || {
        crate::process::stand_spawner::spawn_with_fallback(&cwd_for_spawn, &cmd, 80, 24)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("PtySlot spawn task join: {}", e)})),
        )
    })?;
    let (lane_state, pid) = match spawn_result {
        Ok((slot, _rx)) => {
            let pid = slot.pid();
            let mut pool = state.lane_pool.write().await;
            pool.insert_pty_slot(addr.clone(), slot);
            tracing::info!(
                "Worker Lane spawned: addr={} stand={:?} cwd={} pid={}",
                addr,
                stand,
                cwd,
                pid
            );
            (LaneState::Running, Some(pid))
        }
        Err(e) => {
            tracing::warn!(
                "Worker Lane spawn failed (graceful degrade to Dead): addr={} cwd={}: {}",
                addr,
                cwd,
                e
            );
            (LaneState::Dead, None)
        }
    };

    let info = LaneInfo {
        address: addr.clone(),
        kind: LaneKind::Worker,
        name: Some(req.name.clone()),
        state: lane_state,
        stand,
        created_at: chrono::Utc::now().to_rfc3339(),
        pid,
        cwd,
        // create 時点では git 状態は registry に保存しない、 GET 時に都度 worker_status() で取得
        worker_status: None,
    };

    {
        let mut pool = state.lane_pool.write().await;
        pool.insert(info.clone());
    }

    Ok((StatusCode::CREATED, Json(info)))
}

/// `DELETE /api/lanes?address=<addr>` request の query
#[derive(Debug, Deserialize)]
pub struct DeleteLaneQuery {
    /// Display 形 ("<project>/lead" / "<project>/worker/<name>")
    pub address: String,
    /// Phase 4-B: ccws workspace の dir も rm するか (default true)。
    /// false の場合 PtySlot だけ kill して dir 残置 (= debug / forensic 用途)。
    #[serde(default = "default_cleanup")]
    pub cleanup: bool,
}

fn default_cleanup() -> bool {
    true
}

/// `DELETE /api/lanes?address=<addr>&cleanup=true` — Lane destroy (Phase 4-A) + ccws workspace cleanup (Phase 4-B)
///
/// 動作:
/// 1. address parse (LanePool::parse_address で逆変換)
/// 2. Lead は削除拒否 (400) — Project lifetime 紐付き
/// 3. LanePool::remove で LaneInfo + PtySlot を drop (PtySlot::Drop で child kill + wait)
/// 4. cleanup=true (default) なら `ccws rm <name> --force` を subprocess 実行 (PtySlot drop 後 = file handle 解放後)
/// 5. 200 OK with deleted info + cleanup status
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

    // Lead は削除拒否 — Project per Lead 1 つ固定の architecture rule
    if matches!(addr.kind, LaneKind::Lead) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Lead Lane is fixed per project and cannot be deleted (use SP shutdown instead)"
            })),
        ));
    }

    // Worker name (= ccws workspace name) を保持
    let worker_name = addr.name.clone();

    let removed = {
        let mut pool = state.lane_pool.write().await;
        pool.remove(&addr)
    };

    let info = match removed {
        Some(i) => i,
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Lane not found: {}", addr)})),
            ));
        }
    };

    tracing::info!(
        "Worker Lane deleted: addr={} pid={:?} (PtySlot dropped → child killed)",
        addr,
        info.pid
    );

    // Phase 4-X: ccws workspace dir cleanup を直 lib call に。 旧 subprocess (`Command::new("ccws")`) 撤去。
    let cleanup_result = if q.cleanup
        && let Some(name) = worker_name
    {
        let repo_name = std::path::Path::new(&state.project_dir)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let result = tokio::task::spawn_blocking(move || {
            crate::ccws::commands::remove_worker_in(&repo_name, &name)
        })
        .await;
        match result {
            Ok(Ok(())) => {
                tracing::info!("ccws remove 成功: {}", addr);
                Some("cleaned")
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "ccws remove 失敗 (lane は削除済、 dir 残置): {}: {}",
                    addr,
                    e
                );
                Some("dir_retained_remove_failed")
            }
            Err(e) => {
                tracing::warn!("ccws task join: {}", e);
                Some("dir_retained_join_failed")
            }
        }
    } else {
        None
    };

    Ok((
        StatusCode::OK,
        Json(json!({
            "deleted": addr.to_string(),
            "pid": info.pid,
            "cleanup": cleanup_result,
        })),
    ))
}

/// `POST /api/lanes/restart?address=<addr>` request の query
#[derive(Debug, Deserialize)]
pub struct RestartLaneQuery {
    /// Display 形 ("<project>/lead" / "<project>/worker/<name>")
    pub address: String,
}

/// `POST /api/lanes/restart?address=<addr>` — Lane の Lead Stand restart
///
/// 動作:
/// 1. address parse (LanePool::parse_address で逆変換)
/// 2. LanePool::restart_lane で 既存 PtySlot kill (Drop で child wait) → 同 stand で respawn
/// 3. vp-app の WS は PR #218 (auto-reconnect) で透過的に新 PtySlot に attach し直す
/// 4. 200 OK with new pid / 500 on spawn 失敗 (LaneInfo は state=Dead に遷移)
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

    let pid = {
        let mut pool = state.lane_pool.write().await;
        match pool.restart_lane(&addr) {
            Ok(()) => pool.get(&addr).and_then(|i| i.pid).unwrap_or(0),
            Err(e) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                ));
            }
        }
    };

    tracing::info!("Lane restart OK: addr={} new_pid={}", addr, pid);
    Ok((
        StatusCode::OK,
        Json(json!({
            "restarted": addr.to_string(),
            "pid": pid,
        })),
    ))
}

/// Worker name から default branch を auto-derive する。
///
/// 形式: `<git-user>/<sanitized-name>`。
///
/// - `git-user` は `git config user.name` (repo local > global の標準解決) を lowercase + sanitize したもの。
///   取得失敗・空・sanitize 後 empty なら fallback `worker` prefix を使う。
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
        .unwrap_or_else(|| "worker".to_string());
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
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
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
        assert_eq!(sanitize_for_branch("--leading-trailing--"), "leading-trailing");
        assert_eq!(sanitize_for_branch("symbols!@#$%"), "symbols");
        assert_eq!(sanitize_for_branch(""), "");
    }
}
