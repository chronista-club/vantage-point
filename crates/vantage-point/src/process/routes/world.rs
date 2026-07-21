//! World API ルートハンドラー — TheWorld (Process Manager) REST API
//!
//! プロジェクト CRUD・Process 起動・停止・監視を担当する。
//!
//! ## doc 45（transport 統一）— 入口は 2 つ、実装は 1 つ
//!
//! control plane は Unison (`world-control` channel) に寄せる途中で、同じ操作に HTTP route と
//! Unison RPC の 2 入口が一時的に並ぶ（doc 45 §5 の中間状態）。route 層にしか無かった
//! orchestration（update の rename+enabled 合成 / lane の filter+sort / create の default 導出）は
//! [`apply_project_update`] / [`collect_lanes`] / [`resolve_create_lane_args`] に括り出してあり、
//! **両入口が同じ関数を呼ぶ**。入口が増えても答えが分岐しないのが要点で、これが
//! 「新面が旧面と同じ答えを出す」ことの構造的な担保になる（テストは daemon/server.rs の
//! `world_control_http_parity` 群）。

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use super::super::state::AppState;
use crate::capability::{ProcessManagerCapability, ProjectInfo, RunningProcess};

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

/// project の部分更新（rename / enabled）を適用する — HTTP と Unison `projects/update` の共有実体。
///
/// `name` / `enabled` はどちらも任意で、指定されたものだけを順に適用する。
/// **どちらも未指定なら `Err("No fields to update")`** — 「何も指定しない update」は
/// 呼び出し側のバグなので黙って成功にしない（HTTP の 400 と同じ意味論）。
///
/// doc 45: この合成ロジックは元々 route handler の中にしか無く、Unison に同じ面を出すと
/// 二重実装になるところだった（doc 45 §1「同じ情報に 3 実装」と同型の罠）。
pub(crate) async fn apply_project_update(
    world: &ProcessManagerCapability,
    path: &str,
    name: Option<&str>,
    enabled: Option<bool>,
) -> Result<(), String> {
    let mut updated = false;

    if let Some(new_name) = name {
        world
            .rename_project(path, new_name)
            .await
            .map_err(|e| e.to_string())?;
        updated = true;
    }

    if let Some(enabled) = enabled {
        world
            .set_project_enabled(path, enabled)
            .await
            .map_err(|e| e.to_string())?;
        updated = true;
    }

    if updated {
        Ok(())
    } else {
        Err("No fields to update".to_string())
    }
}

/// POST /api/world/projects/update - プロジェクト名 / enabled を変更
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
    match apply_project_update(&world, &req.path, req.name.as_deref(), req.enabled).await {
        Ok(()) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"status": "updated", "path": req.path})),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        ),
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

/// lane 作成の省略時 default を導出する (= calc) — HTTP と Unison `lanes/create` の共有実体。
///
/// SP create_handler と parity: branch 未指定 → `<user>/<name>` derive、
/// stand 未指定 → config の `default_stand` → `echoes`。返り値は `(branch, stand)`。
///
/// doc 45: default 導出が入口ごとに分かれていると「HTTP で作った lane と Unison で作った lane で
/// branch 名が違う」が起こりうる。入口が増える前に 1 関数に畳んでおく。
pub(crate) fn resolve_create_lane_args(
    path: &str,
    name: &str,
    branch: Option<&str>,
    stand: Option<&str>,
) -> (String, String) {
    let repo_root = std::path::PathBuf::from(path);
    let branch = branch
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| super::lanes::derive_default_branch(&repo_root, name));
    let stand = stand.map(|s| s.to_string()).unwrap_or_else(|| {
        crate::config::Config::load()
            .map(|c| c.default_stand_or_echoes().to_string())
            .unwrap_or_else(|_| "echoes".to_string())
    });
    (branch, stand)
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

    let (branch, stand) = resolve_create_lane_args(
        &req.path,
        &req.name,
        req.branch.as_deref(),
        req.stand.as_deref(),
    );

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

// doc 44 P1 (fold-in): POST /api/world/refresh (world_refresh) は撤去。
// 呼び出し元はゼロで、中身の `refresh_process_status`（PID liveness）が
// fold-in で無意味化したため（pid が全 project 共通の World 自身）。

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
///
/// HTTP は query string (`?project=&lane=&stand=`)、Unison `lanes/list` は同名の payload field
/// から作る。どちらも全 field 省略可 = 無フィルタ。
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct LanesQuery {
    /// Project name filter (LaneAddress.project)
    pub project: Option<String>,
    /// Lane name filter — Conductor は "root"、 Performer は name (例: "sub")
    pub lane: Option<String>,
    /// Stand kind filter — "echoes" or "shell"
    pub stand: Option<String>,
}

/// lane registry を filter + sort して返す — HTTP と Unison `lanes/list` の共有実体。
///
/// 順序: project 名昇順 → 同 project 内は開発起点 (root) 先 → 続いて Performer (created_at 昇順)。
///
/// doc 45: 従来 filter/sort は HTTP handler の中だけにあり、Unison 側 (`lanes/list`) は
/// 素の flatten を返していた。**同じ名前の面が違う答えを返す**状態で、CLI を Unison に
/// 移すと `vp ps` の並びが静かに変わる類の罠。移設の前に 1 実装へ畳む。
pub(crate) async fn collect_lanes(
    world: &ProcessManagerCapability,
    query: &LanesQuery,
) -> Vec<crate::process::lanes_state::LaneInfo> {
    let lane_registry = world.lane_registry_ref();
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
            // doc 44 P2: 旧 kind 分岐（conductor は "root"、performer は name と照合）は
            // フラット化で name 一本の比較に畳まれた（開発起点の name が予約名 "root"）。
            query.lane.as_deref().is_none_or(|n| l.address.name == n)
        })
        .filter(|l| {
            // doc 11 PR-B: l.stand は String 化、 query.stand と直接比較 (wire 上は新 stand 名のみ accept)。
            query.stand.as_deref().is_none_or(|s| l.stand == s)
        })
        .cloned()
        .collect();

    lanes.sort_by(|a, b| {
        use std::cmp::Ordering;
        a.address.project.cmp(&b.address.project).then_with(|| {
            // doc 44 P2: 開発起点を先頭に置く表示順（旧 kind 比較の後継、`LanePool::list` と同型）
            match (a.address.is_root(), b.address.is_root()) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => a.created_at.cmp(&b.created_at),
            }
        })
    });

    lanes
}

/// GET /api/world/lanes — Phase 1c: Currents の Lane → tmux session resolver
///
/// SP が QUIC registry channel で push した lanes (`LaneInfo` の Vec) を全 project に
/// 渡って flatten + filter で返す。 agent (Echoes on Claude CLI) はこの response を見て
/// `vp tmux send-keys -t <session>` の宛先を決める。
///
/// query parameter:
/// - `project=<name>`: 特定 project のみ
/// - `lane=<name>`: 特定 Lane のみ ("root" or performer name)
/// - `stand=<echoes|shell>`: 特定 Stand のみ (LaneInfo.stand に match)
///
/// disconnect された SP の Lane は registry から消えるので、 response = Currents 限定。
pub async fn world_list_lanes(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<LanesQuery>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world_cap = world.read().await;
    let lanes = collect_lanes(&world_cap, &query).await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({
            "count": lanes.len(),
            "lanes": lanes,
        })),
    )
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
