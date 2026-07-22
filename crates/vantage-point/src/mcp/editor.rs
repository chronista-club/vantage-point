//! MCP editor family tools — editor_fields / editor_values / editor_set。
//!
//! doc 48 Phase 2: Editor Mode bridge (creo-ui editor-mode.md D-10「AI agent access」の
//! VP 実装)。GUI (vp-app webview) の Editor Mode field を AI agent が読み書きする口。
//!
//! 経路: MCP → World "process" channel → `handle_editor_command` (request_id 発行 +
//! `EditorCommand` broadcast、非 retained event topic) → vp-app が webview で評価 →
//! `editor_result` で応答 → oneshot 解決 → ここに返る。
//!
//! 書き戻し (調整値 → source) は専用 tool を持たない: agent が `editor_values` で読み、
//! 自分の Edit で source に落とす (doc 48 D-B — 受け手 rewriter は作らない)。
use super::*;

/// Parameters for the editor_set tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EditorSetParams {
    /// 対象 field id（editor_fields が列挙する id）
    #[schemars(description = "Target field id, as listed by editor_fields (e.g. 'sb.text.base')")]
    pub id: String,

    /// 設定する値（number slider は数値、color picker は '#RRGGBB' 等の文字列）
    #[schemars(
        description = "New value for the field. Numbers for sliders (unit is implied by the field), strings for colors/selects."
    )]
    pub value: serde_json::Value,
}

#[tool_router(router = editor_router, vis = "pub(crate)")]
impl VantageMcp {
    /// Editor Mode の bind 済み field 一覧
    #[tool(
        description = "List the live-tunable design knobs (Editor Mode fields) currently bound in the Vantage Point GUI. Returns id, label, type, cssVar, constraints (min/max/step/unit) for each field. Requires vp-app to be running."
    )]
    async fn editor_fields(&self) -> Result<CallToolResult, McpError> {
        let v = self
            .quic_call("editor_fields", serde_json::json!({}))
            .await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            v.to_string(),
        )]))
    }

    /// Editor Mode の現在値 snapshot
    #[tool(
        description = "Read the current values of all Editor Mode fields in the Vantage Point GUI — including values the user tuned by hand with sliders. Use this to pick up the user's visual exploration and persist it to source files with your own edits."
    )]
    async fn editor_values(&self) -> Result<CallToolResult, McpError> {
        let v = self
            .quic_call("editor_values", serde_json::json!({}))
            .await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            v.to_string(),
        )]))
    }

    /// Editor Mode field へ値を書く（画面に即時反映）
    #[tool(
        description = "Set an Editor Mode field value in the Vantage Point GUI. The change reflects on screen immediately (live CSS var / signal update, not persisted). Use editor_fields first to discover ids and valid ranges."
    )]
    async fn editor_set(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<EditorSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let v = self
            .quic_call(
                "editor_set",
                serde_json::json!({ "id": params.id, "value": params.value }),
            )
            .await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            v.to_string(),
        )]))
    }
}
