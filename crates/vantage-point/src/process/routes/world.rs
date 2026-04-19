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

/// GET /api/world/ccwire/sessions - ccwire セッション一覧
pub async fn world_ccwire_sessions() -> impl IntoResponse {
    match crate::ccwire::list_sessions() {
        Ok(sessions) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({ "sessions": sessions })),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
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

// =============================================================================
// Mailbox Registry — Phase 3: cross-Process actor messaging
// =============================================================================

/// Mailbox actor 登録リクエスト
#[derive(serde::Deserialize)]
pub struct MailboxRegisterRequest {
    pub actor: String,
    pub project_name: String,
    pub port: u16,
}

/// POST /api/world/mailbox/register — VP Process が自身の actor を登録
pub async fn world_mailbox_register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MailboxRegisterRequest>,
) -> impl IntoResponse {
    let Some(registry) = &state.mailbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Mailbox registry not available"})),
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

/// Mailbox actor 登録解除リクエスト
#[derive(serde::Deserialize)]
pub struct MailboxUnregisterRequest {
    pub actor: String,
    pub project_name: String,
}

/// POST /api/world/mailbox/unregister — actor 単独 unregister
pub async fn world_mailbox_unregister(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MailboxUnregisterRequest>,
) -> impl IntoResponse {
    let Some(registry) = &state.mailbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Mailbox registry not available"})),
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
pub struct MailboxUnregisterProcessRequest {
    pub port: u16,
}

/// POST /api/world/mailbox/unregister-process — port 配下の全 actor を一括解除
pub async fn world_mailbox_unregister_process(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MailboxUnregisterProcessRequest>,
) -> impl IntoResponse {
    let Some(registry) = &state.mailbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Mailbox registry not available"})),
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

/// Mailbox actor lookup query
#[derive(serde::Deserialize)]
pub struct MailboxLookupQuery {
    /// Actor 名（必須）
    pub actor: String,
    /// project_name または port（どちらか必須）
    pub project_name: Option<String>,
    pub port: Option<u16>,
}

/// GET /api/world/mailbox/lookup?actor=...&project_name=...
/// or  /api/world/mailbox/lookup?actor=...&port=...
pub async fn world_mailbox_lookup(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<MailboxLookupQuery>,
) -> impl IntoResponse {
    let Some(registry) = &state.mailbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Mailbox registry not available"})),
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

/// Mailbox registry list query
#[derive(serde::Deserialize)]
pub struct MailboxListQuery {
    /// project_name でフィルタ（省略時は全件）
    pub project_name: Option<String>,
}

/// GET /api/world/mailbox/list?project_name=... — debug / 確認用
pub async fn world_mailbox_list(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<MailboxListQuery>,
) -> impl IntoResponse {
    let Some(registry) = &state.mailbox_registry else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Mailbox registry not available"})),
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
