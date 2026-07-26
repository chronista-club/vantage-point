//! ヘルスチェック・基本ルートハンドラー
//!
//! UI は native vp-app (WebView) が担う。 旧 localhost browser canvas (`web/canvas.html`
//! を `/` `/canvas` `/vendor` で配信) は未使用のため撤去済 (mako/drop-web-canvas)。

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use super::super::state::AppState;

/// Stand（Capability）のステータス
#[derive(serde::Serialize)]
pub struct StandStatus {
    /// Stand の状態: "active", "idle", "connected", "disabled"
    pub status: &'static str,
    /// Stand 固有の詳細情報
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// `/api/health` の `hub_worlds` 要素 — hub の向こうに居る available world 1 件。
#[derive(serde::Serialize)]
pub struct HubWorldInfo {
    /// world の identity（hostname 由来、hub registry の一意キー相当）
    pub handle: String,
    /// 位置独立 routing key `wld_xxx`（ADR-020 D2）。hub S2 前は空になり得るため空なら omit。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub wld_id: String,
    /// direct 到達 endpoint 候補数（hub S2 前は 0）
    pub endpoints_count: usize,
    /// hub との常駐接続が今生きているか（hub protocol v0.6.0 の relay registry snapshot 由来）。
    /// false = registry には居るが relay は offline（stale entry / 切断中）。旧 hub は常に false。
    pub connected: bool,
}

/// Health check response
#[derive(serde::Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub pid: u32,
    pub project_dir: String,
    /// Terminal チャネル認証トークン（TUI 接続用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_token: Option<String>,
    /// プロセス起動時刻（ISO 8601）
    pub started_at: String,
    /// 配下の Stand（Capability）ステータス
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stands: Option<std::collections::HashMap<String, StandStatus>>,
    /// chronista-hub federation の接続状態
    /// （`"disabled"` | `"connecting"` | `"connected"` | `"disconnected"`）。
    /// World mode のみ意味を持つ（SP mode は常に `"disabled"`）。vp-app が world status 横に表示。
    pub hub: &'static str,
    /// hub の向こうに居る available worlds（**自 world は除外**、handle dedup 済）。
    /// World mode + hub connected の間だけ非空（SP mode / 未接続は空配列）。既存 `hub` field
    /// （string）は不変のまま additive に足す — 旧 client は本 field を無視するだけで壊れない。
    pub hub_worlds: Vec<HubWorldInfo>,
    /// L1 lifecycle (Phase C): World 配下の SP presence 一覧（vp-app sidebar の ●◐○ 表示用）。
    /// daemon-canonical（doc 27 §3.2 / Model Q）。World mode のみ Some、SP mode では None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<crate::capability::ProcessHealthInfo>>,
    /// in-app update: 新しい release が GitHub にあるか。World mode の定期チェック task
    /// （起動時 + 24h 毎）が温めた cache 由来で、本 handler は network を発行しない。
    /// vp-app sidebar が「更新する」ボタンの表示 gate に使う。SP mode / 未チェックは false。
    pub update_available: bool,
    /// 最新 release version（cache 未取得なら omit）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
}

// L0 portless B-4 (wire-unison): SP `/api/wire/*` HTTP proxy handler (wire_send/recv/unread-count/
// latest-msg/thread/ack) は撤去。 MCP は SP "process" channel の `wire_*` dispatch
// (= `handle_wire_send` 等が normalize して `world_wire::call` で World "wire" channel に relay) を
// 使い、 CLI/flow は World "wire" channel に QUIC 直結する (doc 27 §62)。

// L0 portless: `/api/diagnose` (Stand 自己診断 HTTP) は consumer 消滅で撤去。 必要なら将来
// World channel / mailbox query (`devices@world` 等) 経由で再設計する。

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let token = if state.terminal_token == "WORLD_DISABLED" {
        None
    } else {
        Some(state.terminal_token.clone())
    };

    // Stand ステータスを収集（TheWorld モードでは省略）
    let stands = if state.terminal_token != "WORLD_DISABLED" {
        let mut map = std::collections::HashMap::new();

        // 💬 Echoes (Coding Assistant) — interactive_agent の有無で判定
        let echoes_status = {
            let agent = state.interactive_agent.read().await;
            if agent.is_some() { "active" } else { "idle" }
        };
        map.insert(
            "echoes".to_string(),
            StandStatus {
                status: echoes_status,
                detail: None,
            },
        );

        // 🧭 Paisley Park（Canvas）— WebSocket クライアント接続数
        let canvas_clients = state.canvas_senders.lock().await.len();
        map.insert(
            "paisley_park".to_string(),
            StandStatus {
                status: if canvas_clients > 0 {
                    "connected"
                } else {
                    "idle"
                },
                detail: Some(serde_json::json!({ "clients": canvas_clients })),
            },
        );

        // 🌿 Gold Experience（ProcessRunner）— 実行中プロセス数
        let running_processes = state.process_registry.lock().await.list().len();
        map.insert(
            "gold_experience".to_string(),
            StandStatus {
                status: if running_processes > 0 {
                    "active"
                } else {
                    "idle"
                },
                detail: Some(serde_json::json!({ "processes": running_processes })),
            },
        );

        // 🧲 DeviceRegistry（MIDI device registry）— World mode のみ host。
        // SP mode からは「disabled」として報告（α-3 で cross-process query 経由に rewire 予定）。
        #[cfg(feature = "midi")]
        let (devices_status, devices_detail) = {
            if let Some(wc) = state.world_capabilities.as_ref() {
                if let Some(ref devices) = wc.devices {
                    let b = devices.read().await;
                    let count = b.device_count().await;
                    let discovering = b.is_discovering();
                    (
                        if count > 0 { "active" } else { "idle" },
                        Some(serde_json::json!({
                            "devices": count,
                            "discovering": discovering,
                        })),
                    )
                } else {
                    ("disabled", None)
                }
            } else {
                ("disabled", None)
            }
        };
        #[cfg(not(feature = "midi"))]
        let (devices_status, devices_detail) = ("disabled", None);
        map.insert(
            "devices".to_string(),
            StandStatus {
                status: devices_status,
                detail: devices_detail,
            },
        );

        // DB にも Stand ステータスを書き込み（VP-21）
        if let Some(ref db) = state.vpdb {
            for (key, s) in &map {
                if let Err(e) = db
                    .upsert_stand_status(&state.project_dir, key, s.status, s.detail.as_ref())
                    .await
                {
                    tracing::warn!("DB stand_status 書き込み失敗 ({}): {}", key, e);
                }
            }
        }

        Some(map)
    } else {
        // World mode — DeviceRegistry のみ報告（World 階層に host される唯一の observable Stand）
        #[cfg(feature = "midi")]
        {
            let mut map = std::collections::HashMap::new();
            if let Some(devices) = state
                .world_capabilities
                .as_ref()
                .and_then(|wc| wc.devices.as_ref())
            {
                let b = devices.read().await;
                let count = b.device_count().await;
                let discovering = b.is_discovering();
                map.insert(
                    "devices".to_string(),
                    StandStatus {
                        status: if count > 0 { "active" } else { "idle" },
                        detail: Some(serde_json::json!({
                            "devices": count,
                            "discovering": discovering,
                        })),
                    },
                );
            }
            if map.is_empty() { None } else { Some(map) }
        }
        #[cfg(not(feature = "midi"))]
        {
            None
        }
    };

    // L1 lifecycle: World mode は配下 SP の presence 一覧を expose（vp-app sidebar の ●◐○ 用）。
    // SP mode (`state.world` 不在) は None — presence は daemon-canonical で World のみが持つ。
    let processes = match state.world.as_ref() {
        Some(world) => Some(world.read().await.presence_snapshot().await),
        None => None,
    };

    // hub の向こうの available worlds（run_hub_federation が discover で更新する cache を読む）。
    let hub_worlds = state
        .hub_worlds
        .get()
        .into_iter()
        .map(|w| HubWorldInfo {
            handle: w.handle,
            wld_id: w.wld_id,
            endpoints_count: w.endpoints.len(),
            connected: w.connected,
        })
        .collect();

    // in-app update: 定期チェック task が温めた cache を読むだけ（network なし）。
    let (update_available, latest_version) = match state.update.as_ref() {
        Some(update) => update.read().await.cached_update_status(),
        None => (false, None),
    };

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        project_dir: state.project_dir.clone(),
        terminal_token: token,
        started_at: state.started_at.clone(),
        stands,
        hub: state.hub_status.get().as_str(),
        hub_worlds,
        processes,
        update_available,
        latest_version,
    })
}

// L0 portless Group B: pane HTTP handler (show/toggle/split/close) は CLI を process-proxy ask
// (`show`/`toggle_pane`/`split_pane`/`close_pane` → `handle_process_message`) に移管し撤去。
// いずれも `state.hub.broadcast(ProcessMessage)` するだけで、 QUIC dispatch が同じ broadcast を行う。

// doc 45 段 4: `/api/canvas/switch_lane` `/api/canvas/layout` の handler は撤去。
// switch_lane の宛先 `AppState.canvas_senders` は**どこからも populate されない**
// （旧 localhost browser Canvas の WS 撤去で書き手が消えた）ので、常に 0 client に
// 送っていた。layout の `load/save_canvas_layout` も呼び出し元がこの 2 handler だけで、
// end-to-end で dead だった（doc 45 §3.1）。Unison に移すと「読み手のいない書き込み」を
// 新設することになるので、移設先ではなく撤去に置いた。
// CLI / MCP の `switch_lane` は process-proxy 経由の別経路で、この route を通らない。

// L0 portless Group B: file watch/unwatch HTTP handler は CLI を process-proxy ask
// (`watch_file`/`unwatch_file` → `handle_watch_file`/`handle_unwatch_file`) に移管し撤去。
// core (`state.file_watchers`) は QUIC dispatch が同じく呼ぶので維持。

// 旧 GET /wasm/{filename} (vp-mdast-wasm 配信 endpoint) は 2026-05-25 削除。
// frontend (vp-app webview) は `marked` (npm) + `@chronista-club/creo-ui-editor-host`
// に markdown rendering を移行済で、 vp_mdast_wasm 関連 asset は dead 化していた。
// vp-mdast / vp-mdast-wasm crate + web/wasm/ asset (482KB) と共に撤去。

/// POST /api/shutdown - Graceful shutdown
pub async fn shutdown_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    tracing::info!("Shutdown requested via API");
    state.shutdown_token.cancel();
    Json(serde_json::json!({"status": "shutting_down"}))
}

// L0 portless Group B/C: tmux split/close/capture/list/send-keys/resolve-pane の HTTP handler は
// 全て CLI/flow を process-proxy ask (`tmux_*` dispatch) に移管し撤去 (send-keys/resolve-pane は
// lanes portless で flow.rs(try_nudge) が dispatch 化したのが最後)。 `/api/tmux/agent-meta` は
// consumer ゼロで dead 撤去済。

// L0 portless Group B-3: Ruby VM HTTP handler (eval/run/stop/list) は唯一の consumer だった MCP を
// process-proxy ask (`unison_server::handle_ruby_*`、 同じ `process_runner::ruby_*` core) に移管し撤去。
// L0 portless: `/api/process/*` (ProcessRunner 汎用 HTTP) handler 群は consumer 消滅で撤去。
// 生きてる process 操作は QUIC `process` channel (`unison_server::handle_process_*`) が
// 同じ `process_runner` core を呼ぶので、 HTTP 入口だけ落とせば core は維持される。

#[cfg(test)]
mod tests {
    //! VP-13 sub-scope E: health.rs route の Axum oneshot smoke test。

    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt;

    /// doc 45 §2: `/api/shutdown` は HTTP に残す 2 本のうちの 1 本（緊急停止は最も単純な
    /// 経路であるべき）。handler が生きていて `shutdown_token` を実際に cancel することを固定する
    /// —— 撤去の巻き添えで落ちると、Unison が wedge した時に止める手段ごと失う。
    #[tokio::test]
    async fn shutdown_handler_cancels_shutdown_token() {
        let state = crate::process::state::build_test_app_state(None).await;
        let token = state.shutdown_token.clone();
        assert!(!token.is_cancelled(), "前提: まだ cancel されていない");

        let app = Router::new()
            .route("/api/shutdown", post(shutdown_handler))
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            token.is_cancelled(),
            "POST /api/shutdown は shutdown_token を cancel する"
        );
    }

    #[tokio::test]
    async fn health_handler_returns_200_with_stands_field() {
        let state = crate::process::state::build_test_app_state(None).await;
        let app = Router::new()
            .route("/api/health", get(health_handler))
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
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
        // HealthResponse の必須 field を verify (= 構造変更 regression net)
        assert_eq!(body.get("status").and_then(|v| v.as_str()), Some("ok"));
        assert!(body.get("version").is_some(), "version field 必須");
        assert!(body.get("pid").is_some(), "pid field 必須");
        assert!(body.get("project_dir").is_some(), "project_dir field 必須");
        assert!(body.get("started_at").is_some(), "started_at field 必須");
        // stands は test 用 AppState では terminal_token == "test" なので
        // "WORLD_DISABLED" 分岐に入らず populate される
        assert!(
            body.get("stands").is_some(),
            "stands field 必須 (= Stand status map)"
        );
        // hub federation 状態（test AppState は HubFederationStatus::new() = Disabled）。
        // field 名変更 / as_str() パス破壊の regression net。
        assert_eq!(
            body.get("hub").and_then(|v| v.as_str()),
            Some("disabled"),
            "hub field 必須 (SP/test mode は Disabled = \"disabled\")"
        );
        // hub_worlds は常時 serialize（SP/test mode = HubWorldsCache::new() は空配列）。
        assert_eq!(
            body.get("hub_worlds")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(0),
            "hub_worlds field 必須 (SP/test mode は空配列)"
        );
        // in-app update: test AppState は update capability 不在（None）= 常に false。
        // cache 未チェック時も false なので、field の常時 serialize を regression net にする。
        assert_eq!(
            body.get("update_available").and_then(|v| v.as_bool()),
            Some(false),
            "update_available field 必須 (SP/test mode は false)"
        );
        assert!(
            body.get("latest_version").is_none(),
            "latest_version は cache 未取得時 omit"
        );
    }
}
