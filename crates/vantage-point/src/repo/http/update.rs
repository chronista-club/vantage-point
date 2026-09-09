//! Update APIルートハンドラー
//!
//! vp CLI と VantagePoint.app の更新管理。

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};

use super::super::state::AppState;
use crate::capability::UpdateCapability;

/// GET /api/update/check - 更新をチェック
pub async fn update_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    let mut update = update.write().await;
    match update.check_update().await {
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(result).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/apply - 更新を適用（ダウンロード＆置換）
pub async fn update_apply(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // まず更新をチェック
    let mut update = update.write().await;
    let check_result = match update.check_update().await {
        Ok(result) => result,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Update check failed: {}", e)})),
            );
        }
    };

    // 更新がない場合
    if !check_result.update_available {
        return (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": false,
                "message": "No update available",
                "current_version": check_result.current_version,
                "latest_version": check_result.latest_version,
            })),
        );
    }

    // リリース情報を取得
    let Some(release) = check_result.release else {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Release info not available"})),
        );
    };

    // 更新を適用
    match update.apply_update(&release).await {
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(serde_json::to_value(result).unwrap_or_default()),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/rollback - ロールバックを実行
pub async fn update_rollback(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // バックアップパスを取得
    let backup_path = match body.get("backup_path").and_then(|v| v.as_str()) {
        Some(path) => path,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "backup_path is required"})),
            );
        }
    };

    let update = update.read().await;
    match update.rollback(backup_path).await {
        Ok(_) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "Rollback completed. Restart required.",
                "restart_required": true,
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/restart - アプリケーションを再起動
///
/// リクエストボディ:
/// - `app_path`: 再起動するアプリのパス（省略時は現在のバイナリ）
/// - `delay`: 遅延秒数（デフォルト: 1秒）
pub async fn update_restart(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(_update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // パラメータを取得
    let app_path = body.get("app_path").and_then(|v| v.as_str());
    let delay = body.get("delay").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

    // 再起動をスケジュール
    let result = if let Some(path) = app_path {
        UpdateCapability::restart_app(path, delay).await
    } else {
        UpdateCapability::restart_self(delay).await
    };

    match result {
        Ok(_) => {
            // 再起動スクリプトが起動されたので、このプロセスを終了する準備
            // クライアントにレスポンスを返してから終了
            let shutdown_token = state.shutdown_token.clone();

            // 少し遅延してからシャットダウン（レスポンスを返す時間を確保）
            tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                shutdown_token.cancel();
            });

            (
                axum::http::StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "message": format!("Restart scheduled in {} seconds", delay),
                    "delay": delay,
                })),
            )
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// ============================================================================
// Mac App Update API Handlers
// ============================================================================

/// GET /api/update/mac/check - VantagePoint.app の更新をチェック
///
/// クエリパラメータ:
/// - `current_version`: 現在のアプリバージョン（必須）
pub async fn update_mac_check(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    let current_version = match params.get("current_version") {
        Some(v) => v,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "current_version query parameter is required"})),
            );
        }
    };

    let mut update = update.write().await;
    match update.check_mac_update(current_version).await {
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(
                serde_json::to_value(&result)
                    .unwrap_or_else(|_| serde_json::json!({"status": "ok"})),
            ),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/mac/apply - VantagePoint.app の更新を適用
///
/// リクエストボディ:
/// - `current_version`: 現在のバージョン（必須）
/// - `app_path`: アプリパス（省略時は自動検索）
pub async fn update_mac_apply(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    // パラメータを取得
    let current_version = match body.get("current_version").and_then(|v| v.as_str()) {
        Some(v) => v.to_string(),
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "current_version is required"})),
            );
        }
    };

    let app_path = body.get("app_path").and_then(|v| v.as_str());

    // まず最新リリースを取得
    let mut update_guard = update.write().await;
    let check_result = match update_guard.check_mac_update(&current_version).await {
        Ok(r) => r,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    };

    // 更新がなければ終了
    let Some(release) = check_result.release else {
        return (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": false,
                "message": "No update available",
                "current_version": current_version,
                "latest_version": check_result.latest_version,
            })),
        );
    };

    // 更新を適用
    match update_guard
        .apply_mac_update(&release, &current_version, app_path)
        .await
    {
        Ok(result) => (
            axum::http::StatusCode::OK,
            Json(
                serde_json::to_value(&result)
                    .unwrap_or_else(|_| serde_json::json!({"status": "ok"})),
            ),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

/// POST /api/update/mac/rollback - VantagePoint.app をロールバック
pub async fn update_mac_rollback(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(update) = &state.update else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Update capability not available"})),
        );
    };

    let backup_path = match body.get("backup_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "backup_path is required"})),
            );
        }
    };

    let app_path = match body.get("app_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "app_path is required"})),
            );
        }
    };

    let update = update.read().await;
    match update.rollback_mac_app(backup_path, app_path).await {
        Ok(_) => (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "success": true,
                "message": "Rollback completed. Restart required.",
                "restart_required": true,
            })),
        ),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}
