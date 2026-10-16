//! Shared persistent Lane URLs; implementation is also used by CLI and MCP.
pub use vp_local_urls::*;

/// UI の検出 URL は保存しない。名前を取る CLI / MCP の Action は変更しない。
pub fn prepare_ui(
    path: &std::path::Path,
    repo: &str,
    lane: &str,
    payload: serde_json::Value,
) -> Result<Prepared, String> {
    if payload.get("action").and_then(serde_json::Value::as_str) == Some("probe_url") {
        let value = payload
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or("確認する URL がありません")?;
        return Ok(Prepared::ProbeUrl(validate_url(value)?.to_string()));
    }
    let action =
        serde_json::from_value(payload).map_err(|e| format!("URL 操作を確認できません: {e}"))?;
    prepare(path, repo, lane, action)
}
