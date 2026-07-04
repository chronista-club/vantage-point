//! Process クライアント（CLI 用、 World process-proxy ask 経由）
//!
//! L0 portless: 旧 `ProcessClient` (SP HTTP 直結) は撤去。 CLI は World :32000 の process-proxy ask
//! で SP を操作する。
//! - `world_process_request` / `_blocking`: World process-proxy ask の core (async / sync 版)。
//! - `resolve_project_path_from_target`: target → project_path 解決。

use anyhow::{Result, bail};

use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};

// L0 portless: `ProcessClient` (SP HTTP 直結 blocking client) は撤去済。 CLI の World ask は
// `world_process_request_blocking` / `resolve_project_path_from_target` を使う (SP port 解決も不要)。

/// F6 (doc 27 §3.4.5/§6): World :32000 の "process-proxy" channel 経由で SP process method を
/// ask する helper。 L0 portless 後の CLI → SP 操作の唯一の入口 (旧来の SP 直結 port QUIC は撤去)。
/// World が project_path から path_key を正規化して当該 SP の
/// "control" channel を逆引きし、 dispatch_process_method へ forward する (SP 直結 HTTP/QUIC を撤去)。
/// 戻り値は SP の応答 JSON (unison error frame `{"error":..}` は Err に変換)。
///
/// 既定 outer timeout = 35s (delete は tmux kill 等を含むが ≤35s)。 lane clone のような長い
/// operation は [`world_process_request_with_timeout`] で個別に伸ばす。
pub async fn world_process_request(
    world_port: u16,
    project_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    world_process_request_with_timeout(
        world_port,
        project_path,
        method,
        payload,
        std::time::Duration::from_secs(35),
    )
    .await
}

/// [`world_process_request`] の outer timeout を呼び出し側が指定する版。
///
/// `lane_create` は SP 側で git worktree clone (`new_performer_in`、 spawn_blocking) を含み
/// 数 10 sec かかり得るため、 MCP `quic_call_with_timeout("lane_create", .., 60s)` と揃えて 60s を
/// 渡す。 CLI 35s / MCP 60s の非対称だと、 大規模 repo で CLI だけ先に timeout → rollback 発火 →
/// SP が clone 完走 → orphan lane、 という race が起きる (moody-blues review #1)。 timeout を
/// 揃えることで CLI を MCP と同じ挙動にする (残余 race は MCP と同じ既知許容範囲)。
pub async fn world_process_request_with_timeout(
    world_port: u16,
    project_path: &str,
    method: &str,
    payload: serde_json::Value,
    timeout: std::time::Duration,
) -> Result<serde_json::Value> {
    // QUIC(rustls) は CryptoProvider の install が前提（install 済みなら no-op）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let addr = format!("[::1]:{}", world_port);

    // connect → handshake → ask 全体を timeout で bound (caller 指定、 既定 35s)。
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
        // 詰めて返す（mcp.rs quic_call と同じ扱い）。 Err に変換する。
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            bail!("World process-proxy error ({}): {}", method, err);
        }
        Ok::<serde_json::Value, anyhow::Error>(resp)
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        Err(_) => bail!(
            "World ({}) process-proxy.{} が {:?} 以内に応答しませんでした",
            addr,
            method,
            timeout
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

// tmux decoupling PR2: `resolve_pane_via_world`（label/pane_id → tmux pane 解決）は唯一の
// 呼び手だった `vp tmux` と dispatch 先 `tmux_resolve_pane` の撤去で退役。lane の宛先解決は
// lane address 直（`lane_nudge` / `lane_capture`）に一本化。
