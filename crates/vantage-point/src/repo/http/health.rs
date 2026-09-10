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
    /// アイドルとみなすまでの分数（settings.kdl の `idle-timeout-minutes`、doc 59 P3）。
    ///
    /// **daemon が持つ 1 つの値を GUI にも配る**ための field。sidebar の now-line が
    /// 「⏸N分」に沈む閾値（client 判定）と、engine を落とす猶予（daemon 判定）は
    /// 意図的に同値なので、client 側に定数を二重に持たせない。
    pub idle_timeout_minutes: u64,
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
        idle_timeout_minutes: crate::repo::lane::idle_teardown_after_minutes(),
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
// repo-proxy ask (`process_ops::handle_ruby_*`、 同じ `process_runner::ruby_*` core) に移管し撤去。
// L0 portless: `/api/process/*` (ProcessRunner 汎用 HTTP) handler 群は consumer 消滅で撤去。
// 生きてる process 操作は QUIC `process` channel (`process_ops::handle_process_*`) が
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
        let state = crate::repo::state::build_test_app_state().await;
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

    // =====================================================================
    // 棚卸し 9-2 PR-0.5 — daemon 形 `/api/health` の characterization
    //
    // production の `/api/health` は `build_daemon_router` にしか mount されておらず
    // （`repo/server.rs:572`、production 呼び手は `:946` の 1 本）、渡るのは必ず
    // **daemon 役**の `AppState`。つまり `terminal_token != "DAEMON_DISABLED"` の
    // 分岐（`:119-200`）は production で一度も通らない。
    //
    // 9-2 の PR-2c はこの handler の state を `Arc<DaemonState>` に載せ替え、
    // `AppState` の daemon 専用 10 field を同時に削除する。**その前後で応答が
    // 1 bit も変わらないこと**を確かめるための基準線をここに置く。
    //
    // ## 網は 2 層
    //
    // 1. **形**（key 集合と既定値）— field が増減していない
    // 2. **実体**（`daemon_health_projects_the_given_instances`）— 値が
    //    **組み立てで渡した実体**から来ている
    //
    // ⚠️ **2 が本体。** doc 63 §6 の通り、`-D warnings` は孤児 field を検出するが
    //    **同じ型の別実体は検出できない**。PR-2c が `HubFederationStatus::new()` を
    //    新しく作って渡しても compile は通り、既定値のままの health を返し続ける。
    //    層 1 だけでは全部の cache がその壊し方を素通しする。
    //
    // ⚠️ PR-2c で書き換えてよいのは state を組む 2 関数（`daemon_health_body` の本体と
    //    `health_body_of` の引数型）だけ。assert を緩めたら「載せ替えた」ではなく「変えた」。
    //    **例外は `repo_dir`** — doc 63 §3 が PR-2c での削除を承認済みなので、
    //    `daemon_health_carries_repo_dir` を**test ごと消す**（assert の書き換えではなく）。
    // =====================================================================

    /// 既定値のままの daemon 形で `/api/health` を 1 回叩いて body を返す。
    ///
    /// **PR-2c で書き換えるのはここ** — state の組み立てが `DaemonState` に変わるだけで、
    /// 呼び手の assert は verbatim で通るのが合格条件。
    async fn daemon_health_body() -> serde_json::Value {
        let state = crate::repo::state::build_test_daemon_app_state().await;
        health_body_of(state).await
    }

    /// 渡された state で `/api/health` を 1 回叩く。
    ///
    /// `daemon_health_body` と分けてあるのは、cache を非初期値へ動かした state や、
    /// **同じ state を 2 回**叩く必要がある test があるため。
    async fn health_body_of(state: Arc<AppState>) -> serde_json::Value {
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
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 起動直後の daemon が返す **key 集合**を固定する。
    ///
    /// `HealthResponse` は 17 field で、うち **6 つ**が `skip_serializing_if` を持つ
    /// （`terminal_token` / `services` / `hub_auth` / `auth_targets` / `processes` /
    /// `latest_version`）。起動直後の daemon ではそのうち **3 つ**が省略側に倒れる:
    ///
    /// - `terminal_token` — daemon は token を配らない（`None`）
    /// - `hub_auth` — `Unknown` = 空文字列
    /// - `latest_version` — update cache が未取得
    ///
    /// 残る 3 つは出る: `auth_targets` は Hub / Creo が必ず入って空にならず、
    /// `processes` は空配列、`services` は **midi build なら** `devices` 1 件。
    ///
    /// ⚠️ `services` は **cfg 依存**。production の daemon ctor は `with_devices` で
    /// 無条件に `devices: Some(..)` を置く（`daemon/machine_capabilities.rs:82-95`）ので
    /// default build では必ず出る。`--no-default-features` では handler が `None` を返す
    /// （`health.rs:233-236`）。
    ///
    /// **「増えた」も「減った」も落とす**のが要点。field を足した PR は、ここを意識的に
    /// 更新することで「daemon の応答形を変えた」と宣言することになる。
    #[tokio::test]
    async fn daemon_health_key_set_is_pinned() {
        let body = daemon_health_body().await;
        let mut keys: Vec<&str> = body
            .as_object()
            .expect("body は JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();

        let mut expected = vec![
            "actions",
            "actions_rev",
            "auth_targets",
            "hub",
            "hub_nodes",
            "idle_timeout_minutes",
            "pid",
            "processes",
            "repo_dir",
            "started_at",
            "status",
            "update_available",
            "version",
        ];
        if cfg!(feature = "midi") {
            expected.push("services");
        }
        expected.sort_unstable();

        assert_eq!(keys, expected, "起動直後の daemon の key 集合");

        for omitted in ["terminal_token", "hub_auth", "latest_version"] {
            assert!(
                body.get(omitted).is_none(),
                "{omitted} は起動直後の daemon では省略される"
            );
        }
    }

    /// 起動直後の既定値を固定する。
    ///
    /// ⚠️ **ここは形の網であって、実体の網ではない。** 「空 cache だから 0 / 空」なので、
    /// PR-2c が**別の空実体**を渡しても全部緑のまま通る。実体の同一性は
    /// [`daemon_health_projects_the_given_instances`] が見る。
    #[tokio::test]
    async fn daemon_health_defaults_are_pinned() {
        let body = daemon_health_body().await;

        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            body["pid"].as_u64(),
            Some(u64::from(std::process::id())),
            "pid は自プロセス"
        );

        let started_at = body["started_at"].as_str().expect("started_at は文字列");
        assert!(
            chrono::DateTime::parse_from_rfc3339(started_at).is_ok(),
            "started_at は RFC3339: {started_at}"
        );

        // federation 未接続の daemon。`hub_auth` は Unknown = 空文字列で omit される。
        assert_eq!(body["hub"], "disabled");
        assert_eq!(body["hub_nodes"].as_array().map(Vec::len), Some(0));

        // `auth_target_states()` は Hub / Creo を必ず両方返す（= map は空にならず
        // 常に serialize される）。値は local の credential 次第なので domain だけ固定。
        // ⚠️ credential は `~/.vp/credentials.json`（`commands/auth.rs:272-281`）で
        //    **XDG zone ではない**ので、`test_env::StateDirGuard` を取っても隔離できない。
        //    取れば crate 唯一のロックで直列化されるコストだけが乗る。値ではなく
        //    domain を固定するのが正しい扱い。
        let auth = body["auth_targets"]
            .as_object()
            .expect("auth_targets は object");
        let mut targets: Vec<&str> = auth.keys().map(String::as_str).collect();
        targets.sort_unstable();
        assert_eq!(targets, ["creo", "hub"], "auth の宛先は 2 つで固定");
        for (target, state) in auth {
            let state = state.as_str().unwrap_or_default();
            assert!(
                matches!(state, "none" | "expired" | "valid"),
                "{target} の状態は 3 値のいずれか（実際: {state}）"
            );
        }

        // 空 capability なので `presence_snapshot()` は空 Vec。
        // **配列であること**（repo 役の omit と区別が付くこと）に意味がある。
        assert_eq!(
            body["processes"].as_array().map(Vec::len),
            Some(0),
            "daemon 形は必ず配列"
        );

        // update cache 未取得 = `(false, None)`。`latest_version` は omit 側。
        assert_eq!(body["update_available"], false);

        // ACTIONS: poller を回していないので空 + rev 0。**rev 0 が「未取得」の印そのもの**
        // なので、omit させずに常時 serialize することに意味がある（doc 57）。
        assert_eq!(body["actions"].as_array().map(Vec::len), Some(0));
        assert_eq!(body["actions_rev"].as_u64(), Some(0));

        // settings.kdl 由来なので machine ごとに違う。定数ではなく**供給元**に固定する。
        //
        // ⚠️ **これは「file を読み直していない」ことまでは見ていない。** handler と test が
        //    同じ `OnceLock` を読むので恒真。`pool.rs:60-64` が名指しで禁じている
        //    「`SettingsFile::load()` で読み直す」形に戻っても、test process 内では
        //    同じ数になって通る。塞ぐなら OnceLock の固着性（1 度読ませてから config を
        //    差し替えて**最初の値**が返ること）を見る別 test が要る。
        assert_eq!(
            body["idle_timeout_minutes"].as_u64(),
            Some(crate::repo::lane::idle_teardown_after_minutes()),
            "idle_timeout_minutes は idle_teardown_after_minutes() の投影"
        );
    }

    /// `repo_dir` は daemon 役では空文字列。
    ///
    /// ⚠️ **PR-2c でこの test は「まるごと削除」する。** doc 63 §3 が
    /// `HealthResponse.repo_dir` ごとの削除を承認しているので、これは緩めてよい唯一の
    /// assert。単独の test に出してあるのは、PR-2c の review で
    /// **「承認済みの削除」と「緩めた assert」を目視で区別できる**ようにするため
    /// （doc 63 §6「`repo_dir` 等の削除は『承認済み差分』として baseline と分ける」）。
    /// 合格条件は「この test が 1 本まるごと消え、他の assert は 1 文字も変わらない」。
    #[tokio::test]
    async fn daemon_health_carries_repo_dir() {
        let body = daemon_health_body().await;
        assert_eq!(
            body["repo_dir"], "",
            "daemon 役は repo を持たない（repo ctor だけが実 path を入れる、`repo/server.rs:210`）"
        );
    }

    /// **この PR の本体** — 各値が「組み立てで渡した実体」から来ていること。
    ///
    /// doc 63 §6:
    /// > 組み立てに渡した cache を**非初期値へ変更**し、HTTP が同じ値を返す。
    /// > ACTIONS は items と rev、hub は status / nodes / auth、update は available と
    /// > version、presence は代表例。`Arc` 同一性は補助
    ///
    /// なぜ既定値の固定では足りないか: PR-2c は 10 field の供給元を
    /// `AppState` から `DaemonState` へ移す。**移し先で新しい実体を作ってしまっても
    /// compile は通り、`-D warnings` も鳴らない**（doc 63 §6）。全部の cache が
    /// constructor 既定のままだと、新しい実体も同じ既定値を返すので応答は一致する。
    /// 非初期値へ動かして初めて「同じ実体か」を問える。
    ///
    /// `update` の cache は private field（`update_capability.rs:174`）で、Rust の privacy は
    /// **module 単位**なので同 module の test からしか代入できない。network を経ずに温める口
    /// （`seed_cached_release_for_test`）を capability 側に足して、ここも層 2 に載せてある。
    #[tokio::test]
    async fn daemon_health_projects_the_given_instances() {
        use crate::daemon::hub_client::{HubAuthState, HubFederationState, NodeEntry};

        let state = crate::repo::state::build_test_daemon_app_state().await;

        // ── 渡した実体を非初期値へ動かす ────────────────────────────────
        state.hub_status.set(HubFederationState::Connected);
        state.hub_auth.set(HubAuthState::Credentialed);
        state.hub_nodes.set(vec![NodeEntry {
            node_id: "node-1".to_string(),
            endpoints: vec!["[::1]:12879".to_string(), "127.0.0.1:12879".to_string()],
            handle: "@someone".to_string(),
            name: "someone".to_string(),
            registered_at: "2026-09-10T00:00:00Z".to_string(),
            connected: true,
        }]);
        let changed = state
            .creo_actions
            .set(vec![crate::creo::client::CreoAction {
                id: "act-1".to_string(),
                text: "棚卸し 9-2 を進める".to_string(),
                done: false,
                bucket: "today".to_string(),
                order: "a0".to_string(),
            }]);
        assert!(changed, "前提: 内容が変わったので rev が上がる");

        // update cache は private field なので capability 側の test 用の口から温める
        // （network も subprocess も踏まない）。
        state
            .update
            .as_ref()
            .expect("daemon 形は Some")
            .write()
            .await
            .seed_cached_release_for_test("999.0.0");

        // presence は `repos` を軸に map される（`repo_manager_capability.rs:344-366`）
        // ので、repo を 1 件差してから presence を付ける。
        {
            let cap = state
                .daemon
                .as_ref()
                .expect("daemon 形は Some")
                .read()
                .await;
            cap.repos_ref().write().await.insert(
                "/repos/vp".to_string(),
                crate::capability::RepoInfo {
                    name: "vp".to_string(),
                    path: std::path::PathBuf::from("/repos/vp"),
                    process_status: crate::capability::RepoStatus::Stopped,
                    port: None,
                    enabled: true,
                    slot: None,
                    active_lane: None,
                },
            );
            cap.set_presence("/repos/vp", crate::capability::RepoPresenceState::Connected)
                .await;
        }

        // ── 応答が同じ実体を映していること ──────────────────────────────
        let body = health_body_of(state).await;

        assert_eq!(body["hub"], "connected", "hub_status の投影");
        assert_eq!(
            body["hub_auth"], "credentialed",
            "hub_auth の投影（Unknown を脱したので key 自体も現れる）"
        );

        let nodes = body["hub_nodes"].as_array().expect("hub_nodes は配列");
        assert_eq!(nodes.len(), 1, "hub_nodes の投影");
        assert_eq!(nodes[0]["handle"], "@someone");
        assert_eq!(nodes[0]["node_id"], "node-1");
        assert_eq!(
            nodes[0]["endpoints_count"], 2,
            "endpoints は数だけを返す（`HubNodeInfo`）"
        );
        assert_eq!(nodes[0]["connected"], true);

        let actions = body["actions"].as_array().expect("actions は配列");
        assert_eq!(actions.len(), 1, "creo_actions の投影");
        assert_eq!(actions[0]["id"], "act-1");
        assert_eq!(
            body["actions_rev"].as_u64(),
            Some(1),
            "rev も同じ cache から来る（set で 0 → 1）"
        );

        let processes = body["processes"].as_array().expect("processes は配列");
        assert_eq!(processes.len(), 1, "daemon capability の presence 投影");
        assert_eq!(processes[0]["repo"], "vp");
        assert_eq!(processes[0]["presence"], "connected");

        assert_eq!(
            body["update_available"], true,
            "update capability の投影（既定は false なので別実体なら赤くなる）"
        );
        assert_eq!(
            body["latest_version"], "999.0.0",
            "既定では omit される key。値が出ること自体が実体の証拠"
        );
    }

    /// midi build では `services.devices` が出る。
    ///
    /// ⚠️ **これは「艦隊スイッチを agent から読む唯一の経路」**（doc 63 §7 /
    /// `tests/vp_daemon_kdl.rs:176`）。`devices/midi` を agent に露出しない設計判断は
    /// 「読み側は `/api/health` の `services.devices` で知れる」を根拠にしているので、
    /// PR-2c が `machine_capabilities` の結線を落とすとその根拠ごと消える。
    ///
    /// `midi_enabled` は state zone の `midi-switch.json` 由来（daemon 再起動をまたいで
    /// 保つ）なので machine 依存。値ではなく **status の domain** を固定する。
    #[cfg(feature = "midi")]
    #[tokio::test]
    async fn daemon_health_reports_devices_service() {
        let body = daemon_health_body().await;
        let devices = body["services"]["devices"]
            .as_object()
            .expect("midi build の daemon は services.devices を返す");
        let status = devices["status"].as_str().unwrap_or_default();
        assert!(
            matches!(status, "released" | "idle" | "active"),
            "status は 3 値のいずれか（実際: {status}）"
        );
        assert_eq!(
            devices["detail"]["devices"].as_u64(),
            Some(0),
            "hot-plug していない registry なので 0 台"
        );
    }

    /// `started_at` は **state が持つ値そのもの**で、health を何度叩いても動かない。
    ///
    /// doc 63 §7: PR-1 で `DaemonState.started_at` を `Instant` から `String` へ移すとき、
    /// `Utc::now() - elapsed()` で毎回再計算する形にすると sleep をまたいで wall clock と
    /// ずれる。health は 5s 周期で叩かれるので、**vp-app 側から起動時刻が動いて見える**。
    ///
    /// ⚠️ **2 回の応答を突き合わせるだけでは網として弱い。** 応答時刻を返す実装でも、
    /// 2 呼び出しが同じ μs に入れば偶然一致しうる（mutation で実測した差は 6 μs だった）。
    /// **供給元（`state.started_at`）と直接比べる**ことで、時間に依存せず「投影であること」
    /// を固定する。
    #[tokio::test]
    async fn daemon_health_started_at_is_the_states_value() {
        let state = crate::repo::state::build_test_daemon_app_state().await;
        let expected = state.started_at.clone();

        let first = health_body_of(state.clone()).await;
        let second = health_body_of(state).await;

        assert_eq!(
            first["started_at"], expected,
            "started_at は state の値の投影（再計算しない）"
        );
        assert_eq!(
            second["started_at"], expected,
            "2 回目も同じ — 構築時に 1 度確定して以後不変"
        );
    }

    #[tokio::test]
    async fn health_handler_returns_200_with_stands_field() {
        let state = crate::repo::state::build_test_app_state().await;
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
