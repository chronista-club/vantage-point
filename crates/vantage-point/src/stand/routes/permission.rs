//! パーミッション系ルートハンドラー

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use super::super::state::AppState;
use crate::mcp::PermissionResponse;
use crate::protocol::{ChatComponent, StandMessage};

/// POST /api/permission - Receive permission request from MCP tool
pub async fn permission_request_handler(
    State(state): State<Arc<AppState>>,
    Json(msg): Json<StandMessage>,
) -> impl IntoResponse {
    // Extract request_id and input from the ChatComponent
    let (request_id, original_input) = match &msg {
        StandMessage::ChatComponent {
            component:
                ChatComponent::PermissionRequest {
                    request_id, input, ..
                },
            ..
        } => (request_id.clone(), input.clone()),
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Expected ChatComponent with PermissionRequest"})),
            );
        }
    };

    tracing::info!("Permission request received: {}", request_id);

    // Store the pending request with original input (needed for "allow" response)
    state.pending_permissions.write().await.insert(
        request_id.clone(),
        super::super::state::PendingPermission {
            original_input,
            response: None,
        },
    );

    // Broadcast to WebSocket clients
    state.hub.broadcast(msg);

    state.send_debug(
        "permission",
        &format!("Permission request: {}", request_id),
        None,
    );

    // Return accepted and let MCP poll for response
    (
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({"status": "pending", "request_id": request_id})),
    )
}

/// GET /api/permission/{request_id} - Poll for permission response
pub async fn permission_poll_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let mut pending = state.pending_permissions.write().await;

    if let Some(entry) = pending.get(&request_id) {
        if let Some(ref response) = entry.response {
            // Response is ready - return it and remove from pending
            let response_clone = response.clone();
            pending.remove(&request_id);
            return (
                axum::http::StatusCode::OK,
                Json(serde_json::to_value(&response_clone).unwrap_or_default()),
            );
        } else {
            // Still waiting for user response
            return (
                axum::http::StatusCode::ACCEPTED,
                Json(serde_json::json!({"status": "pending"})),
            );
        }
    }

    // Request not found
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "Request not found"})),
    )
}

/// Handle a permission response from WebSocket (called from WebSocket handler)
pub async fn handle_permission_response(
    state: &Arc<AppState>,
    request_id: String,
    approved: bool,
    updated_input: Option<serde_json::Value>,
    message: Option<String>,
) {
    tracing::info!(
        ">>> handle_permission_response called: request_id={}, approved={}",
        request_id,
        approved
    );

    let mut pending = state.pending_permissions.write().await;
    tracing::debug!(
        "Pending permissions count: {}, keys: {:?}",
        pending.len(),
        pending.keys().collect::<Vec<_>>()
    );

    if let Some(entry) = pending.get_mut(&request_id) {
        let response = if approved {
            // For "allow", use updated_input if provided, otherwise use the original input
            // Claude Code expects updatedInput to be present for "allow" responses
            let final_input = updated_input.or_else(|| Some(entry.original_input.clone()));
            tracing::debug!(
                "Creating allow response with updatedInput: {:?}",
                final_input
            );
            PermissionResponse {
                behavior: "allow".to_string(),
                updated_input: final_input,
                message: None,
            }
        } else {
            PermissionResponse {
                behavior: "deny".to_string(),
                updated_input: None,
                message,
            }
        };

        // Store the response (will be retrieved by next poll)
        entry.response = Some(response);

        tracing::info!(
            "Permission {} -> {}",
            request_id,
            if approved { "allow" } else { "deny" }
        );

        // Broadcast component dismissed
        state.hub.broadcast(StandMessage::ComponentDismissed {
            request_id: request_id.clone(),
        });
    } else {
        tracing::warn!("Permission response for unknown request: {}", request_id);
    }
}
