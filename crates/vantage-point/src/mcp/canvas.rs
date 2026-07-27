//! MCP board family tools — show / clear / capture_window。
//!
//! mcp.rs から family module に分割（手書きのまま、description / signature を逐語保持）。
//! doc 52 §7: 死んだ読み手（list_canvas / read_pane と helper CanvasPane / parse_show_payload /
//! fetch_canvas_panes）は撤去した。board を読む口（中継台）は doc 52 §4 で別途新設する。
use super::*;

/// board scope の検証: **'lane' のみ**（mako 決定 2026-07-23 — board は注視中 lane に一本化）。
/// 旧 'proj'（repo 共有 board、2026-07-15〜）と 'vp'（全体 board）構想は撤去。
/// silent に lane 降格せず明示エラーで弾く（書けたつもりで表示されない board を作らない —
/// GUI 側は scope != 'lane' の BoardUpdated を無視するため、通すと writer-without-reader になる）。
fn validate_board_scope(scope: Option<&str>) -> Result<Option<String>, McpError> {
    match scope {
        None | Some("lane") => Ok(None),
        Some(other) => Err(McpError::invalid_params(
            format!(
                "未対応の board scope: '{}'。board は 'lane'(既定) のみです（'proj' は 2026-07-23 に撤去）",
                other
            ),
            None,
        )),
    }
}

/// Parameters for the show tool
///
/// doc 52 §7: `pane_id` は撤去（board は per-lane の 1 枚 = board 固定で、pane_id は
/// dead field だった）。`append` も omit のまま（show は board に新 item を push する semantic）。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// Content to display
    #[schemars(description = "Content to display (markdown, html, or plain text)")]
    pub content: String,

    /// Content type (markdown, html, log, url)
    #[schemars(
        description = "Content type: 'markdown' (default), 'html', 'log', or 'url' (display a web page in an iframe)"
    )]
    pub content_type: Option<String>,

    /// Board item title
    #[schemars(description = "Title for this board item (shown in the history strip).")]
    pub title: Option<String>,

    /// board scope（board モデル）: どの board に貼るか。
    #[schemars(
        description = "Board to pin this content to. Only 'lane' (default; the current lane's board) is supported."
    )]
    pub scope: Option<String>,
}

/// Parameters for the clear tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClearParams {
    /// board scope to clear
    #[schemars(description = "Board to clear. Only 'lane' (default) is supported.")]
    pub scope: Option<String>,
}

/// Parameters for the update tool（doc 52 §5: id 指定 in-place 置換）
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateParams {
    /// Board item id（read_board で取得した現在の id）
    #[schemars(
        description = "The id of the board item to replace. Obtain it from read_board first."
    )]
    pub id: String,

    /// New content
    #[schemars(description = "The new content (markdown, html, or plain text)")]
    pub content: String,

    /// Content type
    #[schemars(
        description = "Content type: 'markdown', 'html', or 'log'. Omit to keep the item's current type (only pass this when you intend to change how the content is rendered)."
    )]
    pub content_type: Option<String>,

    /// board scope
    #[schemars(description = "Board to update. Only 'lane' (default) is supported.")]
    pub scope: Option<String>,
}

/// Parameters for the read_board tool（doc 52 §4 中継台 + §5 identity）
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadBoardParams {
    /// board scope
    #[schemars(description = "Board to read. Only 'lane' (default) is supported.")]
    pub scope: Option<String>,
}

/// Parameters for the capture_window tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptureWindowParams {
    /// Save path
    #[schemars(
        description = "Save path for the PNG screenshot (default: /tmp/vp-window-{timestamp}.png)"
    )]
    pub path: Option<String>,
}

#[tool_router(router = canvas_router, vis = "pub(crate)")]
impl VantageMcp {
    /// Show content in the browser viewer
    #[tool(
        description = "Display content in the Vantage Point browser viewer. Supports markdown, html, log, and url formats. Use content_type='url' to embed a web page in an iframe."
    )]
    async fn show(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let content_type = params
            .content_type
            .unwrap_or_else(|| "markdown".to_string());

        // content_type → protocol::Content enum 変換
        let content = match content_type.as_str() {
            "html" => crate::protocol::Content::Html(params.content),
            "log" => crate::protocol::Content::Log(params.content),
            "url" => crate::protocol::Content::Url(params.content),
            _ => crate::protocol::Content::Markdown(params.content),
        };

        // protocol layer の RepoMessage::Show.pane_id / append は wire 互換のため keep。
        // doc 52 §7: MCP 面から pane_id を撤去したので内部固定 "main"（board は per-lane 1 枚）。
        // board scope: "vp" は Phase 2 未実装なので fail-closed（silent lane 降格を避ける）。
        let scope = validate_board_scope(params.scope.as_deref())?;
        let msg = RepoMessage::Show {
            pane_id: "main".to_string(),
            content,
            append: false,
            title: params.title,
            // per-lane board: この MCP が属する Lane（cwd 由来、root/performer 語彙）を stamp。
            // topic の lane segment になり、retained を lane 別に分離する。
            lane: Some(SelfLane::detect().lane_name),
            scope,
        };

        self.process_call("show", &msg).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            "Content pinned to the board.".to_string(),
        )]))
    }

    /// Clear content in a pane
    #[tool(description = "Clear content in a specific pane of the browser viewer")]
    async fn clear(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClearParams>,
    ) -> Result<CallToolResult, McpError> {
        let scope = validate_board_scope(params.scope.as_deref())?;
        let msg = RepoMessage::Clear {
            pane_id: "main".to_string(),
            lane: Some(SelfLane::detect().lane_name),
            scope,
        };
        self.process_call("clear", &msg).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            "Board cleared.".to_string(),
        )]))
    }

    /// board item を id 指定で in-place 置換する（doc 52 §5）。
    #[tool(
        description = "Update (replace in place) a board item by id, keeping its position and title. Obtain the id from read_board first — updating by an unknown id fails loudly on purpose, so you never silently create a duplicate. Use this to keep a pinned item current (a progress table, test results, a design's current form): read_board → recognize the item by its title/content → update it by id."
    )]
    async fn update(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<UpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        let scope = validate_board_scope(params.scope.as_deref())?;
        // content_type は既定を入れない（None → null）。省略時は server 側が既存 item の type を
        // 保つ（markdown 直書きで html→markdown に silent 降格させない、doc 52 §5 / team-b review）。
        let payload = serde_json::json!({
            "id": params.id,
            "content": params.content,
            "content_type": params.content_type,
            // per-lane board: 呼び出し元 Lane（cwd 由来）を stamp（show / clear と同じ）
            "lane": SelfLane::detect().lane_name,
            "scope": scope,
        });
        self.quic_call("board_update", payload).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Board item {} updated.", params.id),
        )]))
    }

    /// 呼び出し元 Lane の board を id 付き全文で読む（doc 52 §4 中継台 + §5 identity）。
    #[tool(
        description = "Read the current lane's board — every item with its id, title, content_type, and full content (newest first). Use this to (a) get an item's id before calling update, or (b) pull an item's full content to save it elsewhere (e.g. mcp__creo-memories__remember). The id is the stable handle: recognize the item you mean by its title/content, then mode on it by id."
    )]
    async fn read_board(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ReadBoardParams>,
    ) -> Result<CallToolResult, McpError> {
        let scope = validate_board_scope(params.scope.as_deref())?;
        let payload = serde_json::json!({
            "lane": SelfLane::detect().lane_name,
            "scope": scope,
        });
        let resp = self.quic_call("read_board", payload).await?;
        let items = resp
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // cursor = mako が今 main に出している item（doc 52 §5 注視可視化）。AI が「今どれを
        // 見ているか」を知り、その item を優先して update / 中継できるようにマークする。
        let cursor = resp.get("cursor").and_then(|v| v.as_str());
        if items.is_empty() {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                "Board is empty.".to_string(),
            )]));
        }
        let mut out = format!("Board ({} items, newest first):", items.len());
        for it in &items {
            let id = it.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let title = it
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)");
            let ct = it
                .get("contentType")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let content = it.get("content").and_then(|v| v.as_str()).unwrap_or("");
            // updatedAt は wave 3 以降の item のみ持つ（旧 item は createdAt に fallback）。
            let updated = it
                .get("updatedAt")
                .or_else(|| it.get("createdAt"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let viewing = if cursor == Some(id) {
                " ★ 今表示中"
            } else {
                ""
            };
            let stamp = if updated.is_empty() {
                String::new()
            } else {
                format!(" · 更新 {}", updated)
            };
            out.push_str(&format!(
                "\n\n─── id={} [{}] {}{}{} ───\n{}",
                id, ct, title, stamp, viewing, content
            ));
        }
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            out,
        )]))
    }

    /// Capture the Vantage Point GUI window as a PNG screenshot
    ///
    /// ネイティブ screenshot backend（`vp shot` と同じ `screenshot::default_backend()`）で
    /// "Vantage Point" window を直接キャプチャする。旧設計（webview html2canvas を
    /// Daemon→WS 往復で回収）は往復の両端が移行時に撤去されて機能停止していたため、
    /// 往復依存を排して `vp shot` と機構を統一した（bug: canvas 可観測性の複合故障 B）。
    /// window 全体（sidebar + console + board）を撮るので、board が非表示なら「非表示のまま」が
    /// 正直に写る（= GUI の実可視状態が ground truth になる）。保存ファイルは Read ツールで確認可能。
    #[tool(
        description = "Capture the Vantage Point GUI window as a PNG screenshot (the whole window — sidebar, console, and board as actually visible). The saved file can be viewed with the Read tool."
    )]
    async fn capture_window(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<CaptureWindowParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::screenshot::{CaptureFilter, default_backend};

        // 出力 path: 指定が無ければ衝突回避のため timestamp 付き（旧 handler の既定命名を踏襲）。
        let output = params
            .path
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                std::path::PathBuf::from(format!("/tmp/vp-canvas-{}.png", ts))
            });

        // screencapture CLI は同期 blocking のため spawn_blocking で runtime を塞がない。
        // filter.owner="vp-app"（default）は owner_candidates で .app の "Vantage Point" を
        // alias 解決するので、cargo dev binary / brew .app どちらの GUI window も掴める。
        let result = tokio::task::spawn_blocking(move || {
            let backend = default_backend();
            let filter = CaptureFilter {
                owner: "vp-app".into(),
                index: None,
                title_match: None,
            };
            backend.capture(&filter, Some(output))
        })
        .await
        .map_err(|e| McpError::internal_error(format!("capture task join 失敗: {}", e), None))?
        .map_err(|e| {
            McpError::internal_error(
                format!("Canvas capture 失敗: {}. GUI (vp-app) は起動している？", e),
                None,
            )
        })?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!(
                "Screenshot saved: {}\nSize: {}x{} ({}ms)\nUse the Read tool to view this image.",
                result.path.display(),
                result.width,
                result.height,
                result.elapsed_ms
            ),
        )]))
    }

    // doc 52 §7: list_canvas / read_pane は死んだ読み手として撤去（board モデル化で retained
    // Show を読む経路ごと dead に。board を読む口 = 中継台は §4 で別途新設）。
}

#[cfg(test)]
mod show_params_tests {
    use super::*;

    // --- ShowParams serde (backward compat regression guards) ---

    /// `append` field は ShowParams から omit 済み。旧クライアントが `append: true` を送っても
    /// serde の unknown field として silent ignore され、 deserialize が成功すること。
    #[test]
    fn show_params_silently_ignores_append_true() {
        let json = r#"{"content":"hello","append":true}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "hello");
        assert!(params.content_type.is_none());
        assert!(params.title.is_none());
    }

    /// doc 52 §7: `pane_id` を撤去済み。旧クライアントが送ってきても unknown field として
    /// silent ignore され deserialize が成功すること（= backward compat）。
    #[test]
    fn show_params_silently_ignores_removed_pane_id() {
        let json = r#"{"content":"test","pane_id":"main","append":false}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "test");
    }

    /// `append` が無くても (= 新クライアント形式) deserialize が成功すること。
    #[test]
    fn show_params_deserializes_without_append() {
        let json = r#"{"content":"world","content_type":"html","title":"My Page"}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "world");
        assert_eq!(params.content_type.as_deref(), Some("html"));
        assert_eq!(params.title.as_deref(), Some("My Page"));
    }

    /// `content` フィールドが必須であることを確認 (= 省略時は deserialize error)。
    #[test]
    fn show_params_requires_content_field() {
        let json = r#"{"content_type":"markdown"}"#;
        let result: Result<ShowParams, _> = serde_json::from_str(json);
        assert!(result.is_err(), "content が無くても成功してしまう");
    }
}
