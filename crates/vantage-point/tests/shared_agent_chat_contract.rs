//! mem_1CfcoK3QjHurVQ173tZDuA — 共有化で GFP のイベント情報を失わない。
use serde_json::json;
use vantage_point::conversation::ConversationEvent;

#[test]
fn session_init_preserves_mcp_connection_status() {
    let payload = json!({
        "kind": "session_init", "session_id": "session-a",
        "mcp_servers": [{"name": "gfp-estimate", "status": "connected"}]
    });
    let decoded = serde_json::from_value::<ConversationEvent>(payload);
    assert!(decoded.is_ok(), "MCP の名前と状態を受理する: {decoded:?}");
    let encoded = serde_json::to_value(decoded.unwrap()).unwrap();
    assert_eq!(
        encoded["mcp_servers"][0],
        json!({"name":"gfp-estimate","status":"connected"})
    );
}

#[test]
fn permission_request_preserves_session_scope_suggestions() {
    let payload = json!({
        "kind":"permission_request", "request_id":"request-a", "tool_name":"Bash",
        "input":{"command":"pwd"}, "display_name":"作業場所の確認",
        "description":"現在の作業場所を読む", "tool_use_id":"tool-a",
        "suggestions":[{"type":"addRules", "destination":"session", "behavior":"allow",
            "rules":[{"toolName":"Bash", "ruleContent":"pwd"}]}]
    });
    let decoded: ConversationEvent = serde_json::from_value(payload.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), payload);
}

#[test]
fn codex_mcp_request_preserves_session_permission_capability() {
    let payload = json!({"kind":"codex_requests", "requests":[{
        "request_id":"host-a:1", "kind":"mcp_approval", "title":"確認",
        "detail":{"tool":"estimate"}, "questions":[],
        "can_accept":true, "can_remember":true
    }]});
    let decoded = serde_json::from_value::<ConversationEvent>(payload.clone());
    assert!(decoded.is_ok(), "会話内許可の能力を受理する: {decoded:?}");
    assert_eq!(serde_json::to_value(decoded.unwrap()).unwrap(), payload);
}
