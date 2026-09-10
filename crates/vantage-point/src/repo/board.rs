//! board モデル — board Canvas を scope 別の永続 board にする server-authoritative 実装（doc 52、2026-07-15）。
//!
//! board = show した item の scope 別永続リスト（repo が唯一の truth を持つ）。 mcp__show 着信で repo が
//! item を生成し DB に durable append、 更新後 board を BoardUpdated（retained topic
//! `.../state/board/{scope}/{lane}`）で broadcast する。 webview はそれを購読して board を置換する view
//! （旧 Show 揮発 stack / webview self-save は廃止）。 lane board は lane ごと、 proj board は repo 共有
//! （lane_name=''）。 vp board（全体）は cross-project 共有で Daemon store が要るため Phase 2。
//!
//! Unison method: `show` / `clear` / `board_update` / `read_board` / `board_delete_item` / `board_clear` /
//! `board_set_cursor`（受付は `unison_server::dispatch_repo_method`）。`seed_boards` は起動時に DB の board を
//! retained topic へ投入する（`repo/server.rs`）。
//!
//! 棚卸し 9-2 段階 2（doc 63 §2）: handler は `RepoState` を受け取らず、要る 3 つ（`repo_dir` /
//! `vpdb` / `hub`）だけを束ねた [`BoardContext`] を受け取る。呼び手は `RepoState::board()` で作る。
//! この module は `RepoState` を import しない — board が State の何を読むかは、この struct の
//! field が全部で、それ以外に手が届かない。

use super::hub::Hub;
use crate::db::SharedVpDb;
use crate::protocol::{BoardItem, Content, RepoMessage};

/// board 操作が要る依存だけの借用 context（doc 63 §2 段階 2）。
///
/// `RepoState` から 3 field を borrow するだけで、cache や channel は新設しない。同期呼び出し用の
/// 借用なので `Copy`（spawn 先に持ち込むなら必要な handle を個別に clone する — `Arc<RepoState>` を
/// 隠したり `Deref` で全 field を公開したりしない、doc 63 §7）。
#[derive(Clone, Copy)]
pub(crate) struct BoardContext<'a> {
    /// board の永続 key（生パス。`lane_db_key` と違って正規化しない — 不変条件 2）
    pub repo_dir: &'a str,
    /// `None` = DB 接続失敗（daemon の degrade）。handler は `Err` / no-op で返す
    pub vpdb: Option<&'a SharedVpDb>,
    /// `BoardUpdated` の broadcast 先（retained topic → canvas channel → webview）
    pub hub: &'a Hub,
}

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
        .filter(|s| !s.is_empty() && *s != crate::repo::lane::ROOT_LANE_NAME)
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
    ctx: BoardContext<'_>,
    board_scope: &str,
    lane_name: &str,
    broadcast_lane: Option<String>,
) -> Result<(), String> {
    let Some(vpdb) = ctx.vpdb else {
        return Ok(());
    };
    let rec = vpdb
        .load_board(ctx.repo_dir, board_scope, lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board load: {}", e))?;
    let (items, cursor) = extract_stack(rec.as_ref());
    ctx.hub.broadcast(RepoMessage::BoardUpdated {
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
pub(crate) async fn handle_canvas_command(
    ctx: BoardContext<'_>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = ctx.vpdb else {
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
                ctx.repo_dir,
                &board_scope,
                &lane_name,
                BOARD_PANE_ID,
                &item,
                BOARD_CAPACITY,
            )
            .await
            .map_err(|e| format!("board append: {}", e))?;
            broadcast_board(ctx, &board_scope, &lane_name, bc_lane).await?;
            Ok(serde_json::json!({"status": "ok"}))
        }
        RepoMessage::Clear { lane, scope, .. } => {
            let (board_scope, lane_name, bc_lane) = board_key(scope.as_deref(), lane.as_deref());
            vpdb.clear_board(ctx.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
                .await
                .map_err(|e| format!("board clear: {}", e))?;
            broadcast_board(ctx, &board_scope, &lane_name, bc_lane).await?;
            Ok(serde_json::json!({"status": "ok"}))
        }
        _ => Err("canvas_command: show/clear 以外のメッセージ".to_string()),
    }
}

/// webview からの board item 削除（thumbnail ✕）。 DB から消して更新後 board を broadcast。
pub(crate) async fn handle_board_delete_item(
    ctx: BoardContext<'_>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = ctx.vpdb else {
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
        ctx.repo_dir,
        &board_scope,
        &lane_name,
        BOARD_PANE_ID,
        &item_id,
    )
    .await
    .map_err(|e| format!("board delete: {}", e))?;
    broadcast_board(ctx, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// webview からの board clear（Clear ボタン）。 = mcp clear と同じ結果。
pub(crate) async fn handle_board_clear(
    ctx: BoardContext<'_>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = ctx.vpdb else {
        return Err("board_clear: vpdb 未初期化".to_string());
    };
    let (board_scope, lane_name, bc_lane) = board_key(
        payload.get("scope").and_then(|v| v.as_str()),
        payload.get("lane").and_then(|v| v.as_str()),
    );
    vpdb.clear_board(ctx.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board clear: {}", e))?;
    broadcast_board(ctx, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// mcp__update を board（repo truth）に反映する（doc 52 §5 — id 指定 in-place 置換）。
///
/// read-first 前提: id は AI が read_board で読んだ現在の item id。存在確認して**無ければ loud
/// error**（`show` 二挙動を避け `update` に分けた狙い = 静かな重複を作らない）。存在すれば
/// content / contentType を差し替え（id/title/createdAt は保持）→ 更新後 board を broadcast。
pub(crate) async fn handle_board_update(
    ctx: BoardContext<'_>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = ctx.vpdb else {
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
        .load_board(ctx.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
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
        ctx.repo_dir,
        &board_scope,
        &lane_name,
        BOARD_PANE_ID,
        &item_id,
        &content,
        &content_type,
    )
    .await
    .map_err(|e| format!("board update: {}", e))?;
    broadcast_board(ctx, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// webview からの cursor 移動（thumbnail click）を board（repo truth）に反映する（doc 52 §5 —
/// cursor の server 昇格。view-local だった cursor を repo-authoritative にし、scrollback 規則
/// = 「head を見ているときだけ新 show に follow」の判定を server が持てるようにする）。
///
/// read-first: item_id が board に居ることを確認してから set（無い id で cursor を迷子に
/// させない）。set 後の board を broadcast（cursor が真として全 view に配られる）。
pub(crate) async fn handle_board_set_cursor(
    ctx: BoardContext<'_>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = ctx.vpdb else {
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
        .load_board(ctx.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
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
        ctx.repo_dir,
        &board_scope,
        &lane_name,
        BOARD_PANE_ID,
        &item_id,
    )
    .await
    .map_err(|e| format!("board set_cursor: {}", e))?;
    broadcast_board(ctx, &board_scope, &lane_name, bc_lane).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// mcp__read_board を処理する（doc 52 §4 中継台 + §5 identity 兼務）。
///
/// 呼び出し元 lane の board を **id 付き全文**で返す（AI は content/title で「どれか」を認識し、
/// id で update / creo 中継の対象を指す）。read-only（broadcast しない）。
pub(crate) async fn handle_board_read(
    ctx: BoardContext<'_>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(vpdb) = ctx.vpdb else {
        return Err("read_board: vpdb 未初期化".to_string());
    };
    let (board_scope, lane_name, _) = board_key(
        payload.get("scope").and_then(|v| v.as_str()),
        payload.get("lane").and_then(|v| v.as_str()),
    );
    let rec = vpdb
        .load_board(ctx.repo_dir, &board_scope, &lane_name, BOARD_PANE_ID)
        .await
        .map_err(|e| format!("board load: {}", e))?;
    let (items, cursor) = extract_stack(rec.as_ref());
    Ok(serde_json::json!({ "items": items, "cursor": cursor }))
}

/// repo 起動時に DB の全 board を retained topic に seed する。
///
/// webview が canvas channel を購読した瞬間、 retained BoardUpdated として全 board が初期配信される
/// （別 load 経路が不要）。 空 board / 別 pane_id の row は skip。
pub(crate) async fn seed_boards(ctx: BoardContext<'_>) {
    let Some(vpdb) = ctx.vpdb else {
        return;
    };
    let rows = match vpdb.list_pane_contents(ctx.repo_dir).await {
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
        ctx.hub.broadcast(RepoMessage::BoardUpdated {
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

#[cfg(test)]
mod tests {
    /// 棚卸し 9-2 段階 2（doc 63 §2）: **board の handler は `RepoState` 無しで動く。**
    ///
    /// mem db と `Hub` だけで [`BoardContext`] を組み、show → read_board を通し、`hub` の購読者に
    /// `BoardUpdated` が届くことまで見る。`RepoState` を組まないこと自体が「leaf が全体 State を
    /// 知らない」の証明（handler が State の別 field を読み始めれば、ここは compile で落ちる）。
    /// 下の 2 本（`dispatch_repo_method` 経由）は `RepoState::board()` の結線側を固定する。
    #[tokio::test]
    async fn board_handlers_need_only_the_board_context() {
        use super::*;
        use crate::db::VpDb;
        use std::sync::Arc;

        let db: SharedVpDb = Arc::new(VpDb::connect_mem().await.unwrap());
        let hub = Hub::new();
        let mut rx = hub.subscribe();
        let ctx = BoardContext {
            repo_dir: "/repos/vp",
            vpdb: Some(&db),
            hub: &hub,
        };

        handle_canvas_command(
            ctx,
            serde_json::json!({
                "type": "show", "pane_id": "main",
                "content": { "markdown": "leaf" }, "append": false, "title": "t"
            }),
        )
        .await
        .expect("show");

        let read = handle_board_read(ctx, serde_json::json!({}))
            .await
            .expect("read_board");
        assert_eq!(read["items"][0]["content"], "leaf");

        match rx
            .try_recv()
            .expect("show は BoardUpdated を broadcast する")
        {
            RepoMessage::BoardUpdated { scope, items, .. } => {
                assert_eq!(scope, "lane");
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].content, "leaf");
            }
            other => panic!("BoardUpdated 以外が流れた: {other:?}"),
        }

        // vpdb 無し（DB 接続失敗の degrade）は loud error / no-op で、panic しない
        let degraded = BoardContext {
            repo_dir: "/repos/vp",
            vpdb: None,
            hub: &hub,
        };
        assert!(
            handle_board_read(degraded, serde_json::json!({}))
                .await
                .is_err()
        );
        seed_boards(degraded).await;
        assert!(
            rx.try_recv().is_err(),
            "degrade では何も broadcast しない（no-op）"
        );
    }

    /// doc 52 §4/§5: show → read_board（id 取得）→ board_update（in-place 置換）→ read_board の往復。
    /// 未知 id の update が loud error になることも固定する。
    #[tokio::test]
    async fn board_read_and_update_roundtrip() {
        use crate::db::VpDb;
        use crate::repo::state::build_test_app_state_with;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::sync::Arc;

        let db = Arc::new(VpDb::connect_mem().await.unwrap());
        let state = build_test_app_state_with("/repos/vp", Some(db)).await;

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
        use crate::db::VpDb;
        use crate::repo::state::build_test_app_state_with;
        use crate::repo::unison_server::dispatch_repo_method;
        use std::sync::Arc;

        let db = Arc::new(VpDb::connect_mem().await.unwrap());
        // schema（idx_pane_scope UNIQUE）を定義しないと show ごとに新 row になり ON DUPLICATE KEY
        // UPDATE の item 蓄積が起きない（accumulation / follow の検証に必須）。
        db.define_schema().await.unwrap();
        let state = build_test_app_state_with("/repos/vp", Some(db)).await;

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
}
