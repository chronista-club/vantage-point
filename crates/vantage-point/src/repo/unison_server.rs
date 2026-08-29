//! Unison QUIC サーバー
//!
//! MCP <-> Process 間の高速通信レイヤー。
//! Axum HTTP サーバーと並行して起動し、同じ Hub.broadcast() パターンで
//! WebSocket クライアントにメッセージを配信する。
//!
//! ポート: HTTP と同一ポート番号を使う。 HTTP は TCP・QUIC は UDP で OS レベルの
//! ポート名前空間が独立しているため衝突しない (`QUIC_PORT_OFFSET = 0`)。
//!
//! "process" チャネルですべての操作を統一:
//! - show / clear / toggle_pane / split_pane / close_pane
//! - watch_file / unwatch_file

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::state::AppState;
use crate::protocol::{BoardItem, Content, RepoMessage};

/// QUIC ポートのオフセット（HTTP ポートからの差分）
/// TCP (HTTP) と UDP (QUIC) は OS レベルで独立 → 同一ポートで共存可能
pub const QUIC_PORT_OFFSET: u16 = 0;

/// UnwatchFile リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct UnwatchFileRequest {
    pane_id: String,
}

// =============================================================================
// Process チャネル ハンドラー
// =============================================================================

/// RepoMessage を受け取って broadcast + Msgbox 配信する汎用ハンドラー
///
/// MCP → QUIC → ここ の経路では、MCP が RepoMessage をそのままシリアライズして送る。
/// HTTP ハンドラ（health.rs の show_handler 等）と同じ RepoMessage 形式を受ける。
///
/// 配信先:
/// 1. Hub broadcast → WebSocket → Canvas（既存）
/// 2. Msgbox "protocol" → board Capability（VP-24）
fn handle_process_message(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let msg: RepoMessage = serde_json::from_value(payload.clone())
        .map_err(|e| format!("Invalid RepoMessage: {}", e))?;

    // 1. Hub broadcast → WebSocket → Canvas（既存経路）
    // TopicRouter が Hub ブリッジ経由で自動的に retained に保存するため、
    // 明示的なキャッシュは不要。Hub に broadcast するだけ。
    state.hub.broadcast(msg);

    // NOTE: Msgbox 経由の配信は ProtocolCapability 側の受信ループ実装後に追加（VP-24）
    // 現在は Hub broadcast のみで Canvas に配信。

    Ok(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// Editor bridge (doc 48 Phase 2) — MCP → GUI Editor Mode の request-response
// =============================================================================

/// GUI 応答待ちの上限。MCP 側 outer timeout (5s、`quic_call`) より短くすること
/// (VP-163: server が client より長く待つと channel reset → 空振りリトライになる)。
const EDITOR_BRIDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// MCP の editor_fields / editor_values / editor_set を GUI に転送して応答を待つ。
///
/// request_id を発行して `editor_pending` に oneshot を登録し、`EditorCommand` を
/// broadcast (canvas channel、非 retained event topic)。vp-app が webview で評価した
/// 結果を `editor_result` で返すと oneshot が解決する。timeout = GUI 不在 / 対象
/// repo 未表示 / Editor Mode 未 mount。
async fn handle_editor_command(
    state: &AppState,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let op = method.strip_prefix("editor_").unwrap_or(method).to_string();
    let field_id = payload.get("id").and_then(|v| v.as_str()).map(String::from);
    let value = payload.get("value").cloned();
    if op == "set" {
        if field_id.as_deref().unwrap_or("").is_empty() {
            return Err("editor_set: id 必須".to_string());
        }
        if value.is_none() {
            return Err("editor_set: value 必須".to_string());
        }
    }

    let request_id = crate::trace_log::new_trace_id();
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
    state
        .editor_pending
        .lock()
        .await
        .insert(request_id.clone(), tx);
    state.hub.broadcast(RepoMessage::EditorCommand {
        request_id: request_id.clone(),
        op,
        field_id,
        value,
    });

    match tokio::time::timeout(EDITOR_BRIDGE_TIMEOUT, rx).await {
        Ok(Ok(body)) => Ok(body),
        // timeout / sender drop: pending を掃除してから明示エラー
        // (残すと map が leak し、遅延応答が別 request に誤配されうる)
        _ => {
            state.editor_pending.lock().await.remove(&request_id);
            Err(
                "editor bridge timeout — vp-app が起動して当該 repo を表示しているか確認"
                    .to_string(),
            )
        }
    }
}

/// GUI (vp-app) からの editor 応答。request_id で pending oneshot を解決する。
///
/// 不在 key = timeout 済の stale 応答。エラーにせず無視する (idempotent) —
/// GUI 側は応答の成否で挙動を変えないため。
async fn handle_editor_result(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("editor_result: request_id 必須")?;
    let body = payload
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if let Some(tx) = state.editor_pending.lock().await.remove(request_id) {
        let _ = tx.send(body);
    }
    Ok(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// board モデル (2026-07-15): board Canvas を scope 別の永続 board にする server-authoritative 実装
//
// board = show した item の scope 別永続リスト（repo が唯一の truth を持つ）。 mcp__show 着信で repo が
// item を生成し DB に durable append、 更新後 board を BoardUpdated（retained topic
// `.../state/board/{scope}/{lane}`）で broadcast する。 webview はそれを購読して board を置換する view
// （旧 Show 揮発 stack / webview self-save は廃止）。 lane board は lane ごと、 proj board は repo 共有
// （lane_name=''）。 vp board（全体）は cross-project 共有で Daemon store が要るため Phase 2。
// =============================================================================

/// board の DB pane_id（webview の PP_PANE_ID と一致）。
const BOARD_PANE_ID: &str = "board";
/// board の item 上限（永続なので揮発 stack の 10 より大きく取る）。
const BOARD_CAPACITY: usize = 50;

/// board のキーを決める。 返り値 = (board_scope, lane_name, broadcast_lane)。
/// - proj board: lane を無視して repo 共有（lane_name=''、 broadcast_lane=None）。
/// - lane board: lane を main(空)/sub(名) に正規化。
fn board_key(scope: Option<&str>, lane: Option<&str>) -> (String, String, Option<String>) {
    if scope == Some("proj") {
        return ("proj".to_string(), String::new(), None);
    }
    // lane 正規化: None/""/予約名 → '' (開発起点 lane)。
    let lane_name = lane
        .filter(|s| !s.is_empty() && *s != crate::repo::lanes_state::ROOT_LANE_NAME)
        .unwrap_or("")
        .to_string();
    let broadcast_lane = if lane_name.is_empty() {
        None
    } else {
        Some(lane_name.clone())
    };
    ("lane".to_string(), lane_name, broadcast_lane)
}

/// protocol::Content を board item の (contentType, content) に変換する。
/// url / image は Phase 1 board 未対応（webview 側も未対応）なので None（= skip）。
fn content_to_parts(content: &Content) -> Option<(&'static str, String)> {
    match content {
        Content::Markdown(s) => Some(("markdown", s.clone())),
        Content::Html(s) => Some(("html", s.clone())),
        Content::Log(s) => Some(("text", s.clone())),
        Content::Url(_) | Content::ImageBase64 { .. } => None,
    }
}

/// mcp__update の content_type 文字列を board 保存形（stored contentType）に正規化する。
/// show の `content_to_parts` と対称: markdown/html はそのまま、log は text、url/image/未知は
/// None（board 非対応 → 呼び出し側が loud error）。update が show の許さない type を board に
/// 忍び込ませない（webview は markdown/html/text の 3 種のみ render する）。
fn normalize_board_content_type(ct: &str) -> Option<&'static str> {
    match ct {
        "markdown" => Some("markdown"),
        "html" => Some("html"),
        "log" | "text" => Some("text"),
        _ => None,
    }
}

/// board record の stack から items（Vec<BoardItem>）と cursor（Option<String>）を取り出す。
fn extract_stack(rec: Option<&serde_json::Value>) -> (Vec<BoardItem>, Option<String>) {
    let Some(rec) = rec else {
        return (Vec::new(), None);
    };
    let stack = rec.get("stack");
    let items = stack
        .and_then(|s| s.get("items"))
        .and_then(|v| serde_json::from_value::<Vec<BoardItem>>(v.clone()).ok())
        .unwrap_or_default();
    let cursor = stack
        .and_then(|s| s.get("cursor"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (items, cursor)
}

/// 指定 board を DB から読んで BoardUpdated で broadcast する（retained 更新 + live 配信）。
async fn broadcast_board(
    state: &AppState,
    board_scope: &str,
    lane_name: &str,
    broadcast_lane: Option<String>,
) -> Result<(), String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Ok(());
    };
    let rec = vpdb
        .load_board(&state.repo_dir, board_scope, lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board load: {}", e))?;
    let (items, cursor) = extract_stack(rec.as_ref());
    state.hub.broadcast(RepoMessage::BoardUpdated {
        scope: board_scope.to_string(),
        lane: broadcast_lane,
        items,
        cursor,
    });
    Ok(())
}

/// mcp__show / mcp__clear を board（repo truth）に反映する。
///
/// show: item を生成 → DB append（durable）→ 更新後 board を BoardUpdated で broadcast。
/// clear: DB clear → 空 board を broadcast。
async fn handle_canvas_command(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("canvas_command: vpdb 未初期化".to_string());
    };
    let msg: RepoMessage =
        serde_json::from_value(payload).map_err(|e| format!("Invalid RepoMessage: {}", e))?;
    match msg {
        RepoMessage::Show {
            content,
            title,
            lane,
            scope,
            ..
        } => {
            let Some((content_type, content_str)) = content_to_parts(&content) else {
                // url / image は Phase 1 board 未対応。 durable も broadcast もしない。
                return Ok(
                    serde_json::json!({"status": "skipped", "reason": "unsupported content"}),
                );
            };
            let (board_scope, lane_name, bc_lane) = board_key(scope.as_deref(), lane.as_deref());
            // 新規 item は updatedAt = createdAt（貼った瞬間が最終更新）。以後 update で stamp し直す
            // （doc 52 §5 — 鮮度の出力元は server の updatedAt 一箇所、額縁が読む）。
            let created_at = chrono::Utc::now().to_rfc3339();
            let item = serde_json::json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "content": content_str,
                "contentType": content_type,
                "title": title,
                "createdAt": created_at,
                "updatedAt": created_at,
            });
            vpdb.append_board_item(
                &state.repo_dir,
                &board_scope,
                &lane_name,
                BOARD_PANE_ID,
                &item,
                BOARD_CAPACITY,
            )
            .await
            .map_err(|e| format!("board append: {}", e))?;
            broadcast_board(state, &board_scope, &lane_name, bc_lane).await?;
            Ok(serde_json::json!({"status": "ok"}))
        }
        RepoMessage::Clear { lane, scope, .. } => {
            let (board_scope, lane_name, bc_lane) = board_key(scope.as_deref(), lane.as_deref());
            vpdb.clear_board(&state.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
                .await
                .map_err(|e| format!("board clear: {}", e))?;
            broadcast_board(state, &board_scope, &lane_name, bc_lane).await?;
            Ok(serde_json::json!({"status": "ok"}))
        }
        _ => Err("canvas_command: show/clear 以外のメッセージ".to_string()),
    }
}

/// webview からの board item 削除（thumbnail ✕）。 DB から消して更新後 board を broadcast。
async fn handle_board_delete_item(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("board_delete_item: vpdb 未初期化".to_string());
    };
    let item_id = payload
        .get("item_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("board_delete_item: item_id 必須")?
        .to_string();
    let (board_scope, lane_name, bc_lane) = board_key(
        payload.get("scope").and_then(|v| v.as_str()),
        payload.get("lane").and_then(|v| v.as_str()),
    );
    vpdb.delete_board_item(
        &state.repo_dir,
        &board_scope,
        &lane_name,
        BOARD_PANE_ID,
        &item_id,
    )
    .await
    .map_err(|e| format!("board delete: {}", e))?;
    broadcast_board(state, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// webview からの board clear（Clear ボタン）。 = mcp clear と同じ結果。
async fn handle_board_clear(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("board_clear: vpdb 未初期化".to_string());
    };
    let (board_scope, lane_name, bc_lane) = board_key(
        payload.get("scope").and_then(|v| v.as_str()),
        payload.get("lane").and_then(|v| v.as_str()),
    );
    vpdb.clear_board(&state.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board clear: {}", e))?;
    broadcast_board(state, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// mcp__update を board（repo truth）に反映する（doc 52 §5 — id 指定 in-place 置換）。
///
/// read-first 前提: id は AI が read_board で読んだ現在の item id。存在確認して**無ければ loud
/// error**（`show` 二挙動を避け `update` に分けた狙い = 静かな重複を作らない）。存在すれば
/// content / contentType を差し替え（id/title/createdAt は保持）→ 更新後 board を broadcast。
async fn handle_board_update(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("board_update: vpdb 未初期化".to_string());
    };
    let item_id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("board_update: id 必須")?
        .to_string();
    let content = payload
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or("board_update: content 必須")?
        .to_string();
    // content_type は **省略時 = 既存 item の type を保つ**（下で解決）。既定 "markdown" 直書きだと
    // html item を update しただけで markdown に silent 降格し、board-render.ts の trust 境界（html=sandbox
    // iframe / markdown=innerHTML）まで崩れる（team-b review 2026-07-24）。
    let content_type_arg = payload
        .get("content_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let (board_scope, lane_name, bc_lane) = board_key(
        payload.get("scope").and_then(|v| v.as_str()),
        payload.get("lane").and_then(|v| v.as_str()),
    );
    // read-first の loud error: 対象 lane の board に id が居ることを確認してから更新する。
    let rec = vpdb
        .load_board(&state.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board load: {}", e))?;
    let (items, _) = extract_stack(rec.as_ref());
    let Some(existing) = items.iter().find(|it| it.id == item_id) else {
        return Err(format!(
            "board_update: id '{}' が board に無い（read_board で現在の id を確認してください）",
            item_id
        ));
    };
    // content_type: 省略 = 既存 type を保つ / 指定 = show と同じ board-supported set に正規化
    // （url/image は board 非対応。show の content_to_parts と対称、divergence を作らない）。
    let content_type = match content_type_arg.as_deref() {
        None => existing.content_type.clone(),
        Some(ct) => normalize_board_content_type(ct)
            .ok_or_else(|| {
                format!(
                    "board_update: content_type '{}' は board 非対応（markdown / html / log のみ）",
                    ct
                )
            })?
            .to_string(),
    };
    vpdb.update_board_item(
        &state.repo_dir,
        &board_scope,
        &lane_name,
        BOARD_PANE_ID,
        &item_id,
        &content,
        &content_type,
    )
    .await
    .map_err(|e| format!("board update: {}", e))?;
    broadcast_board(state, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// webview からの cursor 移動（thumbnail click）を board（repo truth）に反映する（doc 52 §5 —
/// cursor の server 昇格。view-local だった cursor を repo-authoritative にし、scrollback 規則
/// = 「head を見ているときだけ新 show に follow」の判定を server が持てるようにする）。
///
/// read-first: item_id が board に居ることを確認してから set（無い id で cursor を迷子に
/// させない）。set 後の board を broadcast（cursor が真として全 view に配られる）。
async fn handle_board_set_cursor(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("board_set_cursor: vpdb 未初期化".to_string());
    };
    let item_id = payload
        .get("item_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("board_set_cursor: item_id 必須")?
        .to_string();
    let (board_scope, lane_name, bc_lane) = board_key(
        payload.get("scope").and_then(|v| v.as_str()),
        payload.get("lane").and_then(|v| v.as_str()),
    );
    let rec = vpdb
        .load_board(&state.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board load: {}", e))?;
    let (items, _) = extract_stack(rec.as_ref());
    if !items.iter().any(|it| it.id == item_id) {
        return Err(format!(
            "board_set_cursor: id '{}' が board に無い",
            item_id
        ));
    }
    vpdb.set_board_cursor(
        &state.repo_dir,
        &board_scope,
        &lane_name,
        BOARD_PANE_ID,
        &item_id,
    )
    .await
    .map_err(|e| format!("board set_cursor: {}", e))?;
    broadcast_board(state, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// mcp__read_board を処理する（doc 52 §4 中継台 + §5 identity 兼務）。
///
/// 呼び出し元 lane の board を **id 付き全文**で返す（AI は content/title で「どれか」を認識し、
/// id で update / creo 中継の対象を指す）。read-only（broadcast しない）。
async fn handle_board_read(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return Err("read_board: vpdb 未初期化".to_string());
    };
    let (board_scope, lane_name, _) = board_key(
        payload.get("scope").and_then(|v| v.as_str()),
        payload.get("lane").and_then(|v| v.as_str()),
    );
    let rec = vpdb
        .load_board(&state.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board load: {}", e))?;
    let (items, cursor) = extract_stack(rec.as_ref());
    Ok(serde_json::json!({ "items": items, "cursor": cursor }))
}

/// repo 起動時に DB の全 board を retained topic に seed する。
///
/// webview が canvas channel を購読した瞬間、 retained BoardUpdated として全 board が初期配信される
/// （別 load 経路が不要）。 空 board / 別 pane_id の row は skip。
pub async fn seed_boards(state: &AppState) {
    let Some(vpdb) = state.vpdb.as_ref() else {
        return;
    };
    let rows = match vpdb.list_pane_contents(&state.repo_dir).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("board seed: pane_contents list 失敗: {}", e);
            return;
        }
    };
    let mut seeded = 0usize;
    for rec in rows {
        if rec.get("pane_id").and_then(|v| v.as_str()) != Some(BOARD_PANE_ID) {
            continue;
        }
        let (items, cursor) = extract_stack(Some(&rec));
        if items.is_empty() {
            continue;
        }
        let scope = rec
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("lane")
            .to_string();
        let lane_name = rec.get("lane_name").and_then(|v| v.as_str()).unwrap_or("");
        let bc_lane = if lane_name.is_empty() {
            None
        } else {
            Some(lane_name.to_string())
        };
        state.hub.broadcast(RepoMessage::BoardUpdated {
            scope,
            lane: bc_lane,
            items,
            cursor,
        });
        seeded += 1;
    }
    if seeded > 0 {
        tracing::info!("board seed: {} board を retained に投入", seeded);
    }
}

/// watch_file メソッドのハンドラー
async fn handle_watch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let config: crate::file_watcher::WatchConfig = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid watch_file payload: {}", e))?;

    let pane_id = config.pane_id.clone();

    state
        .file_watchers
        .lock()
        .await
        .start_watch(config, state.hub.clone())
        .map_err(|e| format!("watch_file 開始失敗: {}", e))?;

    Ok(serde_json::json!({"status": "ok", "pane_id": pane_id}))
}

/// unwatch_file メソッドのハンドラー
async fn handle_unwatch_file(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: UnwatchFileRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid unwatch_file payload: {}", e))?;

    state.file_watchers.lock().await.stop_watch(&req.pane_id);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

// =============================================================================
// ProcessRunner ハンドラー
// =============================================================================

/// プロセス起動
async fn handle_process_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::repo::process_runner::RunParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    let process_id = crate::repo::process_runner::process_run(
        &state.process_registry,
        &params,
        &state.repo_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// プロセス停止
async fn handle_process_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::repo::process_runner::process_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// コード注入
async fn handle_process_inject(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::repo::process_runner::InjectParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    crate::repo::process_runner::process_inject(&state.process_registry, &params).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// プロセス一覧
async fn handle_process_list(state: &AppState) -> Result<serde_json::Value, String> {
    let processes = state.process_registry.lock().await.list();
    Ok(serde_json::json!({"status": "ok", "processes": processes}))
}

// L0 portless Group B-3: 旧 SP HTTP `/api/ruby/*` を repo-proxy ask に移管。 HTTP handler と同じ
// `process_runner::ruby_*` core を呼ぶ薄い adapter (payload からフィールド抽出)。 ruby_list は
// `process_registry.list()` = `handle_process_list` と同一なので dispatch 側で再利用する。

/// ruby_eval: 短命 Ruby 実行
async fn handle_ruby_eval(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let r =
        crate::repo::process_runner::ruby_eval(code, file, pane_id, &state.repo_dir, &state.hub)
            .await?;
    Ok(serde_json::json!({
        "status": "ok",
        "stdout": r.stdout,
        "stderr": r.stderr,
        "exit_code": r.exit_code,
        "elapsed_ms": r.elapsed_ms,
    }))
}

/// ruby_run: 長命 Ruby daemon 起動
async fn handle_ruby_run(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let name = payload.get("name").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let process_id = crate::repo::process_runner::ruby_run(
        &state.process_registry,
        code,
        file,
        name,
        pane_id,
        &state.repo_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// ruby_stop: Ruby daemon 停止
async fn handle_ruby_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::repo::process_runner::ruby_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// Terminal チャネル制御メッセージハンドラー
// =============================================================================

// =============================================================================
// サーバー起動
// =============================================================================

/// repo "process" channel の method dispatch（reverse-routing と共有する単一の入口）。
///
/// repo の "process" Unison channel handler と、 Daemon reverse-routing 経由 (repo control
/// keepalive) の **両方**がこの関数を呼ぶことで、 「MCP が repo 直結」「MCP → Daemon → repo
/// reverse」どちらの経路でも同一の dispatch ロジック・同一の AppState 操作になる
/// (L0 SP-portless: repo listen port を Daemon 単一 endpoint に寄せても挙動不変)。
/// S2 (doc 27 §4.1) → doc 53 R2: terminal demand start / stop の共通ハンドラー。
///
/// daemon の TopicRouter demand hook が `repo/terminal/data/{lane}/out` の購読者 0↔1 を
/// 検知して撃つ。旧実装は start / stop がそれぞれ「張る」「畳む」を実行していたが、
/// reconcile 化で両者は**同じ操作**になった — demand の今（購読者数の level）は
/// `TopicRouter::demand_active` が答えるので、 hook の向きは信じず契機としてだけ使う
/// （start 側は「購読者が現れたので pump を揃えろ」、 stop 側は「消えたので畳め」、
/// どちらも reconcile 1 呼びで正しい方に収束する）。
async fn handle_terminal_demand(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("terminal_demand: lane 未指定".to_string());
    }
    if crate::repo::lanes_state::LanePool::parse_address(&lane).is_none() {
        return Err(format!("terminal_demand: lane パース失敗: {}", lane));
    }
    // client が「画面を持っていない」と名乗った場合は replay を必ず流す
    // （JS ready 後の catch-up。webview 準備前に届いた replay は捨てられている）。
    let force_replay = payload
        .get("replay")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Lane 不在 / PtySlot 無でも受理（= Lane 起動後の再 demand 余地を残す）。
    let r = if force_replay {
        crate::repo::terminal_pump::reconcile_lane_pumps_forcing_replay(
            &state.lane_pool,
            &state.terminal_pumps,
            &state.topic_router,
            &lane,
        )
        .await
    } else {
        reconcile_terminal_pumps(state, &lane).await
    };
    Ok(serde_json::json!({
        "status": "reconciled", "lane": lane,
        "attached": r.attached, "removed": r.removed, "kept": r.kept,
    }))
}

/// [`crate::repo::terminal_pump::reconcile_lane_pumps`] の AppState 版（呼び手の糖衣）。
///
/// demand hook / 動詞の末尾（mode 切替・slot 追加・restart）/ boot 復元後 — pump に影響する
/// あらゆる契機がこの 1 本を呼ぶ。旧 `respawn_terminal_pump` の `only` 引数（呼び手ごとの
/// scope 判断）は廃止 — 「pid 一致は触らない」の照合が兄弟保護を構造で保証する。
pub(crate) async fn reconcile_terminal_pumps(
    state: &AppState,
    lane: &str,
) -> crate::repo::terminal_pump::PumpReconcile {
    crate::repo::terminal_pump::reconcile_lane_pumps(
        &state.lane_pool,
        &state.terminal_pumps,
        &state.topic_router,
        lane,
    )
    .await
}

/// [`crate::repo::lane_reconcile::reconcile_lane`] の AppState 版（呼び手の糖衣）。
///
/// **動詞の末尾はこれ 1 本**（doc 53 §12.4 / R3c）。registry に intent を書いた動詞は、
/// 実体（PtySlot / chat engine / 代表値 / pump）を自分で動かさずにこれを呼ぶ。
/// pump だけを合わせたい契機（demand hook）は [`reconcile_terminal_pumps`] のまま —
/// あちらは lane 全体の実体を触らない軽い経路。
pub(crate) async fn reconcile_lane(
    state: &AppState,
    addr: &crate::repo::lanes_state::LaneAddress,
) -> crate::repo::lane_reconcile::LaneReconcile {
    crate::repo::lane_reconcile::reconcile_lane(
        &state.lane_pool,
        &state.terminal_pumps,
        &state.topic_router,
        addr,
    )
    .await
}

/// gui replay-on-attach: conversation demand start ハンドラー。
///
/// daemon の demand hook が `repo/conversation/data/{lane}/event` の購読者 0→1 を検知し、 control
/// reverse-route で本 method を撃つ。 repo は当該 chat lane の **transcript を replay** して topic に
/// route する（`ReplayStart` + 過去会話の ConversationEvent 列）。
///
/// なぜ必要か: conversation topic は非 retained で、 会話履歴は vp-app の in-memory ring buffer に
/// しか無い。 app 再起動で ChatView が空になる（engine 側は `--resume` で会話を保持しているのに
/// 描く履歴が無い）。 唯一の履歴 SSOT である claude の transcript(jsonl) から起こし直す。
///
/// 冪等: 先頭の `ReplayStart` を見て GUI が会話表示をクリアするため、 reconnect / demand 再発火で
/// 二重化しない（terminal replay の clear-prefix と同型）。
///
/// **生成中に着地した場合**: claude は message を完了時にしか transcript へ flush しないので、
/// transcript だけでは生成中 message が欠ける（GUI は reset 済みなので、 復帰後の chunk が文の
/// 途中から新しいバブルを立ててしまう）。 そこで engine host の **in-flight tail** を transcript の
/// 後ろに継ぐ（`replay = transcript(commit 済み) ++ tail(未 commit)`、 `conversation::host` module doc）。
///
/// transcript を読んでいる最中に commit が挟まると tail と transcript が食い違う（欠落 or 二重化）。
/// commit 世代 `seq` を読み前後で検算し、 動いていたら読み直す。 収束しなければ tail を捨てて
/// commit 済み状態に収束させる（= 従来動作にフォールバック、 二重化より安全）。
///
/// chat mode でない lane / cc_session id 不明 / transcript 不在は「replay 無し」で graceful に返す
/// （console は live event を待つだけで壊れない）。
async fn handle_conversation_demand_start(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if lane.is_empty() {
        return Err("conversation_demand_start: lane 未指定".to_string());
    }
    let session = payload_session_key("conversation_demand_start", &payload)?;
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(&lane) else {
        return Err(format!(
            "conversation_demand_start: lane パース失敗: {lane}"
        ));
    };

    // chat でない session は replay しない（tui の履歴は PtySlot の terminal replay が担う）。
    //
    // ⚠️ gate は **その session の mode**（doc 50 §4.6 A6）。旧実装は lane 単位
    // `console_mode`（= root cache）で弾いていたため、**root=tui のまま非 root だけ chat** という
    // A6 の正規構成で `not_chat` を返し、ReplayStart すら送らずに会話が復元されなかった
    // （team-b review 2026-07-25、score 92。§4.7「共通形は lane 単位で判断している箇所」の同型）。
    // lane 不在は graceful に not_chat（従来挙動の温存 — Err にすると boot 窓で騒がしい）。
    // doc 53 §8.4: 旧実装は `console_mode(addr).is_none()` を「lane 実在」の signal に流用
    // していた（値は不使用）。存在 check を明示形にする。
    let resolved = {
        let pool = state.lane_pool.read().await;
        if !pool.contains(&addr) {
            return Ok(serde_json::json!({"status": "not_chat", "lane": lane}));
        }
        let resolved = pool
            .resolve_chat_session(&addr, session)
            .map_err(|e| format!("conversation_demand_start: {e}"))?;
        if resolved.mode != crate::lane::session_registry::SessionMode::Gui {
            return Ok(serde_json::json!({
                "status": "not_chat", "lane": lane, "session": resolved.key
            }));
        }
        resolved
    };

    // doc 38 Phase 3（focused eager）: attach = 会話を見に来た合図。当該 session の engine を
    // ここで eager に resume spawn する（doc 33 C1 の lazy「submit まで engine-less」からの転換 —
    // repo 再起動後も uplink 再接続 → demand 再発火でこの経路に入るため「前回状態キープ」が成立）。
    // ensure は冪等（既起動なら no-op）。失敗しても replay は続行し、engine は次 submit の
    // self-heal で再試行される。shell / legacy agent 等 gui host を持たない session は skip
    //（能力表 = EngineKind が SSOT。bail を warn で騒がせない）。
    if crate::conversation::EngineKind::from_agent(&resolved.agent)
        .is_some_and(crate::conversation::EngineKind::chat_capable)
        && let Err(e) =
            state
                .lane_pool
                .write()
                .await
                .ensure_chat_engine(&addr, session, &state.topic_router)
    {
        tracing::warn!(
            "conversation_demand_start: eager engine spawn 失敗（submit で再試行）: {e}"
        );
    }

    // single-flight 化（2026-07-27）: 起動時の 3 重 demand（daemon の購読 0→1 hook /
    // vp-app の subscribe 直後 / webview showLane）が並行に replay を route すると、
    // 「二重 replay は ReplayStart の clear-prefix で収束（無害）」の前提が破れて event が
    // 混線する（詳細は [`crate::repo::state::ReplayFlights`] の doc — 2026-07-27 fleetstage で
    // 実測）。進行中なら合流して rerun 予約のみ残す。ensure_chat_engine は guard の**外**
    //（demand ごとに冪等実行 = engine 復活の即時性は従来どおり）。
    if !state.replay_flights.begin(&lane, resolved.key) {
        tracing::info!(
            "conversation replay coalesced: 進行中 replay に合流 (lane={lane}, session={})",
            resolved.key
        );
        return Ok(serde_json::json!({
            "status": "coalesced", "lane": lane, "session": resolved.key
        }));
    }
    loop {
        let result = replay_once(state, &addr, &lane, &resolved).await;
        if result.is_err() {
            // エラー中断は予約ごと破棄（次の demand が新規 flight で素直に走れるように）。
            state.replay_flights.abort(&lane, resolved.key);
            return result;
        }
        if !state.replay_flights.finish(&lane, resolved.key) {
            return result;
        }
        // 合流分の rerun: 直列にもう 1 周だけ全量を配送し直す（連続配送なら clear-prefix で
        // 収束する = 元の設計前提に戻る）。進行中 replay の途中から購読した consumer の
        // prefix 取りこぼしもこの 1 周で回復する。`resolved` は初回 resolve の snapshot
        //（rerun 窓は ms 単位なので再 resolve しない）。
        tracing::info!(
            "conversation replay rerun: 合流した demand を直列に消化 (lane={lane}, session={})",
            resolved.key
        );
    }
}

/// replay 1 本分の配送（[`handle_conversation_demand_start`] の single-flight loop の中身）。
///
/// claude × 会話 id ありは transcript replay、それ以外は replay_log（codex 系）or 空を、
/// ReplayStart → 本文 → ReplayEnd の**連続 1 本**で route する。
async fn replay_once(
    state: &AppState,
    addr: &crate::repo::lanes_state::LaneAddress,
    lane: &str,
    resolved: &crate::repo::lanes_state::ResolvedSession,
) -> Result<serde_json::Value, String> {
    let lane_label = crate::repo::agent_spawner::lane_label(addr).to_string();
    let label = crate::lane::session_registry::session_label(&lane_label, resolved.key);
    // transcript replay は claude 専用（jsonl の SSOT を持つのは claude のみ）。会話 id は
    // registry が SSOT（doc 40 §5 reader #6 — resolve 時の registry load から持ち回った
    // `resolved.conversation`。旧 cc_session store 直読みは PR-2 で退役）。codex / grok /
    // opencode session は claude transcript を持たないため None に倒し、必ず下の no_session
    // path（replay_log）を通す。
    let session_id = match crate::conversation::EngineKind::from_agent(&resolved.agent) {
        Some(crate::conversation::EngineKind::Claude) => resolved.conversation.clone(),
        _ => None,
    };
    let Some(session_id) = session_id else {
        // transcript を持たない engine（codex / grok / opencode）は、repo が pump tap で per-session に
        // 記録した replay log を replay 源にする（engine 非依存 replay log。判定は lanes_state の
        // replay_tap と同じ Codex|Grok|OpenCode）。それ以外（claude で会話未開始 等）は log を読まず
        // 空 chat に収束させる。
        let buffered = if matches!(
            crate::conversation::EngineKind::from_agent(&resolved.agent),
            Some(
                crate::conversation::EngineKind::Codex
                    | crate::conversation::EngineKind::Grok
                    | crate::conversation::EngineKind::OpenCode
                    | crate::conversation::EngineKind::Vpcode
            )
        ) {
            crate::conversation::replay_log::load(&addr.repo, &label)
        } else {
            Vec::new()
        };
        // ReplayStart で GUI を clear → buffered を fold → ReplayEnd で streaming を下ろす。
        // log が空なら従来と同じ「ReplayStart + ReplayEnd」= 空 chat（後方互換）。turn-scoped host
        // は attach 時点で生成中 turn を持たないため in_flight=false。
        let count = buffered.len();
        let mut events = Vec::with_capacity(count + 2);
        events.push(crate::conversation::ConversationEvent::ReplayStart);
        events.extend(buffered);
        events.push(crate::conversation::ConversationEvent::ReplayEnd { in_flight: false });
        splice_session_init(
            &mut events,
            state
                .lane_pool
                .read()
                .await
                .chat_session_init(addr, Some(resolved.key)),
        );
        route_conversation(state, lane, resolved.key, events).await;
        tracing::info!(
            "conversation replay-log: {count} events を配送 (lane={lane}, session={})",
            resolved.key
        );
        return Ok(serde_json::json!({
            "status": "no_session", "lane": lane, "session": resolved.key, "events": count
        }));
    };

    let (mut events, tail_len) =
        replay_with_in_flight(state, addr, resolved.key, &session_id).await?;
    // replay 終端で streaming の真値を宣言する。 replay は過去の assistant 発話も MessageChunk で
    // 送るため GUI 側で streaming が立つが、 replay 列は TurnCompleted を運ばない。 生成中 turn が
    // 無ければ（tail_len == 0）ここで下ろさないと、 engine が idle でも「応答中」が永久に残り、
    // turn 完了契機の処理（type-ahead flush 等）が二度と発火しなくなる。
    events.push(crate::conversation::ConversationEvent::ReplayEnd {
        in_flight: tail_len > 0,
    });
    splice_session_init(
        &mut events,
        state
            .lane_pool
            .read()
            .await
            .chat_session_init(addr, Some(resolved.key)),
    );

    let count = events.len();
    route_conversation(state, lane, resolved.key, events).await;
    tracing::info!(
        "conversation transcript replay: {count} events を配送 (lane={lane}, session={}, in-flight tail={tail_len})",
        resolved.key
    );
    Ok(serde_json::json!({
        "status": "replayed", "lane": lane, "session": resolved.key,
        "events": count, "in_flight": tail_len
    }))
}

/// 保持していた `SessionInit` を **`ReplayStart` の直後**へ差し込む。
///
/// ⚠️ `SessionInit` は engine の起動 / resume で**一度きり**流れ、transcript にも載らない。
/// GUI を開き直すと webview の state は作り直されるので、配り直さないと model / permission mode /
/// slash_commands が二度と復元されない（2026-08-07: slash command palette が空のままで発覚）。
///
/// `ReplayStart` の**後**に置くのは、GUI があれを見て会話表示をクリアするから — 前に置くと
/// 直後に消される。先頭が `ReplayStart` でない形（将来の変更）でも壊れないよう位置は実測で決める。
fn splice_session_init(
    events: &mut Vec<crate::conversation::ConversationEvent>,
    init: Option<crate::conversation::ConversationEvent>,
) {
    let Some(init) = init else { return };
    let at = usize::from(matches!(
        events.first(),
        Some(crate::conversation::ConversationEvent::ReplayStart)
    ));
    events.insert(at, init);
}

/// transcript 読み + in-flight tail の結合を、 commit 世代 `seq` で検算しながら行う。
///
/// 戻り値は `(replay 列, 継いだ tail の長さ)`。 tail 長 0 は「生成中でない」か「収束せず捨てた」。
async fn replay_with_in_flight(
    state: &AppState,
    addr: &crate::repo::lanes_state::LaneAddress,
    session: crate::lane::session_registry::SessionKey,
    session_id: &str,
) -> Result<(Vec<crate::conversation::ConversationEvent>, usize), String> {
    /// commit が挟まったときの読み直し回数。 commit 間隔（数百 ms 〜 秒）に対し transcript 読みは
    /// 数 ms なので、 実運用では 1 回目で収束する。
    const MAX_ATTEMPTS: usize = 3;

    for _ in 0..MAX_ATTEMPTS {
        // 先に tail を取る。 「tail → transcript」の順なら、 間に commit が挟まっても
        // transcript 側が新しい = 情報の欠落は起きない（二重化は seq 検算で弾く）。
        let before = state
            .lane_pool
            .read()
            .await
            .chat_in_flight(addr, Some(session));

        // disk read + 翻訳は同期 I/O（数 MB / 数千行）。 tokio worker を塞がないよう隔離する。
        let sid = session_id.to_string();
        let mut events = tokio::task::spawn_blocking(move || {
            crate::conversation::transcript::replay_events(&sid)
        })
        .await
        .map_err(|e| format!("conversation_demand_start: transcript 変換 join 失敗: {e}"))?;

        let after_seq = state
            .lane_pool
            .read()
            .await
            .chat_commit_seq(addr, Some(session));
        let Some(in_flight) = before else {
            // engine 未起動（chat-idle / 再起動直後）= 継ぐ tail が無い。 transcript がすべて。
            return Ok((events, 0));
        };
        if after_seq != Some(in_flight.seq) {
            // 読んでいる間に message が commit された（or engine が入れ替わった）。
            // tail が古い可能性があるので読み直す。
            continue;
        }
        let tail_len = in_flight.tail.len();
        events.extend(in_flight.tail);
        return Ok((events, tail_len));
    }

    // 収束せず（生成が極端に速い / engine が入れ替わり続ける）。 tail を捨て、 commit 済み状態に
    // 収束させる。 欠けた生成中 message は次の attach cycle で復元される。
    tracing::warn!(
        "conversation replay: commit 世代が {MAX_ATTEMPTS} 回連続で動いたため in-flight tail を破棄 (lane={addr})"
    );
    let sid = session_id.to_string();
    let events =
        tokio::task::spawn_blocking(move || crate::conversation::transcript::replay_events(&sid))
            .await
            .map_err(|e| format!("conversation_demand_start: transcript 変換 join 失敗: {e}"))?;
    Ok((events, 0))
}

/// conversation demand stop ハンドラー = **idle teardown の即時契機**。
///
/// 購読が 0 になった（= 名簿から lane タイトルが消えた）時に、その lane の**暇な** chat engine
/// を寝かせる（判定は [`LanePool::drop_idle_chat_engines`] — turn なし + N 分無活動）。
/// 畳んだ**後**に条件が揃う場合は 30s periodic の `spawn_idle_engine_sweep` が拾う
/// （demand hook は購読が切れた瞬間の 1 回きりなので、片方だけでは落ちない）。
///
/// ⚠️ 旧実装は「replay は on-attach の一度きりなので停止対象の task は無い」として noop
/// だった（#699）。当時は正しく、engine を寝かせるという発想が無かっただけ — 穴ではなく
/// 未踏の設計余地だったので、前提が変わった 2026-08-29 に埋めた。
async fn handle_conversation_demand_stop(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload
        .get("lane")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(&lane) else {
        // 宛先が読めない = 落とす相手が決まらない。黙って何もしない（旧 noop と同じ安全側）。
        return Ok(serde_json::json!({"status": "noop", "lane": lane}));
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let dropped = state
        .lane_pool
        .write()
        .await
        .drop_idle_chat_engines(&addr, now_ms);
    if dropped.is_empty() {
        return Ok(serde_json::json!({"status": "kept", "lane": lane}));
    }
    tracing::info!(
        "idle chat engine を寝かせた（購読なし・turn なし・{}分無活動）: lane={lane} sessions={dropped:?}",
        crate::repo::lanes_state::idle_teardown_after_minutes(),
    );
    // 実体が変わったので roster を配る（`pid` / 活動時刻が動く = 名簿の見え方が変わる）。
    crate::repo::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "slept", "lane": lane, "sessions": dropped}))
}

/// ConversationEvent 列を per-lane conversation topic に順に route する（conversation_pump と同じ経路）。
/// `session` は発生元 session の key（doc 38 — topic は per-lane のまま、session は field で運ぶ）。
async fn route_conversation(
    state: &AppState,
    lane: &str,
    session: crate::lane::session_registry::SessionKey,
    events: Vec<crate::conversation::ConversationEvent>,
) {
    for event in events {
        state
            .topic_router
            .route(crate::protocol::RepoMessage::ConversationEvent {
                lane: lane.to_string(),
                session,
                event,
            })
            .await;
    }
}

/// payload の additive な session key（doc 38 / doc 46 P5）。省略 / null = `None`。
///
/// 型不正・0 は Err — 黙って既定に落とすと「指定したつもりの session と別の会話に届く」
/// 誤配送になるため、明示エラーで返す。
///
/// ⚠️ **`None` の解決先は経路で違う**（型が同じなので取り違えやすい）:
/// - chat 系（`conversation_*`）= **focused**（[`LanePool::resolve_chat_session`]）
/// - slot 系（`terminal_*` / `lane_capture` / `lane_nudge`）= **root**
///   （[`LanePool::slot_session`] — slot は lane の設備で、代表は root。doc 39「座と化身」）
/// - 会話報告（`lane_session_changed`）= **root だが「不明」として運ぶ**
///   （[`crate::lane::session_registry::ReportTarget::Unspecified`] — 着地先は root でも、
///   「名乗らなかった」という事実を registry まで届ける。root に丸めてから渡すと、実在しない
///   session の報告も root 宛と見分けが付かなくなる。doc 40 §4）
fn payload_session_key(
    ctx: &str,
    payload: &serde_json::Value,
) -> Result<Option<crate::lane::session_registry::SessionKey>, String> {
    match payload.get("session") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            let n = v
                .as_u64()
                .filter(|n| (1..=u64::from(u32::MAX)).contains(n))
                .ok_or_else(|| format!("{ctx}: session が不正（1 以上の整数）: {v}"))?;
            Ok(Some(n as u32))
        }
    }
}

/// S3 (doc 27 §4.1, 経路 B): terminal 入力。
///
/// surface (vp-app) → daemon canvas channel (upstream request) → repo control → 本 dispatch。
/// `data` は base64 (出力 pump の encoding と対称、 任意バイトを JSON で運ぶため)。 decode して
/// 当該 slot の PtySlot に書き込む (`session` 省略 = root、doc 46 P5)。
async fn handle_terminal_write(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use base64::Engine;
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_write: lane 未指定".to_string());
    }
    let data_b64 = payload.get("data").and_then(|v| v.as_str()).unwrap_or("");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| format!("terminal_write: base64 decode 失敗: {}", e))?;
    vp_paths::term_trace("B:repo-recv", lane, &bytes);
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_write: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .write_to_lane(
            &addr,
            payload_session_key("terminal_write", &payload)?,
            &bytes,
        )
        .map_err(|e| format!("terminal_write 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// gui (doc 33): conversation プロンプト投入。
///
/// surface (vp-app) → daemon canvas channel → repo control → 本 dispatch。
/// **mode=chat が前提**（法: 1 lane 高々 1 エンジン。tui のまま submit は Err で弾き、
/// 生きた TUI を暗黙に殺さない）。engine は LanePool が lazy spawn（初回のみ）し、
/// ConversationEvent は conversation_pump 経由で `repo/conversation/data/{lane}/event` に流れる。
async fn handle_conversation_submit(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_submit: lane 未指定".to_string());
    }
    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if prompt.is_empty() {
        return Err("conversation_submit: prompt 未指定".to_string());
    }
    let session = payload_session_key("conversation_submit", &payload)?;
    ensure_and_submit_chat(state, "conversation_submit", lane, session, prompt).await?;
    // user 発話は pump に流れない（GUI が optimistic bubble を出す設計）ので、transcript を持たない
    // engine の session は replay 源に user turn が残らない。submit 成功後にここで記録する。
    // ⚠️ nudge（下）では書かない — claude の transcript replay が origin.kind=="human" で VP 注入を
    // 間引くのと同じ規律。harness 注入（wire delivery / delegation）は会話として再生しない対称性。
    record_user_message_if_transcriptless(state, lane, session, prompt).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// transcript を持たない engine（codex / grok / opencode）の session に、user 発話を replay log へ記録する。
///
/// claude は transcript が SSOT なので記録しない（二重化回避）。engine 解決に失敗しても submit は
/// 既に成立済みなので warn に留める（配送と replay 記録は独立系統）。tap（pump）が assistant 側を
/// 書くのと対になり、replay で user ⇄ assistant のターンが揃う。
async fn record_user_message_if_transcriptless(
    state: &AppState,
    lane: &str,
    session: Option<crate::lane::session_registry::SessionKey>,
    prompt: &str,
) {
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return;
    };
    let resolved = {
        let pool = state.lane_pool.read().await;
        pool.resolve_chat_session(&addr, session)
    };
    let Ok(resolved) = resolved else {
        return;
    };
    // 記録対象は transcript を持たない engine のみ（tap と同じ Codex|Grok|OpenCode 判定）。
    if !matches!(
        crate::conversation::EngineKind::from_agent(&resolved.agent),
        Some(
            crate::conversation::EngineKind::Codex
                | crate::conversation::EngineKind::Grok
                | crate::conversation::EngineKind::OpenCode
                | crate::conversation::EngineKind::Vpcode
        )
    ) {
        return;
    }
    let lane_label = crate::repo::agent_spawner::lane_label(&addr).to_string();
    let label = crate::lane::session_registry::session_label(&lane_label, resolved.key);
    let event = crate::conversation::ConversationEvent::UserMessage {
        text: prompt.to_string(),
    };
    if let Err(e) = crate::conversation::replay_log::append(&addr.repo, &label, &event) {
        tracing::warn!(
            "conversation replay-log: user 発話の記録に失敗（lane={lane}, session={}）: {e}",
            resolved.key
        );
    }
}

/// channel E（doc 34 §3）: wire delivery / delegation reconcile からの engine 直接注入。
///
/// `{lane, text}` — Tui の `lane_nudge`（PtySlot 直書き）の Chat 対応物。nudge 文言を 1 ターン
/// として submit する。turn 実行中でも engine 側が queue するため任意時点で呼べる
/// （doc 34 Step 0 spike ①実測）。
async fn handle_conversation_nudge(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_nudge: lane 未指定".to_string());
    }
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return Err("conversation_nudge: text 未指定".to_string());
    }
    // doc 39 §3-1: wire 配送は常に **root**（lane の人格）に解決する。lane 宛の nudge を
    // focused に注入すると「gui で別タブを見ている」だけで配送先が変わる誤配送になる
    // （N=1 では root=focused=1 で従来と同一挙動）。lane パース失敗は session=None のまま
    // ensure_and_submit_chat 側の同じパースが報告する（エラー文言の一元化）。
    let session = crate::repo::lanes_state::LanePool::parse_address(lane).map(|addr| {
        crate::lane::session_registry::root(
            &addr.repo,
            crate::repo::agent_spawner::lane_label(&addr),
        )
    });
    ensure_and_submit_chat(state, "conversation_nudge", lane, session, text).await?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// gui HITL (doc 35 PR1): PromptCard の回答を逆方向 `can_use_tool` へ書き戻す。
///
/// surface (vp-app) → daemon canvas channel → repo control → 本 dispatch。`request_id` は Question
/// event 由来の control_response マッチング用。allow は `{lane, request_id, answers}`、deny は
/// `{lane, request_id, behavior:"deny", message?}`。**ensure しない**（応答対象 engine 不在は Err —
/// 質問した engine が死んでいたら応答先が無い、doc §2.3）。
async fn handle_conversation_respond(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_respond: lane 未指定".to_string());
    }
    let request_id = payload
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if request_id.is_empty() {
        return Err("conversation_respond: request_id 未指定".to_string());
    }
    // behavior=="deny" のみ拒否、それ以外（既定 / "allow"）は許可 + answers を運ぶ。
    let decision = if payload.get("behavior").and_then(|v| v.as_str()) == Some("deny") {
        crate::conversation::PermissionDecision::Deny {
            message: payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }
    } else {
        crate::conversation::PermissionDecision::Allow {
            answers: payload.get("answers").cloned(),
        }
    };

    let session = payload_session_key("conversation_respond", &payload)?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_respond: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .respond_permission_chat(&addr, session, request_id, decision)
        .await
        .map_err(|e| format!("conversation_respond: {e}"))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// doc 35 §5: 実行中 turn の中断（stop ボタン / Esc）。`{lane}` → `LanePool::interrupt_chat`。
/// engine は turn を止めるだけでプロセスは生存し、次の submit を受けられる。
async fn handle_conversation_interrupt(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_interrupt: lane 未指定".to_string());
    }
    let session = payload_session_key("conversation_interrupt", &payload)?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_interrupt: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .interrupt_chat(&addr, session)
        .await
        .map_err(|e| format!("conversation_interrupt: {e}"))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// doc 35 §2.5 / PR3: permission mode の動的切替。`{lane, mode}` → LanePool::set_permission_mode_chat。
async fn handle_conversation_set_permission_mode(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_set_permission_mode: lane 未指定".to_string());
    }
    let mode = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    if mode.is_empty() {
        return Err("conversation_set_permission_mode: mode 未指定".to_string());
    }
    let session = payload_session_key("conversation_set_permission_mode", &payload)?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_set_permission_mode: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .set_permission_mode_chat(&addr, session, mode)
        .await
        .map_err(|e| format!("conversation_set_permission_mode: {e}"))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane}))
}

/// doc 38: lane の session 一覧（registry + engine 生死 + 会話 id の view）。
/// `{lane}` → `{lane, focused, sessions: [{key, agent, engine_session_id?, live, focused}]}`。
/// Phase 2 の tab strip はこれを描くだけ（UI は state を持たない）。
async fn handle_conversation_session_list(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_list: lane 未指定".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_list: lane パース失敗: {lane}"))?;
    let sessions = state
        .lane_pool
        .read()
        .await
        .list_chat_sessions(&addr)
        .map_err(|e| format!("conversation_session_list: {e}"))?;
    let focused = sessions.iter().find(|s| s.focused).map(|s| s.key);
    Ok(serde_json::json!({"lane": lane, "focused": focused, "sessions": sessions}))
}

/// doc 38: session を追加する（Phase 2 の chat header「+」の backend）。
/// `{lane, agent?, focus?}` → `{lane, session}`。agent 省略 = lane の agent、focus 省略 = true
/// （「+」で作った session にそのまま話しかける UX が既定）。engine は spawn しない（Draft）。
async fn handle_conversation_session_create(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_create: lane 未指定".to_string());
    }
    let agent = payload.get("agent").and_then(|v| v.as_str());
    let focus = payload
        .get("focus")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_create: lane パース失敗: {lane}"))?;
    let key = state
        .lane_pool
        .read()
        .await
        .create_chat_session(&addr, agent, focus)
        .map_err(|e| format!("conversation_session_create: {e}"))?;
    // doc 53 §12.4 R3c: 動詞は registry に書いた。実体を合わせるのは reconcile。
    // Chat の engine は lazy なので普通は no-op — それでも呼ぶのは「**契機は判断を持たない**」
    // ため（「今回は要らない」を動詞ごとに判断し始めると、要る場合を 1 つ取りこぼす）。
    reconcile_lane(state, &addr).await;
    // doc 53 §11: roster が変わったので知らせる（GUI の pane 一覧は snapshot 1 本で供給される）。
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": key}))
}

/// doc 38: focused session の切替。`{lane, session}`。registry 永続のみ（slot への注入 /
/// eager resume spawn は Phase 3 の attach 状態機械で束ねて実装）。
async fn handle_conversation_session_focus(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_focus: lane 未指定".to_string());
    }
    let session = payload_session_key("conversation_session_focus", &payload)?
        .ok_or_else(|| "conversation_session_focus: session 未指定".to_string())?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_focus: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .focus_chat_session(&addr, session)
        .map_err(|e| format!("conversation_session_focus: {e}"))?;
    // doc 53 §12.4 R3c: focused は intent の一部（registry）。実体側は reconcile。
    reconcile_lane(state, &addr).await;
    {
        // doc 38 Phase 3（focused eager）: tab 切替 = その会話を見る宣言。新 focused の engine を
        // eager に resume spawn する（切替後の初 submit を待たない）。mode=Tui の session（registry のみの
        // 切替 = 正当）/ shell・legacy agent session（gui host なし）等は debug で飲む — 切替自体は成功。
        //
        // reconcile の**後**に置く: reconcile は「mode=Chat でない session の engine」を畳むので、
        // 先に起こすと同じ lock 区間の外で畳まれ得る。順序は「intent を合わせる → 注視に応じて
        // 起こす」（engine の eager は demand 側の判断 = reconcile の仕事ではない）。
        let mut pool = state.lane_pool.write().await;
        if let Err(e) = pool.ensure_chat_engine(&addr, Some(session), &state.topic_router) {
            tracing::debug!("conversation_session_focus: eager spawn せず（{e}）");
        }
    }
    // doc 53 §11: focused も roster の一部（chip / tab の点灯先）なので知らせる。
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session}))
}

/// doc 38 Phase 3: session を取り除く（tab を閉じる）。`{lane, session}` →
/// `{lane, session, focused}`（focused = 除去後の focus 先。GUI は list 再取得で追随）。
/// root は registry が拒否（doc 39 §6 — 最後の 1 本の拒否を包含。GUI も root タブの × を
/// 隠す = 多重防御）。lane を素に戻すのは Reset lane（fresh restart）の役目。
async fn handle_conversation_session_remove(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_remove: lane 未指定".to_string());
    }
    let session = payload_session_key("conversation_session_remove", &payload)?
        .ok_or_else(|| "conversation_session_remove: session 未指定".to_string())?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_remove: lane パース失敗: {lane}"))?;
    let focused = state
        .lane_pool
        .read()
        .await
        .remove_session(&addr, session)
        .map_err(|e| format!("conversation_session_remove: {e}"))?;
    // doc 53 §12.4 R3c: registry から消えた session の実体（PtySlot / chat engine）は
    // reconcile が畳む。旧実装は動詞が種類ごとに手で畳んでいて、A6 で term pane に ✕ が
    // 出たとき **chat 側だけ畳んで PTY が孤児**になるバグを出した（doc 50 §4.6）。
    reconcile_lane(state, &addr).await;
    // ⚠️ replay の破棄は reconcile の**後**。slot が生きている間に消すと `PtySlot::drop` の
    // 最終 flush が書き戻して復活する（`restart_lane` の Reset 分岐が踏んだのと同じ罠）。
    state
        .lane_pool
        .read()
        .await
        .discard_session_traces(&addr, session);
    // doc 53 §11: session が 1 本消えた = roster の変化。
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session, "focused": focused}))
}

/// doc 39 §4: tui の ✨ New — 新 session を作って root をそれへ向ける。
/// `{lane}` → `{lane, session}`。
///
/// R3c-2: 動詞は registry に書くだけで、新 root の実体は reconcile が立てる。
/// **旧 root の pane はそのまま残る** — 旧実装は `restart_lane_orchestrated` で root slot を
/// 張り替えていたので、代表が変わるたびに前の console が消えていた（doc 53 §12.4）。
///
/// ⚠️ **現在 client からの呼び手は無い**（doc 50 §4.6 A6）。picker の「✨ 新 ID から」は
/// 「Add（Conversation を足す）+ Reborn（その場で始め直す）」の合成でしかないため撤去した。
/// この verb は **Reborn の server 側の種**として残す — Reborn は「今の session を終えて
/// 新しい session を同じ場所（Pane）で始める」操作で、root pane に適用したときの挙動が
/// ちょうどこれ（旧 root を残すか閉じるかは Reborn の設計で決める）。
/// A6 で lane 単位 mode の gate（旧「mode=Tui 限定」）は撤去済。
async fn handle_conversation_session_new_root(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_new_root: lane 未指定".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_new_root: lane パース失敗: {lane}"))?;
    let key = state
        .lane_pool
        .read()
        .await
        .create_root_session(&addr, payload.get("agent").and_then(|v| v.as_str()))
        .map_err(|e| format!("conversation_session_new_root: {e}"))?;
    // doc 53 §12.4 R3c-2: **旧 root の console は残る**。旧実装は restart_lane_orchestrated で
    // root slot を張り替えていたので、代表が変わるたびに前の pane が消えていた — session =
    // Pane（doc 50）の今、代表の変更は pane の破棄ではない。reconcile は新 root の実体を
    // 足すだけ（新 root は会話 id を持たないので bare で立つ）。
    reconcile_lane(state, &addr).await;
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": key}))
}

/// doc 39 P3: Root 切替 picker — root（誰が lane の代表か）を既存 session へ向け替える
/// （`{lane, session}` → `{lane, session}`）。
///
/// R3c-2: **実体には何も起きない**のが正しい — 対象 session の pane は既に在るので、
/// reconcile から見て desired は変わらない（doc 53 §12.4）。旧実装は
/// `restart_lane_orchestrated` で root slot を対象 session の会話に張り替えていた =
/// 代表の変更を化身の置き換えと混同していた。
///
/// lane 単位 mode の gate は A6 で撤去（残る制限は既知 engine のみ — `switch_root_session` 参照）。
async fn handle_conversation_session_switch_root(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_session_switch_root: lane 未指定".to_string());
    }
    let key = payload
        .get("session")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "conversation_session_switch_root: session 未指定".to_string())?
        as crate::lane::session_registry::SessionKey;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_session_switch_root: lane パース失敗: {lane}"))?;
    state
        .lane_pool
        .read()
        .await
        .switch_root_session(&addr, key)
        .map_err(|e| format!("conversation_session_switch_root: {e}"))?;
    // doc 53 §12.4 R3c-2: 対象 session の pane は**既に在る**ので、reconcile から見て
    // desired は変わらない = 実体には何も起きないのが正しい（旧実装は root slot を対象 session の
    // 会話で張り替えていた = 代表の変更を化身の置き換えと混同していた）。呼ぶのは
    // 「契機は判断を持たない」の規律と、代表値（pid / state）の導出をやり直すため。
    reconcile_lane(state, &addr).await;
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": key}))
}

/// ensure（mode ガード + lazy spawn）→ submit（+ engine 死亡時 1 回の self-heal retry）の共通核。
///
/// `conversation_submit`（GUI 入力）と `conversation_nudge`（channel E）が共用する。`ctx` はエラー文言の
/// 前置き（呼び出し元 method 名 — 嘘ログ防止のため呼び元を正しく名乗る）。
async fn ensure_and_submit_chat(
    state: &AppState,
    ctx: &str,
    lane: &str,
    session: Option<crate::lane::session_registry::SessionKey>,
    prompt: &str,
) -> Result<(), String> {
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("{ctx}: lane パース失敗: {lane}"))?;

    // ensure（mode ガード + lazy spawn は LanePool = 法の番人が行う）。session=None は focused。
    state
        .lane_pool
        .write()
        .await
        .ensure_chat_engine(&addr, session, &state.topic_router)
        .map_err(|e| format!("{ctx}: {e}"))?;

    // submit（read lock — 他 lane の操作をブロックしない）。
    let submit_result = state
        .lane_pool
        .read()
        .await
        .submit_chat(&addr, session, prompt)
        .await;
    if let Err(e) = submit_result {
        // self-heal: engine が死んでいた場合は当該 session だけ落として 1 回だけ張り直す。
        tracing::warn!("{ctx} 失敗 → engine 再起動して retry: {e}");
        {
            let mut pool = state.lane_pool.write().await;
            pool.drop_chat_engine(&addr, session);
            pool.ensure_chat_engine(&addr, session, &state.topic_router)
                .map_err(|e| format!("{ctx}: engine 再起動失敗: {e}"))?;
        }
        state
            .lane_pool
            .read()
            .await
            .submit_chat(&addr, session, prompt)
            .await
            .map_err(|e| format!("{ctx} 失敗（retry 後）: {e}"))?;
    }
    Ok(())
}

/// session Mode（見え方）切替の共通実体（doc 50 §4.6 A6）。
///
/// 旧 `console_set_mode`（root 固定）と新 `session_set_mode`（session 明示）が共有する。
/// 遷移の実体（旧エンジン stop → mode 永続 → 新エンジン起立）は `LanePool::set_session_mode`。
/// 切替後の同一会話継続は cc_session `--resume` が担う。
///
/// **replay はここで撃たない** — client が新 pane を mount → topic を購読してから
/// `conversation_demand_start`（chat）/ terminal subscribe（tui）で撃つ。動詞側で撃つと購読前
/// replay の順序 race になる（非 retained topic で落ちる）。既存 `ConsoleNewSession` と同じ
/// 「購読してから demand」の規律で、Reborn ⊃ replay（更新済 transcript の再読）を保証する。
async fn apply_session_mode(
    state: &AppState,
    lane: &str,
    addr: &crate::repo::lanes_state::LaneAddress,
    session: crate::lane::session_registry::SessionKey,
    mode: crate::lane::session_registry::SessionMode,
) -> Result<serde_json::Value, String> {
    state
        .lane_pool
        .read()
        .await
        .set_session_mode(addr, session, mode)
        .map_err(|e| format!("session_set_mode 失敗: {e}"))?;
    // doc 53 §12.4 R3c: mode を書けば desired が変わる。**両方向を同じ 1 本が合わせる** —
    // tui 方向は新 PtySlot が立って pump が張られ（「vp-app が購読を跨いで維持している」lane
    // では demand が 1 のままで 0→1 hook が発火しないので、この契機が必須）、chat 方向は
    // slot が畳まれて pump 台帳 entry が撤去される。健在な兄弟 pane は pid 照合で触られない
    // （team-b 10 回目の「隣の pane の clear + 全 replay」は構造で再発しない）。
    reconcile_lane(state, addr).await;
    {
        // doc 33 §9: chat へは engine を eager spawn（切替時に resume を開始 → session_init を
        // 早く出す）。失敗しても切替自体は成功扱い（engine は次 submit で self-heal 再試行）。
        // reconcile の後（`conversation_session_focus` と同じ「intent → 注視」の順序）。
        let mut pool = state.lane_pool.write().await;
        if mode == crate::lane::session_registry::SessionMode::Gui
            && let Err(e) = pool.ensure_chat_engine(addr, Some(session), &state.topic_router)
        {
            tracing::warn!(
                "session_set_mode: eager chat engine spawn 失敗（submit で再試行）: {e}"
            );
        }
    }
    // doc 53 §11: mode は roster の一部（pane の kind を決める）ので知らせる。
    super::routes::lanes::emit_lane_update(state, addr).await;
    Ok(serde_json::json!({
        "status": "ok", "lane": lane, "session": session, "mode": mode.as_str()
    }))
}

/// doc 50 §4.6 A6: session = Pane の Mode 切替。`{lane, session, mode: "tui"|"gui"}`。
///
/// 名札の kind badge が任意 pane を切り替える経路（旧 lane 単位 `console_set_mode` の後継）。
/// session は明示必須（root 決め打ちにしない）。replay は client が新 pane 購読後に撃つ。
async fn handle_session_set_mode(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("session_set_mode: lane 未指定".to_string());
    }
    let session = payload_session_key("session_set_mode", &payload)?
        .ok_or_else(|| "session_set_mode: session 未指定（root 決め打ちにしない）".to_string())?;
    let mode_str = payload.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    let mode = crate::lane::session_registry::SessionMode::parse(mode_str)
        .ok_or_else(|| format!("session_set_mode: mode 不正: {mode_str:?}（tui|gui）"))?;
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("session_set_mode: lane パース失敗: {lane}"))?;
    apply_session_mode(state, lane, &addr, session, mode).await
}

// doc 50 §4.6 A6: 旧 `console_set_mode`（lane 単位の Mode 切替）は撤去した。見え方は session の
// 属性になり、切替は `session_set_mode {lane, session, mode}` 一本（名札 kind badge が撃つ）。
// mode / mode の二語併存を PR 後に残さないため、GUI 移行と同じ PR で消している。

/// doc 51 §1 A3b: session の「今なにを」自己申告を該当 session の conversation topic に注入する。
///
/// 発生源は AI 自身の `vp now` CLI（識別は spawn 時注入の `VP_REPO` / `VP_LANE` /
/// `VP_SESSION_KEY` env）。daemon は値を保存しない — 非 retained topic への fire-and-forget
/// （now-line は揮発。lane 行への掲揚で保持が要るのは Phase B の関心 — その時に retained 化を
/// 判断する）。session 未指定は root（lane の代表）に読み替える。
async fn handle_session_now(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("session_now: lane 未指定".to_string());
    }
    let text = payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err("session_now: text が空です".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("session_now: lane パース失敗: {lane}"))?;
    // ⚠️ route には **canonical を流す**（生の入力を流さない）。`vp now` は env（VP_LANE）
    // 由来の lane 文字列を運ぶため、旧世代の spawn env（`root` 等）が混ざる。parse で
    // 正規化した addr を捨てて raw を流すと、topic 上の lane が旧名のままになり、
    // 新名 key で照合する GUI の now-line と一致せず**無音で表示されない**。
    let lane = addr.canonical();
    let session = match payload.get("session").and_then(serde_json::Value::as_u64) {
        Some(s) => s as crate::lane::session_registry::SessionKey,
        None => crate::lane::session_registry::load(&addr.repo, &addr.name, "claude").root,
    };
    state
        .topic_router
        .route(crate::protocol::RepoMessage::ConversationEvent {
            lane: lane.clone(),
            session,
            event: crate::conversation::ConversationEvent::NowLine {
                text: text.to_string(),
            },
        })
        .await;
    Ok(serde_json::json!({ "ok": true, "lane": lane, "session": session }))
}

/// gui モデル切替: chat engine の `--model` を **session 単位**で切替える（mako 裁定
/// 2026-07-27 — doc 50 session=Pane で 1 lane 多 session になり、旧 `console_set_model`
/// （root slot 単位 + per-lane `engine_model` file）は旧前提として退役。記録先は registry の
/// `SessionEntry.model`）。
///
/// `{lane, session, model: string|null}`。null / 省略 = 記録を消して engine 既定に戻す。
/// spec「セッション進行中でも切り替えられる」の実体はここ — 稼働中 engine を drop して
/// `ensure_chat_engine` で即再 spawn すると、`--resume` + 新 `--model` で
/// **会話コンテキストを保ったままモデルだけ替わる**（CC の `/model` の VP 版）。
/// engine 不在（tui 中 / chat-idle）は記録のみ = 次 spawn から適用。
/// ⚠️ 進行中の turn は engine drop で切れる（UI 側は streaming 中 picker を disable して抑止）。
async fn handle_conversation_set_model(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("conversation_set_model: lane 未指定".to_string());
    }
    let session = payload_session_key("conversation_set_model", &payload)?.ok_or_else(|| {
        "conversation_set_model: session 未指定（root 決め打ちにしない）".to_string()
    })?;
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let Some(ref m) = model
        && !crate::lane::engine_model::is_valid_model(m)
    {
        return Err(format!("conversation_set_model: model 名が不正: {m:?}"));
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("conversation_set_model: lane パース失敗: {lane}"))?;

    {
        let mut pool = state.lane_pool.write().await;
        let info = pool
            .get(&addr)
            .ok_or_else(|| format!("conversation_set_model: Lane not found: {lane}"))?;
        // 切替可否は **当該 session の engine** で判定する（doc 39 P4-A の root-agent 解決の
        // session 版 — session 明示になったので root への丸めは消えた）。可否の真実は
        // `EngineKind::model_choices` の空/非空 1 本（旧 `model_switchable` 述語は catalog に
        // 畳んだ — client にも同じ catalog が LaneSessionView で届くので、UI と server の
        // 判定が同じ表から出る）。
        let lane_label = crate::repo::agent_spawner::lane_label(&addr).to_string();
        let default_agent = info.agent.clone();
        let reg = crate::lane::session_registry::load(&addr.repo, &lane_label, &default_agent);
        let entry_agent = reg
            .sessions
            .iter()
            .find(|s| s.key == session)
            .map(|s| s.agent.clone())
            .ok_or_else(|| {
                format!("conversation_set_model: session が存在しません（lane={lane}, session={session}）")
            })?;
        match crate::conversation::EngineKind::from_agent(&entry_agent) {
            Some(k) if !k.model_choices().is_empty() => {}
            Some(_) => {
                return Err(format!(
                    "{entry_agent} エンジンの model は engine 側で選択します（lane={lane}, session={session}）"
                ));
            }
            None => {
                return Err(format!(
                    "conversation_set_model は model 切替対応 engine の session のみ（lane={lane}, session={session}, agent={entry_agent}）"
                ));
            }
        }
        crate::lane::session_registry::set_model(
            &addr.repo,
            &lane_label,
            &default_agent,
            session,
            model.as_deref(),
        )
        .map_err(|e| format!("conversation_set_model: model 永続失敗: {e}"))?;
        // 稼働中 engine の入替（drop → resume 付き eager 再 spawn）。spawn 失敗しても
        // 記録は成功済みなので mode 切替と同様に成功扱い — 次 submit で self-heal される。
        // 入替は当該 session の engine のみ（chat_engines は (lane, session) の 2 段 map）。
        if pool.drop_chat_engine(&addr, Some(session))
            && let Err(e) = pool.ensure_chat_engine(&addr, Some(session), &state.topic_router)
        {
            tracing::warn!("conversation_set_model: engine 再 spawn 失敗（submit で再試行）: {e}");
        }
    }
    tracing::info!("conversation_set_model: lane={lane} session={session} model={model:?}");
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session, "model": model}))
}

/// tmux decoupling PR1: lane nudge。 論理 lane address 宛に literal text + Enter を PtySlot へ書く。
///
/// 旧制御面 (`tmux send-keys -t <session>`) の repo-proxy 置換。 daemon (delivery/reconcile
/// loop の re-nudge) / CLI (`vp flow handoff`) / MCP (`flow_handoff`) が control channel 経由で
/// この method を ask する。 repo-local な `AppState::nudge_lane` は同じ `deliver_nudge` sink を
/// in-process で呼ぶ (text→Enter の submit 意味論は `deliver_nudge` に集約)。
async fn handle_lane_nudge(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_nudge: lane 未指定".to_string());
    }
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_nudge: lane パース失敗: {}", lane));
    };
    // doc 46 P5: `session` 省略 = root（mailbox を名乗る住人）。明示指定で同居する別 slot に届く。
    let session = payload_session_key("lane_nudge", &payload)?;
    crate::repo::lanes_state::deliver_nudge(&state.lane_pool, &addr, session, text)
        .await
        .map_err(|e| format!("lane_nudge 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "session": session}))
}

/// doc 46 P5: lane が持つ **PTY slot の一覧**（session / pid / 生死 / root か / attach 有無）。
///
/// slot は lane に 1 枚ではなく session ごとになった。表示は当面ミニマム（1 枚ずつ）なので、
/// **UI を通さずに枚数と中身を読む口**をここに置く（doc 47 §7 成立条件② — 「読み手のない
/// 書き込み」を作らない）。CLI `vp lane slots` がこの method を ask する。
async fn handle_lane_slots(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_slots: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_slots: lane パース失敗: {}", lane));
    };
    let pool = state.lane_pool.read().await;
    if pool.get(&addr).is_none() {
        return Err(format!("lane_slots: lane 不在: {lane}"));
    }
    let slots = pool.slot_inventory(&addr);
    Ok(serde_json::json!({
        "status": "ok",
        "lane": lane,
        "count": slots.len(),
        "slots": slots,
    }))
}

/// doc 46 P5 **producer**: 新しい console（slot）を 1 枚立てる。
/// `{lane, agent?}` → `{status, lane, session, pid, count}`。
///
/// - 常に **新しい session** を採番してそこに slot を立てる（doc 46 §1.5「Pane は必ず新しい
///   session id で始まる」= session ↔ Pane 1:1）。既存 session の open は持たない
/// - `agent` 省略 = 現 root の engine を引き継ぐ（doc 46 P2 の「Engine を選んで新コンソール」の
///   tui 版。`conversation_session_create` は Mode=Chat 固定なのでそちらでは作れない）
/// - **root / focused は動かさない** — mailbox も pid も Dead 判定も root のまま（doc 40 §4-1）
///
/// pump は動詞の末尾の reconcile が demand（購読者の有無）に応じて張る（doc 53 R2 —
/// 旧「GUI 配線は張らない」は A6 の pump wiring 追加以降コードと矛盾していた、moody 指摘）。
/// 購読者不在の CLI 運用では pump なしのまま `vp lane slots` / `vp lane capture --session` /
/// `vp lane nudge --session` で読み書きする（capture は TermAttach 直読で topic 非依存）。
async fn handle_lane_slot_new(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_slot_new: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_slot_new: lane パース失敗: {}", lane));
    };
    let agent = payload.get("agent").and_then(|v| v.as_str());
    let session = state
        .lane_pool
        .read()
        .await
        .create_console_session(&addr, agent)
        .map_err(|e| format!("lane_slot_new: {e}"))?;
    // doc 53 §12.4 R3c: slot を立てるのは reconcile（desired = mode=Tui の session）。
    //
    // ⚠️ pump もこの中で合う。これが無いと新 session の PTY 出力が誰にも届かない: client 側の
    // terminal 購読は **lane 単位で 1 本**（`terminal_sessions` は lane key）なので、root が既に
    // tui で購読済の lane に 2 枚目を足すと購読者数は 1→1 のまま = demand hook のエッジが
    // 立たない → 入力は通る（terminal_write は直送）のに**出力だけ永久に沈黙**する。
    // 「lane 単位のハンドルが session の増加を捉えない」= 制約撤廃の随伴（doc 50 §4.7）の一族。
    reconcile_lane(state, &addr).await;
    // pid は**導出**する（動詞の戻り値ではなくなった）。spawn に失敗していれば None =
    // 「intent はあるが立っていない」の観測値そのもの（doc 53 §12.2 — 巻き戻さない）。
    let (pid, count) = {
        let pool = state.lane_pool.read().await;
        let pid = pool
            .slot_pids(&addr)
            .into_iter()
            .find(|(k, _)| *k == session)
            .map(|(_, p)| p);
        (pid, pool.slot_sessions(&addr).len())
    };
    // doc 53 §11: **本バグの当事者** — CLI / MCP から console を足しても、これが無いと
    // GUI の roster に出ない（GUI 自身の動詞しか fetch の契機にならなかった）。
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({
        "status": "ok",
        "lane": lane,
        "session": session,
        "pid": pid,
        "count": count,
    }))
}

/// tmux decoupling: lane console capture。 lane の Term grid（TermAttach）を text で返す。
///
/// 旧 `tmux capture-pane`（`handle_tmux_capture`）の native 代替 — main が sub の
/// console を読む dev-flow 用途。 CLI `vp lane capture` / 将来の MCP がこの method を ask する。
async fn handle_lane_capture(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_capture: lane 未指定".to_string());
    }
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("lane_capture: lane パース失敗: {}", lane));
    };
    // doc 46 P5: `session` 省略 = root（lane の代表 slot）。明示指定で同居する別 slot を読む。
    let session = payload_session_key("lane_capture", &payload)?;
    let pool = state.lane_pool.read().await;
    let content = match pool.capture_lane(&addr, session) {
        Some(c) => c,
        None => {
            // capture 不能の理由を分岐して UX 混乱を減らす（dogfood 2026-07-19: chat mode lane で
            // 一律「lane 不在 or console 未配線」に混乱した）。chat lane は term_attach 無しが
            // 正常なので、pool に実在して root の mode（registry 直読、doc 53 R1）が Chat なら
            // 「tui に切り替えよ」と案内する。
            // doc 46 P5: slot が複数枚になったので「その lane には何枚あるか」も添える
            // （--session の指し先が無い時に、存在する session key が判る）。
            let available = pool.slot_sessions(&addr);
            let msg = match pool.get(&addr) {
                Some(_) if session.is_some_and(|k| !available.contains(&k)) => format!(
                    "lane_capture: 指定 session に console はありません（session={}, この lane の slot: {:?}）: {lane}",
                    session.unwrap_or(0),
                    available
                ),
                Some(_)
                    if pool.root_mode(&addr) == crate::lane::session_registry::SessionMode::Gui =>
                {
                    format!(
                        "lane_capture: chat mode の lane に console はありません（tui に切り替えると capture できます）: {lane}"
                    )
                }
                Some(_) => format!("lane_capture: console 未配線: {lane}"),
                None => format!("lane_capture: lane 不在: {lane}"),
            };
            return Err(msg);
        }
    };
    Ok(serde_json::json!({
        "status": "ok",
        "lane": lane,
        "session": session,
        "slots": pool.slot_sessions(&addr),
        "content": content,
    }))
}

/// S3: terminal resize。 PtySlot (+ TermAttach grid) を cols×rows に同期する。
async fn handle_terminal_resize(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("terminal_resize: lane 未指定".to_string());
    }
    // bounds check: 0 / 極大値で PTY に不正 dims を渡さない (u64→u16 silent wrap も防ぐ)。
    // 旧 daemon "terminal" channel の resize 経路と同じ範囲 (1..=1000)。
    let cols = payload.get("cols").and_then(|v| v.as_u64()).unwrap_or(80);
    let rows = payload.get("rows").and_then(|v| v.as_u64()).unwrap_or(24);
    if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
        return Err(format!(
            "terminal_resize: 不正な dims (cols={cols} rows={rows})"
        ));
    }
    let (cols, rows) = (cols as u16, rows as u16);
    let Some(addr) = crate::repo::lanes_state::LanePool::parse_address(lane) else {
        return Err(format!("terminal_resize: lane パース失敗: {}", lane));
    };
    state
        .lane_pool
        .read()
        .await
        .resize_lane(
            &addr,
            payload_session_key("terminal_resize", &payload)?,
            cols,
            rows,
        )
        .map_err(|e| format!("terminal_resize 失敗: {}", e))?;
    Ok(serde_json::json!({"status": "ok", "lane": lane, "cols": cols, "rows": rows}))
}

/// F6② (doc 27 §3.4.5/§6): Lane delete。 旧 SP HTTP `DELETE /api/lanes` を repo-proxy ask に
/// 移管（surface→repo 直結 HTTP を撤去、 daemon 経由の ask に統一）。 logic は旧 `delete_handler`
/// から移設し、 core の `delete_lane_orchestrated` を再利用（HTTP route + handler は削除）。
async fn handle_lane_delete(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_delete: address 必須")?;
    // cleanup default = true (旧 DeleteLaneQuery default_cleanup と一致、 dir も rm する)。
    let cleanup = payload
        .get("cleanup")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let addr = crate::repo::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_delete: invalid lane address: {}", address))?;
    match super::routes::lanes::delete_lane_orchestrated(state, addr, cleanup).await {
        Ok(info) => Ok(serde_json::json!({
            "deleted": info.address,
            "pid": info.pid,
            "cleanup": info.cleanup_status,
        })),
        Err(e) => Err(e.to_string()),
    }
}

/// F6③ (doc 27 §3.4.5/§6): Lane restart。 旧 SP HTTP `POST /api/lanes/restart` を repo-proxy
/// ask に移管。 core の `restart_lane_orchestrated` (VP-131 透過 retry loop) を呼ぶ薄い adapter。
async fn handle_lane_restart(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let address = payload
        .get("address")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("lane_restart: address 必須")?;
    // fresh default=false (旧 RestartLaneQuery の #[serde(default)] と一致)。wire は bool のまま
    // （fresh=true = Reset lane / false = 会話を継ぐ）。
    //
    // R3c-2: server 側では `RespawnMode` ではなく**別の動詞**に分かれた（restart = 実体だけ
    // 捨てる / Reset = intent ごと素に戻す）。wire の bool は互換のため維持 — client の語彙を
    // 変える話は doc 54 の schema 束で扱う。
    let fresh = payload
        .get("fresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let addr = crate::repo::lanes_state::LanePool::parse_address(address)
        .ok_or_else(|| format!("lane_restart: invalid lane address: {}", address))?;
    if fresh {
        super::routes::lanes::reset_lane_orchestrated(state, addr).await
    } else {
        super::routes::lanes::restart_lane_orchestrated(state, addr).await
    }
}

/// 供給 push 根治（session chip 凍結、2026-07-17）: engine session pointer の変化通知。
///
/// pointer（cc_sessions 等の state file）の書き手は claude の UserPromptSubmit hook で、
/// repo プロセスの外にいる — repo は file を「読みに行った時だけ」変化を知る（ask 経路は正しく、
/// push 経路に変化イベントが存在しなかった）。hook → Daemon "wire" channel
/// (`lane/session-changed`) → 本 method で repo に届き、repo が focused session 規則で真値を
/// re-enrich して `Diff::Update` を emit する（daemon は routing のみ、真実源は repo のまま）。
///
/// doc 40 §4 / doc 46 P5: payload の `session` は**報告者が名乗った session**。会話 id は
/// その session に記録される（root 固定ではない）— 同じ lane に複数の console slot が
/// 同居しても、同居人の報告が root の `--resume` を壊さない。
async fn handle_lane_session_changed(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_session_changed: lane 必須".to_string());
    }
    let addr = crate::repo::lanes_state::LanePool::parse_address(lane)
        .ok_or_else(|| format!("lane_session_changed: invalid lane address: {lane}"))?;
    let Some(agent) = state
        .lane_pool
        .read()
        .await
        .get(&addr)
        .map(|l| l.agent.clone())
    else {
        return Err(format!("lane_session_changed: lane が存在しません: {lane}"));
    };
    // doc 40 §4/§6: hook の会話報告（session_id + event + 報告者が名乗る session）を
    // **報告された session** に適用する — policy（宛先解決 + F1/F2 guard）の唯一の実装点は
    // record_conversation。session_id 無し = 旧 hook / 旧 daemon からの「変化通知のみ」
    // （従来互換、enrich だけ行う）。
    if let Some(sid) = payload.get("session_id").and_then(|v| v.as_str()) {
        use crate::lane::session_registry::{ConversationReport, ReportTarget, ReportTrigger};
        let trigger = match payload.get("event").and_then(|v| v.as_str()) {
            Some("issued") => ReportTrigger::Issued,
            _ => ReportTrigger::Spoken,
        };
        // `session` 不在 = 報告者が名乗らなかった（VP_SESSION_KEY 無しで spawn 済の slot /
        // VP 外起動）→ 後方互換で root 宛。**ここで root に丸めない**（Unspecified のまま
        // 渡す）ことで、実在しない session の報告が root に化けるのを registry 側が拒める。
        let target = match payload_session_key("lane_session_changed", &payload)? {
            Some(key) => ReportTarget::Session(key),
            None => ReportTarget::Unspecified,
        };
        let report = ConversationReport {
            target,
            conversation: sid,
            trigger,
        };
        let lane_label = crate::repo::agent_spawner::lane_label(&addr);
        match crate::lane::session_registry::record_conversation(
            &addr.repo, lane_label, &agent, report,
        ) {
            Ok(outcome) => {
                tracing::info!(
                    "conversation report: addr={addr} report={report:?} outcome={outcome:?}"
                );
            }
            Err(e) => {
                tracing::warn!("conversation report 適用失敗: addr={addr} err={e}");
            }
        }
    }
    super::routes::lanes::emit_lane_update(state, &addr).await;
    Ok(serde_json::json!({ "status": "ok", "lane": lane }))
}

/// lanes portless (doc 27 §3.4.5): Lane create。 旧 SP HTTP `POST /api/lanes` を repo-proxy ask に
/// 移管。 core の `create_sub_orchestrated` (lane clone + PtySlot spawn) を呼ぶ薄い adapter。
/// payload は `CreateLaneReq` 互換 JSON (kind/name/agent?/cwd?/branch?/base?)。 成功は LaneInfo JSON、
/// 失敗は core が返す String error (旧 HTTP の CONFLICT="already exists" 等を保持)。
async fn handle_lane_create(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: super::routes::lanes::CreateLaneReq = serde_json::from_value(payload)
        .map_err(|e| format!("lane_create: invalid payload: {}", e))?;
    let info = super::routes::lanes::create_sub_orchestrated(state, req).await?;
    serde_json::to_value(&info).map_err(|e| format!("lane_create: LaneInfo serialize 失敗: {}", e))
}

/// 帳簿が起点を解決するための lane 一覧（id と表示名の対だけ）。
///
/// `LaneInfo` 全体ではなく [`crate::host::ledger::LaneRef`] に落とすのは、帳簿が lane の
/// 中身に依存しないため（`host::farewell` が git を知らないのと同じ切り方）。
async fn ledger_lane_refs(state: &Arc<AppState>) -> Vec<crate::host::ledger::LaneRef> {
    state
        .lane_pool
        .read()
        .await
        .list()
        .into_iter()
        .map(|l| crate::host::ledger::LaneRef::new(l.id.to_string(), l.address.name))
        .collect()
}

/// doc 44 D4: 帳簿から開発起点を読む。応答は [`crate::host::ledger::Origin`] の JSON。
///
/// 未設定 / dangling でも error にせず、**どう決まったか**を `source` で返す
/// （起点が読めないだけで呼び出し側が止まる方が困る）。
async fn handle_lane_origin_get(state: &Arc<AppState>) -> Result<serde_json::Value, String> {
    let lanes = ledger_lane_refs(state).await;
    let origin = crate::host::ledger::origin(state.vpdb.as_ref(), &state.repo_dir, &lanes).await;
    serde_json::to_value(&origin).map_err(|e| format!("lane_origin_get: serialize 失敗: {e}"))
}

/// doc 44 D4: 開発起点を設定する。payload = `{ "lane": "<lane 名>" }`。
///
/// 人が打つのは名前、帳簿に入るのは `lane_id` — 変換は
/// [`crate::host::ledger::set_origin`] が境界で 1 回だけ行う。
/// D5 の通り **何も動かさない**（cwd も active lane も変えない、ポインタの書き換えだけ）。
async fn handle_lane_origin_set(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let lane = payload.get("lane").and_then(|v| v.as_str()).unwrap_or("");
    if lane.is_empty() {
        return Err("lane_origin_set: lane 必須".to_string());
    }
    let lanes = ledger_lane_refs(state).await;
    crate::host::ledger::set_origin(state.vpdb.as_ref(), &state.repo_dir, lane, &lanes).await?;
    // 起点は snapshot の `origin` に載るので、投影が変わった = 即 publish する。
    // これが無いと 5s periodic tick まで sidebar の star が動かず「押しても無反応」に見える。
    let _ = state
        .system_event_tx
        .send(crate::repo::lanes_state::SystemEvent::LanesProjectionChanged);
    let origin = crate::host::ledger::origin(state.vpdb.as_ref(), &state.repo_dir, &lanes).await;
    serde_json::to_value(&origin).map_err(|e| format!("lane_origin_set: serialize 失敗: {e}"))
}

/// doc 44 §12: lane の並び順を帳簿に保存する。payload = `{ "order": ["<lane 名>", ...] }`。
///
/// 起点と同じく、人が触るのは名前で帳簿に入るのは `lane_id`。保存後の反映は
/// 次の lanes snapshot に載って戻る（`build_lanes_snapshot` が帳簿の順で並べる）。
async fn handle_lane_order_set(
    state: &Arc<AppState>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let order: Vec<String> = payload
        .get("order")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if order.is_empty() {
        return Err("lane_order_set: order 必須".to_string());
    }
    let lanes = ledger_lane_refs(state).await;
    crate::host::ledger::set_lane_order(state.vpdb.as_ref(), &state.repo_dir, &order, &lanes)
        .await?;
    // 並び順が変わった = snapshot が変わるので、publish して vp-app を起こす
    // （doc 44 §11 の指紋は lanes の並びも含むため、次の publish で必ず届く）。
    let _ = state
        .system_event_tx
        .send(crate::repo::lanes_state::SystemEvent::LanesProjectionChanged);
    Ok(serde_json::json!({ "status": "ok", "count": order.len() }))
}

/// lanes portless (doc 27 §3.4.5): Lane list。 旧 SP HTTP `GET /api/lanes` を repo-proxy ask に
/// 移管。 core の `build_lanes_snapshot` を呼び `{lanes:[...]}` で wrap (旧 HTTP `LanesResponse` 互換)。
async fn handle_lanes_list(state: &Arc<AppState>) -> Result<serde_json::Value, String> {
    let lanes = super::routes::lanes::build_lanes_snapshot(state).await;
    // doc 46 P5: slot は lane に 1 枚ではなく session ごとになった。`vp lane ls --detail` から
    // 枚数が見えるよう、lane ごとの slot session key を snapshot に添える（LaneInfo 自体には
    // 足さない — descriptor は帳簿の永続形で、slot は in-memory な runtime 事実だから。
    // 混ぜると「再起動で復元されるべき値」に見えてしまう）。
    let pool = state.lane_pool.read().await;
    let lanes: Vec<serde_json::Value> = lanes
        .iter()
        .map(|lane| {
            let mut v = serde_json::to_value(lane).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "slots".to_string(),
                    serde_json::json!(pool.slot_sessions(&lane.address)),
                );
            }
            v
        })
        .collect();
    Ok(serde_json::json!({ "lanes": lanes }))
}

/// F6④ (doc 27 §3.4.5/§6): Agent 一覧。 旧 SP HTTP `GET /api/agents` を repo-proxy ask に移管。
/// tmux decoupling PR2: built-in 静的テーブル (旧 mise task scan + TTL cache は廃止)。
async fn handle_stands_list() -> Result<serde_json::Value, String> {
    let agents = super::routes::agents::list_agents();
    Ok(serde_json::json!({ "agents": agents }))
}

pub(crate) async fn dispatch_repo_method(
    state: &Arc<AppState>,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match method {
        // switch_lane も generic broadcast 経路に乗せる（B1: 遠隔 active Lane 制御）。
        // hub → topic `repo/board/event/switch-lane`（一時コマンド=非
        // retained）→ canvas channel → vp-app が受信して active Lane を切り替える。
        // board モデル (2026-07-15): show/clear は repo-authoritative な board 経路へ。
        // item を DB に durable append し、 更新後 board を BoardUpdated(retained) で broadcast する。
        "show" | "clear" => handle_canvas_command(state, payload).await,
        // doc 52 §5: id 指定 in-place 置換（read-first、id 不在は loud error）
        "board_update" => handle_board_update(state, payload).await,
        // doc 52 §4/§5: 呼び出し元 lane の board を id 付き全文で返す（中継台 + identity lookup）
        "read_board" => handle_board_read(state, payload).await,
        // doc 48 Phase 2: editor bridge (MCP → GUI request-response)
        "editor_fields" | "editor_values" | "editor_set" => {
            handle_editor_command(state, method, payload).await
        }
        // doc 49 LE-P2 PR2: layout bridge (LE-15)。editor bridge と同じ配管を op を変えて共用
        // (method に editor_ prefix が無いので op = method のまま vp-app に届く)
        "layout_get" | "layout_set" | "layout_history" => {
            handle_editor_command(state, method, payload).await
        }
        "editor_result" => handle_editor_result(state, payload).await,
        "toggle_pane" | "split_pane" | "close_pane" | "switch_lane" => {
            handle_process_message(state, payload)
        }
        "watch_file" => handle_watch_file(state, payload).await,
        "unwatch_file" => handle_unwatch_file(state, payload).await,
        // S2: demand-driven terminal pump (Daemon demand hook → control reverse-route)
        // doc 53 R2: start / stop は同じ reconcile の契機（demand の今は level で読む）。
        "terminal_demand_start" | "terminal_demand_stop" => {
            handle_terminal_demand(state, payload).await
        }
        // gui replay-on-attach: chat lane の transcript を attach 時に replay
        "conversation_demand_start" => handle_conversation_demand_start(state, payload).await,
        "conversation_demand_stop" => handle_conversation_demand_stop(state, payload).await,
        // S3: terminal 入力/resize (surface → canvas channel upstream → control reverse-route)
        "terminal_write" => handle_terminal_write(state, payload).await,
        "conversation_submit" => handle_conversation_submit(state, payload).await,
        // channel E (doc 34): wire/delegation nudge の chat-engine 注入 (lane_nudge の Chat 対応物)
        "conversation_nudge" => handle_conversation_nudge(state, payload).await,
        // gui HITL (doc 35 PR1): PromptCard 回答 → 逆方向 can_use_tool へ control_response 書き戻し
        "conversation_respond" => handle_conversation_respond(state, payload).await,
        // doc 35 §5: 実行中 turn の中断（stop ボタン / Esc）。
        "conversation_interrupt" => handle_conversation_interrupt(state, payload).await,
        // doc 35 §2.5 / PR3: permission mode の動的切替（承認 opt-in）。
        "conversation_set_permission_mode" => {
            handle_conversation_set_permission_mode(state, payload).await
        }
        // doc 38 (1 Lane = N session): session registry の list / create / focus。
        // Phase 2 の tab strip はこの 3 本 + 既存 RPC の additive session param だけで成立する。
        "conversation_session_list" => handle_conversation_session_list(state, payload).await,
        "conversation_session_create" => handle_conversation_session_create(state, payload).await,
        // doc 39 §4: tui の ✨ New（新 session + root 張り替え + slot の bare respawn、非破壊）
        "conversation_session_new_root" => {
            handle_conversation_session_new_root(state, payload).await
        }
        // doc 39 P3: Root 切替 picker（既存 session へ root を向け替え + Resume slot 張り替え）
        "conversation_session_switch_root" => {
            handle_conversation_session_switch_root(state, payload).await
        }
        "conversation_session_focus" => handle_conversation_session_focus(state, payload).await,
        // doc 38 Phase 3: tab を閉じる（session remove）。
        "conversation_session_remove" => handle_conversation_session_remove(state, payload).await,
        "session_set_mode" => handle_session_set_mode(state, payload).await,
        "conversation_set_model" => handle_conversation_set_model(state, payload).await,
        // doc 51 §1 A3b: `vp now` — session の「今なにを」自己申告を now-line に注入
        "session_now" => handle_session_now(state, payload).await,
        // tmux decoupling PR1: 制御面 nudge の repo-proxy 入口 (旧 tmux send-keys の置換)
        "lane_nudge" => handle_lane_nudge(state, payload).await,
        // tmux decoupling PR2: lane console capture (旧 tmux capture-pane の native 代替)
        "lane_capture" => handle_lane_capture(state, payload).await,
        // doc 46 P5: lane が持つ PTY slot の一覧（UI を通さない slot 枚数の読み手）
        "lane_slots" => handle_lane_slots(state, payload).await,
        // doc 46 P5 producer: 新 session を採番して console を 1 枚立てる（`lane_slots` の書き手）
        "lane_slot_new" => handle_lane_slot_new(state, payload).await,
        "terminal_resize" => handle_terminal_resize(state, payload).await,
        // board モデル (2026-07-15): webview からの board mutate（thumbnail ✕ / Clear ボタン）。
        // 旧 pp_state_save/load は撤去（board は repo truth、 webview は BoardUpdated 購読 + mutate へ）。
        "board_delete_item" => handle_board_delete_item(state, payload).await,
        "board_clear" => handle_board_clear(state, payload).await,
        // cursor の server 昇格（doc 52 §5 計器盤）: thumbnail click / scrollback の注視を repo truth に。
        "board_set_cursor" => handle_board_set_cursor(state, payload).await,
        // lanes portless: Lane create/list (旧 SP HTTP POST/GET /api/lanes を repo-proxy ask に移管)
        "lane_create" => handle_lane_create(state, payload).await,
        "lanes_list" => handle_lanes_list(state).await,
        // F6②: Lane delete (旧 SP HTTP DELETE /api/lanes を repo-proxy ask に移管)
        "lane_delete" => handle_lane_delete(state, payload).await,
        // F6③: Lane restart (旧 SP HTTP POST /api/lanes/restart を repo-proxy ask に移管)
        "lane_restart" => handle_lane_restart(state, payload).await,
        // 供給 push 根治: hook → daemon 経由の session pointer 変化通知（Diff::Update push の起点）
        "lane_session_changed" => handle_lane_session_changed(state, payload).await,
        // doc 44 D4: Repo Host の帳簿 — 開発起点ポインタの読み書き
        "lane_origin_get" => handle_lane_origin_get(state).await,
        "lane_origin_set" => handle_lane_origin_set(state, payload).await,
        "lane_order_set" => handle_lane_order_set(state, payload).await,
        // F6④: Agent 一覧 (旧 SP HTTP GET /api/agents を repo-proxy ask に移管)
        "agents_list" => handle_stands_list().await,
        // L0 finale: repo graceful shutdown を QUIC で (旧 SP HTTP POST /api/shutdown を置換、
        // Daemon stop_process / restart_process 用)。 shutdown_token.cancel() で graceful 停止
        // (DB close 等)。 repo が即 QUIC server を畳むため応答が返らない事もあるが best-effort。
        "shutdown" => {
            tracing::info!("Shutdown requested via QUIC dispatch");
            state.shutdown_token.cancel();
            Ok(serde_json::json!({"status": "shutting_down"}))
        }
        // tmux decoupling PR2: 旧 "tmux_*" dispatch (split/list/close/capture/agent_meta/
        // send_keys/resolve_pane) は退役。 後継は lane 語彙の "lane_nudge" / "lane_capture"。
        // ProcessRunner
        "process_run" => handle_process_run(state, payload).await,
        "process_stop" => handle_process_stop(state, payload).await,
        "process_inject" => handle_process_inject(state, payload).await,
        "process_list" => handle_process_list(state).await,
        // L0 portless Group B-3: Ruby VM (旧 SP HTTP /api/ruby/* を repo-proxy ask に移管)。
        // ruby_list は process_registry.list() = process_list と同一なので handle_process_list 再利用。
        "ruby_eval" => handle_ruby_eval(state, payload).await,
        "ruby_run" => handle_ruby_run(state, payload).await,
        "ruby_stop" => handle_ruby_stop(state, payload).await,
        "ruby_list" => handle_process_list(state).await,
        // wiremsg threaded inbox (Phase A ①、 R2 で wire_thread 追加)
        "wire_send" => handle_wire_send(state, payload).await,
        "wire_recv" => handle_wire_recv(state, payload).await,
        "wire_thread" => handle_wire_thread(state, payload).await,
        // flow_progress 用 read-only 未読 count (cursor 不触り)
        "wire_unread_count" => handle_wire_unread_count(state, payload).await,
        // flow_progress 5-state FSM derive 用 read-only 最新 wmsg
        "wire_latest_msg" => handle_wire_latest_msg(state, payload).await,
        // flow_progress AwaitingUser 判定用 read-only 未 ack needs_user
        "wire_needs_user_pending" => handle_wire_needs_user_pending(state, payload).await,
        "wire_ack" => handle_wire_ack(state, payload).await,
        // Agent 委譲 (doc 28 §4): delegate=B を wake / complete=A を wake /
        // respond=NeedsInput(Reborn) に A が回答して B を再 wake (Active へ loop)。
        "delegate" => super::delegation::handle_delegate(state, payload).await,
        "complete" => super::delegation::handle_complete(state, payload).await,
        "respond" => super::delegation::handle_respond(state, payload).await,
        _ => Err(format!("不明なメソッド: process.{}", method)),
    }
}

// =============================================================================
// wiremsg ハンドラー (R2-a: daemon 中央 store への proxy 層)
//
// store 直結のロジックは routes/wire.rs (daemon 側) に移設済。 repo の責務は
// 「アドレス正規化 (N1) → daemon へ HTTP relay」 のみ。 QUIC dispatch と
// HTTP wrapper (routes/health.rs) は本 proxy 群を呼ぶため signature 不変。
// =============================================================================

/// agent address を canonical (qualified) 形に正規化する (wiremsg N1、 refactor R1 PR-B)
///
/// bare `"agent"` を qualified (`agent@<repo>`) に正規化する。
///
/// 現行 MCP (`SelfLane::from_address`) は main も canonical `agent@<repo>` を
/// 自前で送るため、本関数は実質 **冪等な素通し + 後方互換 (旧 client / bare 送信者) 用の
/// 防御層**。bare を残す理由: 旧 bare 送信が来ても store 識別子を qualified 一本に揃え、
/// cross-process 返信 (`agent@<repo>` 宛 forward) が bare query と完全一致せず届かない
/// バグ (B2、 レビュー mem_1CbuxQuNRwHBiZgBVUWVfN) を防ぐため。
/// bare 以外 (qualified / board@... / runner@... 等) はそのまま返す。
///
/// ⚠️ 正規化先 `self_repo` は「繋いだ repo の repo」なので、bare のままだと誤 repo 接続で
/// identity が化ける (= 旧 main バグの根)。だから identity の SSOT は MCP 側 canonical
/// 送出に移した。本関数は qualified を受けたら何もしない (= repo 非依存) のが正常運用。
fn normalize_agent_addr(addr: &str, self_repo: &str) -> String {
    if addr == "agent" {
        format!("agent@{}", self_repo)
    } else {
        addr.to_string()
    }
}

/// wiremsg を送信する (R2-a: daemon 中央 store への proxy)
///
/// payload: `{ from, to: [..], body, reply_to? }`
///
/// repo の責務はアドレス正規化 (N1: bare `"agent"` → `"agent@<self_repo>"`) のみ。
/// 保存・notify・local_seq 採番・body coerce は全て daemon 側
/// ([`crate::repo::routes::wire`])。 cross-process forward は中央化で概念ごと消滅。
pub(crate) async fn handle_wire_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_send: 'from' required".to_string())?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| normalize_agent_addr(s, &state.repo_name))
                .collect()
        })
        .unwrap_or_default();
    let mut forwarded = serde_json::json!({
        "from": from,
        "to": to,
    });
    if let Some(body) = payload.get("body") {
        forwarded["body"] = body.clone();
    }
    if let Some(reply_to) = payload.get("reply_to") {
        forwarded["reply_to"] = reply_to.clone();
    }
    super::daemon_wire::call("/api/wire/send", forwarded).await
}

/// wiremsg を受信する (R2-a: daemon 中央 store への proxy、 long-poll は daemon 側)
///
/// payload: `{ agent, timeout? }` — timeout の clamp (default 5s / max 30s) も daemon 側。
pub(crate) async fn handle_wire_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?;
    let timeout = payload.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);
    super::daemon_wire::call(
        "/api/wire/recv",
        serde_json::json!({ "agent": agent, "timeout": timeout }),
    )
    .await
}

/// wiremsg の ancestor-chain (系譜) を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ message_id }` — agent 文脈不要のため正規化なしで relay。
pub(crate) async fn handle_wire_thread(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _ = state; // thread は repo 文脈 (正規化) 不要。 signature は他 handler と統一
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/thread",
        serde_json::json!({ "message_id": message_id }),
    )
    .await
}

/// wiremsg の agent 関与最新 message を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の 5-state FSM derive で使う。
pub(crate) async fn handle_wire_latest_msg(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_latest_msg: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/latest-msg",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の agent 発 未 ack needs_user を取得する (daemon proxy、 read-only)
///
/// payload: `{ agent }` → `{ status, message }`。 `flow_progress` の `AwaitingUser` 判定で使う。
pub(crate) async fn handle_wire_needs_user_pending(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_needs_user_pending: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/needs-user-pending",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg の per-agent 未読 count を取得する (R2-a: daemon proxy、 read-only)
///
/// payload: `{ agent }`。 `flow_progress` の集約 view / `wire_inbox` MCP tool で使う。
pub(crate) async fn handle_wire_unread_count(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_unread_count: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/unread-count",
        serde_json::json!({ "agent": agent }),
    )
    .await
}

/// wiremsg を ack する (R2-a 新設、 決定 D3: cursor 非破壊の ack 台帳への proxy)
///
/// payload: `{ message_id, agent }` → `{ status, acked }`
pub(crate) async fn handle_wire_ack(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.repo_name))
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    super::daemon_wire::call(
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}

#[cfg(test)]
mod tests {

    use crate::conversation::ConversationEvent;

    fn init_ev() -> ConversationEvent {
        ConversationEvent::SessionInit {
            session_id: "sid".into(),
            model: None,
            permission_mode: None,
            cwd: None,
            tools: vec![],
            mcp_servers: vec![],
            slash_commands: vec!["compact".into()],
            command_docs: Default::default(),
        }
    }

    /// ⚠️ **`ReplayStart` の後**に置く。GUI はあれを見て会話表示をクリアするので、
    /// 前に置くと配り直した session 状態が直後に消える。
    #[test]
    fn session_init_goes_after_replay_start() {
        let mut ev = vec![
            ConversationEvent::ReplayStart,
            ConversationEvent::ReplayEnd { in_flight: false },
        ];
        super::splice_session_init(&mut ev, Some(init_ev()));
        assert!(matches!(ev[0], ConversationEvent::ReplayStart));
        assert!(matches!(ev[1], ConversationEvent::SessionInit { .. }));
    }

    /// engine 未起動（chat-idle / tui）は配り直すものが無い = 列を触らない。
    #[test]
    fn no_retained_init_leaves_events_untouched() {
        let mut ev = vec![ConversationEvent::ReplayStart];
        super::splice_session_init(&mut ev, None);
        assert_eq!(ev.len(), 1);
    }

    /// 先頭が `ReplayStart` でない形（将来の変更）でも落とさず先頭に置く。
    #[test]
    fn without_replay_start_it_goes_first() {
        let mut ev = vec![ConversationEvent::ReplayEnd { in_flight: false }];
        super::splice_session_init(&mut ev, Some(init_ev()));
        assert!(matches!(ev[0], ConversationEvent::SessionInit { .. }));
    }
    use super::normalize_agent_addr;

    /// doc 48 Phase 2: editor bridge の相関 — command が pending を作り broadcast、
    /// GUI 相当の `editor_result` が request_id で解決して呼び出し元に payload が返る。
    #[tokio::test]
    async fn editor_command_roundtrip_resolves_via_result() {
        use super::{handle_editor_command, handle_editor_result};
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // broadcast より先に購読しておかないと EditorCommand を取りこぼす
        let mut hub_rx = state.hub.subscribe();

        let (cmd_res, ()) = tokio::join!(
            handle_editor_command(&state, "editor_values", serde_json::json!({})),
            async {
                let msg = hub_rx.recv().await.expect("EditorCommand broadcast");
                let RepoMessage::EditorCommand { request_id, op, .. } = msg else {
                    panic!("EditorCommand 以外が broadcast された");
                };
                assert_eq!(op, "values");
                handle_editor_result(
                    &state,
                    serde_json::json!({
                        "request_id": request_id,
                        "payload": { "values": { "sb.text.base": 13 } }
                    }),
                )
                .await
                .expect("editor_result ok");
            }
        );
        let body = cmd_res.expect("roundtrip 成功");
        assert_eq!(body["values"]["sb.text.base"], 13);
        // 解決後の pending は空 (leak しない)
        assert!(state.editor_pending.lock().await.is_empty());
    }

    /// 不在 request_id への応答 (= timeout 済 stale) はエラーにせず no-op で吸収する。
    #[tokio::test]
    async fn editor_result_with_unknown_request_id_is_noop_ok() {
        use super::handle_editor_result;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let r = handle_editor_result(
            &state,
            serde_json::json!({"request_id": "gone", "payload": {}}),
        )
        .await;
        assert!(r.is_ok());
    }

    /// editor_set は id / value 必須 (broadcast 前に弾く = pending を作らない)。
    #[tokio::test]
    async fn editor_set_requires_id_and_value() {
        use super::handle_editor_command;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"id": "x"}),
            serde_json::json!({"value": 1}),
        ] {
            assert!(
                handle_editor_command(&state, "editor_set", payload.clone())
                    .await
                    .is_err(),
                "payload {payload} が弾かれていない"
            );
        }
        assert!(state.editor_pending.lock().await.is_empty());
    }

    #[test]
    fn normalize_bare_agent_to_qualified() {
        assert_eq!(normalize_agent_addr("agent", "vp"), "agent@vp");
    }

    #[test]
    fn normalize_keeps_qualified_and_other_addrs() {
        assert_eq!(normalize_agent_addr("agent@vp", "vp"), "agent@vp");
        assert_eq!(normalize_agent_addr("agent@other", "vp"), "agent@other");
        assert_eq!(normalize_agent_addr("agent@vp/sub", "vp"), "agent@vp/sub");
        assert_eq!(normalize_agent_addr("board@vp", "vp"), "board@vp");
    }

    /// テスト用の spawn 可能な shell。 `$SHELL` があればそれを、 無ければ OS 既定
    /// (Unix: `/bin/sh`、 Windows: `cmd.exe`) を使う。 Windows には `/bin/sh` が無いので
    /// OS 分岐が必須 (pty_slot の `default_test_shell` と同方針)。
    fn default_test_shell() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(windows) {
                "cmd.exe".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
    }

    // =========================================================================
    // S2 (doc 27 §4.1): demand-driven terminal pump の repo 側 e2e
    // =========================================================================

    /// 実 PtySlot を lane_pool に仕込み、 demand_start → pump 起動 → PTY 出力が
    /// per-lane terminal topic に届く → demand_stop → pump 除去、 を 1 本で検証する
    /// (daemon 側 demand hook の reverse-route 先 = repo dispatch の責務範囲)。
    #[tokio::test]
    async fn terminal_demand_start_routes_pty_output_then_stop() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string(); // "vp/main"

        // 実 PtySlot を attach (subscribe_output が Some を返す前提を作る)。
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // surface 相当: repo topic_router に per-lane terminal topic を購読
        // （= demand の level が立つ。 doc 53 R2: reconcile は購読者数を直読する）。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start（= reconcile 契機）→ pump 起動。
        let started = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(started["status"], "reconciled");
        assert_eq!(started["attached"], 1);
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump が登録される"
        );

        // shell プロンプト等の PTY 出力が terminal topic に流れてくる。
        let (rtopic, msg) = tokio::time::timeout(Duration::from_secs(5), srx.recv())
            .await
            .expect("PTY 出力が terminal topic に届かない (timeout)")
            .expect("topic channel closed");
        assert_eq!(rtopic, topic);
        assert!(matches!(msg, RepoMessage::LaneTerminalOutput { .. }));

        // 購読を畳んでから demand_stop（production の hook は 1→0 遷移の後に撃つ —
        // reconcile は edge の向きを信じず level を読むので、購読が残ったままの stop は
        // no-op になるのが正。 その裏は下の keeps_pumps テストが固定する）。
        state.topic_router.unsubscribe(sub_id).await;
        let stopped = dispatch_repo_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_stop");
        assert_eq!(stopped["status"], "reconciled");
        assert_eq!(stopped["removed"], 1);
        assert!(
            !state.terminal_pumps.read().await.contains_key(&lane),
            "pump が除去される"
        );

        // 嘘の edge への耐性: 購読が生きているのに stop が届いても pump は畳まれない
        // （level が真実源）。再購読して demand を立て直し、start で 1 本張ってから検証する。
        let (sub_id2, _srx2) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start 2");
        let lying_stop = dispatch_repo_method(
            &state,
            "terminal_demand_stop",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("lying stop");
        assert_eq!(lying_stop["removed"], 0, "購読が残る限り stop は畳まない");
        assert!(
            state.terminal_pumps.read().await.contains_key(&lane),
            "pump は維持される"
        );
        state.topic_router.unsubscribe(sub_id2).await;
    }

    /// doc 52 §4/§5: show → read_board（id 取得）→ board_update（in-place 置換）→ read_board の往復。
    /// 未知 id の update が loud error になることも固定する。
    #[tokio::test]
    async fn board_read_and_update_roundtrip() {
        use super::dispatch_repo_method;
        use crate::db::VpDb;
        use crate::repo::state::build_test_app_state_with;
        use std::sync::Arc;

        let db = Arc::new(VpDb::connect_mem().await.unwrap());
        let state = build_test_app_state_with("/repos/vp", Some(db), None).await;

        // show で 1 件貼る（lane/scope 省略 = main lane / scope=lane）。
        let show = serde_json::json!({
            "type": "show", "pane_id": "main",
            "content": { "markdown": "original" }, "append": false, "title": "t"
        });
        dispatch_repo_method(&state, "show", show)
            .await
            .expect("show");

        // read_board で item と id を取る。
        let read = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read_board");
        let items = read["items"].as_array().expect("items array");
        assert_eq!(items.len(), 1);
        let id = items[0]["id"].as_str().expect("id").to_string();
        assert_eq!(items[0]["content"], "original");

        // update で in-place 置換。
        dispatch_repo_method(
            &state,
            "board_update",
            serde_json::json!({ "id": id, "content": "revised", "content_type": "html" }),
        )
        .await
        .expect("board_update");

        let read2 = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read_board 2");
        assert_eq!(read2["items"][0]["content"], "revised", "in-place で反映");
        assert_eq!(read2["items"][0]["contentType"], "html");
        assert_eq!(read2["items"][0]["id"], id, "id 不変");
        assert_eq!(
            read2["items"].as_array().unwrap().len(),
            1,
            "item 数は増えない（重複を作らない）"
        );

        // content_type 省略 = 既存 type を保つ（html→markdown の silent 降格を防ぐ、team-b review）。
        dispatch_repo_method(
            &state,
            "board_update",
            serde_json::json!({ "id": id, "content": "revised-2" }),
        )
        .await
        .expect("board_update (content_type 省略)");
        let read3 = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read_board 3");
        assert_eq!(read3["items"][0]["content"], "revised-2");
        assert_eq!(
            read3["items"][0]["contentType"], "html",
            "content_type 省略で既存 type(html) が保たれる"
        );

        // board 非対応の content_type（url）は loud error（show の content_to_parts と対称）。
        let bad_ct = dispatch_repo_method(
            &state,
            "board_update",
            serde_json::json!({ "id": id, "content": "x", "content_type": "url" }),
        )
        .await;
        assert!(
            bad_ct.is_err(),
            "url content_type の update は error: {bad_ct:?}"
        );

        // 未知 id の update は loud error（静かな重複を作らない = update に分けた狙い）。
        let err = dispatch_repo_method(
            &state,
            "board_update",
            serde_json::json!({ "id": "no-such", "content": "x" }),
        )
        .await;
        assert!(err.is_err(), "未知 id の update は error: {err:?}");
    }

    /// wave 3 計器盤（doc 52 §5）: scrollback 規則（head を見ているときだけ follow）+ cursor
    /// server 昇格 + updatedAt の鮮度 stamp を往復で固定する。
    #[tokio::test]
    async fn board_cursor_follow_and_freshness() {
        use super::dispatch_repo_method;
        use crate::db::VpDb;
        use crate::repo::state::build_test_app_state_with;
        use std::sync::Arc;

        let db = Arc::new(VpDb::connect_mem().await.unwrap());
        // schema（idx_pane_scope UNIQUE）を定義しないと show ごとに新 row になり ON DUPLICATE KEY
        // UPDATE の item 蓄積が起きない（accumulation / follow の検証に必須）。
        db.define_schema().await.unwrap();
        let state = build_test_app_state_with("/repos/vp", Some(db), None).await;

        let show = |body: &str| {
            serde_json::json!({
                "type": "show", "pane_id": "main",
                "content": { "markdown": body }, "append": false, "title": body
            })
        };

        // A を貼る → cursor = A、updatedAt = createdAt（貼った瞬間が最終更新）。
        dispatch_repo_method(&state, "show", show("A"))
            .await
            .expect("show A");
        let r1 = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read 1");
        let id_a = r1["items"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(r1["cursor"], id_a, "貼った直後は cursor が新 item");
        assert_eq!(
            r1["items"][0]["updatedAt"], r1["items"][0]["createdAt"],
            "新規 item は updatedAt = createdAt"
        );

        // B を貼る → cursor は head(A) を見ていたので follow して B へ（scrollback: 最新追従）。
        dispatch_repo_method(&state, "show", show("B"))
            .await
            .expect("show B");
        let r2 = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read 2");
        let id_b = r2["items"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(r2["cursor"], id_b, "head を見ていたら新着に follow");

        // 古い A に cursor を移す（thumbnail click 相当 = server 昇格）。
        dispatch_repo_method(
            &state,
            "board_set_cursor",
            serde_json::json!({ "item_id": id_a }),
        )
        .await
        .expect("set_cursor A");
        let r_click = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read after click");
        assert_eq!(r_click["cursor"], id_a, "cursor が A に移った");

        // C を貼る → cursor は head でない A を見ているので **据え置き**（洗い流されない = 本丸）。
        dispatch_repo_method(&state, "show", show("C"))
            .await
            .expect("show C");
        let r3 = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read 3");
        assert_eq!(r3["cursor"], id_a, "古い item を見ていたら新着に流されない");
        assert_eq!(r3["items"].as_array().unwrap().len(), 3, "item は 3 件");

        // A を update → updatedAt が createdAt より後になる（鮮度が動く）。createdAt は保つ。
        let created_a = r3["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|it| it["id"] == serde_json::json!(id_a))
            .unwrap()["createdAt"]
            .as_str()
            .unwrap()
            .to_string();
        dispatch_repo_method(
            &state,
            "board_update",
            serde_json::json!({ "id": id_a, "content": "A-updated" }),
        )
        .await
        .expect("update A");
        let r4 = dispatch_repo_method(&state, "read_board", serde_json::json!({}))
            .await
            .expect("read 4");
        let item_a = r4["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|it| it["id"] == serde_json::json!(id_a))
            .unwrap();
        assert_eq!(
            item_a["createdAt"],
            serde_json::json!(created_a),
            "createdAt は保たれる"
        );
        assert_ne!(
            item_a["updatedAt"], item_a["createdAt"],
            "update で updatedAt が進む"
        );

        // 未知 id への set_cursor は loud error（cursor を迷子にさせない）。
        let err = dispatch_repo_method(
            &state,
            "board_set_cursor",
            serde_json::json!({ "item_id": "no-such" }),
        )
        .await;
        assert!(err.is_err(), "未知 id の set_cursor は error: {err:?}");
    }

    /// PtySlot を持たない Lane への demand_start は graceful（受理して何も張らない —
    /// Lane 起動後の再 demand 余地を残す）。
    #[tokio::test]
    async fn terminal_demand_start_without_lane_is_graceful() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "reconciled");
        assert_eq!(res["attached"], 0);
        assert!(state.terminal_pumps.read().await.is_empty());
    }

    /// S3: terminal_write の base64 入力が実 PTY に届き (echo 出力で確認)、 terminal_resize が
    /// status ok を返す。 surface→Daemon→repo control の終端 = repo dispatch の責務範囲を検証する。
    #[tokio::test]
    async fn terminal_write_reaches_pty_and_resize_ok() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string();

        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // PTY 出力を write 前に購読 (echo を取りこぼさない)。
        let mut out = state
            .lane_pool
            .read()
            .await
            .subscribe_output(&addr, None)
            .expect("subscribe_output");

        // シェル初期化待ち。
        tokio::time::sleep(Duration::from_millis(500)).await;

        // terminal_write: "echo VP_S3_OK" を PtySlot に届ける。 行確定の改行は OS 依存
        // (Unix shell は LF、 cmd.exe(ConPTY) は Enter=CR、 pty_slot の write test と同方針)。
        let echo_cmd: &[u8] = if cfg!(windows) {
            b"echo VP_S3_OK\r"
        } else {
            b"echo VP_S3_OK\n"
        };
        let data = base64::engine::general_purpose::STANDARD.encode(echo_cmd);
        let res = dispatch_repo_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": lane, "data": data }),
        )
        .await
        .expect("terminal_write");
        assert_eq!(res["status"], "ok");

        // 出力に "VP_S3_OK" が現れる (= 入力が実 PTY に届いた)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), out.recv()).await {
                Ok(Ok(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    // ConPTY は DSR (`\x1b[6n` = カーソル位置問い合わせ) の応答を端末側から
                    // 受け取るまで描画を進めない。 本番は xterm.js が応答するが、 test では
                    // 端末役として terminal_write 経由で応答する (pty_slot の write test と同型)。
                    if text.contains("\u{1b}[6n") {
                        let dsr = base64::engine::general_purpose::STANDARD.encode(b"\x1b[1;1R");
                        let _ = dispatch_repo_method(
                            &state,
                            "terminal_write",
                            serde_json::json!({ "lane": lane, "data": dsr }),
                        )
                        .await;
                    }
                    if text.contains("VP_S3_OK") {
                        found = true;
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        assert!(found, "terminal_write の入力が PTY 出力に反映されない");

        // terminal_resize: status ok + cols/rows echo。
        let res = dispatch_repo_method(
            &state,
            "terminal_resize",
            serde_json::json!({ "lane": lane, "cols": 120, "rows": 40 }),
        )
        .await
        .expect("terminal_resize");
        assert_eq!(res["status"], "ok");
        assert_eq!(res["cols"], 120);
        assert_eq!(res["rows"], 40);
    }

    /// replay-on-attach を **sub lane** で end-to-end 検証する。
    ///
    /// 「vp-app 再起動 → 新 xterm が後発 subscribe → 前回画面が replay で戻る」を再現:
    /// PTY 出力を先に発生させ (= replay buffer に溜める)、 その **後で** topic を新規購読し、
    /// demand_start (= reconcile_terminal_pumps → attach_output) を撃つ。 購読が出力より後でも
    /// replay snapshot 経由でマーカーが届けば、 sub でも画面復元が効くことの証明になる。
    /// sub は main と別 topic key (`vp~sub~<name>`) に載るため、 main
    /// テストとは別に経路を固める価値がある。
    #[tokio::test]
    async fn replay_on_attach_restores_screen_for_sub_lane() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        // sub lane (main とは別 topic key になる)
        let addr = LaneAddress::sub("vp", "feat-replay");
        let lane = addr.to_string();
        assert_eq!(lane, "vp/lane/feat-replay"); // canonical（名前空間つき）

        // 実 PtySlot を sub address で登録
        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // マーカーを PTY に出力させる (echo)。 この出力は「過去」= replay buffer に溜まる。
        // 改行は OS 依存 (S3 test と同方針)。 ConPTY の DSR gating は reader task が起動時に
        // 自己応答する (pty_slot の Windows 分岐) ため、 ここでは端末役の応答は不要。
        let marker = "VP_SUB_REPLAY_MARKER";
        let echo_cmd: Vec<u8> = if cfg!(windows) {
            format!("echo {marker}\r").into_bytes()
        } else {
            format!("echo {marker}\n").into_bytes()
        };
        {
            let pool = state.lane_pool.write().await;
            pool.write_to_lane(&addr, None, &echo_cmd)
                .expect("write to PTY");
        }

        // マーカーが replay buffer に確実に入るまで待つ (PtySlot が echo を読み終える猶予)。
        tokio::time::sleep(Duration::from_millis(800)).await;

        // ここで初めて topic を新規購読する (= 再起動後の新 xterm。 出力より後発)。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub_id, mut srx) = state.topic_router.subscribe(&topic).await;

        // demand_start → reconcile_terminal_pumps → attach_output → replay 先頭配送。
        let res = dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "reconciled");
        assert_eq!(res["attached"], 1, "sub lane に pump が張れるはず");

        // 後発購読でも replay 経由でマーカーが届く (= 画面復元)。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut seen = String::new();
        let mut found = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), srx.recv()).await {
                Ok(Some((
                    got_topic,
                    RepoMessage::LaneTerminalOutput {
                        lane: l,
                        session: _,
                        data,
                    },
                ))) => {
                    assert_eq!(got_topic, topic);
                    assert_eq!(l, lane, "message は full lane address を載せる");
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                    if seen.contains(marker) {
                        found = true;
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            found,
            "sub lane の後発 attach で replay されず画面が復元しない (seen={seen:?})"
        );
    }

    /// doc 50 §4.6 A6: demand_start が lane の**各 session** に pump を張り、共有 topic に
    /// session stamp 付きで route する（Design B）。2 slot を立て、両 session の出力が
    /// それぞれ正しい `session` field で届くことを検証する。
    #[tokio::test]
    async fn reconcile_covers_all_sessions() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-multi");
        let lane = addr.to_string();

        // root（None）と 2 枚目の session（Some(2)）を立てる。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn root");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
        }
        // root の実 key（fresh lane の既定）を控える。もう片方は 2。
        let sessions = state.lane_pool.read().await.slot_sessions(&addr);
        assert_eq!(sessions.len(), 2, "root + session 2 の 2 枚");
        let root_key = *sessions.iter().find(|&&k| k != 2).expect("root key");

        // 各 slot に別マーカーを echo（過去出力 = replay buffer に溜まる）。
        let nl = if cfg!(windows) { "\r" } else { "\n" };
        {
            let pool = state.lane_pool.write().await;
            pool.write_to_lane(&addr, None, format!("echo VP_ROOT_MARK{nl}").as_bytes())
                .expect("write root");
            pool.write_to_lane(&addr, Some(2), format!("echo VP_SESS2_MARK{nl}").as_bytes())
                .expect("write s2");
        }
        tokio::time::sleep(Duration::from_millis(800)).await;

        // 後発 subscribe → demand_start → 全 session に pump。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");

        // session 別に受信を畳み、両マーカーが正しい session field で届くまで待つ。
        let mut by_session: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), srx.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, data, .. }))) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    by_session
                        .entry(session)
                        .or_default()
                        .push_str(&String::from_utf8_lossy(&bytes));
                    let root_ok = by_session
                        .get(&root_key)
                        .is_some_and(|s| s.contains("VP_ROOT_MARK"));
                    let s2_ok = by_session
                        .get(&2)
                        .is_some_and(|s| s.contains("VP_SESS2_MARK"));
                    if root_ok && s2_ok {
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            by_session
                .get(&root_key)
                .is_some_and(|s| s.contains("VP_ROOT_MARK")),
            "root session の出力が root_key stamp で届く (got={by_session:?})"
        );
        assert!(
            by_session
                .get(&2)
                .is_some_and(|s| s.contains("VP_SESS2_MARK")),
            "session 2 の出力が session=2 stamp で届く (got={by_session:?})"
        );
        // マーカーが session をまたいで混ざらない（振り分けの健全性）。
        assert!(
            !by_session
                .get(&root_key)
                .is_some_and(|s| s.contains("VP_SESS2_MARK")),
            "root stream に session 2 のマーカーが混ざらない"
        );
    }

    /// reconcile が **差し替わった slot だけ**に replay を撃ち、兄弟 pane を乱さないこと
    /// （team-b 10 回目の regression の doc 53 R2 版）。
    ///
    /// pump の張り直しは client に `REPLAY_CLEAR_PREFIX` + 全 replay を送る。旧実装は呼び手が
    /// `only` 引数で範囲を判断していた（判断を間違えると**触っていない pane が clear されて
    /// scroll 位置も飛ぶ**）。reconcile では「pid 一致は触らない」の照合が同じ保護を構造で
    /// 与える — 呼び手は範囲を指定できない（引数が存在しない）。
    ///
    /// 観測は **client が見るもの**で行う: slot 差替 + reconcile 後に流れてくる出力の
    /// session stamp が、差し替わった session だけであること。
    /// **GUI が再起動したら replay を流し直す**（doc 53 §6.5.0 の最終段、2026-07-26）。
    ///
    /// GUI プロセスが入れ替わっても **slot は生きたまま**なので、pump の identity（slot pid）は
    /// 一致する。旧実装はそれを「変化なし」と判定して attach を skip し、**新しい GUI に過去の
    /// 画面が届かなかった**（console が黒いまま = 実機で観測）。
    ///
    /// pump が答えるべきは 2 つの別の問い:
    /// - **張り直すべきか**（server 側の生産）→ slot pid
    /// - **replay を流すべきか**（client が画面を持っているか）→ **購読の世代**
    ///
    /// 1 つの述語で兼ねていたのを分けた（[[one-predicate-three-properties]]）。
    ///
    /// 壊し方: attach 判定を `current.get(s) != Some(pid)`（pid だけ）に戻すと、②の
    /// 「再購読後に replay が届く」が落ちる。
    #[cfg(unix)]
    #[tokio::test]
    async fn reconnecting_client_gets_replay_even_when_slot_is_unchanged() {
        use super::reconcile_terminal_pumps;
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-reconnect");
        let lane = addr.to_string();
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));

        // slot を 1 本立てて、識別できる出力を出しておく（= replay の中身になる）。
        {
            let mut pool = state.lane_pool.write().await;
            let (slot, rx) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn");
            pool.insert_pty_slot(addr.clone(), None, slot, rx);
            pool.write_to_lane(&addr, None, b"echo VP_RECONNECT_MARK\n")
                .expect("write");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;

        // ① 最初の client が購読 → pump が張られ replay が流れる。
        let (sub1, _rx1) = state.topic_router.subscribe(&topic).await;
        reconcile_terminal_pumps(&state, &lane).await;
        let pid_before = {
            let pumps = state.terminal_pumps.read().await;
            pumps
                .get(&lane)
                .and_then(|m| m.values().next())
                .map(|p| p.slot_pid)
        };
        assert!(pid_before.is_some(), "前提: pump が張られている");

        // ① の client に replay が届ききるのを待ってから捨てる（= 以降 live 出力は流れない）。
        // ⚠️ ここで待たないと ② の観測が **replay ではなく live 出力**を拾ってしまい、
        // pid だけの旧判定でも緑になる（テストが性質を守らない）。
        tokio::time::sleep(Duration::from_millis(800)).await;
        drop(_rx1);

        // ② client が入れ替わる（GUI 再起動）。**slot は触らない** = pid は変わらない。
        //    旧購読の掃除は QUIC idle timeout 待ちで遅れるので、ここでは外さない（実機と同じ形）。
        let (_sub2, mut rx2) = state.topic_router.subscribe(&topic).await;
        reconcile_terminal_pumps(&state, &lane).await;

        // 新しい購読者に **過去の画面（replay）が届く**こと。
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(400), rx2.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { data, .. }))) => {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                        seen.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    if seen.contains("VP_RECONNECT_MARK") {
                        break;
                    }
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
        assert!(
            seen.contains("VP_RECONNECT_MARK"),
            "再購読した client に replay が届く（slot は同じでも購読者が変われば流し直す）: got={seen:?}"
        );

        // slot は張り替えていない（兄弟保護 = R2 の性質を壊していない）。
        let pid_after = {
            let pumps = state.terminal_pumps.read().await;
            pumps
                .get(&lane)
                .and_then(|m| m.values().next())
                .map(|p| p.slot_pid)
        };
        assert_eq!(
            pid_after, pid_before,
            "slot は差し替えていない（pump の張り直しだけ）"
        );
        state.topic_router.unsubscribe(sub1).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reconcile_touches_only_the_swapped_slot_leaving_siblings_alone() {
        use super::{dispatch_repo_method, reconcile_terminal_pumps};
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-scoped");
        let lane = addr.to_string();

        // root + 2 枚目。どちらも出力を持たせて replay buffer を非空にする。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("main");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
            pool.write_to_lane(&addr, None, b"echo VP_A\n")
                .expect("w root");
            pool.write_to_lane(&addr, Some(2), b"echo VP_B\n")
                .expect("w s2");
        }
        let sessions = state.lane_pool.read().await.slot_sessions(&addr);
        let root_key = *sessions.iter().find(|&&k| k != 2).expect("root key");
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 初回 demand で両方に pump を張る。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        // 初回 replay を吸い切る（ここまでは両 session に流れて正常）。
        //
        // ⚠️ 時間だけで待ってはいけない: login shell は rc 読み込みの分だけ起動が遅れ、root の
        // 初回出力が後段の観測窓へ漏れる。すると「触っていない root にも流れた」と誤検出して
        // 間欠的に落ちる（v0.60.0 の release ゲートで顕在化。product は正しく、測り方の race
        // だった）。まず **両 session の実出力を見る = readiness を待ち**、その後で無音まで
        // drain する。
        let mut warmed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let warm_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while warmed.len() < 2 && tokio::time::Instant::now() < warm_deadline {
            if let Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, .. }))) =
                tokio::time::timeout(Duration::from_millis(500), srx.recv()).await
            {
                warmed.insert(session);
            }
        }
        assert_eq!(
            warmed.len(),
            2,
            "両 session の初回出力が揃ってから観測に入る (warmed={warmed:?})"
        );
        while tokio::time::timeout(Duration::from_millis(400), srx.recv())
            .await
            .is_ok()
        {}

        // 変化が無ければ reconcile は何もしない（= 契機が重なっても pane は無傷）。
        let idle = reconcile_terminal_pumps(&state, &lane).await;
        assert_eq!(
            (idle.attached, idle.removed, idle.kept),
            (0, 0, 2),
            "無変化の reconcile は全 pump を keep する"
        );

        // session 2 の slot を差し替える（= restart / mode 切替で実際に起きること）。
        {
            let mut pool = state.lane_pool.write().await;
            let (s2b, rx2b) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2b");
            pool.insert_pty_slot(addr.clone(), Some(2), s2b, rx2b);
            pool.write_to_lane(&addr, Some(2), b"echo VP_B2\n")
                .expect("w s2b");
        }
        let swapped = reconcile_terminal_pumps(&state, &lane).await;
        assert_eq!(
            (swapped.attached, swapped.kept),
            (1, 1),
            "pid 照合で差し替わった session 2 だけ attach、root は keep"
        );

        // 以後届く出力の session stamp を集める。root が混ざったら兄弟を乱している。
        let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(300), srx.recv()).await {
                Ok(Some((_t, RepoMessage::LaneTerminalOutput { session, .. }))) => {
                    seen.insert(session);
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            seen.contains(&2),
            "差し替わった session 2 には replay が流れる (seen={seen:?})"
        );
        assert!(
            !seen.contains(&root_key),
            "**触っていない root には何も流れない**（lane 全体を張り直すと clear + 全 replay が \
             飛んで scroll 位置が消える）(seen={seen:?})"
        );

        // 両方の pump が生きている（片方を差し替えても他方を落とさない）。
        let pumps = state.terminal_pumps.read().await;
        let lane_pumps = pumps.get(&lane).expect("lane の pump 集合");
        assert!(
            lane_pumps.contains_key(&2) && lane_pumps.contains_key(&root_key),
            "session ごとに 1 本ずつ残る (keys={:?})",
            lane_pumps.keys().collect::<Vec<_>>()
        );
    }

    /// doc 53 R2 受け入れ条件①（doc 50 §4.7「直さないと決めた 1 件」の根治）:
    /// **demand が立った後から現れた slot** にも、次の reconcile 契機で pump が張られる。
    ///
    /// 実機の形: daemon 再起動 → GUI が購読（demand edge）→ boot 復元が 800ms×N で遅れて
    /// slot を立てる → 旧実装ではその slot に pump が張られず**永久に沈黙**した。
    /// reconcile は「復元完了 = 動詞の末尾」（lane_spawn_actor / server boot）で呼ばれ、
    /// 現在の demand（level）× 現在の slot で収束する — edge の順序に依存しない。
    #[cfg(unix)]
    #[tokio::test]
    async fn late_restored_slot_gets_pump_on_next_reconcile() {
        use super::{dispatch_repo_method, reconcile_terminal_pumps};
        use crate::daemon::pty_slot::PtySlot;
        use crate::protocol::RepoMessage;
        use crate::repo::lanes_state::LaneAddress;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-late");
        let lane = addr.to_string();

        // boot 途中の姿: root slot だけが立った時点で GUI が購読 → demand edge が先に立つ。
        {
            let mut pool = state.lane_pool.write().await;
            let (s0, rx0) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("main");
            pool.insert_pty_slot(addr.clone(), None, s0, rx0);
        }
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, mut srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        assert_eq!(
            state
                .terminal_pumps
                .read()
                .await
                .get(&lane)
                .map(|m| m.len()),
            Some(1),
            "edge 時点では root の 1 本だけ"
        );

        // 復元の続き: session 2 の slot が後から立つ（edge はもう来ない）。
        {
            let mut pool = state.lane_pool.write().await;
            let (s2, rx2) = PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("s2");
            pool.insert_pty_slot(addr.clone(), Some(2), s2, rx2);
            pool.write_to_lane(&addr, Some(2), b"echo VP_LATE_MARK\n")
                .expect("w s2");
        }
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 復元完了の契機（lane_spawn_actor / server boot 相当）→ 不足分だけ attach。
        let r = reconcile_terminal_pumps(&state, &lane).await;
        assert_eq!(
            (r.attached, r.removed, r.kept),
            (1, 0, 1),
            "後から現れた slot だけ attach、既存 root pump は無傷"
        );

        // session 2 の出力（replay 込み）が届く = 沈黙しない。
        let mut seen2 = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(400), srx.recv()).await {
                Ok(Some((
                    _t,
                    RepoMessage::LaneTerminalOutput {
                        session: 2, data, ..
                    },
                ))) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .expect("base64");
                    seen2.push_str(&String::from_utf8_lossy(&bytes));
                    if seen2.contains("VP_LATE_MARK") {
                        break;
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        assert!(
            seen2.contains("VP_LATE_MARK"),
            "後から復元された slot の出力が届く（旧実装は永久に沈黙）(seen={seen2:?})"
        );
    }

    /// PtySlot を持たない Lane への terminal_write は Err (lane 不在を上位に伝える)。
    #[tokio::test]
    async fn terminal_write_unknown_lane_errs() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;
        use base64::Engine;

        let state = build_test_app_state(None).await;
        let data = base64::engine::general_purpose::STANDARD.encode(b"x");
        let res = dispatch_repo_method(
            &state,
            "terminal_write",
            serde_json::json!({ "lane": "vp/main", "data": data }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への write は Err");
    }

    /// tmux decoupling PR1-2: lane_nudge dispatch の error 経路 3 種
    /// (lane 未指定 / parse 失敗 / lane 不在 = PtySlot 無)。 happy path は実機検証済 (design §13.6)。
    #[tokio::test]
    async fn lane_nudge_dispatch_error_paths() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res =
            dispatch_repo_method(&state, "lane_nudge", serde_json::json!({ "text": "x" })).await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_repo_method(
            &state,
            "lane_nudge",
            serde_json::json!({ "lane": "%3", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // lane 不在 (PtySlot 無)
        let res = dispatch_repo_method(
            &state,
            "lane_nudge",
            serde_json::json!({ "lane": "vp/main", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "PtySlot 無 lane への nudge は Err: {res:?}");
    }

    /// channel E (doc 34): conversation_nudge dispatch の error 経路 4 種
    /// (lane 未指定 / text 未指定 / parse 失敗 / lane 不在)。happy path は実 engine 要のため
    /// conversation_host_roundtrip (ignored) と実機 dogfood で検証。
    #[tokio::test]
    async fn conversation_nudge_dispatch_error_paths() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // text 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        assert!(res.is_err(), "text 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "lane": "%3", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // lane 不在 (ensure_chat_engine が Lane not found)
        let res = dispatch_repo_method(
            &state,
            "conversation_nudge",
            serde_json::json!({ "lane": "vp/main", "text": "x" }),
        )
        .await;
        assert!(res.is_err(), "不在 lane への nudge は Err: {res:?}");
    }

    /// doc 35 PR1: conversation_respond dispatch の error 経路 4 種
    /// (lane 未指定 / request_id 未指定 / parse 失敗 / engine 不在)。happy path は実 engine 要のため
    /// conversation_host_question_roundtrip (ignored) と実機 dogfood で検証。
    #[tokio::test]
    async fn conversation_respond_dispatch_error_paths() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // lane 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "request_id": "r1" }),
        )
        .await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        // request_id 未指定
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        assert!(res.is_err(), "request_id 未指定は Err: {res:?}");
        // parse 失敗 (lane address 形式でない)
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "lane": "%3", "request_id": "r1" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");
        // engine 不在 (respond_permission_chat が chat engine 未起動)。ensure しないので Err。
        let res = dispatch_repo_method(
            &state,
            "conversation_respond",
            serde_json::json!({ "lane": "vp/main", "request_id": "r1", "answers": {} }),
        )
        .await;
        assert!(res.is_err(), "engine 不在への respond は Err: {res:?}");
    }

    /// doc 38: session param（additive）の入口検証。省略/null は OK（focused に解決）、
    /// 型不正・0 は Err — 黙って focused に落とすと誤配送になる。
    #[test]
    fn payload_session_key_validates_additive_param() {
        use super::payload_session_key;
        // 省略 / null = None（後方互換の要）。
        assert_eq!(payload_session_key("t", &serde_json::json!({})), Ok(None));
        assert_eq!(
            payload_session_key("t", &serde_json::json!({"session": null})),
            Ok(None)
        );
        assert_eq!(
            payload_session_key("t", &serde_json::json!({"session": 2})),
            Ok(Some(2))
        );
        // 0 / 負数 / 文字列 / 小数は Err。
        for bad in [
            serde_json::json!({"session": 0}),
            serde_json::json!({"session": -1}),
            serde_json::json!({"session": "2"}),
            serde_json::json!({"session": 1.5}),
        ] {
            assert!(
                payload_session_key("t", &bad).is_err(),
                "不正な session は Err: {bad}"
            );
        }
    }

    /// doc 38: session registry RPC 3 本の error 経路（lane 未指定 / parse 失敗 / lane 不在 /
    /// session 未指定）。happy path は LanePool 側のテスト（lanes_state）が持つ。
    #[tokio::test]
    async fn conversation_session_rpc_dispatch_error_paths() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        for method in [
            "conversation_session_list",
            "conversation_session_create",
            "conversation_session_focus",
            "conversation_session_remove",
        ] {
            // lane 未指定
            let res = dispatch_repo_method(&state, method, serde_json::json!({})).await;
            assert!(res.is_err(), "{method}: lane 未指定は Err: {res:?}");
            // parse 失敗
            let res = dispatch_repo_method(
                &state,
                method,
                serde_json::json!({ "lane": "%3", "session": 1 }),
            )
            .await;
            assert!(res.is_err(), "{method}: parse 不能 lane は Err: {res:?}");
            // lane 不在（pool 空）
            let res = dispatch_repo_method(
                &state,
                method,
                serde_json::json!({ "lane": "vp/main", "session": 1 }),
            )
            .await;
            assert!(res.is_err(), "{method}: 不在 lane は Err: {res:?}");
        }
        // focus は session 必須。
        let res = dispatch_repo_method(
            &state,
            "conversation_session_focus",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        assert!(res.is_err(), "session 未指定の focus は Err: {res:?}");
    }

    /// tmux decoupling PR2 → capture error 明確化（2026-07-19）: lane_capture dispatch の error 経路。
    /// 未指定 / parse 不能 / pool 不在（lane 不在）/ chat mode lane（console 無しが正常）を分岐して返す。
    #[tokio::test]
    async fn lane_capture_dispatch_error_paths() {
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

        // doc 53 R1: chat 案内の分岐が registry（root_mode）直読になったため、state dir を
        // 隔離して registry に書く（隔離しないと実 state を読み書きしてしまう）。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(&state, "lane_capture", serde_json::json!({})).await;
        assert!(res.is_err(), "lane 未指定は Err: {res:?}");
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": "some-label" }),
        )
        .await;
        assert!(res.is_err(), "parse 不能 lane は Err: {res:?}");

        // pool に実在しない lane = 「lane 不在」。
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await;
        let err = res.expect_err("pool 不在 lane の capture は Err");
        assert!(
            err.contains("lane 不在"),
            "pool 不在は『lane 不在』を返す: {err}"
        );

        // pool に実在して root mode==Chat の lane = 「chat mode の lane に console はありません」。
        // chat lane は term_attach を持たないので capture_lane は None だが、これは正常状態。
        // mode は registry（SSOT）に書く — 案内分岐の読み手（root_mode 直読）と同じ経路を通す。
        let addr = LaneAddress::sub("vp", "chat-x");
        crate::lane::session_registry::set_root_mode(
            "vp",
            "chat-x",
            "claude",
            crate::lane::session_registry::SessionMode::Gui,
        )
        .expect("test registry へ root mode を書けること");
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "claude".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: std::env::temp_dir().to_string_lossy().to_string(),
                sub_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                agent_name: None,
                flow_state: None,
            });
        }
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": addr.to_string() }),
        )
        .await;
        let err = res.expect_err("chat mode lane の capture は Err");
        assert!(
            err.contains("chat mode"),
            "chat mode lane は専用メッセージを返す: {err}"
        );
    }

    /// doc 46 P5: slot が (lane, session) key になったので、**UI を通さずに枚数と中身を読む口**を
    /// 用意した（doc 47 §7 成立条件② — 「読み手のない書き込み」を作らない）。
    /// `lane_slots` の一覧と、`lane_capture --session` の指し先不在エラーを固定する。
    #[cfg(unix)]
    #[tokio::test]
    async fn lane_slots_lists_every_session_slot() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

        // slot_inventory は root を registry から解決する → tempdir に隔離。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;

        let addr = LaneAddress::root("vp");
        let res = dispatch_repo_method(
            &state,
            "lane_slots",
            serde_json::json!({ "lane": addr.to_string() }),
        )
        .await;
        assert!(
            res.expect_err("pool 不在 lane は Err")
                .contains("lane 不在"),
            "pool に居ない lane は『lane 不在』"
        );

        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "shell".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                sub_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                agent_name: None,
                flow_state: None,
            });
            for key in [1u32, 2] {
                let (slot, rx) = PtySlot::spawn(
                    &cwd,
                    "/bin/sh",
                    &["-c".to_string(), "cat".to_string()],
                    &[],
                    80,
                    24,
                    None,
                )
                .expect("PTY spawn");
                pool.insert_pty_slot(addr.clone(), Some(key), slot, rx);
            }
        }

        let res = dispatch_repo_method(
            &state,
            "lane_slots",
            serde_json::json!({ "lane": addr.to_string() }),
        )
        .await
        .expect("lane_slots");
        assert_eq!(res["count"], 2, "同居する slot の枚数が読める: {res}");
        assert_eq!(res["slots"][0]["session"], 1);
        assert_eq!(
            res["slots"][0]["root"], true,
            "#1 が root（registry 既定形）"
        );
        assert_eq!(res["slots"][1]["session"], 2);
        assert_eq!(res["slots"][1]["root"], false);

        // capture は session 指定で slot を選べる。応答に slots を添えるので、
        // 「今どれを読んだか」「他に何枚あるか」が CLI から判る。
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": addr.to_string(), "session": 2 }),
        )
        .await
        .expect("capture #2");
        assert_eq!(res["session"], 2);
        assert_eq!(res["slots"], serde_json::json!([1, 2]));

        // 指し先が無い session は、存在する slot を添えて Err（探し方が判るエラー）。
        let err = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": addr.to_string(), "session": 9 }),
        )
        .await
        .expect_err("不在 session の capture は Err");
        assert!(
            err.contains("この lane の slot") && err.contains("[1, 2]"),
            "存在する slot を案内する: {err}"
        );
    }

    /// doc 46 P5 producer の end-to-end（RPC → CLI が見る形）: `lane_slot_new` で立てた console が
    /// `lane_slots` に **2 枚目として出る**こと。#854 が用意した容量に production の書き手が
    /// 付いたことの証跡（「読み手のない書き込み」の逆 — 読み手は先にあり、書き手が来た）。
    #[cfg(unix)]
    #[tokio::test]
    async fn lane_slot_new_adds_a_console_visible_in_lane_slots() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

        // session registry / slot_inventory の root 解決は vp_state_dir() を読む → tempdir に隔離。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string();

        // lane 不在は Err（他の lane_* dispatch と同じ入口検査）。
        for payload in [
            serde_json::json!({}),
            serde_json::json!({ "lane": "%3" }),
            serde_json::json!({ "lane": lane.clone() }),
        ] {
            let res = dispatch_repo_method(&state, "lane_slot_new", payload.clone()).await;
            assert!(res.is_err(), "入口検査: {payload} は Err: {res:?}");
        }

        // agent="shell" の lane（console に engine を注入しない）+ 既存の root slot。
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "shell".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                sub_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                agent_name: None,
                flow_state: None,
            });
            let (slot, rx) = PtySlot::spawn(
                &cwd,
                "/bin/sh",
                &["-c".to_string(), "cat".to_string()],
                &[],
                80,
                24,
                None,
            )
            .expect("PTY spawn");
            pool.insert_pty_slot(addr.clone(), Some(1), slot, rx);
        }

        // agent 省略 = 現 root の engine を引き継ぐ（registry 不在 = lane agent の "shell"）。
        let res = dispatch_repo_method(
            &state,
            "lane_slot_new",
            serde_json::json!({ "lane": lane.clone() }),
        )
        .await
        .expect("lane_slot_new");
        assert_eq!(res["session"], 2, "新 session を採番して立てる: {res}");
        assert_eq!(res["count"], 2, "この lane の console は 2 枚に: {res}");

        let res = dispatch_repo_method(
            &state,
            "lane_slots",
            serde_json::json!({ "lane": lane.clone() }),
        )
        .await
        .expect("lane_slots");
        assert_eq!(res["count"], 2, "`vp lane slots` に 2 枚出る: {res}");
        assert_eq!(res["slots"][1]["session"], 2);
        assert_eq!(res["slots"][1]["root"], false, "同居人であって代表ではない");
        assert_eq!(res["slots"][1]["alive"], true);

        // 立てた console は `vp lane capture --session 2` で読める（UI を通さない読み手）。
        let res = dispatch_repo_method(
            &state,
            "lane_capture",
            serde_json::json!({ "lane": lane, "session": 2 }),
        )
        .await
        .expect("capture #2");
        assert_eq!(res["session"], 2);
    }

    /// **✕ の end-to-end（dispatch → 動詞 → reconcile → replay 破棄）**（doc 53 §12.4 / R3c-1）。
    ///
    /// R3c で「動詞は registry に書くだけ / 実体は reconcile が畳む」に割れたので、**配線が
    /// 繋がっているか**は handler を通してしか見えない（LanePool 単体テストは動詞と reconcile を
    /// テストが手で並べるため、本番で片方を呼び忘れても緑になる）。
    ///
    /// 併せて**順序**も固定する: `PtySlot::drop` は最終 flush で replay を disk に書き戻すので、
    /// replay 破棄が reconcile より前だと消したそばから復活する。ここでは slot に固有の目印を
    /// 出力させ、✕ の後にそれが**残っていない**ことを見る（team-b 指摘 2026-07-26）。
    #[cfg(unix)]
    #[tokio::test]
    async fn session_remove_drops_slot_and_replay_through_dispatch() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = LaneAddress::root("vp");
        let lane = addr.to_string();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();

        // agent="shell" の lane + root slot（root は ✕ できないので #2 を足して閉じる）。
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "shell".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                sub_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                agent_name: None,
                flow_state: None,
            });
        }
        let res = dispatch_repo_method(
            &state,
            "lane_slot_new",
            serde_json::json!({ "lane": lane.clone() }),
        )
        .await
        .expect("lane_slot_new");
        let session = res["session"].as_u64().expect("session") as u32;

        // #2 の slot を **replay_path 付き**で立て直し、固有の目印を出力させる。
        //
        // ⚠️ ここが**順序の罠を検出できる形**にする鍵: replay を持たない test slot だと Drop が
        // 何も書かないので、`discard_session_traces` を reconcile の**前**に動かしても緑のまま
        // 通ってしまう（= 壊し方を先に決める規律。この test は逆順にすると赤くなる）。
        let replay = crate::daemon::pty_slot::replay_file_path_session(
            &addr.repo,
            crate::repo::agent_spawner::lane_label(&addr),
            session,
        );
        {
            let (slot, mut rx) = PtySlot::spawn(
                &cwd,
                "/bin/sh",
                &["-c".to_string(), "printf GHOST_SCREEN; cat".to_string()],
                &[],
                80,
                24,
                Some(replay.clone()),
            )
            .expect("PTY spawn");
            // 出力が replay buffer に入るまで待つ（空 buffer は Drop が書かない）。
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
            state
                .lane_pool
                .write()
                .await
                .insert_pty_slot(addr.clone(), Some(session), slot, rx);
        }

        dispatch_repo_method(
            &state,
            "conversation_session_remove",
            serde_json::json!({ "lane": lane.clone(), "session": session }),
        )
        .await
        .expect("conversation_session_remove");

        let pool = state.lane_pool.read().await;
        assert!(
            !pool.slot_sessions(&addr).contains(&session),
            "✕ で PtySlot が畳まれる（動詞 → reconcile の配線が繋がっている証拠）"
        );
        assert!(
            !crate::lane::session_registry::load("vp", "main", "shell")
                .sessions
                .iter()
                .any(|s| s.key == session),
            "registry からも消える"
        );
        // 「file が消えたか」ではなく「**旧画面が残っていないか**」を見る
        // （[[verify-the-cleanup-not-just-the-disappearance]]）。
        let ghost = std::fs::read(&replay)
            .map(|b| String::from_utf8_lossy(&b).contains("GHOST_SCREEN"))
            .unwrap_or(false);
        assert!(
            !ghost,
            "閉じた session の画面が残っている（replay 破棄が reconcile より前だと \
             PtySlot::drop の最終 flush が書き戻す）: {replay:?}"
        );
    }

    /// doc 51 §1 A3b: `session_now`（`vp now` の daemon 側）が NowLine event を該当 session の
    /// conversation topic に注入する。session は message の別 field で運ぶ（doc 38 落とし穴① —
    /// topic key は per-lane のまま）。非 retained なので subscribe が先。
    #[tokio::test]
    async fn session_now_routes_nowline_to_session_topic() {
        use super::dispatch_repo_method;
        use crate::conversation::ConversationEvent;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let (_id, mut srx) = state
            .topic_router
            .subscribe("repo/conversation/data/vp~lane~main/event")
            .await;

        // ⚠️ 入力はあえて**旧世代の env 形**（`vp/root`）。session_now は parse で正規化した
        // canonical を route する契約なので、旧 env の agent からでも新名 topic に届くことを固定。
        let resp = dispatch_repo_method(
            &state,
            "session_now",
            serde_json::json!({ "lane": "vp/root", "session": 3, "text": "panic 箇所を特定中" }),
        )
        .await
        .expect("session_now");
        assert_eq!(resp["session"], 3);

        let (topic, msg) = tokio::time::timeout(std::time::Duration::from_secs(1), srx.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(topic, "repo/conversation/data/vp~lane~main/event");
        match msg {
            RepoMessage::ConversationEvent {
                lane,
                session,
                event,
            } => {
                assert_eq!(lane, "vp/lane/main", "route には canonical が流れる");
                assert_eq!(session, 3);
                assert_eq!(
                    event,
                    ConversationEvent::NowLine {
                        text: "panic 箇所を特定中".into()
                    }
                );
            }
            other => panic!("想定外の message: {other:?}"),
        }

        // 空 text は明示エラー（無音の no-op にしない）。
        let err = dispatch_repo_method(
            &state,
            "session_now",
            serde_json::json!({ "lane": "vp/main", "text": "  " }),
        )
        .await
        .expect_err("空 text は拒否");
        assert!(err.contains("text"), "エラーが理由を運ぶ: {err}");
    }

    /// conversation_set_model の可否判定は **当該 session の agent** で決まる（旧
    /// `console_set_model` の root 決め打ちは session 明示化で退役 — doc 50 session=Pane、
    /// mako 裁定 2026-07-27）。cross-engine lane（#812）で lane agent と食い違っても、
    /// **同一 lane 内で session ごとに可否が分かれる**ことを固定する。可否の真実は
    /// `EngineKind::model_choices` の空/非空 1 本（旧 `model_switchable` 述語は catalog に畳んだ）。
    #[tokio::test]
    async fn conversation_set_model_gates_on_session_agent() {
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

        // session_registry は vp_state_dir() を読む → tempdir に隔離。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;

        // sub LaneInfo を組む（chat engine 不在なので drop→ensure の engine 入替は
        // no-op — drop_chat_engine が false を返し ensure は走らない）。
        let build = |name: &str, agent: &str| LaneInfo {
            id: Default::default(),
            address: LaneAddress::sub("vp", name),
            state: LaneState::Running,
            agent: agent.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            pid: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        };

        // cross-engine lane: lane 固定 agent=codex、session 1 = codex / session 2 = claude（root）。
        crate::lane::session_registry::create_root(
            "vp",
            "mixed",
            "codex",
            "claude",
            crate::lane::session_registry::SessionMode::Tui,
        )
        .expect("claude session を root に");
        state
            .lane_pool
            .write()
            .await
            .insert(build("mixed", "codex"));
        let lane = LaneAddress::sub("vp", "mixed").to_string();

        // claude session（key=2）は catalog 非空 → 成功。永続先は **当該 session** の registry entry。
        dispatch_repo_method(
            &state,
            "conversation_set_model",
            serde_json::json!({ "lane": lane.as_str(), "session": 2, "model": "sonnet" }),
        )
        .await
        .expect("claude session は切替可（lane agent=codex に引きずられない）");
        let reg = crate::lane::session_registry::load("vp", "mixed", "codex");
        assert_eq!(
            reg.sessions
                .iter()
                .find(|s| s.key == 2)
                .and_then(|s| s.model.as_deref()),
            Some("sonnet"),
            "model が session 2 の registry entry に永続される"
        );
        assert_eq!(
            reg.sessions
                .iter()
                .find(|s| s.key == 1)
                .and_then(|s| s.model.clone()),
            None,
            "他 session は無傷（per-session — 旧 lane 単位との違いの核）"
        );

        // codex session（key=1）は catalog 空 → 拒否（同一 lane 内で session ごとに可否が分かれる）。
        let err = dispatch_repo_method(
            &state,
            "conversation_set_model",
            serde_json::json!({ "lane": lane.as_str(), "session": 1, "model": "sonnet" }),
        )
        .await
        .expect_err("codex session は拒否");
        assert!(
            err.contains("codex"),
            "拒否メッセージは session の engine(codex)を指す: {err}"
        );

        // session 未指定は Err（root 決め打ちにしない — session_set_mode と同じ規律）。
        let err = dispatch_repo_method(
            &state,
            "conversation_set_model",
            serde_json::json!({ "lane": lane.as_str(), "model": "sonnet" }),
        )
        .await
        .expect_err("session 必須");
        assert!(err.contains("session"), "{err}");
    }

    /// F6②: lane_delete dispatch e2e — sub lane を pool に作り、 lane_delete で除去できる。
    /// 二度目の delete は LaneNotFound で Err (= idempotent re-call の契約)。 Err message が
    /// "Lane not found" を含むことも固定する (MCP/CLI の idempotent 判定がこの文字列に依存)。
    #[tokio::test]
    async fn lane_delete_removes_sub_and_idempotent() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "chore");
        let address = addr.to_string();

        {
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("PTY spawn");
            let mut pool = state.lane_pool.write().await;
            // delete は lanes map (LaneInfo) を remove するので LaneInfo + PtySlot 両方を登録する。
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "claude".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                sub_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                agent_name: None,
                flow_state: None,
            });
            pool.insert_pty_slot(addr.clone(), None, slot, rx);
        }

        // cleanup=false: test lane に実 workspace dir はないので Phase 2b (fs rm) をスキップ。
        let res = dispatch_repo_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect("lane_delete");
        assert_eq!(res["deleted"], address);

        // pool から PtySlot が消えている (subscribe_output が None)。
        assert!(
            state
                .lane_pool
                .read()
                .await
                .subscribe_output(&addr, None)
                .is_none(),
            "lane_delete 後も PtySlot が pool に残っている"
        );

        // 二度目の delete は LaneNotFound で Err (idempotent re-call の契約)。
        let err = dispatch_repo_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": address, "cleanup": false }),
        )
        .await
        .expect_err("既に消えた lane の delete は Err (LaneNotFound)");
        assert!(
            err.contains("Lane not found"),
            "Err message に LaneNotFound が含まれる (MCP/CLI の idempotent 判定が依存): {err}"
        );
    }

    /// F6②: Main lane は lane_delete で拒否される (architecture rule: repo lifetime 紐付き)。
    #[tokio::test]
    async fn lane_delete_rejects_main() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // delete_lane_orchestrated は LanePool の有無に関係なく kind=Main を最初に弾く。
        let err = dispatch_repo_method(
            &state,
            "lane_delete",
            serde_json::json!({ "address": "vp/main" }),
        )
        .await
        .expect_err("Main の delete は Err");
        assert!(
            err.contains("Main"),
            "Main delete は MainCannotBeDeleted: {err}"
        );
    }

    /// F6③: lane_restart dispatch — 存在しない lane の restart は透過 retry (3 attempts) 後 Err。
    /// dispatch 配線 + restart_lane_orchestrated 到達を確認 (respawn 成功 path は実機検証で担保)。
    #[tokio::test]
    async fn lane_restart_unknown_lane_errs() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(
            &state,
            "lane_restart",
            serde_json::json!({ "address": "vp/sub/ghost" }),
        )
        .await;
        assert!(res.is_err(), "存在しない lane の restart は Err");
    }

    /// 供給 push 根治: 存在しない lane の session 変化通知は Err（黙って成功にしない）。
    #[tokio::test]
    async fn lane_session_changed_unknown_lane_errs() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({ "lane": "vp/sub/ghost" }),
        )
        .await;
        assert!(res.is_err(), "存在しない lane の session 変化通知は Err");
    }

    /// 供給 push 根治: `lane_session_changed` が `Diff::Update` を emit し、payload の
    /// engine_session_id が state file の現値（focused session 規則の re-enrich）を映す。
    /// これが Daemon lane_registry / vp-app header を追従させる push の起点になる。
    #[tokio::test]
    async fn lane_session_changed_emits_enriched_lane_update() {
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{Diff, LaneAddress, LaneInfo, LaneState, SystemEvent};
        use crate::repo::state::build_test_app_state;

        // refresh_engine_session_id は vp_state_dir() を読む — tempdir guard で隔離。
        let state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        state.lane_pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: LaneAddress::root("vp"),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-07-17T00:00:00Z".to_string(),
            pid: Some(1),
            cwd: state_dir.path().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        });
        // hook 相当の会話 id 記録（記録契機 UserPromptSubmit の後の状態）。doc 40: SSOT は registry。
        crate::lane::session_registry::set_conversation("vp", "main", "claude", 1, Some("sid-new"))
            .expect("record conversation");

        let mut rx = state.system_event_tx.subscribe();
        let res = dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({ "lane": "vp/main" }),
        )
        .await
        .expect("lane_session_changed ok");
        assert_eq!(res["status"], "ok");

        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("Diff::Update が 1s 以内に届く")
            .expect("broadcast recv");
        match event {
            SystemEvent::Lane(Diff::Update { payload }) => {
                assert_eq!(payload.address, LaneAddress::root("vp"));
                assert_eq!(
                    payload.engine_session_id.as_deref(),
                    Some("sid-new"),
                    "emit 時に state file の現値で re-enrich される"
                );
            }
            other => panic!("expected Diff::Update, got: {other:?}"),
        }
    }

    /// doc 40 §4/§6: hook の会話報告（session_id + event 付き payload）が root session の
    /// registry に記録され（旧 store への直書きは発生しない = 漏斗一本化）、Diff::Update が
    /// 新 id と sessions snapshot を運ぶ。eager（issued）でも fresh な root には即記録される
    /// = 「発行時点で chip が点く」の配線検証。
    #[tokio::test]
    async fn lane_session_changed_records_conversation_report_into_registry() {
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{Diff, LaneAddress, LaneInfo, LaneState, SystemEvent};
        use crate::repo::state::build_test_app_state;

        let state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        state.lane_pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: LaneAddress::root("vp"),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-07-18T00:00:00Z".to_string(),
            pid: Some(1),
            cwd: state_dir.path().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        });

        let mut rx = state.system_event_tx.subscribe();
        dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({
                "lane": "vp/main",
                "session_id": "sid-issued",
                "event": "issued",
            }),
        )
        .await
        .expect("lane_session_changed ok");

        // registry（SSOT）に記録され、旧 store には書かれない
        let reg = crate::lane::session_registry::load("vp", "main", "claude");
        let root_conv = reg
            .sessions
            .iter()
            .find(|s| s.key == reg.root)
            .and_then(|s| s.conversation.as_deref().map(str::to_string));
        assert_eq!(
            root_conv.as_deref(),
            Some("sid-issued"),
            "issued 報告が fresh root に即記録される（発行時点点灯の核。書き手は registry に漏斗化 —\
             doc 40 PR-2 で旧 store への直書き経路そのものが撤去済み）"
        );

        // Diff::Update が新 id + sessions snapshot を運ぶ
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("Diff::Update が 1s 以内に届く")
            .expect("broadcast recv");
        match event {
            SystemEvent::Lane(Diff::Update { payload }) => {
                assert_eq!(payload.engine_session_id.as_deref(), Some("sid-issued"));
                assert_eq!(
                    payload.cc_session_id.as_deref(),
                    Some("sid-issued"),
                    "root=claude なので channel D 契約（cc_session_id）にも同値が載る"
                );
                let sessions = payload.sessions.expect("sessions snapshot が同梱される");
                assert_eq!(sessions.root, 1);
            }
            other => panic!("expected Diff::Update, got: {other:?}"),
        }
    }

    /// doc 40 §4 / doc 46 P5 の配線: `session` を名乗った報告は**その session** に着地し、
    /// root の会話 id を上書きしない（同じ lane に console slot が同居できる前提）。
    /// 実在しない session の報告は root に化けず、何も書かない。
    #[tokio::test]
    async fn lane_session_changed_records_into_reported_session() {
        use super::dispatch_repo_method;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;

        let state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        state.lane_pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: LaneAddress::root("vp"),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: "2026-07-22T00:00:00Z".to_string(),
            pid: Some(1),
            cwd: state_dir.path().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        });
        // root(#1) は発話済み、同居人 #2 が立っている状態。
        crate::lane::session_registry::set_conversation(
            "vp",
            "main",
            "claude",
            1,
            Some("sid-root"),
        )
        .expect("root conversation");
        let k2 = crate::lane::session_registry::create(
            "vp",
            "main",
            "claude",
            "claude",
            crate::lane::session_registry::SessionMode::Tui,
            false,
        )
        .expect("create #2");

        // 同居人（#2）の hook 報告
        dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({
                "lane": "vp/main",
                "session_id": "sid-roommate",
                "event": "spoken",
                "session": k2,
            }),
        )
        .await
        .expect("lane_session_changed ok");

        let reg = crate::lane::session_registry::load("vp", "main", "claude");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some("sid-root"),
            "同居人の報告で root の会話 id（= root の --resume 先）が化けない"
        );
        assert_eq!(
            reg.sessions[1].conversation.as_deref(),
            Some("sid-roommate"),
            "報告は名乗った session に着地する"
        );

        // 実在しない session の報告 → root に落ちない（黙って root を潰さない）
        dispatch_repo_method(
            &state,
            "lane_session_changed",
            serde_json::json!({
                "lane": "vp/main",
                "session_id": "sid-ghost",
                "event": "spoken",
                "session": 99,
            }),
        )
        .await
        .expect("lane_session_changed ok（記録はしないが配線は成功）");
        let reg = crate::lane::session_registry::load("vp", "main", "claude");
        assert_eq!(
            reg.sessions[0].conversation.as_deref(),
            Some("sid-root"),
            "実在しない session の報告は root に化けない"
        );
        assert_eq!(reg.sessions.len(), 2, "session は増えない");
    }

    /// F6④: agents_list dispatch — repo-proxy ask が `{agents:[...]}` 形で返る。
    /// list_stands_cached は mise 不在 (CI) でも空 Vec に graceful degrade するので、 配線 +
    /// wire shape (agents array 常在) を CI でも固定できる (実 agent 内容は stands.rs の
    /// mise-gated test が担保)。
    #[tokio::test]
    async fn stands_list_returns_stands_array() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(&state, "agents_list", serde_json::json!({}))
            .await
            .expect("agents_list dispatch");
        assert!(
            res.get("agents").map(|s| s.is_array()).unwrap_or(false),
            "agents_list は {{agents:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lanes_list` dispatch arm が `{lanes:[...]}` 形で返る (build_lanes_snapshot 経由)。
    #[tokio::test]
    async fn lanes_list_returns_lanes_array() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let res = dispatch_repo_method(&state, "lanes_list", serde_json::json!({}))
            .await
            .expect("lanes_list dispatch");
        assert!(
            res.get("lanes").map(|s| s.is_array()).unwrap_or(false),
            "lanes_list は {{lanes:[...]}} 形で返る: {res}"
        );
    }

    /// lanes portless: `lane_create` dispatch arm が validation error を unison error frame
    /// (= Err) として返す (core の `create_sub_orchestrated` に到達している証)。
    ///
    /// doc 44 P2: 旧版は `kind != "sub"` を叩いていたが、`kind` は撤去された
    /// （lane に種別が無くなり指定の余地が消えた）。後継の validation = 開発起点の予約名拒否。
    #[tokio::test]
    async fn lane_create_rejects_reserved_name() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        let err = dispatch_repo_method(
            &state,
            "lane_create",
            serde_json::json!({ "name": crate::repo::lanes_state::ROOT_LANE_NAME }),
        )
        .await
        .expect_err("予約名は Err");
        // doc 44 §9: 判定は `validate_sub_name` に一本化された（両経路で同じ gate）。
        // message は同関数のものになるので、予約名を名指ししていることだけを見る。
        assert!(
            err.contains(crate::repo::lanes_state::ROOT_LANE_NAME) && err.contains("reserved"),
            "error は予約名である旨を含む: {err}"
        );

        // 旧 client が送る `kind` は unknown field として無視され、name だけで通ること
        // （name が空なら別の validation で弾かれる = kind に依存しない）
        let err = dispatch_repo_method(
            &state,
            "lane_create",
            serde_json::json!({ "kind": "sub", "name": "  " }),
        )
        .await
        .expect_err("空 name は Err");
        assert!(err.contains("empty"), "name 制約で弾かれる: {err}");
    }

    // =========================================================================
    // Agent 委譲 (doc 28 §4) の repo dispatch — early validation のみ。
    // 状態遷移ロジックは daemon 中央 store に移管したため (doc 28 §6)、その単体 test は
    // `capability::delegation_store` が担う。repo handler は必須 field 検証後に daemon へ proxy
    // する (daemon_wire::call) ので、ここでは Daemon 不要な早期 Err 経路だけを固定する。
    // =========================================================================

    /// delegate/complete/respond の必須 field 欠落 / 不正 outcome は Daemon 到達前に Err。
    #[tokio::test]
    async fn delegation_dispatch_validates_before_proxy() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        // delegate: doer 欠落 → Err (proxy 前)。
        assert!(
            dispatch_repo_method(
                &state,
                "delegate",
                serde_json::json!({ "task": "x", "requester": "agent@vp" }),
            )
            .await
            .is_err(),
            "delegate doer 欠落は Err"
        );
        // complete: id 欠落 → Err。
        assert!(
            dispatch_repo_method(
                &state,
                "complete",
                serde_json::json!({ "outcome": { "kind": "done", "result": "x" } }),
            )
            .await
            .is_err(),
            "complete id 欠落は Err"
        );
        // complete: outcome の kind が未知 → from_value で Err (proxy 前)。
        assert!(
            dispatch_repo_method(
                &state,
                "complete",
                serde_json::json!({ "id": "dlg-x", "outcome": { "kind": "weird" } }),
            )
            .await
            .is_err(),
            "complete 不正 outcome は Err"
        );
        // respond: answer 欠落 → Err。
        assert!(
            dispatch_repo_method(&state, "respond", serde_json::json!({ "id": "dlg-x" }))
                .await
                .is_err(),
            "respond answer 欠落は Err"
        );
    }

    /// C1 test 用の chat-mode main LaneInfo を pool に登録する（claude 不要）。
    async fn insert_test_lane(
        state: &crate::repo::state::AppState,
        repo: &str,
        mode: crate::lane::session_registry::SessionMode,
    ) -> crate::repo::lanes_state::LaneAddress {
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        let addr = LaneAddress::root(repo);
        // doc 53 R1: mode の SSOT は registry（pool cache は退役）。テストも registry に書いて
        // 読み手（root_mode 直読）と同じ経路を通す。
        crate::lane::session_registry::set_root_mode(repo, "main", "claude", mode)
            .expect("test registry へ root mode を書けること");
        state.lane_pool.write().await.insert(LaneInfo {
            id: Default::default(),
            address: addr.clone(),
            state: LaneState::Running,
            agent: "claude".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
            flow_state: None,
        });
        addr
    }

    /// conversation_submit の lane / prompt 欠落は graceful Err（claude 不要）。
    #[tokio::test]
    async fn conversation_submit_missing_fields_is_graceful() {
        use super::dispatch_repo_method;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        assert!(
            dispatch_repo_method(
                &state,
                "conversation_submit",
                serde_json::json!({ "prompt": "hi" })
            )
            .await
            .is_err(),
            "lane 欠落は Err"
        );
        assert!(
            dispatch_repo_method(
                &state,
                "conversation_submit",
                serde_json::json!({ "lane": "vp/main" })
            )
            .await
            .is_err(),
            "prompt 欠落は Err"
        );
        // pool 未登録 lane への submit も graceful Err（engine spawn は起きない）。
        assert!(
            dispatch_repo_method(
                &state,
                "conversation_submit",
                serde_json::json!({ "lane": "vp/main", "prompt": "hi" })
            )
            .await
            .is_err(),
            "未登録 lane は Err"
        );
    }

    /// doc 33 の法: mode=tui の lane への conversation_submit は Err（暗黙切替しない）。
    /// claude 不要 — mode ガードは engine spawn 前に弾く。
    #[tokio::test]
    async fn conversation_submit_rejected_in_tui_mode() {
        use super::dispatch_repo_method;
        use crate::lane::session_registry::SessionMode;
        use crate::repo::state::build_test_app_state;

        let state = build_test_app_state(None).await;
        insert_test_lane(&state, "vptest-c1-tui", SessionMode::Tui).await;
        let err = dispatch_repo_method(
            &state,
            "conversation_submit",
            serde_json::json!({ "lane": "vptest-c1-tui/main", "prompt": "hi" }),
        )
        .await
        .expect_err("tui mode は Err");
        // ⚠️ 旧 assertion は `contains("console_set_mode") || contains("mode")` で、`||` の第 2 項が
        // 何にでも当たるため **撤去済 verb を案内し続けていることを検知できなかった**（team-b review
        // 2026-07-25）。案内する verb 名そのものを固定する（動詞を rename したらここが落ちる）。
        assert!(
            err.contains("session_set_mode"),
            "現役の切替 verb を案内するメッセージであること: {err}"
        );
    }

    /// engine 非依存 replay log: codex session に会話を仕込むと、demand_start が replay_log を
    /// 読み `ReplayStart → 記録 events → ReplayEnd` を配送する（transcript を持たない engine の
    /// replay 源）。codex host の spawn は exec-free なので claude / codex CLI は不要。
    #[tokio::test]
    async fn conversation_demand_start_replays_buffered_log_for_codex_session() {
        use super::dispatch_repo_method;
        use crate::conversation::ConversationEvent;
        use crate::lane::session_registry::SessionMode;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        // replay_log / session_registry は vp_state_dir() を読む → tempdir に隔離。
        let _state_guard = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = insert_test_lane(&state, "vptest-replaylog", SessionMode::Gui).await;

        // focused な codex session #2 を作る（session=None がこれに解決される）。
        let k2 = state
            .lane_pool
            .write()
            .await
            .create_chat_session(&addr, Some("codex"), true)
            .expect("create codex session");
        assert_eq!(k2, 2);

        // #2 の replay 源に会話を仕込む（session label = "main#2"）。
        for ev in [
            ConversationEvent::MessageChunk {
                text: "codex says hi".to_string(),
            },
            ConversationEvent::TurnCompleted {
                session_id: "s".to_string(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            },
        ] {
            crate::conversation::replay_log::append("vptest-replaylog", "main#2", &ev)
                .expect("replay log append");
        }

        // conversation topic を購読（非 retained なので dispatch 前に張る）。
        let topic = "repo/conversation/data/vptest-replaylog~main/event";
        let (_id, mut srx) = state.topic_router.subscribe(topic).await;

        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-replaylog/main" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "no_session");
        assert_eq!(res["events"], 2, "仕込んだ 2 event が replay される");

        // 配送列: ReplayStart → MessageChunk → TurnCompleted → ReplayEnd。
        let mut got = Vec::new();
        for _ in 0..4 {
            let (_t, msg) = tokio::time::timeout(Duration::from_secs(2), srx.recv())
                .await
                .expect("replay event timeout")
                .expect("topic closed");
            match msg {
                RepoMessage::ConversationEvent { session, event, .. } => {
                    assert_eq!(session, 2, "session field で #2 を運ぶ");
                    got.push(event);
                }
                other => panic!("想定外の message: {other:?}"),
            }
        }
        assert_eq!(got[0], ConversationEvent::ReplayStart);
        assert_eq!(
            got[1],
            ConversationEvent::MessageChunk {
                text: "codex says hi".to_string()
            }
        );
        assert!(matches!(got[2], ConversationEvent::TurnCompleted { .. }));
        assert_eq!(got[3], ConversationEvent::ReplayEnd { in_flight: false });
    }

    /// 起動時 3 重 demand の single-flight 化（2026-07-27 fleetstage 実測の根治）: 進行中
    /// flight がある間の demand は**配送せず**合流し（status=coalesced）、rerun 予約だけ残す。
    /// 予約は flight 完了側が直列に消化する — 並行 route による event 混線（clear-prefix
    /// 収束論の破れ = 孤児 ToolCallUpdate / ReplayEnd 欠落）を構造的に排除する。
    #[tokio::test]
    async fn conversation_demand_start_coalesces_while_replay_in_flight() {
        use super::dispatch_repo_method;
        use crate::lane::session_registry::SessionMode;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let _state_guard = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = insert_test_lane(&state, "vptest-coalesce", SessionMode::Gui).await;

        // focused な codex session #2（session 省略の demand がこれに解決される）。
        let k2 = state
            .lane_pool
            .write()
            .await
            .create_chat_session(&addr, Some("codex"), true)
            .expect("create codex session");
        assert_eq!(k2, 2);

        // 進行中 flight を模擬（handler と同じ key = lane display 形 + session key）。
        assert!(state.replay_flights.begin("vptest-coalesce/main", 2));

        let topic = "repo/conversation/data/vptest-coalesce~main/event";
        let (_id, mut srx) = state.topic_router.subscribe(topic).await;

        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-coalesce/main" }),
        )
        .await
        .expect("demand_start");
        assert_eq!(res["status"], "coalesced", "進行中は合流して配送しない");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), srx.recv())
                .await
                .is_err(),
            "coalesced の demand は event を 1 つも route しない"
        );
        assert!(
            state.replay_flights.finish("vptest-coalesce/main", 2),
            "合流は rerun 予約として残る（flight 完了側が直列に消化する契約）"
        );
        assert!(!state.replay_flights.finish("vptest-coalesce/main", 2));

        // flight 終了後の demand は通常配送に戻る（begin → replay → finish で entry が残らない）。
        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-coalesce/main" }),
        )
        .await
        .expect("demand_start after flight");
        assert_eq!(res["status"], "no_session", "codex は replay_log path");
        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-coalesce/main" }),
        )
        .await
        .expect("demand_start twice");
        assert_eq!(
            res["status"], "no_session",
            "連続 demand も通る = flight entry が leak していない"
        );
    }

    /// doc 50 §4.6 A6: **root=tui のまま非 root だけ chat** の構成で replay が届く。
    ///
    /// team-b review 2026-07-25（score 92）: `handle_conversation_demand_start` の gate が lane 単位
    /// `console_mode`（= root cache）だったため、A6 が正規にサポートする構成で `not_chat` を返し、
    /// **ReplayStart すら送らずに会話が復元されなかった**。gate を「その session の mode」に直した
    /// ことを、root が tui のままである状態で固定する（root=chat の既存テストでは検出できない）。
    #[tokio::test]
    async fn conversation_demand_start_replays_non_root_chat_while_root_is_tui() {
        use super::dispatch_repo_method;
        use crate::conversation::ConversationEvent;
        use crate::lane::session_registry::SessionMode;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let _state_guard = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        // **root は tui**（= 旧 gate ならここで not_chat に落ちる）。
        let addr = insert_test_lane(&state, "vptest-nonroot-chat", SessionMode::Tui).await;

        // 非 root の chat session を作る（create_chat_session は mode=Chat で作る）。
        let k2 = state
            .lane_pool
            .write()
            .await
            .create_chat_session(&addr, Some("codex"), true)
            .expect("create chat session");

        // その session の replay 源に会話を仕込む。
        for ev in [
            ConversationEvent::MessageChunk {
                text: "non-root chat lives".to_string(),
            },
            ConversationEvent::TurnCompleted {
                session_id: "s".to_string(),
                cost_usd: None,
                context_tokens: None,
                context_window: None,
            },
        ] {
            crate::conversation::replay_log::append(
                "vptest-nonroot-chat",
                &format!("main#{k2}"),
                &ev,
            )
            .expect("replay log append");
        }

        let topic = "repo/conversation/data/vptest-nonroot-chat~main/event";
        let (_id, mut srx) = state.topic_router.subscribe(topic).await;

        // session を明示して demand（client の mode 切替後の明示 demand と同じ形）。
        let res = dispatch_repo_method(
            &state,
            "conversation_demand_start",
            serde_json::json!({ "lane": "vptest-nonroot-chat/main", "session": k2 }),
        )
        .await
        .expect("demand_start");
        assert_ne!(
            res["status"], "not_chat",
            "root が tui でも非 root の chat には replay が届く（旧 gate はここで落ちていた）"
        );
        assert_eq!(res["events"], 2, "仕込んだ 2 event が replay される");

        // 配送列の先頭が ReplayStart（= GUI の会話 clear 契機）で、session field が非 root を運ぶ。
        let (_t, msg) = tokio::time::timeout(Duration::from_secs(2), srx.recv())
            .await
            .expect("replay event timeout")
            .expect("topic closed");
        match msg {
            RepoMessage::ConversationEvent { session, event, .. } => {
                assert_eq!(session, k2, "session field で非 root を運ぶ");
                assert_eq!(event, ConversationEvent::ReplayStart);
            }
            other => panic!("想定外の message: {other:?}"),
        }
    }

    /// doc 50 §4.6 A6: `lane_slot_new` は **新 slot に pump を張る**。
    ///
    /// team-b review 2026-07-25 の指摘（score 87）: pump の起動契機は ①demand hook（購読者数
    /// 0→1 のエッジ）②act 切替 の 2 つしかなく、slot 追加はどちらでもなかった。しかも client の
    /// terminal 購読は lane 単位 1 本なので、root が既に tui で購読済の lane に「+ New」で 2 枚目を
    /// 足すと購読者数は 1→1 のまま = エッジが立たない → **入力は通るのに出力だけ永久に沈黙**。
    ///
    /// 「既に pump がある lane に slot を足す」順序（= 実運用で最頻の経路）で回帰を止める。
    #[tokio::test]
    async fn lane_slot_new_attaches_pump_to_the_new_slot() {
        use super::dispatch_repo_method;
        use crate::daemon::pty_slot::PtySlot;
        use crate::repo::lanes_state::{LaneAddress, LaneInfo, LaneState};
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let shell = default_test_shell();
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let addr = LaneAddress::sub("vp", "feat-slotpump");
        let lane = addr.to_string();

        // lane を登録（agent=shell = console に engine を注入しない）+ root slot を立てる。
        {
            let mut pool = state.lane_pool.write().await;
            pool.insert(LaneInfo {
                id: Default::default(),
                address: addr.clone(),
                state: LaneState::Running,
                agent: "shell".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                pid: None,
                cwd: cwd.clone(),
                sub_status: None,
                cc_session_id: None,
                sessions: None,
                engine_session_id: None,
                agent_name: None,
                flow_state: None,
            });
            let (slot, rx) =
                PtySlot::spawn(&cwd, &shell, &[], &[], 80, 24, None).expect("spawn root");
            pool.insert_pty_slot(addr.clone(), None, slot, rx);
        }
        // 購読を張って demand_start → root に pump（= 「既に購読済」の状態を作る）。
        let topic = format!("repo/terminal/data/{}/out", lane.replace('/', "~"));
        let (_sub, _srx) = state.topic_router.subscribe(&topic).await;
        dispatch_repo_method(
            &state,
            "terminal_demand_start",
            serde_json::json!({ "lane": lane }),
        )
        .await
        .expect("demand_start");
        let pumps_before = state
            .terminal_pumps
            .read()
            .await
            .get(&lane)
            .map(|m| m.len())
            .unwrap_or(0);
        assert_eq!(pumps_before, 1, "root の pump が 1 本張られている");

        // ここで「+ New」相当（lane_slot_new）。**購読者数は 1→1 のままでエッジは立たない**。
        let res = dispatch_repo_method(
            &state,
            "lane_slot_new",
            serde_json::json!({ "lane": lane, "agent": "shell" }),
        )
        .await
        .expect("lane_slot_new");
        let new_session = res["session"].as_u64().expect("session") as u32;

        // それでも新 slot に pump が張られている（動詞の末尾 reconcile が不足分を検知するため）。
        tokio::time::sleep(Duration::from_millis(200)).await;
        let pumps = state.terminal_pumps.read().await;
        let lane_pumps = pumps.get(&lane).expect("lane の pump map");
        assert_eq!(
            lane_pumps.len(),
            2,
            "root + 新 session の 2 本（got keys={:?}）",
            lane_pumps.keys().collect::<Vec<_>>()
        );
        assert!(
            lane_pumps.contains_key(&new_session),
            "新 session の pump がある（無いと出力が永久に届かない）"
        );
    }

    // doc 50 §4.6 A6: 旧 `console_set_mode_validates_and_transitions` は動詞ごと撤去した
    // （検証内容は下の `session_set_act_*` が session 単位で引き継いでいる）。

    /// doc 50 §4.6 A6: `session_set_mode` は session 明示必須で、その session の mode を切り替える。
    /// 旧 `console_set_mode`（root 固定）と同じ実体に委譲されるが、session を省略できない。
    #[tokio::test]
    async fn session_set_mode_requires_session_and_switches_that_session() {
        use super::dispatch_repo_method;
        use crate::lane::session_registry::{self, SessionMode};
        use crate::repo::state::build_test_app_state;

        // set_session_mode は registry（disk = vp_state_dir）を読み書きする → tempdir に隔離。
        // ⚠️ 隔離しないと実 state dir を汚染し、**2 回目以降の run で mode が既に chat のため
        // no-op 早期 return して落ちる**（= 実行順・実行回数に依存する偽の緑/赤）。
        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = insert_test_lane(&state, "vptest-ssa", SessionMode::Tui).await;
        let lane = "vptest-ssa/main";

        // session 省略は Err（root 決め打ちにしない = 誤配送を黙って起こさない）。
        assert!(
            dispatch_repo_method(
                &state,
                "session_set_mode",
                serde_json::json!({ "lane": lane })
            )
            .await
            .is_err(),
            "session 未指定は Err"
        );
        // mode 不正も Err。
        assert!(
            dispatch_repo_method(
                &state,
                "session_set_mode",
                serde_json::json!({ "lane": lane, "session": 1, "mode": "bogus" })
            )
            .await
            .is_err(),
            "mode 不正は Err"
        );

        // root session の tui→chat（engine-less でも registry が更新される）。
        let root = crate::repo::lanes_state::LanePool::root_session_key(&addr);
        let res = dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": root, "mode": "gui" }),
        )
        .await
        .expect("tui→chat ok");
        assert_eq!(res["mode"], "gui");
        assert_eq!(res["session"], root);
        // registry（disk SSOT）に mode が永続し、読み手（root_mode 直読）が新しい値を見る。
        // doc 53 R1: 旧 root cache は退役 — 「cache も追従する」の性質は「読み手が SSOT を
        // 直読する」に言い直された（§8.6: テストの消滅 = 性質の消滅にしない）。
        assert_eq!(
            session_registry::root_mode(&addr.repo, "main"),
            SessionMode::Gui,
            "root session の mode が registry に永続する"
        );
        assert_eq!(
            state.lane_pool.read().await.root_mode(&addr),
            SessionMode::Gui,
            "boot spawn / nudge 配送が使う読み手（pool.root_mode）が新しい mode を見る"
        );

        // 同一 mode への再切替は no-op Ok。
        dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": root, "mode": "gui" }),
        )
        .await
        .expect("chat→chat no-op ok");

        // 実在しない session は Err（registry の住人だけが切り替えられる）。
        assert!(
            dispatch_repo_method(
                &state,
                "session_set_mode",
                serde_json::json!({ "lane": lane, "session": 99, "mode": "gui" })
            )
            .await
            .is_err(),
            "実在しない session は Err"
        );
    }

    /// doc 50 §4.6 A6 ②: Chat 化の可否は **その session の agent** の能力で決まる。
    ///
    /// GUI 側 badge も同じ能力表（`chat_capable`）で gating するが、**server が最終的な門番**。
    /// root 決め打ちにしないこと（非 root は engine が違いうる — shell の console を chat に
    /// しようとしても、その session の agent で弾く）を固定する。
    #[tokio::test]
    async fn session_set_mode_gui_requires_chat_capable_agent() {
        use super::dispatch_repo_method;
        use crate::lane::session_registry::{self, SessionMode};
        use crate::repo::state::build_test_app_state;

        let _state_dir = crate::test_env::state_dir_async().await;
        let state = build_test_app_state(None).await;
        let addr = insert_test_lane(&state, "vptest-cap", SessionMode::Tui).await;
        let lane = "vptest-cap/main";

        // lane の agent は conversation（chat 可能）だが、**非 root に shell の session** を足す。
        let shell = session_registry::create(
            &addr.repo,
            "main",
            "claude",
            "shell",
            SessionMode::Tui,
            false,
        )
        .expect("shell session 作成");

        // その session を chat にしようとすると、**その session の agent（shell）**で弾かれる。
        let err = dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": shell, "mode": "gui" }),
        )
        .await
        .expect_err("shell session の chat 化は Err");
        assert!(
            err.contains("shell") || err.contains("gui"),
            "エラーは能力不足を説明する（got={err}）"
        );

        // 逆向き（chat → tui）は engine を問わず可能（tui は login shell に流し込むだけ）。
        // shell session は既に tui なので no-op Ok になることで「拒否されない」ことを示す。
        dispatch_repo_method(
            &state,
            "session_set_mode",
            serde_json::json!({ "lane": lane, "session": shell, "mode": "tui" }),
        )
        .await
        .expect("tui 方向は engine を問わず通る");
    }

    /// 実機統合: mode=chat の lane への conversation_submit が engine を lazy spawn し、ConversationEvent が
    /// `repo/conversation/data/{lane}/event` topic に届く repo 終端 round-trip を検証する。
    /// `cargo test -p vantage-point --ignored conversation_submit_roundtrip`（要 claude CLI）。
    #[tokio::test]
    #[ignore = "requires claude CLI + subscription"]
    async fn conversation_submit_roundtrip() {
        use super::dispatch_repo_method;
        use crate::conversation::ConversationEvent;
        use crate::lane::session_registry::SessionMode;
        use crate::protocol::RepoMessage;
        use crate::repo::state::build_test_app_state;
        use std::time::Duration;

        let state = build_test_app_state(None).await;
        // doc 33: submit には mode=chat の lane が pool に要る。
        // repo 名はテスト固有にする — 実在 repo だと registry の会話 id が本物の
        // session id を返し、temp cwd との不整合で resume が失敗する。
        insert_test_lane(&state, "vptest-c1-rt", SessionMode::Gui).await;
        // conversation data は非 retained なので submit 前に subscribe。
        let (_id, mut srx) = state
            .topic_router
            .subscribe("repo/conversation/data/vptest-c1-rt~main/event")
            .await;

        dispatch_repo_method(
            &state,
            "conversation_submit",
            serde_json::json!({ "lane": "vptest-c1-rt/main", "prompt": "Reply with exactly: PONG" }),
        )
        .await
        .expect("conversation_submit ok");

        let mut got_init = false;
        let mut text = String::new();
        let mut got_done = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(90), srx.recv()).await {
                Ok(Some((_topic, RepoMessage::ConversationEvent { event, .. }))) => match event {
                    ConversationEvent::SessionInit { .. } => got_init = true,
                    ConversationEvent::MessageChunk { text: t } => text.push_str(&t),
                    ConversationEvent::TurnCompleted { .. } => {
                        got_done = true;
                        break;
                    }
                    ConversationEvent::Error { message } => panic!("engine error: {message}"),
                    _ => {}
                },
                _ => break,
            }
        }

        assert!(got_init, "SessionInit が topic に届く");
        assert!(got_done, "TurnCompleted が topic に届く");
        assert!(text.to_uppercase().contains("PONG"), "本文 PONG: {text:?}");
    }
}
