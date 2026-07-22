//! MCP layout family tools — layout_get / layout_set / layout_history。
//!
//! doc 49 LE-P2 PR2: Layout Engine bridge (creo-ui layout-engine.md LE-15「AI access」の
//! VP 実装)。GUI (vp-app webview) の attention 連続場を AI agent が読み書きする口。
//! doc 48 Phase 2 の editor bridge (editor.rs) と同じ配管 (`EditorCommand` request-response、
//! 非 retained event topic) を op を変えて共用する。
//!
//! apply policy (LE-16): gallery scope は dev surface なので auto — `layout_set` は即適用され、
//! settle log に author="ai" の監査が残る。work lane の hitl 既定 (propose 承認) は
//! per-scope policy として P4 で実装する。
use super::*;

/// Parameters for the layout_set tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LayoutSetParams {
    /// 構造記法（例: "sidebar-text-scale | sidebar-connector ~ board"）。省略 = 構造は現状維持
    #[schemars(
        description = "Structure notation: `|` separates columns, `/` stacks panes vertically inside a column, `~` marks floating panes. Sizes are never written here — they live in the attention field. Omit to keep the current structure."
    )]
    pub notation: Option<String>,

    /// pane id → attention raw 値（0 = 非表示）。省略 id は現状維持。
    /// 型付き map にするのが重要 — `serde_json::Value` だと schema が型なしになり、
    /// MCP client が object を JSON 文字列のまま送る（実機で踏んだ regression）
    #[schemars(
        description = "Partial overlay of attention values (pane id -> raw >= 0; 0 hides the pane). Ids not mentioned keep their current value. A value > 0 for an id outside the structure makes it float."
    )]
    pub attention: Option<std::collections::BTreeMap<String, f64>>,

    /// 列幅 lock（pane id → 幅 share）。指定すると全置換、省略 = 現状維持
    #[schemars(
        description = "Column width locks (pane id -> width share in (0,1)). When provided, replaces the whole lock map; omit to keep current locks."
    )]
    pub locks: Option<std::collections::BTreeMap<String, f64>>,
}

/// Parameters for the layout_history tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LayoutHistoryParams {
    /// 返す settle entry 数（新しい順、既定 10）
    #[schemars(description = "Number of most-recent settle entries to return (default 10)")]
    pub limit: Option<u32>,
}

#[tool_router(router = layout_router, vis = "pub(crate)")]
impl VantageMcp {
    /// Layout Engine の現在状態 snapshot
    #[tool(
        description = "Read the current pane layout of the Vantage Point GUI gallery (creo-ui-layout attention field). Returns { scope, notation, attention (raw values), shares (normalized), locks }. The notation string (e.g. 'a | b/c ~ d') is the human-readable topology. Requires vp-app to be running with the gallery engine loaded."
    )]
    async fn layout_get(&self) -> Result<CallToolResult, McpError> {
        let v = self.quic_call("layout_get", serde_json::json!({})).await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            v.to_string(),
        )]))
    }

    /// Layout の書き込み（構造記法 + attention overlay + locks。即適用・author="ai" で settle）
    #[tool(
        description = "Set the pane layout in the Vantage Point GUI. Provide structure notation and/or a partial attention overlay and/or column width locks. Applies immediately (the user watches the screen — this is the HITL loop) and records a settle-log entry with author 'ai' for audit. Rejected if the result would hide every pane. Use layout_get first to see current state."
    )]
    async fn layout_set(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<LayoutSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let v = self
            .quic_call(
                "layout_set",
                serde_json::json!({ "value": {
                    "notation": params.notation,
                    "attention": params.attention,
                    "locks": params.locks,
                } }),
            )
            .await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            v.to_string(),
        )]))
    }

    /// settle log（履歴 = 無名 Scene の列）の直近 entry
    #[tool(
        description = "Read the recent settle-log entries of the GUI layout (newest last): [{author: 'human'|'ai'|'scene', at, notation}]. Use to see what the user arranged by hand, audit AI changes, or find a shape to restore via layout_set."
    )]
    async fn layout_history(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<LayoutHistoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let v = self
            .quic_call(
                "layout_history",
                serde_json::json!({ "value": { "limit": params.limit } }),
            )
            .await?;
        Ok(CallToolResult::success(vec![rmcp::model::Content::text(
            v.to_string(),
        )]))
    }
}

#[cfg(test)]
mod layout_params_schema_tests {
    /// attention / locks が型付き object として schema に出る回帰を固定する。
    /// `serde_json::Value` のままだと schema が型なし（any）になり、MCP client が
    /// object を JSON 文字列で送る → webview guard で `attention["22"] が不正` になる
    /// （2026-07-23 実機で踏んだ初弾 bug）。
    #[test]
    fn set_params_declare_typed_objects() {
        let schema = serde_json::to_value(schemars::schema_for!(super::LayoutSetParams))
            .expect("schema serialize");
        for field in ["attention", "locks"] {
            let prop = schema["properties"][field].to_string();
            assert!(
                prop.contains("object"),
                "{field} が object 型でない: {prop}"
            );
            assert!(
                prop.contains("number"),
                "{field} の値が number 型でない: {prop}"
            );
        }
    }
}
