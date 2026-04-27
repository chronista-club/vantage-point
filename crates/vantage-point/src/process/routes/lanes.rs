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
pub async fn list_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pool = state.lane_pool.read().await;
    Json(LanesResponse { lanes: pool.list() })
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

    // Phase 3-A: cwd 決定 ── 優先順位 explicit cwd > ccws clone > project_dir share
    let cwd = if let Some(c) = req.cwd {
        c
    } else if let Some(branch) = req.branch.as_deref() {
        // ccws new <name> <branch> を subprocess で実行
        // ccws CLI は vp-cli から install 済 (Phase 2.x-e で statically linked)
        let project_dir = state.project_dir.clone();
        let name = req.name.clone();
        let branch = branch.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let output = std::process::Command::new("ccws")
                .args(["new", &name, &branch])
                .current_dir(&project_dir)
                .output();
            match output {
                Ok(o) if o.status.success() => {
                    // ccws new の stdout は worker dir path (commands.rs の new_worker)
                    let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if path.is_empty() {
                        Err("ccws new output was empty".to_string())
                    } else {
                        Ok(path)
                    }
                }
                Ok(o) => Err(format!(
                    "ccws new exited {}: stderr={}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr)
                )),
                Err(e) => Err(format!("ccws new spawn failed: {}", e)),
            }
        })
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("ccws task join: {}", e)})),
            )
        })?;
        result.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("ccws clone failed: {}", e)})),
            )
        })?
    } else {
        state.project_dir.clone()
    };

    // PtySlot::spawn で実 PTY 起動 (Lead と同じ stand_spawner 経由)
    let cmd =
        crate::process::stand_spawner::build_stand_command(stand, std::path::Path::new(&cwd));
    let (lane_state, pid) = match crate::daemon::pty_slot::PtySlot::spawn(
        &cwd,
        &cmd.program,
        &cmd.args,
        80,
        24,
    ) {
        Ok((slot, _rx)) => {
            let pid = slot.pid();
            // PtySlot を LanePool に insert する必要がある (Lead と同じ pattern)
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

    // Phase 4-B: ccws workspace dir cleanup
    let cleanup_result = if q.cleanup
        && let Some(name) = worker_name
    {
        let project_dir = state.project_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            std::process::Command::new("ccws")
                .args(["rm", &name, "--force"])
                .current_dir(&project_dir)
                .output()
        })
        .await;
        match result {
            Ok(Ok(o)) if o.status.success() => {
                tracing::info!("ccws rm 成功: {}", addr);
                Some("cleaned")
            }
            Ok(Ok(o)) => {
                tracing::warn!(
                    "ccws rm 失敗 (lane は削除済、 dir 残置): {}: {}",
                    addr,
                    String::from_utf8_lossy(&o.stderr)
                );
                Some("dir_retained_ccws_rm_failed")
            }
            Ok(Err(e)) => {
                tracing::warn!("ccws rm spawn 失敗: {}: {}", addr, e);
                Some("dir_retained_spawn_failed")
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
