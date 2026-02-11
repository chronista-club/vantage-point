//! プロンプト系ルートハンドラー (REQ-PROMPT-001 to REQ-PROMPT-005)

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};
use serde::Deserialize;

use super::super::state::{
    AppState, PendingPrompt, PendingPromptRequest, PromptOption, UserPromptResponseData,
};
use crate::agui::AgUiEvent;
use crate::protocol::StandMessage;

/// Request body for prompt creation
#[derive(Debug, Deserialize)]
pub struct PromptRequest {
    run_id: String,
    request_id: String,
    prompt_type: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    options: Option<Vec<crate::agui::PromptOption>>,
    #[serde(default)]
    default_value: Option<String>,
    #[serde(default = "default_prompt_timeout_secs")]
    timeout_seconds: u32,
}

fn default_prompt_timeout_secs() -> u32 {
    300
}

/// POST /api/prompt - Create a user prompt and wait for response
pub async fn prompt_request_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PromptRequest>,
) -> impl IntoResponse {
    let request_id = req.request_id.clone();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    tracing::info!(
        "User prompt request received: {} (type: {})",
        request_id,
        req.prompt_type
    );

    // Convert agui::PromptOption to local PromptOption
    let options = req.options.as_ref().map(|opts| {
        opts.iter()
            .map(|o| PromptOption {
                id: o.id.clone(),
                label: o.label.clone(),
                description: o.description.clone(),
            })
            .collect::<Vec<_>>()
    });

    // Store the pending request with full data
    state.pending_prompts.write().await.insert(
        request_id.clone(),
        PendingPrompt {
            request: PendingPromptRequest {
                request_id: request_id.clone(),
                prompt_type: req.prompt_type.clone(),
                title: req.title.clone(),
                description: req.description.clone(),
                options,
                default_value: req.default_value.clone(),
                timeout_seconds: req.timeout_seconds,
                created_at,
            },
            response: None,
        },
    );

    // Convert prompt_type string to enum
    let prompt_type = match req.prompt_type.as_str() {
        "confirm" => crate::agui::UserPromptType::Confirm,
        "input" => crate::agui::UserPromptType::Input,
        "select" => crate::agui::UserPromptType::Select,
        "multi_select" => crate::agui::UserPromptType::MultiSelect,
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Invalid prompt_type"})),
            );
        }
    };

    // Create and broadcast the AG-UI event
    let event = AgUiEvent::UserPrompt {
        run_id: req.run_id,
        request_id: request_id.clone(),
        prompt_type,
        title: req.title,
        description: req.description,
        options: req.options,
        default_value: req.default_value,
        timeout_seconds: req.timeout_seconds,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    };

    // Broadcast to WebSocket clients
    state.hub.broadcast(StandMessage::AgUi { event });

    state.send_debug(
        "prompt",
        &format!("User prompt created: {}", request_id),
        None,
    );

    // Return accepted and let caller poll for response
    (
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({"status": "pending", "request_id": request_id})),
    )
}

/// GET /api/prompt/{request_id} - Poll for prompt response
pub async fn prompt_poll_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let mut pending = state.pending_prompts.write().await;

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

/// POST /api/prompt/{request_id} - Submit a prompt response via HTTP
pub async fn prompt_respond_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
    Json(response): Json<UserPromptResponseData>,
) -> impl IntoResponse {
    // Use the existing WebSocket handler logic
    handle_user_prompt_response(
        &state,
        request_id.clone(),
        response.outcome.clone(),
        response.message,
        response.selected_options,
    )
    .await;

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"status": "accepted", "request_id": request_id})),
    )
}

/// Handle a user prompt response from WebSocket
pub async fn handle_user_prompt_response(
    state: &Arc<AppState>,
    request_id: String,
    outcome: String,
    message: Option<String>,
    selected_options: Option<Vec<String>>,
) {
    let mut pending = state.pending_prompts.write().await;

    if let Some(entry) = pending.get_mut(&request_id) {
        let response = UserPromptResponseData {
            outcome: outcome.clone(),
            message,
            selected_options,
        };

        // Store the response (will be retrieved by next poll)
        entry.response = Some(response);

        tracing::info!("User prompt {} -> {}", request_id, outcome);

        // Broadcast component dismissed
        state.hub.broadcast(StandMessage::ComponentDismissed {
            request_id: request_id.clone(),
        });

        // Also send AG-UI event
        let agui_outcome = match outcome.as_str() {
            "approved" => crate::agui::UserPromptOutcome::Approved,
            "rejected" => crate::agui::UserPromptOutcome::Rejected,
            "cancelled" => crate::agui::UserPromptOutcome::Cancelled,
            _ => crate::agui::UserPromptOutcome::Timeout,
        };

        // Clone request_id before moving it
        let request_id_for_agent = request_id.clone();

        state.hub.broadcast(StandMessage::AgUi {
            event: AgUiEvent::UserPromptResponse {
                run_id: String::new(), // Will be set by the client
                request_id,
                outcome: agui_outcome,
                message: entry.response.as_ref().and_then(|r| r.message.clone()),
                selected_options: entry
                    .response
                    .as_ref()
                    .and_then(|r| r.selected_options.clone()),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            },
        });

        // Release pending lock before sending to agent
        drop(pending);

        // Send user_input_result to Interactive Claude agent
        let confirmed = matches!(agui_outcome, crate::agui::UserPromptOutcome::Approved);
        let agent_guard = state.interactive_agent.read().await;
        if let Some(ref agent) = *agent_guard {
            if let Err(e) = agent
                .send_user_input_result(&request_id_for_agent, confirmed)
                .await
            {
                tracing::error!("Failed to send user_input_result to agent: {}", e);
            } else {
                tracing::info!(
                    "user_input_result sent: {} -> {}",
                    request_id_for_agent,
                    if confirmed { "approved" } else { "rejected" }
                );
            }
        } else {
            tracing::warn!("No Interactive agent running, cannot send user_input_result");
        }
    } else {
        tracing::warn!("User prompt response for unknown request: {}", request_id);
    }
}

/// List pending prompts (for external polling, e.g., VantagePoint.app)
pub async fn prompts_list_pending_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let pending = state.pending_prompts.read().await;

    // Filter prompts that don't have a response yet and return full request data
    let prompts_without_response: Vec<&PendingPromptRequest> = pending
        .values()
        .filter(|entry| entry.response.is_none())
        .map(|entry| &entry.request)
        .collect();

    (
        axum::http::StatusCode::OK,
        Json(serde_json::json!({"prompts": prompts_without_response})),
    )
}
