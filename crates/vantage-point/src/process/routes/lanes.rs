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

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::lanes_state::{LaneAddress, LaneInfo, LaneKind, LaneStand, LaneState};
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

/// `POST /api/lanes` request body (A6 Worker Lane create)
#[derive(Debug, Deserialize)]
pub struct CreateLaneReq {
    /// "worker" のみ受付 (Lead は project ごと固定)
    pub kind: String,
    /// Worker name (人間可読、 LaneAddress.name に入る)
    pub name: String,
    /// LaneStand: "heavens_door" (default) or "the_hand"
    #[serde(default)]
    pub stand: Option<String>,
    /// 既存 worktree path (auto-clone は A6-2 で)
    #[serde(default)]
    pub cwd: Option<String>,
}

/// `POST /api/lanes` — Worker Lane create
///
/// **A6 Phase 1 (今)**: Worker Lane を `LanePool` に insert (state=Spawning stub)。
/// 実 PTY spawn は A5-2 で `LanePool` の PtySlot 連動で実装。
///
/// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Process recursive)
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
    let cwd = req.cwd.unwrap_or_else(|| state.project_dir.clone());

    // 重複チェック
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

    let info = LaneInfo {
        address: addr.clone(),
        kind: LaneKind::Worker,
        name: Some(req.name.clone()),
        state: LaneState::Spawning, // A5-2 で実 spawn 後 Running に
        stand,
        created_at: chrono::Utc::now().to_rfc3339(),
        pid: None,
        cwd,
    };

    {
        let mut pool = state.lane_pool.write().await;
        pool.insert(info.clone());
    }

    tracing::info!("Worker Lane created: {} (stand={:?})", addr, stand);
    Ok((StatusCode::CREATED, Json(info)))
}
