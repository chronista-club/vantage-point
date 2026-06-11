//! SP → TheWorld の wire HTTP client (R2-a、 設計 mem_1CbvcJj4ppU3QKH9d7xMpT)
//!
//! wire store は TheWorld (port 32000、 config override 可) に中央化された。
//! SP の wire ハンドラ ([`crate::process::unison_server`]) / actor
//! (notify / lane-spawn) はこの client 経由で中央 store を読み書きする。
//!
//! TheWorld 停止 = wire 停止 (設計決定 D1-c で許容済、 実運用は既に事実上依存)。
//! 呼び出し側は Err を受けて各自の方針で扱う:
//! - proxy (handle_wire_*): エラーをそのまま caller に返す
//! - actor (notify / lane-spawn): retry loop で TheWorld 復帰を待つ

use std::sync::OnceLock;
use std::time::Duration;

/// long-poll (server 側 max 30s) を内包するため余裕を持った client timeout
const REQUEST_TIMEOUT: Duration = Duration::from_secs(40);

/// 接続確立だけの timeout (TheWorld 不在の検出を速くする)
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client build failed")
    })
}

/// TheWorld の port を解決する (config override → default 32000)
pub(crate) fn world_port() -> u16 {
    crate::config::Config::load()
        .map(|c| c.port_layout().world_port)
        .unwrap_or(crate::cli::WORLD_PORT)
}

/// TheWorld の wire API を呼ぶ。 `path` は `/api/wire/send` 等。
///
/// エラー規約:
/// - transport 失敗 → `Err("TheWorld unreachable ...")`
/// - 応答 JSON に `error` field → その内容を Err として relay
///   (TheWorld 側 handler は `{status: "error", error: <msg>}` 形で返す)
pub(crate) async fn call(
    path: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("http://127.0.0.1:{}{}", world_port(), path);
    let resp = http_client()
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| {
            format!(
                "TheWorld unreachable ({url}): {e} — wire store は TheWorld に中央化されています。 \
                 `vp daemon start` を確認してください"
            )
        })?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("TheWorld wire 応答の JSON parse 失敗: {e}"))?;
    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    Ok(body)
}
