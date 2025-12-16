//! HTTP Server
//!
//! 選択肢UIのAPIエンドポイントを提供

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;
use vantage_core::{Choice, ChoicePrompt, CooperationMode, UserResponse};

/// サーバー状態
#[derive(Default)]
struct AppState {
    mode: CooperationMode,
    current_prompt: Option<ChoicePrompt>,
}

type SharedState = Arc<RwLock<AppState>>;

pub async fn run(port: u16) -> Result<()> {
    let state: SharedState = Arc::new(RwLock::new(AppState::default()));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/api/mode", get(get_mode).post(set_mode))
        .route("/api/prompt", get(get_prompt))
        .route("/api/respond", post(respond))
        .route("/api/demo", post(demo_prompt))
        .layer(cors)
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    info!("Vantage Server starting on http://{}", addr);
    info!("  GET  /health     - ヘルスチェック");
    info!("  GET  /api/mode   - 協調モード取得");
    info!("  POST /api/mode   - 協調モード設定");
    info!("  GET  /api/prompt - 現在の選択肢取得");
    info!("  POST /api/respond - ユーザー回答");
    info!("  POST /api/demo   - デモ選択肢生成");

    axum::serve(listener, app).await?;

    Ok(())
}

async fn root() -> &'static str {
    "Vantage Point - 開発行為を拡張する"
}

async fn health() -> &'static str {
    "ok"
}

// --- Mode API ---

#[derive(Serialize)]
struct ModeResponse {
    mode: CooperationMode,
    description: String,
}

async fn get_mode(State(state): State<SharedState>) -> Json<ModeResponse> {
    let state = state.read().await;
    Json(ModeResponse {
        mode: state.mode,
        description: state.mode.description().to_string(),
    })
}

#[derive(Deserialize)]
struct SetModeRequest {
    mode: CooperationMode,
}

async fn set_mode(
    State(state): State<SharedState>,
    Json(req): Json<SetModeRequest>,
) -> Json<ModeResponse> {
    let mut state = state.write().await;
    state.mode = req.mode;
    info!("モード変更: {}", req.mode);
    Json(ModeResponse {
        mode: state.mode,
        description: state.mode.description().to_string(),
    })
}

// --- Prompt API ---

#[derive(Serialize)]
struct PromptResponse {
    prompt: Option<ChoicePrompt>,
}

async fn get_prompt(State(state): State<SharedState>) -> Json<PromptResponse> {
    let state = state.read().await;
    Json(PromptResponse {
        prompt: state.current_prompt.clone(),
    })
}

async fn respond(
    State(state): State<SharedState>,
    Json(response): Json<UserResponse>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut state = state.write().await;

    match &response {
        UserResponse::Choice { id } => {
            info!("ユーザー選択: {}", id);
        }
        UserResponse::Text { content } => {
            info!("ユーザー入力: {}", content);
        }
        UserResponse::Cancel => {
            info!("キャンセル");
        }
    }

    // 選択肢をクリア
    state.current_prompt = None;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "received",
            "response": response
        })),
    )
}

// --- Demo API ---

async fn demo_prompt(State(state): State<SharedState>) -> Json<ChoicePrompt> {
    let prompt = ChoicePrompt::new(
        "次のステップはどうしますか？",
        vec![
            Choice::new("A", "テストを書く").with_description("ユニットテストを追加"),
            Choice::new("B", "リファクタリング").with_description("コードを整理"),
            Choice::new("C", "次の機能へ").with_description("新機能の実装に進む"),
        ],
    );

    let mut state = state.write().await;
    state.current_prompt = Some(prompt.clone());

    info!("デモプロンプト生成");
    Json(prompt)
}
