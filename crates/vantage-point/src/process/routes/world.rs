//! World API ルートハンドラー - PP (Paisley Park) プロセス管理
//!
//! World（TheWorld）から呼び出される REST API。
//! PP の起動・停止・監視を行う。

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

    let world = world.read().await;
    match world.open_pointview(&project_name).await {
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
