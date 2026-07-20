//! World API ルートハンドラー — TheWorld (Process Manager) REST API
//!
//! プロジェクト CRUD・Process 起動・停止・監視を担当する。

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use super::super::state::AppState;
use crate::capability::{ProjectInfo, RunningProcess};

/// World projects response
#[derive(serde::Serialize)]
struct WorldProjectsResponse {
    projects: Vec<ProjectInfo>,
}

/// World processes response
#[derive(serde::Serialize)]
struct WorldProcessesResponse {
    processes: Vec<RunningProcess>,
}

/// GET /api/world/projects - List all registered projects
pub async fn world_list_projects(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    let projects = world.list_projects().await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!(WorldProjectsResponse { projects })),
    )
}

/// GET /api/world/processes - List all running processes
pub async fn world_list_processes(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    let processes = world.list_running_processes().await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!(WorldProcessesResponse { processes })),
    )
}

/// POST /api/world/processes/{project_name}/start - Start a process for project
pub async fn world_start_process(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    // start_process は内部でスリープ + ポートスキャンがあるため、
    // read ガードを長時間保持しないよう clone して解放する
    let world_cap = {
        let w = world.read().await;
        w.clone()
    };
    match world_cap.start_process(&project_name).await {
        Ok(process) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(&process).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/world/processes/{project_name}/stop - Stop a process for project
pub async fn world_stop_process(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    match world.stop_process(&project_name).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "stopped", "project": project_name})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// Phase 5-C: POST /api/world/processes/{project_name}/restart — SP restart (stop + start chain)
pub async fn world_restart_process(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };
    let world_cap = {
        let w = world.read().await;
        w.clone()
    };
    match world_cap.restart_process(&project_name).await {
        Ok(process) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(&process).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/world/processes/{project_name}/pointview - Open PointView for project
pub async fn world_open_pointview(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    // open_pointview は内部で start_process を呼ぶ可能性があり、
    // スリープ + ポートスキャンを含むため read ガードを即座に解放する
    let world_cap = {
        let w = world.read().await;
        w.clone()
    };
    match world_cap.open_pointview(&project_name).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "opened", "project": project_name})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// プロジェクト追加リクエスト
#[derive(serde::Deserialize)]
pub struct AddProjectRequest {
    pub name: String,
    pub path: String,
}

/// POST /api/world/projects - プロジェクトを追加
pub async fn world_add_project(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddProjectRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    match world.add_project(&req.name, &req.path).await {
        Ok(info) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(&info).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// プロジェクト更新リクエスト
#[derive(serde::Deserialize)]
pub struct UpdateProjectRequest {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// POST /api/world/projects/update - プロジェクト名を変更
pub async fn world_update_project(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateProjectRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    let mut updated = false;

    if let Some(new_name) = &req.name {
        match world.rename_project(&req.path, new_name).await {
            Ok(()) => updated = true,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": e.to_string()})),
                );
            }
        }
    }

    if let Some(enabled) = req.enabled {
        match world.set_project_enabled(&req.path, enabled).await {
            Ok(()) => updated = true,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": e.to_string()})),
                );
            }
        }
    }

    if updated {
        (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "updated", "path": req.path})),
        )
    } else {
        (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "No fields to update"})),
        )
    }
}

/// プロジェクト削除リクエスト
#[derive(serde::Deserialize)]
pub struct RemoveProjectRequest {
    pub path: String,
}

/// POST /api/world/projects/remove - プロジェクトを削除
pub async fn world_remove_project(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RemoveProjectRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    match world.remove_project(&req.path).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "removed", "path": req.path})),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// プロジェクト並び替えリクエスト
#[derive(serde::Deserialize)]
pub struct ReorderProjectsRequest {
    pub paths: Vec<String>,
}

/// POST /api/world/projects/reorder - プロジェクトの並び順を変更
pub async fn world_reorder_projects(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReorderProjectsRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    match world.reorder_projects(&req.paths).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "reordered"})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// active lane (presence、 Model Q) 設定リクエスト
#[derive(serde::Deserialize)]
pub struct SetActiveLaneRequest {
    pub path: String,
    pub address: String,
}

/// POST /api/world/lanes/active - project の active lane を設定 (daemon-canonical、 db/world に永続)
pub async fn world_set_active_lane(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetActiveLaneRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    match world.set_active_lane(&req.path, &req.address).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "active_lane set"})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// performer lane 作成リクエスト (doc 24 §10 Phase 2 B-create)
#[derive(serde::Deserialize)]
pub struct CreateLaneRequest {
    /// project の path (= normalize_path_key の起点、 repo_root)。
    pub path: String,
    /// performer 名。
    pub name: String,
    /// branch (省略時は `<user>/<name>` を derive)。
    #[serde(default)]
    pub branch: Option<String>,
    /// stand (省略時は config の default_stand → echoes)。
    #[serde(default)]
    pub stand: Option<String>,
}

/// POST /api/world/lanes - daemon が performer lane を create する (§5.3 ground provision +
/// descriptor を daemon-canonical truth として所有)。 PtySlot spawn は lane_watcher 経由で SP が行う。
pub async fn world_create_lane(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateLaneRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    // branch / stand の default 導出 (= calc) は route の責務。 SP create_handler と parity:
    // branch 未指定 → `<user>/<name>` derive、 stand 未指定 → config の default_stand → echoes。
    let repo_root = std::path::PathBuf::from(&req.path);
    let branch = req
        .branch
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| super::lanes::derive_default_branch(&repo_root, &req.name));
    let stand = req.stand.clone().unwrap_or_else(|| {
        crate::config::Config::load()
            .map(|c| c.default_stand_or_echoes().to_string())
            .unwrap_or_else(|_| "echoes".to_string())
    });

    let world = world.read().await;
    match world
        .create_lane(&req.path, &req.name, &branch, &stand)
        .await
    {
        Ok(info) => (
            axum::http::StatusCode::CREATED,
            Json(
                serde_json::to_value(&info)
                    .unwrap_or_else(|_| serde_json::json!({"status": "created"})),
            ),
        ),
        Err(e) => {
            // create_handler と parity: 重複は CONFLICT (vp-app が form 下に inline 表示)。
            let msg = e.to_string();
            let status = if msg.contains("already exists") || msg.contains("既に存在") {
                axum::http::StatusCode::CONFLICT
            } else {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({"error": msg})))
        }
    }
}

/// slot 設定リクエスト (PR-D: CLI の slot 永続化を daemon 経由に)
#[derive(serde::Deserialize)]
pub struct SetSlotRequest {
    pub path: String,
    pub slot: u16,
}

/// POST /api/world/projects/set_slot - project の slot を設定 (db/world に永続化)
pub async fn world_set_slot(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetSlotRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };
    let world = world.read().await;
    match world.set_project_slot(&req.path, req.slot).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "ok", "path": req.path, "slot": req.slot})),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/world/projects/unassign_slot - project の slot を解除
pub async fn world_unassign_slot(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RemoveProjectRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };
    let world = world.read().await;
    match world.unset_project_slot(&req.path).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "unassigned", "path": req.path})),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/world/projects/sync - ghost project 除去 (db/world に永続化)。
///
/// かつては body の `start_dir` で起点 dir を自動登録もしていたが、 削除済 project を
/// 復活させる resurrection バグの温床だったため撤去した (登録は add_project 経由のみ)。
/// 旧 client が body を送っても無視される (後方互換)。
pub async fn world_sync_projects(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };
    let world = world.read().await;
    match world.sync_projects().await {
        Ok(outcome) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"removed": outcome.removed})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// doc 44 P1 (fold-in): world_register_process / world_unregister_process は撤去。
// project は World 自身が起こすため「外から自己登録される」経路が存在しない。

/// POST /api/world/refresh - Refresh process status
pub async fn world_refresh(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    match world.refresh_process_status().await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "refreshed"})),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/world/projects/reload — projects.kdl を再読み込みして in-memory に反映
///
/// VP-189: `vp sync` / 起動時 sync (`vp app start` / `vp sp start`) が projects.kdl を
/// 書き換えた後、 稼働中 daemon の in-memory projects を projects.kdl と同期させる
/// ための通知エンドポイント。 `reload_config()` が add (起点 dir 登録) と remove
/// (ghost project 除去) を双方向に反映する。
pub async fn world_reload_projects(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };
    world.read().await.reload_config().await;
    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"status": "reloaded"})),
    )
}

// =============================================================================
// Lane Registry — Phase 1c: agent (Echoes on Claude CLI) が tmux session 名を引く
// =============================================================================

/// Phase 1c: Lane filter query
#[derive(serde::Deserialize)]
pub struct LanesQuery {
    /// Project name filter (LaneAddress.project)
    pub project: Option<String>,
    /// Lane name filter — Conductor は "conductor"、 Performer は name (例: "sub")
    pub lane: Option<String>,
    /// Stand kind filter — "echoes" or "shell"
    pub stand: Option<String>,
}

/// GET /api/world/lanes — Phase 1c: Currents の Lane → tmux session resolver
///
/// SP が QUIC registry channel で push した lanes (`LaneInfo` の Vec) を全 project に
/// 渡って flatten + filter で返す。 agent (Echoes on Claude CLI) はこの response を見て
/// `vp tmux send-keys -t <session>` の宛先を決める。
///
/// query parameter:
/// - `project=<name>`: 特定 project のみ
/// - `lane=<name>`: 特定 Lane のみ ("conductor" or performer name)
/// - `stand=<echoes|shell>`: 特定 Stand のみ (LaneInfo.stand に match)
///
/// disconnect された SP の Lane は registry から消えるので、 response = Currents 限定。
pub async fn world_list_lanes(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<LanesQuery>,
) -> impl IntoResponse {
    use crate::process::lanes_state::LaneKind;

    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world_cap = world.read().await;
    let lane_registry = world_cap.lane_registry_ref();
    let registry = lane_registry.read().await;

    // 全 project の Lane を flatten + filter (project / lane / stand)
    let mut lanes: Vec<crate::process::lanes_state::LaneInfo> = registry
        .values()
        .flatten()
        .filter(|l| {
            query
                .project
                .as_deref()
                .is_none_or(|p| l.address.project == p)
        })
        .filter(|l| {
            query.lane.as_deref().is_none_or(|n| {
                match (&l.address.kind, l.address.name.as_deref()) {
                    (LaneKind::Conductor, _) => n == "conductor",
                    (LaneKind::Performer, Some(name)) => name == n,
                    (LaneKind::Performer, None) => false,
                }
            })
        })
        .filter(|l| {
            // doc 11 PR-B: l.stand は String 化、 query.stand と直接比較 (wire 上は新 stand 名のみ accept)。
            query.stand.as_deref().is_none_or(|s| l.stand == s)
        })
        .cloned()
        .collect();

    // 順序: project 名昇順 → 同 project 内は Conductor 先 → 続いて Performer (created_at 昇順)
    lanes.sort_by(|a, b| {
        use std::cmp::Ordering;
        a.address.project.cmp(&b.address.project).then_with(|| {
            match (a.address.kind, b.address.kind) {
                (LaneKind::Conductor, LaneKind::Performer) => Ordering::Less,
                (LaneKind::Performer, LaneKind::Conductor) => Ordering::Greater,
                _ => a.created_at.cmp(&b.created_at),
            }
        })
    });

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({
            "count": lanes.len(),
            "lanes": lanes,
        })),
    )
}

// =============================================================================
// VP-165 PR-6: /api/world/port_for — slot ベース SP port resolver (decision C 完成)
// =============================================================================

/// VP-165 PR-6: query param for `/api/world/port_for`
#[derive(serde::Deserialize)]
pub struct PortForQuery {
    /// Project name (config の `projects[].name` に一致するもの)
    pub project: String,
}

/// GET /api/world/port_for?project=<name> — project 名から SP port を解決
///
/// `Config::resolve_sp_port` 経由で `port` 明示 override → `ensure_slot` (未割当なら
/// 次の空き slot を割当 + config 永続) → `PORT_RANGE_START + slot` を返す。slot は config
/// 永続なので、project リスト変更でも既存 project の port は不変。
///
/// 用途:
/// - `vp sp start` を `-p` 無しで叩いた時に TheWorld に聞く（cross-process port authority）
/// - UI / 外部 script が「project X の SP port は？」を local config を読まずに問い合わせる
/// - `start_process` (in-process) は `crate::resolve::sp_port_for_project` を直接呼ぶ
///   （HTTP roundtrip 不要）
///
/// project が config に未登録なら 404。
pub async fn world_port_for(
    axum::extract::Query(query): axum::extract::Query<PortForQuery>,
) -> impl IntoResponse {
    match crate::resolve::sp_port_for_project(&query.project) {
        Ok(port) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "project": query.project,
                "port": port,
            })),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("port_for failed: {}", e),
                "project": query.project,
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    //! VP-13 sub-scope E: world.rs route の Axum oneshot smoke test。
    //!
    //! `crate::process::state::build_test_app_state` で minimal AppState を構築し、
    //! `world` field が None / Some の 503 / 200 path をそれぞれ verify する。
    //!
    //! 注意: AppState は `pub(crate)`、 fixture も `pub(crate)` なので integration test
    //! (`crates/vantage-point/tests/`) からは触れず、 本 inline tests でのみ run。

    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt; // oneshot

    fn router_for_list_projects(state: std::sync::Arc<AppState>) -> Router {
        Router::new()
            .route("/api/world/projects", get(world_list_projects))
            .with_state(state)
    }

    fn router_for_list_processes(state: std::sync::Arc<AppState>) -> Router {
        Router::new()
            .route("/api/world/processes", get(world_list_processes))
            .with_state(state)
    }

    #[tokio::test]
    async fn world_list_projects_returns_503_when_world_none() {
        let state = crate::process::state::build_test_app_state(None).await;
        let app = router_for_list_projects(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/world/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn world_list_projects_returns_200_with_empty_list_when_world_some() {
        // 空の ProcessManagerCapability を build (= projects 0 件)
        let world = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::capability::ProcessManagerCapability::new(),
        ));
        let state = crate::process::state::build_test_app_state(Some(world)).await;
        let app = router_for_list_projects(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/world/projects")
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
        // 0 件でも `projects` field は array で返る
        assert!(body.get("projects").map(|v| v.is_array()).unwrap_or(false));
    }

    #[tokio::test]
    async fn world_list_processes_returns_503_when_world_none() {
        let state = crate::process::state::build_test_app_state(None).await;
        let app = router_for_list_processes(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/world/processes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn world_list_processes_returns_200_with_empty_list_when_world_some() {
        let world = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::capability::ProcessManagerCapability::new(),
        ));
        let state = crate::process::state::build_test_app_state(Some(world)).await;
        let app = router_for_list_processes(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/world/processes")
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
        assert!(body.get("processes").map(|v| v.is_array()).unwrap_or(false));
    }
}
