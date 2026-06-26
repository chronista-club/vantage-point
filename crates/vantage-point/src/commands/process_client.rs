//! Process クライアント（CLI 用、 World process-proxy ask 経由）
//!
//! L0 portless: 旧 `ProcessClient` (SP HTTP 直結) は撤去。 CLI は World :32000 の process-proxy ask
//! で SP を操作する。
//! - `world_process_request` / `_blocking`: World process-proxy ask の core (async / sync 版)。
//! - `resolve_project_path_from_target` / `resolve_pane_via_world`: target → project_path /
//!   tmux pane 解決。
//! - `send_process_message`: Unison(QUIC) "process" チャネルへの ProcessMessage 送信
//!   （MCP の `process_call`/`quic_call` 相当、sync CLI 用。PR1a で switch_lane が移行）。

use anyhow::{Result, bail};

use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};

// L0 portless: `ProcessClient` (SP HTTP 直結 blocking client) は最後の user だった `vp tmux` が
// World process-proxy ask に移行して全 dead 化したため撤去。 CLI の World ask は
// `world_process_request_blocking` / `resolve_pane_via_world` / `resolve_project_path_from_target`
// を使う (SP port 解決も不要)。

/// CLI 用: local SP の Unison(QUIC) "process" チャネルに ProcessMessage を 1 つ送る同期 helper。
///
/// MCP の `process_call`(mcp.rs) の CLI 同期版。sync CLI から呼べるよう内部で tokio runtime を
/// 建てて block_on する。`method` は `unison_server.rs:536-541` の dispatch arm 名
/// （"switch_lane" / "show" / "clear" 等）、`msg` はそのまま JSON 化されて payload になる。
/// caller は method と `ProcessMessage` variant が dispatch arm と一致することを保証する。
///
/// PR1a で switch_lane を HTTP `/api/show` から Unison に移行する入口。
/// PR1b (ROTO routing) 等、CLI から QUIC で ProcessMessage を投げる経路で再利用する。
pub fn send_process_message(
    port: u16,
    method: &str,
    msg: &crate::protocol::ProcessMessage,
) -> Result<()> {
    // QUIC(rustls) は CryptoProvider の install が前提（install 済みなら no-op）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let payload = serde_json::to_value(msg)?;
    // QUIC_PORT_OFFSET = 0 のため SP の HTTP port と同一 UDP port で QUIC が listen する。
    let addr = format!("[::1]:{}", port);

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow::anyhow!("tokio runtime build failed: {}", e))?;
    rt.block_on(async move {
        // connect+open+request 全体を 10s で bound（旧 HTTP の reqwest timeout 10s と等価)。
        // unison default request timeout は 30s、 connect の QUIC handshake は無 bound なので、
        // SP が wedge した場合に CLI が無言で長時間刺さるのを防ぐ。
        let work = async {
            let transport = unison::network::quic::QuicClient::builder()
                .trust_anchors(unison::network::TrustAnchors::SkipVerification)
                .build()
                .map_err(|e| anyhow::anyhow!("QUIC client build failed: {}", e))?;
            let client = unison::ProtocolClient::new(transport);
            client
                .connect(&addr)
                .await
                .map_err(|e| anyhow::anyhow!("QUIC connect {} 失敗: {}", addr, e))?;
            let channel = client
                .open_channel("process")
                .await
                .map_err(|e| anyhow::anyhow!("open process channel 失敗: {}", e))?;
            let resp: serde_json::Value = channel
                .request::<serde_json::Value, serde_json::Value>(method, &payload)
                .await
                .map_err(|e| anyhow::anyhow!("QUIC process.{} 失敗: {}", method, e))?;
            // unison は専用 error frame を持たず、 server Err は成功フレームに {"error":..} を
            // 詰めて返す（mcp.rs quic_call_with_timeout と同じ扱い）。素通しせず Err に変換する。
            if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                bail!("SP error (process.{}): {}", method, err);
            }
            Ok::<(), anyhow::Error>(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), work).await {
            Ok(result) => result,
            Err(_) => bail!(
                "SP ({}) QUIC process.{} が 10s 以内に応答しませんでした",
                addr,
                method
            ),
        }
    })
}

/// F6 (doc 27 §3.4.5/§6): World :32000 の "process-proxy" channel 経由で SP process method を
/// ask する同期 helper。 旧来の SP 直結 (`send_process_message` の port 直結) を World 経由の
/// reverse-route に移す CLI 用入口。 World が project_path から path_key を正規化して当該 SP の
/// "control" channel を逆引きし、 dispatch_process_method へ forward する (SP 直結 HTTP/QUIC を撤去)。
/// 戻り値は SP の応答 JSON (unison error frame `{"error":..}` は Err に変換)。
pub async fn world_process_request(
    world_port: u16,
    project_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    // QUIC(rustls) は CryptoProvider の install が前提（install 済みなら no-op）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let addr = format!("[::1]:{}", world_port);

    // connect → handshake → ask 全体を 35s で bound (delete は tmux kill 等の orchestration を
    // 含むため `send_process_message` の 10s では不足。 旧 MCP/CLI reqwest の 30s + handshake 往復)。
    let work = async {
        let transport = unison::network::quic::QuicClient::builder()
            .trust_anchors(unison::network::TrustAnchors::SkipVerification)
            .build()
            .map_err(|e| anyhow::anyhow!("QUIC client build failed: {}", e))?;
        let client = unison::ProtocolClient::new(transport);
        client
            .connect(&addr)
            .await
            .map_err(|e| anyhow::anyhow!("QUIC connect {} 失敗: {}", addr, e))?;
        let channel = client
            .open_channel("process-proxy")
            .await
            .map_err(|e| anyhow::anyhow!("open process-proxy channel 失敗: {}", e))?;
        // handshake: project_path を渡す (World が path_key に正規化して control channel を逆引き)。
        channel
            .request::<serde_json::Value, serde_json::Value>(
                "subscribe",
                &serde_json::json!({ "project_path": project_path }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("process-proxy handshake 失敗: {}", e))?;
        let resp: serde_json::Value = channel
            .request::<serde_json::Value, serde_json::Value>(method, &payload)
            .await
            .map_err(|e| anyhow::anyhow!("process-proxy {} 失敗: {}", method, e))?;
        // unison は専用 error frame を持たず、 server Err を成功フレームに {"error":..} で
        // 詰めて返す（send_process_message / mcp.rs quic_call と同じ扱い）。 Err に変換する。
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            bail!("World process-proxy error ({}): {}", method, err);
        }
        Ok::<serde_json::Value, anyhow::Error>(resp)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(35), work).await {
        Ok(result) => result,
        Err(_) => bail!(
            "World ({}) process-proxy.{} が 35s 以内に応答しませんでした",
            addr,
            method
        ),
    }
}

/// 同期版 (CLI 用入口)。 専用 runtime で `world_process_request` を block_on する。
/// async context (flow.rs 等) からは runtime 内 runtime で panic するので `world_process_request`
/// を直接 await すること。
pub fn world_process_request_blocking(
    world_port: u16,
    project_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow::anyhow!("tokio runtime build failed: {}", e))?;
    rt.block_on(world_process_request(
        world_port,
        project_path,
        method,
        payload,
    ))
}

/// L0 portless: target → project_path（World process-proxy handshake の安定 identifier）。
///
/// `resolve_target` の path 抽出版（旧 port 解決 `resolve_port_from_target` 撤去後の後継）。 SP port を
/// 使わず、 全 ResolvedTarget variant から project path を取り出す（Running=project_dir /
/// Configured=path / Cwd=path）。 SP が未起動でも path は決まるので、 旧 HTTP 経路の「起動してないと
/// 使えない」制約が消える。
pub fn resolve_project_path_from_target(target: Option<&str>, config: &Config) -> Result<String> {
    Ok(match resolve::resolve_target(target, config)? {
        ResolvedTarget::Running { project_dir, .. } => project_dir,
        ResolvedTarget::Configured { path, .. } => path,
        ResolvedTarget::Cwd { path } => path,
    })
}

/// L0 portless: tmux の label / pane_id / lane address を `(pane_id, 表示名)` に解決する。
///
/// 旧 `ProcessClient::resolve_pane`（HTTP `/api/tmux/resolve-pane?q=`）の World ask 版。
/// `tmux_resolve_pane` dispatch に `{query}` を送り、 応答 `{pane_id, meta:{label}}` から組み立てる。
/// `%`-prefix の pane_id は resolve を display 用 best-effort にし、 pane_id は query をそのまま使う
/// （旧挙動を保持）。 label / lane address は resolve 必須。
pub fn resolve_pane_via_world(project_path: &str, query: &str) -> Result<(String, String)> {
    let resp = world_process_request_blocking(
        crate::cli::WORLD_PORT,
        project_path,
        "tmux_resolve_pane",
        serde_json::json!({ "query": query }),
    );
    if query.starts_with('%') {
        // pane_id 直指定: resolve は display 用 best-effort、 pane_id は query をそのまま使う。
        let display = match resp {
            Ok(r) => match r.pointer("/meta/label").and_then(|v| v.as_str()) {
                Some(label) => format!("{} ({})", label, query),
                None => query.to_string(),
            },
            Err(_) => query.to_string(),
        };
        return Ok((query.to_string(), display));
    }
    // label / lane address: resolve 必須。
    let resp = resp?;
    if let Some(pane_id) = resp.get("pane_id").and_then(|v| v.as_str()) {
        let label = resp.pointer("/meta/label").and_then(|v| v.as_str());
        let display = match label {
            Some(l) => format!("{} ({})", l, pane_id),
            None => pane_id.to_string(),
        };
        Ok((pane_id.to_string(), display))
    } else {
        let err = resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("ペインが見つかりません");
        bail!("{}", err);
    }
}
