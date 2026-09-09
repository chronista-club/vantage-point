//! board（`pane_contents` table）の永続層。
//!
//! repo / lane の scope ごとに Canvas の item 列と cursor を持つ。**item の identity は
//! repo が一元発行する**（doc 52 §5）ので、ここは read-modify-write を壊さないことだけを見る。
//!
//! ## 触ってはいけない順序
//!
//! - [`VpDb::append_board_item`]: head push → cursor の昇格 → 容量 eviction の順。
//!   cursor を新 item に移すのは server 側の仕事（doc 52 §5、view-local cursor は廃止済み）
//! - [`VpDb::update_board_item`]: **in-place**（read-modify-write）で cursor を動かさない
//! - repo scope の隔離は `repo_path` 列が持つ（doc 44 §5.2）
//!
//! ⚠️ table 名 `pane_contents` は board への改名を保留してある（doc 47 の未決事項）。
//! logic 側の owner は `repo/board.rs`。設計は [doc 62](../../../../docs/design/62-db-module-layout.md)。

use anyhow::Result;

use super::VpDb;

impl VpDb {
    // =========================================================================
    // Pane Contents CRUD（Canvas ペイン状態の永続化）
    // =========================================================================

    /// ペイン状態を保存（UPSERT: repo_path + pane_id で一意）
    pub async fn upsert_pane_content(
        &self,
        repo_path: &str,
        pane_id: &str,
        content_type: &str,
        content: &str,
        title: Option<&str>,
    ) -> Result<()> {
        // lane_name='' (= main sentinel) の row として upsert。 新 schema (lane_name/stack/ui_state) は
        // ON DUPLICATE KEY UPDATE 句で **触らない** — 旧 caller (= 純粋な content / title 更新)
        // が board Canvas Stack の stack / ui_state を巻き戻さないようにする。
        self.db
            .query(
                "INSERT INTO pane_contents {
                    repo_path: $repo_path,
                    pane_id: $pane_id,
                    lane_name: '',
                    content_type: $content_type,
                    content: $content,
                    title: $title,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("content_type", content_type.to_string()))
            .bind(("content", content.to_string()))
            .bind(("title", title.map(|s| s.to_string())))
            .await
            .map_err(|e| anyhow::anyhow!("pane_content upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pane_content upsert エラー: {}", e))?;
        Ok(())
    }

    /// board Canvas Stack Model の lane scope な永続状態を upsert する (= doc 19 + pp-content-persist)。
    ///
    /// - `lane_name`: None なら main (= 内部で `''` sentinel)、 Some(name) なら sub。 UNIQUE INDEX は
    ///   (repo_path, lane_name, pane_id) のため root/sub は別 record として独立。
    /// - `stack`: Canvas Stack (= items + cursor + capacity)。 None なら未保存。
    /// - `ui_state`: visibility/collapsed/サイズ等。 None なら未保存。
    /// - `content` / `content_type` / `title` は **現在 main pane で render 中の item の reflection**
    ///   (= 旧 caller 互換)。 stack が主、 content は seek 用 fallback。
    #[allow(clippy::too_many_arguments)] // pane_contents の field count に追従、 caller (route handler) も flat に展開する
    pub async fn upsert_board_state(
        &self,
        repo_path: &str,
        lane_name: Option<&str>,
        pane_id: &str,
        content_type: &str,
        content: &str,
        title: Option<&str>,
        stack: Option<&serde_json::Value>,
        ui_state: Option<&serde_json::Value>,
    ) -> Result<()> {
        // IPC contract 上は lane_name: Option<&str> を維持しつつ、 DB row では '' sentinel に変換。
        let lane_sentinel = lane_name.unwrap_or("");
        self.db
            .query(
                "INSERT INTO pane_contents {
                    repo_path: $repo_path,
                    pane_id: $pane_id,
                    lane_name: $lane_name,
                    content_type: $content_type,
                    content: $content,
                    title: $title,
                    stack: $stack,
                    ui_state: $ui_state,
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    stack = $input.stack,
                    ui_state = $input.ui_state,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("lane_name", lane_sentinel.to_string()))
            .bind(("content_type", content_type.to_string()))
            .bind(("content", content.to_string()))
            .bind(("title", title.map(|s| s.to_string())))
            .bind(("stack", stack.cloned()))
            .bind(("ui_state", ui_state.cloned()))
            .await
            .map_err(|e| anyhow::anyhow!("board_state upsert 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board_state upsert エラー: {}", e))?;
        Ok(())
    }

    /// 特定 (repo_path, lane_name, pane_id) の board state を 1 件取得。 不在なら Ok(None)。
    ///
    /// 旧 record (= lane_name field なし) は schema DEFAULT '' で self-heal され、 main として読める。
    pub async fn load_board_state(
        &self,
        repo_path: &str,
        lane_name: Option<&str>,
        pane_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let lane_sentinel = lane_name.unwrap_or("");
        let mut result = self
            .db
            .query(
                "SELECT * FROM pane_contents
                 WHERE repo_path = $path
                   AND pane_id = $pane_id
                   AND lane_name = $lane
                 LIMIT 1",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("lane", lane_sentinel.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board_state load 失敗: {}", e))?;
        let mut records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records.pop())
    }

    // =========================================================================
    // board モデル (2026-07-15): scope 別 Canvas board の CRUD
    //
    // board = board Canvas に show した item の scope 別永続リスト（repo が唯一の truth を持つ）。
    // stack = { items: [...新→古], cursor: <id|NONE> } を pane_contents.stack に保存する。
    // キーは (repo_path, scope, lane_name, pane_id)。 lane board は lane_name で lane ごとに
    // 分離、 proj board は lane_name='' (repo 共有)。
    // =========================================================================

    /// board に item を atomic に head-push する（= mcp__show 着信 1 件）。
    ///
    /// - item を items の先頭に追加し（新→古）、 `capacity` を超えた末尾（最古）を切り、
    ///   cursor を新 item に更新する。
    /// - RMW を避け ON DUPLICATE KEY UPDATE 内の array 関数で atomic に行う
    ///   （人/agent が連続 show した際の read-modify-write race を排除）。
    /// - `item` は webview の BoardItem 形（camelCase: id/content/contentType/title/createdAt）。
    ///   top-level content/content_type/title は「現在 main で見せる item の reflection」(seek fallback)。
    pub async fn append_board_item(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item: &serde_json::Value,
        capacity: usize,
    ) -> Result<()> {
        let item_id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let content = item
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let content_type = item
            .get("contentType")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown")
            .to_string();
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        self.db
            .query(
                "INSERT INTO pane_contents {
                    repo_path: $repo_path,
                    scope: $scope,
                    lane_name: $lane_name,
                    pane_id: $pane_id,
                    content_type: $content_type,
                    content: $content,
                    title: $title,
                    stack: { items: [$item], cursor: $item_id },
                    updated_at: time::now()
                } ON DUPLICATE KEY UPDATE
                    stack = {
                        items: array::slice(array::prepend(stack.items ?? [], $item), 0, $cap),
                        -- cursor 据え置き（scrollback）は 3 条件を全て満たすときだけ: (1) NONE でない
                        -- (2) 旧 head でない（head を見ていたら follow）(3) capacity trim 後も生き残る。
                        -- (3) が無いと、最古 item を pin した状態で show が来ると cursor が指す item が
                        -- evict されて孤児化し主画面が無言で空白化する（team-b review, doc 52 §5「流されない」に反する）。
                        cursor: IF stack.cursor IS NOT NONE
                            AND stack.cursor != (stack.items ?? [])[0].id
                            AND stack.cursor IN array::slice(array::prepend(stack.items ?? [], $item), 0, $cap).id
                            THEN stack.cursor ELSE $item_id END
                    },
                    content_type = $input.content_type,
                    content = $input.content,
                    title = $input.title,
                    updated_at = time::now()",
            )
            .bind(("repo_path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane_name", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("content_type", content_type))
            .bind(("content", content))
            .bind(("title", title))
            .bind(("item", item.clone()))
            .bind(("item_id", item_id))
            .bind(("cap", capacity as i64))
            .await
            .map_err(|e| anyhow::anyhow!("board append 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board append エラー: {}", e))?;
        Ok(())
    }

    /// board から item を 1 件削除する（= thumbnail ✕）。 cursor が削除対象を指していたら
    /// 削除後の先頭（最新）に fallback、 空なら NONE。
    pub async fn delete_board_item(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item_id: &str,
    ) -> Result<()> {
        // SET 内の右辺は「更新前の stack」で評価されるため、 cursor 判定と items 更新は整合する。
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack.cursor = IF stack.cursor = $item_id
                        THEN array::filter(stack.items ?? [], |$it| $it.id != $item_id)[0].id
                        ELSE stack.cursor END,
                    stack.items = array::filter(stack.items ?? [], |$it| $it.id != $item_id),
                    updated_at = time::now()
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("item_id", item_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board delete 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board delete エラー: {}", e))?;
        Ok(())
    }

    /// board の item を id で **in-place 置換**する（= mcp__update、doc 52 §5）。
    ///
    /// stack 内の id 一致 item の content / contentType だけ差し替え、 id / title / createdAt は
    /// 保つ（計器の更新 = 位置も生成時刻も動かさない。fresh 判定 createdAt<BOOT_TS のままなので
    /// focus を奪わない）。cursor が対象を指していれば top-level reflection も更新する。
    /// id 不一致は array::map が no-op（呼び出し側が事前に存在確認して loud error にする）。
    // (repo_path, scope, lane_name, pane_id) の board-key 4 分割は sibling（append/delete）と
    // 揃えているため、item_id + content + content_type を足すと 8 引数になる。bundle すると
    // 兄弟と不整合になるので許容する。
    #[allow(clippy::too_many_arguments)]
    pub async fn update_board_item(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item_id: &str,
        content: &str,
        content_type: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack.items = array::map(stack.items ?? [], |$it|
                        IF $it.id = $item_id
                        THEN {
                            id: $it.id,
                            content: $content,
                            contentType: $content_type,
                            title: $it.title,
                            createdAt: $it.createdAt,
                            updatedAt: $updated_at
                        }
                        ELSE $it END),
                    content = IF stack.cursor = $item_id THEN $content ELSE content END,
                    content_type = IF stack.cursor = $item_id THEN $content_type ELSE content_type END,
                    updated_at = time::now()
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("item_id", item_id.to_string()))
            .bind(("content", content.to_string()))
            .bind(("content_type", content_type.to_string()))
            // updatedAt は RFC3339 文字列で stamp（show の createdAt と型を揃える = 額縁が
            // 一様に parse できる。time::now() の datetime 型だと read 時に型がばらつく）。
            .bind(("updated_at", chrono::Utc::now().to_rfc3339()))
            .await
            .map_err(|e| anyhow::anyhow!("board update 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board update エラー: {}", e))?;
        Ok(())
    }

    /// board の cursor（= 注視 = main に出す item）を id で更新する（doc 52 §5 — cursor の
    /// server 昇格。thumbnail click / scrollback で mako の注視を repo truth にする）。
    ///
    /// cursor が指す item の content / contentType を top-level reflection にも写す
    /// （update_board_item の cursor 一致時と同じ扱い）。存在確認は呼び出し側が read-first で
    /// 行う（無い id を渡すと WHERE の item 条件で no-op になり cursor は動かない = 安全側）。
    pub async fn set_board_cursor(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
        item_id: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack.cursor = $item_id,
                    content = (array::filter(stack.items ?? [], |$it| $it.id = $item_id)[0].content) ?? content,
                    content_type = (array::filter(stack.items ?? [], |$it| $it.id = $item_id)[0].contentType) ?? content_type,
                    updated_at = time::now()
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id
                   AND $item_id IN (stack.items ?? []).id",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .bind(("item_id", item_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board set_cursor 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board set_cursor エラー: {}", e))?;
        Ok(())
    }

    /// board を空にする（= mcp__clear / Clear ボタン）。
    pub async fn clear_board(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE pane_contents SET
                    stack = { items: [], cursor: NONE },
                    content = '', title = NONE,
                    updated_at = time::now()
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board clear 失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("board clear エラー: {}", e))?;
        Ok(())
    }

    /// 特定 (repo_path, scope, lane_name, pane_id) の board を 1 件取得。 不在なら Ok(None)。
    pub async fn load_board(
        &self,
        repo_path: &str,
        scope: &str,
        lane_name: &str,
        pane_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let mut result = self
            .db
            .query(
                "SELECT * FROM pane_contents
                 WHERE repo_path = $path AND scope = $scope
                   AND lane_name = $lane AND pane_id = $pane_id
                 LIMIT 1",
            )
            .bind(("path", repo_path.to_string()))
            .bind(("scope", scope.to_string()))
            .bind(("lane", lane_name.to_string()))
            .bind(("pane_id", pane_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("board load 失敗: {}", e))?;
        let mut records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records.pop())
    }

    /// repoの全ペイン状態を取得
    pub async fn list_pane_contents(&self, repo_path: &str) -> Result<Vec<serde_json::Value>> {
        let mut result = self
            .db
            .query("SELECT * FROM pane_contents WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 取得失敗: {}", e))?;
        let records: Vec<serde_json::Value> = result.take(0)?;
        Ok(records)
    }

    /// repoの全ペイン状態を削除
    pub async fn clear_pane_contents(&self, repo_path: &str) -> Result<()> {
        self.db
            .query("DELETE FROM pane_contents WHERE repo_path = $path")
            .bind(("path", repo_path.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("pane_contents 削除失敗: {}", e))?
            .check()
            .map_err(|e| anyhow::anyhow!("pane_contents 削除エラー: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::make_test_db;

    // =========================================================================
    // Pane Contents CRUD テスト
    // =========================================================================

    /// 基本的な INSERT → SELECT フロー
    #[tokio::test]
    async fn test_pane_contents_basic_crud() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r##"{"Markdown":"# Hello"}"##,
            Some("テストペイン"),
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["pane_id"], "pane-1");
        assert_eq!(panes[0]["content_type"], "markdown");
        assert_eq!(panes[0]["content"], r##"{"Markdown":"# Hello"}"##);
        assert_eq!(panes[0]["title"], "テストペイン");
    }

    /// 同一 (repo_path, pane_id) で再度 upsert → content が更新される（UPSERT 冪等性）
    #[tokio::test]
    async fn test_pane_contents_upsert_updates_content() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r#"{"Markdown":"初回内容"}"#,
            Some("初回タイトル"),
        )
        .await
        .unwrap();

        // 同じ pane_id で異なる content
        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "html",
            r#"{"Html":"<h1>更新後</h1>"}"#,
            Some("更新後タイトル"),
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        // レコード数は1のまま（UPSERT）
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0]["content_type"], "html");
        assert_eq!(panes[0]["content"], r#"{"Html":"<h1>更新後</h1>"}"#);
        assert_eq!(panes[0]["title"], "更新後タイトル");
    }

    /// 異なる repo_path のペインは list_pane_contents で見えない（repo分離）
    #[tokio::test]
    async fn test_pane_contents_repo_isolation() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-1",
            "markdown",
            r#"{"Markdown":"VP の内容"}"#,
            None,
        )
        .await
        .unwrap();

        db.upsert_pane_content(
            "/repos/creo",
            "pane-1",
            "markdown",
            r#"{"Markdown":"Creo の内容"}"#,
            None,
        )
        .await
        .unwrap();

        // VP のペイン → VP の内容だけ見える
        let vp_panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(vp_panes.len(), 1);
        assert_eq!(vp_panes[0]["content"], r#"{"Markdown":"VP の内容"}"#);

        // Creo のペイン → Creo の内容だけ見える
        let creo_panes = db.list_pane_contents("/repos/creo").await.unwrap();
        assert_eq!(creo_panes.len(), 1);
        assert_eq!(creo_panes[0]["content"], r#"{"Markdown":"Creo の内容"}"#);
    }

    /// clear_pane_contents は対象 repo_path のみ削除（他repoに影響なし）
    #[tokio::test]
    async fn test_pane_contents_clear_isolates_repos() {
        let db = make_test_db().await;

        db.upsert_pane_content("/repos/vp", "pane-1", "log", r#"{"Log":[]}"#, None)
            .await
            .unwrap();
        db.upsert_pane_content("/repos/creo", "pane-2", "log", r#"{"Log":[]}"#, None)
            .await
            .unwrap();

        // VP のみクリア
        db.clear_pane_contents("/repos/vp").await.unwrap();

        let vp_panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(vp_panes.len(), 0, "VP のペインはクリアされている");

        let creo_panes = db.list_pane_contents("/repos/creo").await.unwrap();
        assert_eq!(creo_panes.len(), 1, "Creo のペインは残っている");
    }

    // =========================================================================
    // board Canvas Stack Model (lane scope) — pp-content-persist
    // =========================================================================

    /// 新 API: lane_name=None (main) と Some(name) (sub) が独立 record として共存できる
    #[tokio::test]
    async fn test_board_state_main_and_sub_independent() {
        let db = make_test_db().await;

        let main_stack = serde_json::json!({
            "items": [{"id":"i1","content":"# root\n","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "i1",
            "capacity": 10
        });
        let sub_stack = serde_json::json!({
            "items": [{"id":"i2","content":"# sub\n","contentType":"markdown","createdAt":"2026-05-28T00:00:01Z"}],
            "cursor": "i2",
            "capacity": 10
        });
        let ui =
            serde_json::json!({"visible": true, "collapsed": false, "width": 480, "height": 720});

        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "# root\n",
            None,
            Some(&main_stack),
            Some(&ui),
        )
        .await
        .unwrap();
        db.upsert_board_state(
            "/repos/vp",
            Some("foo"),
            "board",
            "markdown",
            "# sub\n",
            None,
            Some(&sub_stack),
            Some(&ui),
        )
        .await
        .unwrap();

        // main 読み込み
        let main = db
            .load_board_state("/repos/vp", None, "board")
            .await
            .unwrap()
            .expect("root record 不在");
        assert_eq!(
            main["lane_name"], "",
            "root は lane_name='' sentinel (= None)"
        );
        assert_eq!(main["stack"]["cursor"], "i1");

        // sub 読み込み — main と独立した record
        let sub = db
            .load_board_state("/repos/vp", Some("foo"), "board")
            .await
            .unwrap()
            .expect("sub record 不在");
        assert_eq!(sub["lane_name"], "foo");
        assert_eq!(sub["stack"]["cursor"], "i2");

        // list_pane_contents は両方見える (repo scope)
        let all = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(all.len(), 2, "root + sub で 2 record");
    }

    /// upsert_board_state は同 (repo_path, lane_name, pane_id) で stack を上書きする (= roundtrip)
    #[tokio::test]
    async fn test_board_state_upsert_roundtrip() {
        let db = make_test_db().await;
        let stack_v1 = serde_json::json!({"items": [], "cursor": null, "capacity": 10});
        let stack_v2 = serde_json::json!({
            "items": [{"id":"a","content":"x","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "a",
            "capacity": 10
        });

        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "",
            None,
            Some(&stack_v1),
            None,
        )
        .await
        .unwrap();
        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "x",
            None,
            Some(&stack_v2),
            None,
        )
        .await
        .unwrap();

        let rec = db
            .load_board_state("/repos/vp", None, "board")
            .await
            .unwrap()
            .expect("record 不在");
        assert_eq!(rec["stack"]["cursor"], "a");
        assert_eq!(rec["stack"]["items"][0]["id"], "a");

        // 1 record だけ (UPSERT 冪等)
        let all = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(all.len(), 1);
    }

    /// 旧 caller (upsert_pane_content) は lane_name=None で row を作る。 stack/ui_state を巻き戻さない。
    #[tokio::test]
    async fn test_board_state_legacy_upsert_keeps_stack() {
        let db = make_test_db().await;
        let stack = serde_json::json!({
            "items": [{"id":"keep","content":"keep","contentType":"markdown","createdAt":"2026-05-28T00:00:00Z"}],
            "cursor": "keep",
            "capacity": 10
        });

        // 新 API で stack を先に保存
        db.upsert_board_state(
            "/repos/vp",
            None,
            "board",
            "markdown",
            "keep",
            Some("t"),
            Some(&stack),
            None,
        )
        .await
        .unwrap();

        // 旧 API (content / title だけ更新)。 stack は触らないことを期待
        db.upsert_pane_content("/repos/vp", "board", "markdown", "updated", Some("t2"))
            .await
            .unwrap();

        let rec = db
            .load_board_state("/repos/vp", None, "board")
            .await
            .unwrap()
            .expect("record 不在");
        assert_eq!(rec["content"], "updated", "content は旧 API で更新される");
        assert_eq!(rec["title"], "t2");
        assert_eq!(
            rec["stack"]["cursor"], "keep",
            "stack は旧 API で巻き戻されてはいけない"
        );
    }

    /// load_board_state: 不在の (repo_path, lane_name, pane_id) は Ok(None)
    #[tokio::test]
    async fn test_board_state_load_missing_returns_none() {
        let db = make_test_db().await;
        let v = db
            .load_board_state("/repos/vp", None, "missing")
            .await
            .unwrap();
        assert!(v.is_none());
    }

    // ===== board モデル (2026-07-15): scope 別 board CRUD の SurrealQL 検証 =====

    fn mk_item(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "content": id, "contentType": "markdown",
            "createdAt": "2026-07-15T00:00:00Z"
        })
    }

    /// append は item を head-push し（新→古）、 cursor を新 item に更新、 capacity で最古を落とす。
    #[tokio::test]
    async fn test_board_append_head_push_cursor_and_cap() {
        let db = make_test_db().await;
        for id in ["a", "b", "c"] {
            db.append_board_item("/repos/vp", "proj", "", "board", &mk_item(id), 2)
                .await
                .unwrap();
        }
        let rec = db
            .load_board("/repos/vp", "proj", "", "board")
            .await
            .unwrap()
            .expect("board 不在");
        // head-push: 最新 c が先頭、 cap=2 で最古 a が落ちる → [c, b]
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 2);
        assert_eq!(rec["stack"]["items"][0]["id"], "c");
        assert_eq!(rec["stack"]["items"][1]["id"], "b");
        assert_eq!(rec["stack"]["cursor"], "c");
    }

    /// delete: cursor が削除対象なら削除後の先頭に fallback、 非 cursor 削除は cursor 不変。
    #[tokio::test]
    async fn test_board_delete_item_cursor_fallback() {
        let db = make_test_db().await;
        for id in ["a", "b", "c"] {
            db.append_board_item("/repos/vp", "lane", "wing", "board", &mk_item(id), 10)
                .await
                .unwrap();
        }
        // items=[c,b,a], cursor=c。 c を削除 → items=[b,a], cursor=b（先頭 fallback）。
        db.delete_board_item("/repos/vp", "lane", "wing", "board", "c")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "wing", "board")
            .await
            .unwrap()
            .unwrap();
        let items = rec["stack"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "b");
        assert_eq!(rec["stack"]["cursor"], "b");

        // items=[b,a], cursor=b。 a（非 cursor）削除 → cursor=b 不変。
        db.delete_board_item("/repos/vp", "lane", "wing", "board", "a")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "wing", "board")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rec["stack"]["cursor"], "b",
            "非 cursor 削除で cursor は不変"
        );
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 1);
    }

    /// scrollback で最古 item を pin した状態で show が来て、その item が capacity trim で
    /// evict される場合、cursor は孤児化せず新 head に fallback する（team-b review、doc 52 §5
    /// 「流されない」に反する孤児 cursor = 主画面が無言で空白化するのを防ぐ）。
    #[tokio::test]
    async fn test_board_cursor_survives_capacity_eviction() {
        let db = make_test_db().await;
        // capacity=2 で a, b を貼る → items=[b,a]、cursor は head 追従で b。
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("a"), 2)
            .await
            .unwrap();
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("b"), 2)
            .await
            .unwrap();
        // 最古の a を pin（scrollback で遡って見ている状態）。
        db.set_board_cursor("/repos/vp", "lane", "", "board", "a")
            .await
            .unwrap();
        // c を貼る → items=[c,b]（a が evict）。cursor=a は消えるので新 head c に fallback。
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("c"), 2)
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        let items = rec["stack"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            !items.iter().any(|i| i["id"] == "a"),
            "最古 a は capacity trim で evict される"
        );
        assert_eq!(
            rec["stack"]["cursor"], "c",
            "evict された cursor は孤児化せず新 head c に fallback（主画面が空白化しない）"
        );
    }

    /// update は id 一致 item の content/contentType を in-place 置換し、id/createdAt/位置は保つ。
    /// cursor が対象を指していれば top-level content も反映する（doc 52 §5）。
    #[tokio::test]
    async fn test_board_update_item_in_place() {
        let db = make_test_db().await;
        for id in ["a", "b", "c"] {
            db.append_board_item("/repos/vp", "lane", "", "board", &mk_item(id), 10)
                .await
                .unwrap();
        }
        // items=[c,b,a], cursor=c。 非 cursor の b を html で更新。
        db.update_board_item(
            "/repos/vp",
            "lane",
            "",
            "board",
            "b",
            "updated-body",
            "html",
        )
        .await
        .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        let items = rec["stack"]["items"].as_array().unwrap();
        // 位置不変（[c,b,a]）、b だけ content/contentType 差し替え、id/createdAt 保持。
        assert_eq!(items.len(), 3);
        assert_eq!(items[1]["id"], "b");
        assert_eq!(items[1]["content"], "updated-body");
        assert_eq!(items[1]["contentType"], "html");
        assert_eq!(items[1]["createdAt"], "2026-07-15T00:00:00Z");
        // cursor(c) 非対象なので top-level reflection は不変。
        assert_eq!(rec["stack"]["cursor"], "c");
        // 他 item は無傷。
        assert_eq!(items[0]["id"], "c");
        assert_eq!(items[0]["content"], "c");

        // cursor(c) を更新 → top-level content も反映。
        db.update_board_item(
            "/repos/vp",
            "lane",
            "",
            "board",
            "c",
            "c-latest",
            "markdown",
        )
        .await
        .unwrap();
        let rec = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            rec["content"], "c-latest",
            "cursor 対象の更新は top-level に反映"
        );
    }

    /// clear は board を空にする。
    #[tokio::test]
    async fn test_board_clear() {
        let db = make_test_db().await;
        db.append_board_item("/repos/vp", "proj", "", "board", &mk_item("a"), 10)
            .await
            .unwrap();
        db.clear_board("/repos/vp", "proj", "", "board")
            .await
            .unwrap();
        let rec = db
            .load_board("/repos/vp", "proj", "", "board")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec["stack"]["items"].as_array().unwrap().len(), 0);
        assert!(rec["stack"]["cursor"].is_null());
    }

    /// lane board と proj board は同 repo でも独立（scope 軸で分離）。
    #[tokio::test]
    async fn test_board_scope_isolation() {
        let db = make_test_db().await;
        db.append_board_item("/repos/vp", "lane", "", "board", &mk_item("L"), 10)
            .await
            .unwrap();
        db.append_board_item("/repos/vp", "proj", "", "board", &mk_item("P"), 10)
            .await
            .unwrap();
        let lane = db
            .load_board("/repos/vp", "lane", "", "board")
            .await
            .unwrap()
            .unwrap();
        let proj = db
            .load_board("/repos/vp", "proj", "", "board")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lane["stack"]["items"][0]["id"], "L");
        assert_eq!(proj["stack"]["items"][0]["id"], "P");
        // lane/proj は別 row
        assert_eq!(db.list_pane_contents("/repos/vp").await.unwrap().len(), 2);
    }

    /// title が None → NULL で保存・復元できる
    #[tokio::test]
    async fn test_pane_contents_title_null() {
        let db = make_test_db().await;

        db.upsert_pane_content(
            "/repos/vp",
            "pane-notitle",
            "url",
            r#"{"Url":"https://example.com"}"#,
            None,
        )
        .await
        .unwrap();

        let panes = db.list_pane_contents("/repos/vp").await.unwrap();
        assert_eq!(panes.len(), 1);
        assert!(
            panes[0]["title"].is_null(),
            "title が NULL でない: {:?}",
            panes[0]["title"]
        );
    }
}
