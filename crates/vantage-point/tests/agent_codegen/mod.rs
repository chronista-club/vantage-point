//! VP-local agent-tool codegen — `schema/vp-agent.kdl` → `src/generated/agent_tools.rs`。
//!
//! rebuild Epic L2 第二手（doc 27 §5-1/§5-2）。MCP の手書き tool zoo を、KDL を SSOT に
//! した rmcp tool 定義の codegen へ畳む第一歩（wire/delegation family）。
//!
//! 配置: 生の `club-kdl`（KDL serde deserializer、vantage-point が既に依存）で自前の
//! agent-tool 語彙を parse → 小さな IR → Rust emit。汎用 lib（club-kdl-codegen）には
//! 触らない（VP 固有の `quic_call` / `self_lane` 知識を汎用 emitter に混ぜない layering）。
//!
//! 抽出の縫い目: [`emit_params_struct`]（app 非依存の tool-scaffold 生成）と
//! [`emit_tool_method`]（VP forward glue）を分離してある。安定形が見えたら前者を
//! club-kdl-codegen の 5th target として切り出せる。
//!
//! build-time tooling なので IR の一部フィールドは emit で未使用でも保持する（SSOT 完全性）。
#![allow(dead_code)]

use club_kdl::KdlDeserialize;

// =============================================================================
// raw — KDL-shaped 構造（club-kdl の KdlDeserialize derive で埋める）
// =============================================================================

#[derive(Debug, KdlDeserialize)]
#[kdl(document)]
struct RawSchema {
    #[kdl(child)]
    tools: Option<RawTools>,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "tools")]
struct RawTools {
    #[kdl(argument)]
    namespace: String,
    #[kdl(property)]
    version: String,
    #[kdl(children, name = "group")]
    groups: Vec<RawGroup>,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "group")]
struct RawGroup {
    #[kdl(argument)]
    name: String,
    #[kdl(property)]
    forward: String,
    #[kdl(children, name = "tool")]
    tools: Vec<RawTool>,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "tool")]
struct RawTool {
    #[kdl(argument)]
    name: String,
    /// SP 側 method 名（省略時は tool 名）。
    #[kdl(property)]
    method: Option<String>,
    /// resp の pretty-print 失敗時の代替テキスト（省略時 "null"）。
    #[kdl(property)]
    fallback: Option<String>,
    /// "custom" のとき wrapper だけ生成し本体は手書き `self.<name>_impl(params)` に委譲。
    #[kdl(property)]
    body: Option<String>,
    #[kdl(child, unwrap_arg)]
    description: Option<String>,
    #[kdl(children, name = "inject")]
    injects: Vec<RawInject>,
    #[kdl(children, name = "field")]
    fields: Vec<RawField>,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "inject")]
struct RawInject {
    #[kdl(argument)]
    key: String,
    #[kdl(property)]
    source: String,
}

#[derive(Debug, KdlDeserialize)]
#[kdl(name = "field")]
struct RawField {
    #[kdl(argument)]
    name: String,
    #[kdl(property, rename = "type")]
    type_str: String,
    #[kdl(property, default)]
    optional: bool,
    #[kdl(property)]
    description: Option<String>,
}

// =============================================================================
// IR — validate 済みの target-agnostic 表現
// =============================================================================

pub struct ToolSchema {
    pub namespace: String,
    pub version: String,
    pub groups: Vec<ToolGroup>,
}

pub struct ToolGroup {
    pub name: String,
    pub forward: Forward,
    pub tools: Vec<Tool>,
}

/// group の転送機構。
pub enum Forward {
    /// `self.quic_call(method, payload)` で SP "process" channel へ。
    QuicProcess,
}

pub struct Tool {
    pub name: String,
    /// SP method 名（resolve 済み: 既定は tool 名）。
    pub method: String,
    /// pretty-print 失敗時の代替（resolve 済み: 既定 "null"）。
    pub fallback: String,
    /// body="custom" → 本体を手書き helper に委譲。
    pub custom_body: bool,
    pub description: String,
    pub injects: Vec<Inject>,
    pub fields: Vec<Field>,
}

pub struct Inject {
    pub key: String,
    pub source: InjectSource,
}

pub enum InjectSource {
    /// `self.self_lane.from_address()?`（caller の wire address）。
    SelfLane,
}

pub struct Field {
    pub name: String,
    pub ty: FieldTy,
    pub optional: bool,
    pub description: String,
}

pub enum FieldTy {
    /// Rust `String`。
    String,
    /// Rust `u64`。
    U64,
    /// Rust `Vec<String>`。
    ArrayString,
    /// Rust `serde_json::Value`（JsonSchema は object を明示）。
    JsonObject,
}

// =============================================================================
// parse — raw → IR（lower + validate）
// =============================================================================

/// `vp-agent.kdl` のテキストを [`ToolSchema`] に parse する。
pub fn parse(src: &str) -> Result<ToolSchema, String> {
    let raw: RawSchema = club_kdl::from_str(src).map_err(|e| format!("KDL parse 失敗: {e}"))?;
    let raw_tools = raw
        .tools
        .ok_or_else(|| "ルートに `tools` ノードが無い".to_string())?;

    let mut groups = Vec::new();
    for g in raw_tools.groups {
        let forward = match g.forward.as_str() {
            "quic-process" => Forward::QuicProcess,
            other => return Err(format!("group {:?}: 未知の forward {other:?}", g.name)),
        };
        let mut tools = Vec::new();
        for t in g.tools {
            let description = t
                .description
                .ok_or_else(|| format!("tool {:?}: description が無い", t.name))?;
            let custom_body = match t.body.as_deref() {
                None => false,
                Some("custom") => true,
                Some(other) => {
                    return Err(format!("tool {:?}: 未知の body {other:?}", t.name));
                }
            };
            let method = t.method.unwrap_or_else(|| t.name.clone());
            let fallback = t.fallback.unwrap_or_else(|| "null".to_string());

            let mut injects = Vec::new();
            for i in t.injects {
                let source = match i.source.as_str() {
                    "self_lane" => InjectSource::SelfLane,
                    other => {
                        return Err(format!(
                            "tool {:?} inject {:?}: 未知の source {other:?}",
                            t.name, i.key
                        ));
                    }
                };
                injects.push(Inject { key: i.key, source });
            }

            let mut fields = Vec::new();
            for f in t.fields {
                let ty = match f.type_str.as_str() {
                    "string" => FieldTy::String,
                    "u64" => FieldTy::U64,
                    "array<string>" => FieldTy::ArrayString,
                    "json-object" => FieldTy::JsonObject,
                    other => {
                        return Err(format!(
                            "tool {:?} field {:?}: 未知の type {other:?}",
                            t.name, f.name
                        ));
                    }
                };
                fields.push(Field {
                    name: f.name,
                    ty,
                    optional: f.optional,
                    description: f.description.unwrap_or_default(),
                });
            }

            tools.push(Tool {
                name: t.name,
                method,
                fallback,
                custom_body,
                description,
                injects,
                fields,
            });
        }
        groups.push(ToolGroup {
            name: g.name,
            forward,
            tools,
        });
    }

    Ok(ToolSchema {
        namespace: raw_tools.namespace,
        version: raw_tools.version,
        groups,
    })
}

// =============================================================================
// emit — IR → Rust
// =============================================================================

/// `snake_case` → `PascalCase`。
fn pascal(name: &str) -> String {
    name.split('_')
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            let mut c = seg.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

/// tool 名 → params struct 名（`wire_send` → `WireSendParams`）。
fn params_ty(tool: &str) -> String {
    format!("{}Params", pascal(tool))
}

/// Rust 文字列リテラル用の escape（`\` と `"` のみ。schema には制御文字なし）。
fn rust_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// FieldTy → (Rust 型, schemars の `with=` 上書き)。
fn rust_ty(ty: &FieldTy) -> (&'static str, Option<&'static str>) {
    match ty {
        FieldTy::String => ("String", None),
        FieldTy::U64 => ("u64", None),
        FieldTy::ArrayString => ("Vec<String>", None),
        // Value のまま JsonSchema を導出すると型無し schema になり client が string で
        // 送る。object 型を明示して object を送らせる（手書き mcp.rs と同じ救済）。
        FieldTy::JsonObject => (
            "serde_json::Value",
            Some("std::collections::HashMap<String, serde_json::Value>"),
        ),
    }
}

/// app 非依存の tool-scaffold: params struct（club-kdl-codegen 抽出候補の縫い目）。
fn emit_params_struct(tool: &Tool) -> String {
    let mut out = String::new();
    out.push_str("#[derive(Debug, Serialize, Deserialize, JsonSchema)]\n");
    let ty = params_ty(&tool.name);
    if tool.fields.is_empty() {
        // MCP client の zod validation 対応で空構造体でも明示。
        out.push_str(&format!("pub struct {ty} {{}}\n"));
        return out;
    }
    out.push_str(&format!("pub struct {ty} {{\n"));
    for f in &tool.fields {
        let (rust, with) = rust_ty(&f.ty);
        let desc = rust_str(&f.description);
        match with {
            Some(w) => out.push_str(&format!(
                "    #[schemars(description = \"{desc}\", with = \"{w}\")]\n"
            )),
            None => out.push_str(&format!("    #[schemars(description = \"{desc}\")]\n")),
        }
        let ty_str = if f.optional {
            format!("Option<{rust}>")
        } else {
            rust.to_string()
        };
        out.push_str(&format!("    pub {}: {ty_str},\n", f.name));
    }
    out.push_str("}\n");
    out
}

/// VP forward glue: `#[tool]` メソッド本体（quic_call / self_lane inject / custom 委譲）。
fn emit_tool_method(group: &ToolGroup, tool: &Tool) -> String {
    let mut out = String::new();
    let params = params_ty(&tool.name);
    // params binding は、本体で参照しないなら `_params`（field 無し非-custom tool）。
    let bind = if tool.custom_body || !tool.fields.is_empty() {
        "params"
    } else {
        "_params"
    };
    out.push_str(&format!(
        "    #[tool(description = \"{}\")]\n",
        rust_str(&tool.description)
    ));
    out.push_str(&format!("    async fn {}(\n", tool.name));
    out.push_str("        &self,\n");
    out.push_str(&format!(
        "        rmcp::handler::server::wrapper::Parameters({bind}): rmcp::handler::server::wrapper::Parameters<{params}>,\n"
    ));
    out.push_str("    ) -> Result<CallToolResult, McpError> {\n");

    if tool.custom_body {
        // 宣言に落ちない imperative ロジックは手書き helper に委譲。
        out.push_str(&format!("        self.{}_impl(params).await\n", tool.name));
        out.push_str("    }\n");
        return out;
    }

    let Forward::QuicProcess = group.forward;

    if !tool.injects.is_empty() {
        out.push_str("        let __self_lane = self.self_lane.from_address()?;\n");
    }
    out.push_str("        let payload = serde_json::json!({\n");
    for inject in &tool.injects {
        let InjectSource::SelfLane = inject.source;
        out.push_str(&format!("            \"{}\": __self_lane,\n", inject.key));
    }
    for f in &tool.fields {
        out.push_str(&format!("            \"{0}\": params.{0},\n", f.name));
    }
    out.push_str("        });\n");
    out.push_str(&format!(
        "        let resp = self.quic_call(\"{}\", payload).await?;\n",
        tool.method
    ));
    out.push_str("        Ok(CallToolResult::success(vec![rmcp::model::Content::text(\n");
    out.push_str(&format!(
        "            serde_json::to_string_pretty(&resp).unwrap_or_else(|_| \"{}\".to_string()),\n",
        rust_str(&tool.fallback)
    ));
    out.push_str("        )]))\n");
    out.push_str("    }\n");
    out
}

/// 生成ファイル全体（header + params structs + #[tool_router] impl）。
pub fn emit_rust(schema: &ToolSchema) -> String {
    let mut out = String::new();
    out.push_str(
        "// @generated by VP-local agent-tool codegen — DO NOT EDIT.\n\
         // SSOT: crates/vantage-point/schema/vp-agent.kdl\n\
         // 再生成: cargo test -p vantage-point --test agent_tools_codegen\n\
         #![allow(unused_imports, dead_code)]\n\n\
         use rmcp::{\n\
         \x20   ErrorData as McpError, handler::server::wrapper::Parameters, model::*,\n\
         \x20   schemars::JsonSchema, tool, tool_router,\n\
         };\n\
         use serde::{Deserialize, Serialize};\n\n\
         use crate::mcp::VantageMcp;\n\n",
    );

    // params structs（source 順）。
    for group in &schema.groups {
        for tool in &group.tools {
            out.push_str(&emit_params_struct(tool));
            out.push('\n');
        }
    }

    // 単一の named router impl。手書き router と `Self::tool_router() +
    // Self::agent_tool_router()` で合流する（rmcp の ToolRouter は Add 実装済）。
    out.push_str("#[tool_router(router = agent_tool_router, vis = \"pub(crate)\")]\n");
    out.push_str("impl VantageMcp {\n");
    let mut first = true;
    for group in &schema.groups {
        for tool in &group.tools {
            if !first {
                out.push('\n');
            }
            first = false;
            out.push_str(&emit_tool_method(group, tool));
        }
    }
    out.push_str("}\n");

    out
}
