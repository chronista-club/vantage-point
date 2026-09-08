//! editor bridge / fleet の **JS 式 builder**（純 calculation、webview で評価する文字列を作るだけ）。
//!
//! 旧 `app.rs` から移設（棚卸し 項目 6 / 6-1 #2、2026-09-08。本文は順序付き diff で一致、差分は
//! `fn` → `pub(crate) fn` の 3 箇所）。呼び手は canvas 購読（`editor_command` → `AppEvent::EditorEval`、
//! doc 60 §2 の既知の例外 = daemon 側から webview の builder を呼ぶ）、IPC handler（`fleet:feedback`）、
//! device event arm（`fleet:dispatch`）。

/// `editor_command` の op を JS に組む。未知の op（`editor_bridge_js` が None）は評価せず、
/// 購読側がそのまま daemon へ返す `{"error": "未知の editor op: <op>"}` を Err で返す（文言は
/// 旧 `run_canvas_session` の None 分岐と同じ）。
pub(crate) fn editor_command_js(
    op: &str,
    field_id: Option<&str>,
    value: Option<&serde_json::Value>,
) -> Result<String, serde_json::Value> {
    editor_bridge_js(op, field_id, value)
        .ok_or_else(|| serde_json::json!({"error": format!("未知の editor op: {op}")}))
}

/// doc 48 Phase 2: `EditorCommand` op → webview で評価する JS 式 (純 calculation)。
///
/// 式は object を返し、wry (`evaluate_script_with_callback`) がそれを JSON 文字列化して
/// callback に渡す。editor host は webview 側 `ExposeEditorHostForBridge` (entry.tsx) が
/// `window.vpEditorHost` に明示 expose したものを `mcp` API (editor-mode.md D-10、
/// listFields / getValue / setValue) 経由で叩く — `creoEditor` console API は localhost
/// hostname heuristic 依存で vp-asset:// origin では当てにならないため使わない。
/// host 不在 (bundle 未 mount 等) は `{error}` object を返す JS にして Rust 側は透過。
/// 未知 op / set の引数欠落は None。
pub(crate) fn editor_bridge_js(
    op: &str,
    field_id: Option<&str>,
    value: Option<&serde_json::Value>,
) -> Option<String> {
    // h = EditorHostMcpApi
    const PRELUDE: &str = "const h=window.vpEditorHost&&window.vpEditorHost.mcp;if(!h)return{error:\"editor host not available\"};";
    // h = layout bridge（doc 49 LE-P2 PR2 → P4 PR3: layout-mcp.ts の scope dispatcher が
    // window.vpLayoutHost を所有。gallery-panes.tsx は 1 scope handler として登録される）
    const LAYOUT_PRELUDE: &str = "const h=window.vpLayoutHost&&window.vpLayoutHost.mcp;if(!h)return{error:\"layout host not available\"};";
    match op {
        // === layout bridge (LE-15) — editor と同じ配管、別 host global ===
        // LE-P4 PR3: get も body（{scope}）を渡す。value 欠落は null = 既定 scope
        "layout_get" => {
            let body = value
                .and_then(|v| serde_json::to_string(v).ok())
                .unwrap_or_else(|| "null".to_string());
            Some(format!("(()=>{{{LAYOUT_PRELUDE}return h.get({body})}})()"))
        }
        "layout_set" => {
            // body(JSON) はそのまま JS literal として合法 (JSON ⊂ JS)。value 欠落は防御的に None
            let body = serde_json::to_string(value?).ok()?;
            Some(format!("(()=>{{{LAYOUT_PRELUDE}return h.set({body})}})()"))
        }
        "layout_history" => {
            let body = value
                .and_then(|v| serde_json::to_string(v).ok())
                .unwrap_or_else(|| "null".to_string());
            Some(format!(
                "(()=>{{{LAYOUT_PRELUDE}return h.history({body})}})()"
            ))
        }
        "fields" => Some(format!(
            "(()=>{{{PRELUDE}return{{fields:h.listFields().map(f=>({{id:f.id,label:f.label,type:f.type,semantic:f.semantic,group:f.group??null,cssVar:f.cssVar??null,initial:f.initial??null,constraints:f.constraints??null,role:f.role??null}}))}}}})()"
        )),
        "values" => Some(format!(
            "(()=>{{{PRELUDE}return{{values:Object.fromEntries(h.listFields().map(f=>[f.id,h.getValue(f.id)]))}}}})()"
        )),
        "set" => {
            // serde_json::to_string の出力はそのまま JS literal として合法 (JSON ⊂ JS)。
            // field_id/value の必須検証は daemon 側 handler 済みだが、欠落は防御的に None。
            let id = serde_json::to_string(field_id?).ok()?;
            let value = serde_json::to_string(value?).ok()?;
            Some(format!(
                "(()=>{{{PRELUDE}h.setValue({id},{value});return{{ok:true,id:{id}}}}})()"
            ))
        }
        _ => None,
    }
}

/// fleet 配線 (doc 49 LE-19): `DeviceEvent` payload → webview の mapping registry へ渡す JS 式。
///
/// `control_event` のみ転送する (device_connected 等は sidebar registry の領分)。
/// editor bridge と違い応答不要の一方向 push なので callback なしの `evaluate_script` で投げる。
/// 受け手不在 (gallery 未 mount 等) は JS 側の `window.vpFleet` guard が吸収する。
/// フィードバック方向 (LE-19): webview の ipc body から fleet feedback payload を取り出す。
///
/// `{"t":"fleet:feedback","feedback":{...}}` の形のみ Some。tag 不一致 / 形不正は None
/// (通常の ipc dispatch に流す)。payload の中身の検証は daemon 側 (serde) が担う。
pub(crate) fn fleet_feedback_payload(body: &str) -> Option<serde_json::Value> {
    let v = serde_json::from_str::<serde_json::Value>(body).ok()?;
    if v.get("t").and_then(|t| t.as_str()) != Some("fleet:feedback") {
        return None;
    }
    v.get("feedback").cloned()
}

pub(crate) fn fleet_dispatch_js(payload: &serde_json::Value) -> Option<String> {
    if payload.get("kind").and_then(|v| v.as_str()) != Some("control_event") {
        return None;
    }
    // serde_json::to_string の出力はそのまま JS literal として合法 (JSON ⊂ JS)
    let body = serde_json::to_string(payload).ok()?;
    Some(format!(
        "(()=>{{const f=window.vpFleet;if(f&&f.dispatch)f.dispatch({body})}})()"
    ))
}

#[cfg(test)]
mod fleet_dispatch_js_tests {
    use super::fleet_dispatch_js;
    use serde_json::json;

    #[test]
    fn control_event_becomes_dispatch_call() {
        let payload = json!({
            "kind": "control_event",
            "port_name": "ROTO-CONTROL",
            "event": {"type": "knob", "index": 0, "value": 0.5},
        });
        let js = fleet_dispatch_js(&payload).expect("control_event は転送される");
        assert!(js.contains("window.vpFleet"));
        assert!(js.contains("\"port_name\":\"ROTO-CONTROL\""));
        assert!(js.contains("\"type\":\"knob\""));
    }

    #[test]
    fn non_control_events_are_not_forwarded() {
        let connected = json!({"kind": "device_connected", "port_name": "LPD8", "has_input": true});
        assert_eq!(fleet_dispatch_js(&connected), None);
        assert_eq!(fleet_dispatch_js(&json!({})), None);
    }

    #[test]
    fn feedback_payload_extracts_only_fleet_tag() {
        use super::fleet_feedback_payload;
        let body = r#"{"t":"fleet:feedback","feedback":{"knobs":[{"index":0,"value":0.5}],"fader":null,"pads":[]}}"#;
        let fb = fleet_feedback_payload(body).expect("fleet:feedback は抽出される");
        assert_eq!(fb["knobs"][0]["value"], 0.5);
        // 他 tag / 非 JSON は None（通常の ipc dispatch へ）
        assert_eq!(
            fleet_feedback_payload(r#"{"t":"term:write","d":"x"}"#),
            None
        );
        assert_eq!(fleet_feedback_payload("not json"), None);
    }
}

#[cfg(test)]
mod editor_bridge_js_tests {
    use super::{editor_bridge_js, editor_command_js};

    /// fields / values は bridge global (`vpEditorHost.mcp`) を経由する。
    #[test]
    fn read_ops_use_bridge_global() {
        for op in ["fields", "values"] {
            let js = editor_bridge_js(op, None, None).expect(op);
            assert!(
                js.contains("window.vpEditorHost"),
                "{op}: bridge global 不使用"
            );
            assert!(js.contains("listFields"), "{op}: mcp API 不使用");
        }
    }

    /// set は id / value を JS literal として埋め込む (quote は escape される)。
    #[test]
    fn set_encodes_arguments_as_js_literals() {
        let js = editor_bridge_js("set", Some("sb.text.base"), Some(&serde_json::json!(13.5)))
            .expect("set");
        assert!(js.contains(r#"h.setValue("sb.text.base",13.5)"#), "js={js}");

        let tricky = serde_json::json!("#FF3DAE\"</script>");
        let js = editor_bridge_js("set", Some("sb.conn.hitl"), Some(&tricky)).expect("set");
        assert!(
            js.contains(r##""#FF3DAE\"</script>""##),
            "escape されていない: {js}"
        );
    }

    /// 未知 op / set の引数欠落は None (daemon 側で {error} 応答に変換される)。
    #[test]
    fn unknown_op_and_missing_args_are_none() {
        assert!(editor_bridge_js("enter", None, None).is_none());
        assert!(editor_bridge_js("set", None, Some(&serde_json::json!(1))).is_none());
        assert!(editor_bridge_js("set", Some("id"), None).is_none());
    }

    /// layout 系 op は layout bridge global (`vpLayoutHost.mcp`) を経由する（LE-P2 PR2）。
    #[test]
    fn layout_ops_use_layout_bridge_global() {
        let get = editor_bridge_js("layout_get", None, None).expect("layout_get");
        assert!(get.contains("window.vpLayoutHost"), "js={get}");
        // LE-P4 PR3: value 欠落は null = 既定 scope（gallery、後方互換）
        assert!(get.contains("h.get(null)"), "js={get}");
        // editor 側の global には触れない（host の取り違え防止）
        assert!(!get.contains("vpEditorHost"), "js={get}");
    }

    /// layout_get は scope body（LE-P4 PR3）を JS literal として渡す。
    #[test]
    fn layout_get_passes_scope_body() {
        let js = editor_bridge_js(
            "layout_get",
            None,
            Some(&serde_json::json!({"scope": "app"})),
        )
        .expect("layout_get");
        assert!(js.contains(r#"h.get({"scope":"app"})"#), "js={js}");
    }

    /// layout_set は body(JSON) を JS literal として埋め込む。value 欠落は None。
    #[test]
    fn layout_set_encodes_body_and_requires_value() {
        let body = serde_json::json!({"notation": "a | b ~ c", "attention": {"a": 0.5}});
        let js = editor_bridge_js("layout_set", None, Some(&body)).expect("layout_set");
        assert!(
            js.contains(r#"h.set({"attention":{"a":0.5},"notation":"a | b ~ c"})"#),
            "js={js}"
        );
        assert!(editor_bridge_js("layout_set", None, None).is_none());
    }

    /// layout_history は value 省略で null（既定 limit）に落ちる。
    #[test]
    fn layout_history_defaults_to_null_body() {
        let js = editor_bridge_js("layout_history", None, None).expect("layout_history");
        assert!(js.contains("h.history(null)"), "js={js}");
        let js = editor_bridge_js(
            "layout_history",
            None,
            Some(&serde_json::json!({"limit": 5})),
        )
        .expect("layout_history");
        assert!(js.contains(r#"h.history({"limit":5})"#), "js={js}");
    }

    /// PR-EX: 未知 op は旧購読側と同じ文言の error JSON、既知 op は editor_bridge_js と同じ JS。
    #[test]
    fn editor_command_js_maps_unknown_op_to_error_json() {
        let err = editor_command_js("nope", None, None).unwrap_err();
        assert_eq!(err, serde_json::json!({"error": "未知の editor op: nope"}));
        let js = editor_command_js("fields", None, None).unwrap();
        assert_eq!(Some(js), editor_bridge_js("fields", None, None));
    }
}
