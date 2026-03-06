//! Conductor APIルートハンドラー - Process プロセス管理
//!
//! Conductor Process（メニューバーアプリ連携）から呼び出される REST API。
//! Project Process の起動・停止・監視を行う。
//!
//! **注意**: `world::conductor` は Paisley Park（プロジェクトAgent）の管理を担当。
//! こちらは Process プロセス自体のライフサイクル管理を担当する。

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use super::super::state::AppState;
use crate::capability::{ProjectInfo, RunningProcess};

/// Conductor projects response
#[derive(serde::Serialize)]
struct ConductorProjectsResponse {
    projects: Vec<ProjectInfo>,
}

/// Conductor processes response
#[derive(serde::Serialize)]
struct ConductorProcessesResponse {
    processes: Vec<RunningProcess>,
}

/// GET /api/conductor/projects - List all registered projects
pub async fn conductor_list_projects(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    let projects = conductor.list_projects().await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!(ConductorProjectsResponse { projects })),
    )
}

/// GET /api/conductor/processes - List all running processes
pub async fn conductor_list_processes(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    let processes = conductor.list_running_processes().await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!(ConductorProcessesResponse { processes })),
    )
}

/// POST /api/conductor/processes/{project_name}/start - Start a process for project
pub async fn conductor_start_process(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.start_stand(&project_name).await {
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

/// POST /api/conductor/processes/{project_name}/stop - Stop a process for project
pub async fn conductor_stop_process(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.stop_process(&project_name).await {
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

/// POST /api/conductor/processes/{project_name}/pointview - Open PointView for project
pub async fn conductor_open_pointview(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(project_name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.open_pointview(&project_name).await {
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

/// POST /api/conductor/refresh - Refresh process status
pub async fn conductor_refresh(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(conductor) = &state.conductor else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Conductor not available"})),
        );
    };

    let conductor = conductor.read().await;
    match conductor.refresh_process_status().await {
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
