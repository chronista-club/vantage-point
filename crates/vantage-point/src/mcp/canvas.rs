//! MCP canvas family tools — show / clear / capture_canvas / list_canvas / read_pane。
//!
//! mcp.rs から family module に分割（手書きのまま、description / signature を逐語保持）。
//! helper（process_call / fetch_canvas_panes / quic_call 等）と CanvasPane /
//! parse_show_payload は親 mcp.rs に据え置き、子 module から呼ぶ（Rust privacy: 子は親の
//! private item を参照可）。
use super::*;

/// board scope の検証（board モデル 2026-07-15）: None/"lane"→None(=lane 既定)、
/// "proj"→Some("proj")、 "vp"/その他→fail-closed。 "vp"(全体 board) は cross-project 共有で
/// World store が要り Phase 2 未実装のため、 silent に lane 降格せず明示エラーで弾く。
fn validate_board_scope(scope: Option<&str>) -> Result<Option<String>, McpError> {
    match scope {
        None | Some("lane") => Ok(None),
        Some("proj") => Ok(Some("proj".to_string())),
        Some(other) => Err(McpError::invalid_params(
            format!(
                "未対応の board scope: '{}'。 'lane'(既定) か 'proj' を指定してください（'vp' は未実装）",
                other
            ),
            None,
        )),
    }
}

/// Parameters for the show tool
///
/// ## doc 19 PP Canvas Stack Model (2026-05-27)
///
/// `append` field は spec から omit。 mcp__show は **canvas に新 item を push** する
/// semantic に統一されたため、 「既存に追記」 は新 item 化で表現する。
/// 外部 MCP client が `append: true` を送ってきても serde が unknown field を silent
/// ignore (= backward compat)、 stack model 上は無効。
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

    /// Pane ID
    ///
    /// doc 19 PP Canvas Stack Model: vp-app の canvas-handler は pane_id を無視して
    /// 全 show を PP body の stack に集約する (= dead field、 backward compat のため
    /// 残置)。 v2 で削除候補。
    #[schemars(description = "Pane ID (currently ignored; reserved for future)")]
    pub pane_id: Option<String>,

    /// Pane title (for tab display)
    #[schemars(description = "Title for the pane tab. If not provided, the pane_id is used.")]
    pub title: Option<String>,

    /// board scope（board モデル）: どの board に貼るか。
    #[schemars(
        description = "Board to pin this content to: 'lane' (default; the current lane's board) or 'proj' (the project-wide board shared across all lanes)."
    )]
    pub scope: Option<String>,
}

/// Parameters for the clear tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ClearParams {
    /// Pane ID to clear
    #[schemars(description = "Pane ID to clear (default: 'main')")]
    pub pane_id: Option<String>,

    /// board scope to clear
    #[schemars(
        description = "Board to clear: 'lane' (default) or 'proj' (the project-wide board)."
    )]
    pub scope: Option<String>,
}

/// Parameters for the capture_canvas tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CaptureCanvasParams {
    /// Save path
    #[schemars(
        description = "Save path for the PNG screenshot (default: /tmp/vp-canvas-{timestamp}.png)"
    )]
    pub path: Option<String>,

    /// Capture specific pane only
    ///
    /// ネイティブ window capture は GUI window 全体を撮るため、pane 単位の分離は
    /// 未対応（現状は無視）。将来 region 解決で per-pane 対応の余地あり。
    #[schemars(
        description = "Currently ignored — capture is window-level (the whole Vantage Point GUI window). Reserved for future per-pane support."
    )]
    pub pane_id: Option<String>,
}

/// Parameters for the read_pane tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadPaneParams {
    /// pane_id to read (省略時: pane が 1 つだけならそれを返す)
    #[schemars(
        description = "The pane_id to read. If omitted and exactly one pane is on the Canvas, that pane is returned."
    )]
    pub pane_id: Option<String>,
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
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());
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

        // doc 19 PP Canvas Stack Model: append は spec から omit。 protocol layer の
        // ProcessMessage::Show.append は keep (= wire 互換)、 値は false 固定で送る。
        // WebView 側 canvas-handler が stack model で新 item として push する。
        // board scope: "vp" は Phase 2 未実装なので fail-closed（silent lane 降格を避ける）。
        let scope = validate_board_scope(params.scope.as_deref())?;
        let msg = ProcessMessage::Show {
            pane_id: pane_id.clone(),
            content,
            append: false,
            title: params.title,
            // per-lane PP: この MCP が属する Lane（cwd 由来、root/performer 語彙）を stamp。
            // topic の lane segment になり、retained を lane 別に分離する。
            lane: Some(SelfLane::detect().lane_name),
            scope,
        };

        self.process_call("show", &msg).await?;

        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Content displayed in pane '{}'", pane_id),
        )]))
    }

    /// Clear content in a pane
    #[tool(description = "Clear content in a specific pane of the browser viewer")]
    async fn clear(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ClearParams>,
    ) -> Result<CallToolResult, McpError> {
        let pane_id = params.pane_id.unwrap_or_else(|| "main".to_string());

        let scope = validate_board_scope(params.scope.as_deref())?;
        let msg = ProcessMessage::Clear {
            pane_id: pane_id.clone(),
            lane: Some(SelfLane::detect().lane_name),
            scope,
        };
        self.process_call("clear", &msg).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("Pane '{}' cleared", pane_id),
        )]))
    }

    /// Capture the Vantage Point GUI window as a PNG screenshot
    ///
    /// ネイティブ screenshot backend（`vp shot` と同じ `screenshot::default_backend()`）で
    /// "Vantage Point" window を直接キャプチャする。旧設計（webview html2canvas を
    /// World→WS 往復で回収）は往復の両端が移行時に撤去されて機能停止していたため、
    /// 往復依存を排して `vp shot` と機構を統一した（bug: canvas 可観測性の複合故障 B）。
    /// window 全体（sidebar + console + PP）を撮るので、PP が非表示なら「非表示のまま」が
    /// 正直に写る（= GUI の実可視状態が ground truth になる）。保存ファイルは Read ツールで確認可能。
    #[tool(
        description = "Capture the Vantage Point GUI window as a PNG screenshot (the whole window — sidebar, console, and Canvas as actually visible). The saved file can be viewed with the Read tool."
    )]
    async fn capture_canvas(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<CaptureCanvasParams>,
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

    /// Paisley Park Canvas の pane 一覧 (= 表示中の各 pane の最新内容) を返す。
    #[tool(
        description = "List the panes currently on the Paisley Park Canvas (PP). Returns each pane_id with its latest content's title, content_type, and a short preview. Use read_pane to fetch a pane's full source content (e.g. to save it to memory). Reads the retained snapshot over the Unison canvas channel."
    )]
    async fn list_canvas(&self) -> Result<CallToolResult, McpError> {
        let panes = self.fetch_canvas_panes().await?;
        if panes.is_empty() {
            return Ok(CallToolResult::success(vec![rmcp::model::Content::text(
                "Canvas に表示中の pane はありません (retained snapshot が空)。".to_string(),
            )]));
        }
        let mut lines = vec![format!("Canvas panes ({}):", panes.len())];
        for p in &panes {
            let preview: String = p
                .content
                .chars()
                .take(80)
                .collect::<String>()
                .replace('\n', " ");
            lines.push(format!(
                "- pane_id={} [{}] title={} | {}",
                p.pane_id,
                p.content_type,
                p.title.as_deref().unwrap_or("(none)"),
                preview
            ));
        }
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            lines.join("\n"),
        )]))
    }

    /// Canvas pane の全文を返す (= remember 等に渡せるソース内容)。
    #[tool(
        description = "Read the full source content of a Paisley Park Canvas pane by pane_id (markdown/html/log/url text), so it can be saved to creo-memories (mcp__creo-memories__remember) or otherwise processed. If pane_id is omitted and exactly one pane exists, that pane is returned. Reads over the Unison canvas channel."
    )]
    async fn read_pane(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<ReadPaneParams>,
    ) -> Result<CallToolResult, McpError> {
        let panes = self.fetch_canvas_panes().await?;
        let target = match &params.pane_id {
            Some(id) => panes.iter().find(|p| &p.pane_id == id),
            None if panes.len() == 1 => panes.first(),
            None => None,
        };
        let Some(p) = target else {
            let ids: Vec<&str> = panes.iter().map(|p| p.pane_id.as_str()).collect();
            let hint = if params.pane_id.is_some() {
                format!("pane_id が見つかりません。 現在の pane: {:?}", ids)
            } else if panes.is_empty() {
                "Canvas に表示中の pane はありません (retained snapshot が空)。".to_string()
            } else {
                format!(
                    "pane が複数あります。 pane_id を指定してください: {:?}",
                    ids
                )
            };
            return Err(McpError::invalid_params(hint, None));
        };
        let header = format!(
            "pane_id: {}\ncontent_type: {}\ntitle: {}\n---\n",
            p.pane_id,
            p.content_type,
            p.title.as_deref().unwrap_or("(none)")
        );
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            format!("{}{}", header, p.content),
        )]))
    }
}

#[cfg(test)]
mod canvas_read_tests {
    use super::*;

    #[test]
    fn parse_show_extracts_pane() {
        let v = serde_json::json!({
            "type": "show", "pane_id": "main",
            "content": {"markdown": "# Hello"}, "append": false, "title": "My Pane"
        });
        let p = parse_show_payload(&v).expect("show parse");
        assert_eq!(p.pane_id, "main");
        assert_eq!(p.content_type, "markdown");
        assert_eq!(p.content, "# Hello");
        assert_eq!(p.title.as_deref(), Some("My Pane"));
    }

    #[test]
    fn parse_show_html_without_title() {
        let v = serde_json::json!({
            "type": "show", "pane_id": "side",
            "content": {"html": "<b>x</b>"}, "append": false
        });
        let p = parse_show_payload(&v).expect("show parse");
        assert_eq!(p.content_type, "html");
        assert_eq!(p.title, None);
    }

    #[test]
    fn parse_rejects_non_show_and_malformed() {
        assert!(
            parse_show_payload(&serde_json::json!({"type":"clear","pane_id":"main"})).is_none()
        );
        assert!(parse_show_payload(&serde_json::json!({"foo": 1})).is_none());
    }

    #[test]
    fn parse_reads_append_flag() {
        let base = serde_json::json!({
            "type": "show", "pane_id": "main", "content": {"markdown": "a"}, "append": false
        });
        let app = serde_json::json!({
            "type": "show", "pane_id": "main", "content": {"markdown": "b"}, "append": true
        });
        assert!(!parse_show_payload(&base).unwrap().append);
        assert!(parse_show_payload(&app).unwrap().append);
        // append 不在は false 扱い
        let no_field = serde_json::json!({
            "type": "show", "pane_id": "x", "content": {"markdown": "c"}
        });
        assert!(!parse_show_payload(&no_field).unwrap().append);
    }
}

#[cfg(test)]
mod show_params_tests {
    use super::*;

    // --- ShowParams serde (doc 19 regression guards) ---

    /// doc 19: `append` field は ShowParams から omit 済み。
    /// 旧クライアントが `append: true` を送っても serde の unknown field として
    /// silent ignore され、 deserialize が成功すること (= backward compat)。
    #[test]
    fn show_params_silently_ignores_append_true() {
        let json = r#"{"content":"hello","append":true}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "hello");
        assert!(params.content_type.is_none());
        assert!(params.pane_id.is_none());
        assert!(params.title.is_none());
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

    /// `append: false` も silent ignore される (= 古い show handler の常時 false 送信経路)。
    #[test]
    fn show_params_silently_ignores_append_false() {
        let json = r#"{"content":"test","append":false,"pane_id":"main"}"#;
        let params: ShowParams = serde_json::from_str(json).expect("deserialize 失敗");
        assert_eq!(params.content, "test");
        assert_eq!(params.pane_id.as_deref(), Some("main"));
    }

    /// `content` フィールドが必須であることを確認 (= 省略時は deserialize error)。
    #[test]
    fn show_params_requires_content_field() {
        let json = r#"{"content_type":"markdown"}"#;
        let result: Result<ShowParams, _> = serde_json::from_str(json);
        assert!(result.is_err(), "content が無くても成功してしまう");
    }
}
