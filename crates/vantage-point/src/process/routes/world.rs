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

/// Process 自己登録リクエスト
#[derive(serde::Deserialize)]
pub struct RegisterRequest {
    pub port: u16,
    pub project_dir: String,
    pub pid: u32,
    #[serde(default)]
    pub terminal_token: Option<String>,
}

/// POST /api/world/processes/register - Process が自己登録
pub async fn world_register_process(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    world
        .register_external_process(req.port, &req.project_dir, req.pid)
        .await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"status": "registered", "port": req.port})),
    )
}

/// Process 登録解除リクエスト
#[derive(serde::Deserialize)]
pub struct UnregisterRequest {
    pub port: u16,
}

/// POST /api/world/processes/unregister - Process が自己登録解除
pub async fn world_unregister_process(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UnregisterRequest>,
) -> impl IntoResponse {
    let Some(world) = &state.world else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "World not available"})),
        );
    };

    let world = world.read().await;
    world.unregister_external_process(req.port).await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"status": "unregistered", "port": req.port})),
    )
}

/// GET /api/world/ccwire/sessions - msgbox セッション一覧
///
/// Phase L7d: ccwire registry 廃止、Mailbox Router 経由に切替るまでは
/// 空 list を返す stub。endpoint path は互換のため維持 (Mac app が叩く)。
/// 将来: daemon の Mailbox Router.boxes + msgbox table を aggregate。
pub async fn world_ccwire_sessions() -> impl IntoResponse {
    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({ "sessions": Vec::<()>::new() })),
    )
}

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
// Msgbox Registry — Phase 3: cross-Process actor messaging
// =============================================================================

/// Msgbox actor 登録リクエスト
#[derive(serde::Deserialize)]
pub struct MsgboxRegisterRequest {
    pub actor: String,
    pub project_name: String,
    pub port: u16,
}

/// POST /api/world/msgbox/register — VP Process が自身の actor を登録
pub async fn world_msgbox_register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MsgboxRegisterRequest>,
) -> impl IntoResponse {
    let Some(registry) = &state.msgbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Msgbox registry not available"})),
        );
    };

    if let Err(e) = registry
        .register(&req.actor, &req.project_name, req.port)
        .await
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({
            "status": "registered",
            "actor": req.actor,
            "project_name": req.project_name,
            "port": req.port,
        })),
    )
}

/// Msgbox actor 登録解除リクエスト
#[derive(serde::Deserialize)]
pub struct MsgboxUnregisterRequest {
    pub actor: String,
    pub project_name: String,
}

/// POST /api/world/msgbox/unregister — actor 単独 unregister
pub async fn world_msgbox_unregister(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MsgboxUnregisterRequest>,
) -> impl IntoResponse {
    let Some(registry) = &state.msgbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Msgbox registry not available"})),
        );
    };

    let removed = registry.unregister(&req.project_name, &req.actor).await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({
            "status": "unregistered",
            "actor": req.actor,
            "project_name": req.project_name,
            "removed": removed,
        })),
    )
}

/// Process 単位の一括 unregister リクエスト（Process 停止時）
#[derive(serde::Deserialize)]
pub struct MsgboxUnregisterProcessRequest {
    pub port: u16,
}

/// POST /api/world/msgbox/unregister-process — port 配下の全 actor を一括解除
pub async fn world_msgbox_unregister_process(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MsgboxUnregisterProcessRequest>,
) -> impl IntoResponse {
    let Some(registry) = &state.msgbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Msgbox registry not available"})),
        );
    };

    let removed = registry.unregister_process(req.port).await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({
            "status": "unregistered",
            "port": req.port,
            "removed": removed,
        })),
    )
}

/// Msgbox actor lookup query
#[derive(serde::Deserialize)]
pub struct MsgboxLookupQuery {
    /// Actor 名（必須）
    pub actor: String,
    /// project_name または port（どちらか必須）
    pub project_name: Option<String>,
    pub port: Option<u16>,
}

/// GET /api/world/msgbox/lookup?actor=...&project_name=...
/// or  /api/world/msgbox/lookup?actor=...&port=...
pub async fn world_msgbox_lookup(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<MsgboxLookupQuery>,
) -> impl IntoResponse {
    let Some(registry) = &state.msgbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Msgbox registry not available"})),
        );
    };

    let entry = match (query.project_name.as_deref(), query.port) {
        (Some(project), _) => registry.lookup_by_project(&query.actor, project).await,
        (None, Some(port)) => registry.lookup_by_port(&query.actor, port).await,
        (None, None) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "either project_name or port is required"})),
            );
        }
    };

    match entry {
        Some(e) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({"entry": e})),
        ),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "actor not found"})),
        ),
    }
}

/// Msgbox registry list query
#[derive(serde::Deserialize)]
pub struct MsgboxListQuery {
    /// project_name でフィルタ（省略時は全件）
    pub project_name: Option<String>,
}

/// GET /api/world/msgbox/list?project_name=... — debug / 確認用
pub async fn world_msgbox_list(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<MsgboxListQuery>,
) -> impl IntoResponse {
    let Some(registry) = &state.msgbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Msgbox registry not available"})),
        );
    };

    let entries = match query.project_name.as_deref() {
        Some(project) => registry.list_by_project(project).await,
        None => registry.list().await,
    };

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({
            "count": entries.len(),
            "entries": entries,
        })),
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
    /// Lane name filter — Lead は "lead"、 Worker は name (例: "sub")
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
/// - `lane=<name>`: 特定 Lane のみ ("lead" or worker name)
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
                    (LaneKind::Lead, _) => n == "lead",
                    (LaneKind::Worker, Some(name)) => name == n,
                    (LaneKind::Worker, None) => false,
                }
            })
        })
        .filter(|l| {
            // doc 11 PR-B: l.stand は String 化、 query.stand と直接比較。
            // legacy migration shim (heavens_door / the_hand) は 2026-05-03 削除済 (PR #257
            // → 即削除)、 wire 上は新 stand 名のみ accept。
            query.stand.as_deref().is_none_or(|s| l.stand == s)
        })
        .cloned()
        .collect();

    // 順序: project 名昇順 → 同 project 内は Lead 先 → 続いて Worker (created_at 昇順)
    lanes.sort_by(|a, b| {
        use std::cmp::Ordering;
        a.address.project.cmp(&b.address.project).then_with(|| {
            match (a.address.kind, b.address.kind) {
                (LaneKind::Lead, LaneKind::Worker) => Ordering::Less,
                (LaneKind::Worker, LaneKind::Lead) => Ordering::Greater,
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
