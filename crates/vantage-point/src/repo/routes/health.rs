//! ヘルスチェック・基本ルートハンドラー
//!
//! UI は native vp-app (WebView) が担う。 旧 localhost browser canvas (`web/canvas.html`
//! を `/` `/canvas` `/vendor` で配信) は未使用のため撤去済 (mako/drop-web-canvas)。

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};

use super::super::state::AppState;

/// 機能（service）のステータス
#[derive(serde::Serialize)]
pub struct ServiceStatus {
    /// service の状態: "active", "idle", "connected", "disabled"
    pub status: &'static str,
    /// Agent 固有の詳細情報
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// `/api/health` の `hub_nodes` 要素 — hub の向こうに居る available node 1 件。
#[derive(serde::Serialize)]
pub struct HubNodeInfo {
    /// daemon の identity（hostname 由来、hub registry の一意キー相当）
    pub handle: String,
    /// 位置独立 routing key `nd_xxx`（ADR-020 D2）。hub S2 前は空になり得るため空なら omit。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub node_id: String,
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
    pub repo_dir: String,
    /// Terminal チャネル認証トークン（TUI 接続用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_token: Option<String>,
    /// プロセス起動時刻（ISO 8601）
    pub started_at: String,
    /// 配下の service ステータス
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<std::collections::HashMap<String, ServiceStatus>>,
    /// chronista-hub federation の接続状態
    /// （`"disabled"` | `"connecting"` | `"connected"` | `"disconnected"`）。
    /// daemon mode のみ意味を持つ（repo mode は常に `"disabled"`）。vp-app が daemon status 横に表示。
    pub hub: &'static str,
    /// hub の向こうに居る available nodes（**自 daemon は除外**、handle dedup 済）。
    /// daemon mode + hub connected の間だけ非空（repo mode / 未接続は空配列）。既存 `hub` field
    /// （string）は不変のまま additive に足す — 旧 client は本 field を無視するだけで壊れない。
    pub hub_nodes: Vec<HubNodeInfo>,
    /// hub 接続の credential 提示結果（`"credentialed"` | `"anonymous"`、未接続 / 判定前は omit）。
    /// **file でなく現在の接続がどう成立したか**（プロセスの真実）。vp-app sidebar の Hub 行が
    /// Login / Logout ボタンの切替に使う。additive field — 旧 client は無視するだけで壊れない。
    #[serde(skip_serializing_if = "str::is_empty")]
    pub hub_auth: &'static str,
    /// **宛先ごとの credential 状態**（`"hub"` / `"creo"` → `"valid"` | `"expired"` | `"none"`）。
    ///
    /// ⚠️ `hub_auth` とは別物。あちらは「hub 接続がどう成立したか」= 接続の副産物で、
    /// **hub に繋いでいないと何も分からない**。こちらは `~/.vp/credentials.json` を読むだけの
    /// local 判定なので、hub federation を切っていても「creo にログイン済みか」が言える
    /// （doc 57 Phase 2 の Creo ID 行が hub から独立するのに要る）。
    ///
    /// 「local に有効な token を持っている」までしか主張しない — 実際に通るかは相手が決める。
    /// additive field、旧 client は無視するだけ。
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub auth_targets: std::collections::BTreeMap<String, String>,
    /// L1 lifecycle (Phase C): Daemon 配下の repo presence 一覧（vp-app sidebar の ●◐○ 表示用）。
    /// daemon-canonical（doc 27 §3.2 / Model Q）。daemon mode のみ Some、repo mode では None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processes: Option<Vec<crate::capability::RepoHealthInfo>>,
    /// in-app update: 新しい release が GitHub にあるか。daemon mode の定期チェック task
    /// （起動時 + 24h 毎）が温めた cache 由来で、本 handler は network を発行しない。
    /// vp-app sidebar が「更新する」ボタンの表示 gate に使う。repo mode / 未チェックは false。
    pub update_available: bool,
    /// 最新 release version（cache 未取得なら omit）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// ACTIONS（doc 57 Phase 3）— daemon の 30s poller が creo-memories から温めた cache 由来で、
    /// 本 handler は network を発行しない。repo mode / 未取得は空配列。
    pub actions: Vec<crate::creo::client::CreoAction>,
    /// ACTIONS の版。**内容が変わった時だけ**上がる。`0` = 一度も取得していない。
    ///
    /// vp-app 側はこの値が変わった時だけ sidebar に当てる。5s ごとに同じ一覧を当て直すと、
    /// **編集中の行を書き戻して caret が飛ぶ**（`<Index>` は位置キーイングなので値だけ差し戻る）。
    pub actions_rev: u32,
}

// L0 portless B-4 (wire-unison): repo `/api/wire/*` HTTP proxy handler (wire_send/recv/unread-count/
// latest-msg/thread/ack) は撤去。 MCP は repo "process" channel の `wire_*` dispatch
// (= `handle_wire_send` 等が normalize して `daemon_wire::call` で Daemon "wire" channel に relay) を
// 使い、 CLI/flow は Daemon "wire" channel に QUIC 直結する (doc 27 §62)。

// L0 portless: `/api/diagnose` (Agent 自己診断 HTTP) は consumer 消滅で撤去。 必要なら将来
// Daemon channel / mailbox query (`devices@machine` 等) 経由で再設計する。

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let token = if state.terminal_token == "DAEMON_DISABLED" {
        None
    } else {
        Some(state.terminal_token.clone())
    };

    // Agent ステータスを収集（daemon モードでは省略）
    let services = if state.terminal_token != "DAEMON_DISABLED" {
        let mut map = std::collections::HashMap::new();

        // 💬 Conversation (Coding Assistant) — interactive_agent の有無で判定
        let conversation_status = {
            let agent = state.interactive_agent.read().await;
            if agent.is_some() { "active" } else { "idle" }
        };
        map.insert(
            "claude".to_string(),
            ServiceStatus {
                status: conversation_status,
                detail: None,
            },
        );

        // 🧭 Board（Canvas）— WebSocket クライアント接続数
        let canvas_clients = state.canvas_senders.lock().await.len();
        map.insert(
            "board".to_string(),
            ServiceStatus {
                status: if canvas_clients > 0 {
                    "connected"
                } else {
                    "idle"
                },
                detail: Some(serde_json::json!({ "clients": canvas_clients })),
            },
        );

        // 🌿 Runner（ProcessRunner）— 実行中プロセス数
        let running_repos = state.process_registry.lock().await.list().len();
        map.insert(
            "runner".to_string(),
            ServiceStatus {
                status: if running_repos > 0 { "active" } else { "idle" },
                detail: Some(serde_json::json!({ "processes": running_repos })),
            },
        );

        // 🧲 DeviceRegistry（MIDI device registry）— daemon mode のみ host。
        // repo mode からは「disabled」として報告（α-3 で cross-process query 経由に rewire 予定）。
        #[cfg(feature = "midi")]
        let (devices_status, devices_detail) = {
            if let Some(wc) = state.machine_capabilities.as_ref() {
                if let Some(ref devices) = wc.devices {
                    let b = devices.read().await;
                    let count = b.device_count().await;
                    let discovering = b.is_discovering();
                    // 艦隊スイッチ: OFF は「device が居ない」ではなく「**握っていない**」。
                    // 一覧は保つので count は落とさず、status で区別する（他アプリへ譲っている状態）。
                    let enabled = b.midi_enabled();
                    (
                        match (enabled, count > 0) {
                            (false, _) => "released",
                            (true, true) => "active",
                            (true, false) => "idle",
                        },
                        Some(serde_json::json!({
                            "devices": count,
                            "discovering": discovering,
                            "midi_enabled": enabled,
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
            ServiceStatus {
                status: devices_status,
                detail: devices_detail,
            },
        );

        // DB にも Agent ステータスを書き込み（VP-21）
        if let Some(ref db) = state.vpdb {
            for (key, s) in &map {
                if let Err(e) = db
                    .upsert_service_status(&state.repo_dir, key, s.status, s.detail.as_ref())
                    .await
                {
                    tracing::warn!("DB service_status 書き込み失敗 ({}): {}", key, e);
                }
            }
        }

        Some(map)
    } else {
        // daemon mode — DeviceRegistry のみ報告（machine 階層に host される唯一の observable Agent）
        #[cfg(feature = "midi")]
        {
            let mut map = std::collections::HashMap::new();
            if let Some(devices) = state
                .machine_capabilities
                .as_ref()
                .and_then(|wc| wc.devices.as_ref())
            {
                let b = devices.read().await;
                let count = b.device_count().await;
                let discovering = b.is_discovering();
                // 艦隊スイッチ（repo mode 側と同じ規律 — OFF は「握っていない」）。
                let enabled = b.midi_enabled();
                map.insert(
                    "devices".to_string(),
                    ServiceStatus {
                        status: match (enabled, count > 0) {
                            (false, _) => "released",
                            (true, true) => "active",
                            (true, false) => "idle",
                        },
                        detail: Some(serde_json::json!({
                            "devices": count,
                            "discovering": discovering,
                            "midi_enabled": enabled,
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

    // L1 lifecycle: daemon mode は配下 repo の presence 一覧を expose（vp-app sidebar の ●◐○ 用）。
    // repo mode (`state.daemon` 不在) は None — presence は daemon-canonical で daemon のみが持つ。
    let processes = match state.daemon.as_ref() {
        Some(daemon) => Some(daemon.read().await.presence_snapshot().await),
        None => None,
    };

    // hub の向こうの available nodes（run_hub_federation が discover で更新する cache を読む）。
    let hub_nodes = state
        .hub_nodes
        .get()
        .into_iter()
        .map(|w| HubNodeInfo {
            handle: w.handle,
            node_id: w.node_id,
            endpoints_count: w.endpoints.len(),
            connected: w.connected,
        })
        .collect();

    // in-app update: 定期チェック task が温めた cache を読むだけ（network なし）。
    let (update_available, latest_version) = match state.update.as_ref() {
        Some(update) => update.read().await.cached_update_status(),
        None => (false, None),
    };

    // ACTIONS: 30s poller が温めた cache を読むだけ（network なし）。
    let actions_snapshot = state.creo_actions.get();

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        repo_dir: state.repo_dir.clone(),
        terminal_token: token,
        started_at: state.started_at.clone(),
        services,
        hub: state.hub_status.get().as_str(),
        hub_nodes,
        hub_auth: state.hub_auth.get().as_str(),
        auth_targets: auth_target_states(),
        processes,
        update_available,
        latest_version,
        actions: actions_snapshot.items,
        actions_rev: actions_snapshot.rev,
    })
}

/// 宛先ごとの credential 状態を local file から判定する（network を叩かない純粋な読み取り）。
///
/// 値は `"valid"` / `"expired"` / `"none"` の 3 値。**「持っているか」までしか言わない** —
/// 実際にその token が通るかは相手の API が決めるので、ここで「認証済み」とは主張しない。
/// file が壊れている / 読めない場合も `"none"`（= 使えない）に倒す（fail-closed）。
fn auth_target_states() -> std::collections::BTreeMap<String, String> {
    use crate::commands::auth::AuthTarget;
    let store = crate::commands::auth::read_store().unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    [AuthTarget::Hub, AuthTarget::Creo]
        .into_iter()
        .map(|t| {
            let state = match store.get(&t.audience()) {
                None => "none",
                // expires_at 不明は valid 扱い（`Credentials::is_expired` と同じ規律 —
                // 分からないものを期限切れと決めつけない）。
                Some(c) => match c.expires_at {
                    Some(exp) if exp <= now => "expired",
                    _ => "valid",
                },
            };
            (t.label().to_string(), state.to_string())
        })
        .collect()
}

// L0 portless Group B: pane HTTP handler (show/toggle/split/close) は CLI を repo-proxy ask
// (`show`/`toggle_pane`/`split_pane`/`close_pane` → `handle_process_message`) に移管し撤去。
// いずれも `state.hub.broadcast(RepoMessage)` するだけで、 QUIC dispatch が同じ broadcast を行う。

// doc 45 段 4: `/api/canvas/switch_lane` `/api/canvas/layout` の handler は撤去。
// switch_lane の宛先 `AppState.canvas_senders` は**どこからも populate されない**
// （旧 localhost browser Canvas の WS 撤去で書き手が消えた）ので、常に 0 client に
// 送っていた。layout の `load/save_canvas_layout` も呼び出し元がこの 2 handler だけで、
// end-to-end で dead だった（doc 45 §3.1）。Unison に移すと「読み手のいない書き込み」を
// 新設することになるので、移設先ではなく撤去に置いた。
// CLI / MCP の `switch_lane` は repo-proxy 経由の別経路で、この route を通らない。

// L0 portless Group B: file watch/unwatch HTTP handler は CLI を repo-proxy ask
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
// 全て CLI/flow を repo-proxy ask (`tmux_*` dispatch) に移管し撤去 (send-keys/resolve-pane は
// lanes portless で flow.rs(try_nudge) が dispatch 化したのが最後)。 `/api/tmux/agent-meta` は
// consumer ゼロで dead 撤去済。

// L0 portless Group B-3: Ruby VM HTTP handler (eval/run/stop/list) は唯一の consumer だった MCP を
// repo-proxy ask (`unison_server::handle_ruby_*`、 同じ `process_runner::ruby_*` core) に移管し撤去。
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
        let state = crate::repo::state::build_test_app_state(None).await;
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
        let state = crate::repo::state::build_test_app_state(None).await;
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
        assert!(body.get("repo_dir").is_some(), "repo_dir field 必須");
        assert!(body.get("started_at").is_some(), "started_at field 必須");
        // stands は test 用 AppState では terminal_token == "test" なので
        // "DAEMON_DISABLED" 分岐に入らず populate される
        assert!(
            body.get("services").is_some(),
            "services field 必須 (= Agent status map)"
        );
        // hub federation 状態（test AppState は HubFederationStatus::new() = Disabled）。
        // field 名変更 / as_str() パス破壊の regression net。
        assert_eq!(
            body.get("hub").and_then(|v| v.as_str()),
            Some("disabled"),
            "hub field 必須 (repo/test mode は Disabled = \"disabled\")"
        );
        // hub_nodes は常時 serialize（repo/test mode = HubNodesCache::new() は空配列）。
        assert_eq!(
            body.get("hub_nodes")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(0),
            "hub_nodes field 必須 (repo/test mode は空配列)"
        );
        // in-app update: test AppState は update capability 不在（None）= 常に false。
        // cache 未チェック時も false なので、field の常時 serialize を regression net にする。
        assert_eq!(
            body.get("update_available").and_then(|v| v.as_bool()),
            Some(false),
            "update_available field 必須 (repo/test mode は false)"
        );
        assert!(
            body.get("latest_version").is_none(),
            "latest_version は cache 未取得時 omit"
        );
        // ACTIONS（doc 57 Phase 3）: test/repo mode は poller を持たないので空 + rev 0。
        // **常時 serialize** を固定する — omit すると vp-app 側で「未取得」と「0 件」の
        // 区別が付かなくなる（rev 0 が「当てない」の印そのもの）。
        assert_eq!(
            body.get("actions").and_then(|v| v.as_array()).map(Vec::len),
            Some(0),
            "actions field 必須 (repo/test mode は空配列)"
        );
        assert_eq!(
            body.get("actions_rev").and_then(|v| v.as_u64()),
            Some(0),
            "actions_rev field 必須 (未取得 = 0)"
        );
    }
}
