//! MCP editor family tools — editor_fields / editor_values / editor_set。
//!
//! doc 48 Phase 2: Editor Mode bridge (creo-ui editor-mode.md D-10「AI agent access」の
//! VP 実装)。doc 86 で届け先が「vp-app の webview」から **Editor Link の面**（vp-app の webview と、
//! Chrome で開いた creo-ui site など `/link` に繋いだ全ての面）に広がった。
//!
//! 経路: MCP → Daemon "link" channel（`link_call`）→ `daemon::link::handle_link_channel` → hub が
//! active な面（または `surface` で指名した面）へ WebSocket で `set` / `get` を送る。旧経路（repo
//! "process" channel → `EditorCommand` broadcast → vp-app が JS 評価）は撤去した。
//!
//! 書き戻し (調整値 → source) は専用 tool を持たない: agent が `editor_values` で読み、
//! 自分の Edit で source に落とす (doc 48 D-B — 受け手 rewriter は作らない)。
use super::*;

/// editor_set が受ける値 — number slider / color・select 文字列 / bool toggle の union。
///
/// ⚠️ ここを `serde_json::Value` にすると schema が型なし (any) になり、**MCP client が
/// 数値を JSON 文字列で送る**。画面には反映される (CSS var は文字列でも通る) が
/// `editor_values` が `"1.75"` を返し、doc 48 の書き戻し (agent が読んで source に Edit)
/// で `value: "1.75"` と文字列を焼く。層を跨いだ後にしか症状が出ないため、**ループを
/// 一周させないと見えない**。layout の `attention` で踏んだ #875 と同型
/// (2026-07-23 の答え合わせで editor 側の取り残しを発見)。
///
/// `untagged` なので serialize は値そのまま = `editor_bridge_js` が組む JS literal
/// (`h.setValue("sb.conn.width",1.75)`) が壊れない。
/// `inline` は必須 — 既定だと schema が `{"$ref":"#/$defs/EditorValue"}` になり、
/// **client が `$ref` を解決しなければ型が伝わらない**（型なし schema と同じ状態に戻る）。
/// 型を宣言する目的そのものが `$ref` の解決可否に依存してしまうので、展開して疑いを消す。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(inline)]
pub enum EditorValue {
    /// number slider (単位は field の constraints が持つ)。判定順は先頭 = 数値優先
    Number(f64),
    /// bool toggle
    Bool(bool),
    /// color picker ('#RRGGBB') / select。何でも受けるので必ず最後
    Text(String),
}

/// Parameters for editor_fields / editor_values（面の指名だけ、省略可）
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct EditorSurfaceParams {
    /// 対象の面（Link の surface id か名札。省略 = active な面）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Target surface: an Editor Link surface id, or an app label such as 'Vantage Point' (the vp-app GUI) or 'creo-ui' (a creo-ui site open in a browser with link enabled). Omit to address the active surface — the one that most recently turned Editor Mode on, else the most recently connected."
    )]
    pub surface: Option<String>,
}

/// Parameters for the editor_set tool
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EditorSetParams {
    /// 対象 field id（editor_fields が列挙する id）
    #[schemars(description = "Target field id, as listed by editor_fields (e.g. 'sb.text.base')")]
    pub id: String,

    /// 設定する値（number slider は数値、color picker は '#RRGGBB' 等の文字列）
    #[schemars(
        description = "New value for the field. Numbers for sliders (unit is implied by the field; the hub clamps to the field's min/max), strings for colors/selects."
    )]
    pub value: EditorValue,

    /// 対象の面（Link の surface id か名札。省略 = active な面）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Target surface: an Editor Link surface id, or an app label such as 'Vantage Point' (the vp-app GUI) or 'creo-ui' (a creo-ui site open in a browser with link enabled). Omit to address the active surface — the one that most recently turned Editor Mode on, else the most recently connected."
    )]
    pub surface: Option<String>,
}

#[tool_router(router = editor_router, vis = "pub(crate)")]
impl VantageMcp {
    /// Editor Mode の bind 済み field 一覧（active な面、または `surface` で指名した面）
    #[tool(
        description = "List the live-tunable design knobs (Editor Mode fields) bound on an Editor Link surface — the Vantage Point GUI, or a creo-ui site open in a browser with link enabled. Returns the surface, its mode, and each field's id, label, type, semantic, constraints (min/max/step/unit), initial and cssVar. Omit `surface` to address the active surface."
    )]
    async fn editor_fields(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<EditorSurfaceParams>,
    ) -> Result<CallToolResult, McpError> {
        let v = self
            .link_call(
                "editor_fields",
                serde_json::json!({ "surface": params.surface }),
            )
            .await?;
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(v.to_string()),
        ]))
    }

    /// Editor Mode の現在値 snapshot（active な面、または `surface` で指名した面）
    #[tool(
        description = "Read the current values of all Editor Mode fields on an Editor Link surface — including values the user tuned by hand with sliders or desk knobs — plus the current selection. Use this to pick up the user's visual exploration and persist it to source files with your own edits. Omit `surface` to address the active surface."
    )]
    async fn editor_values(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<EditorSurfaceParams>,
    ) -> Result<CallToolResult, McpError> {
        let v = self
            .link_call(
                "editor_values",
                serde_json::json!({ "surface": params.surface }),
            )
            .await?;
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(v.to_string()),
        ]))
    }

    /// Editor Mode field へ値を書く（画面に即時反映）
    #[tool(
        description = "Set an Editor Mode field value on an Editor Link surface. The change reflects on screen immediately (live CSS var / signal update, not persisted); numbers are clamped to the field's constraints and the applied value is returned. Use editor_fields first to discover ids and valid ranges. Omit `surface` to address the active surface."
    )]
    async fn editor_set(
        &self,
        rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<EditorSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let v = self
            .link_call(
                "editor_set",
                serde_json::json!({ "id": params.id, "value": params.value, "surface": params.surface }),
            )
            .await?;
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(v.to_string()),
        ]))
    }
}

#[cfg(test)]
mod editor_params_schema_tests {
    use super::EditorValue;

    /// value が型付き union として schema に出る回帰を固定する（layout の #875 と対）。
    /// 型なし（`serde_json::Value`）だと MCP client が数値を JSON 文字列で送り、
    /// `editor_values` が `"1.75"` を返して doc 48 の書き戻しが source に文字列を焼く。
    #[test]
    fn set_params_value_declares_types() {
        let schema = serde_json::to_value(schemars::schema_for!(super::EditorSetParams))
            .expect("schema serialize");
        let prop = schema["properties"]["value"].to_string();
        for ty in ["number", "boolean", "string"] {
            assert!(prop.contains(ty), "value schema に {ty} が無い: {prop}");
        }
    }

    /// untagged の**判定順**を固定する。Text を先頭に置くと数値まで文字列に飲まれ、
    /// 型を付けた意味が消える（この bug そのものの再来）。
    #[test]
    fn value_deserializes_to_expected_variant() {
        let n: EditorValue = serde_json::from_str("1.75").expect("number");
        assert!(matches!(n, EditorValue::Number(v) if (v - 1.75).abs() < f64::EPSILON));

        let b: EditorValue = serde_json::from_str("true").expect("bool");
        assert!(matches!(b, EditorValue::Bool(true)));

        let s: EditorValue = serde_json::from_str("\"#FF4A2D\"").expect("string");
        assert!(matches!(s, EditorValue::Text(ref t) if t == "#FF4A2D"));
    }

    /// `surface` は 3 tool とも省略可（required に入らない）。入れると「active な面に書く」という
    /// 既定の呼び方が壊れ、既存 agent の editor_set が全部 schema error になる。
    #[test]
    fn surface_is_optional_on_every_editor_tool() {
        for schema in [
            serde_json::to_value(schemars::schema_for!(super::EditorSetParams)).expect("set"),
            serde_json::to_value(schemars::schema_for!(super::EditorSurfaceParams)).expect("query"),
        ] {
            assert!(schema["properties"]["surface"].is_object(), "{schema}");
            let required = schema["required"].as_array().cloned().unwrap_or_default();
            assert!(
                !required.iter().any(|r| r == "surface"),
                "surface が required に入っている: {required:?}"
            );
        }
    }

    /// serialize が untagged（値そのまま）であること — Link の `set` frame（`{"t":"set","id","value"}`）に
    /// 値が型のまま乗り、creo-ui 側で `"1.75"` と文字列化されないことの担保。
    #[test]
    fn value_serializes_without_tag() {
        assert_eq!(
            serde_json::to_string(&EditorValue::Number(1.75)).expect("number"),
            "1.75"
        );
        assert_eq!(
            serde_json::to_string(&EditorValue::Text("#FF4A2D".into())).expect("text"),
            "\"#FF4A2D\""
        );
    }
}
