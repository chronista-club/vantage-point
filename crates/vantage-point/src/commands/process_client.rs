//! Process クライアント（CLI 用、 Daemon ask 経由）
//!
//! L0 portless: 旧 `ProcessClient` (repo HTTP 直結) は撤去。 CLI は Daemon :32000 の repo-proxy ask
//! で repo を操作する。
//! - `daemon_repo_request` / `_blocking`: daemon repo-proxy ask の core (async / sync 版)。
//! - `daemon_lanes_snapshot_blocking`: Daemon "daemon-repo" channel の cross-project lane 一覧。
//! - `resolve_repo_path_from_target`: target → repo_path 解決。

use anyhow::{Result, bail};

use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};

// L0 portless: `ProcessClient` (repo HTTP 直結 blocking client) は撤去済。 CLI の Daemon ask は
// `daemon_repo_request_blocking` / `resolve_repo_path_from_target` を使う (repo port 解決も不要)。

/// F6 (doc 27 §3.4.5/§6): Daemon :32000 の "repo-proxy" channel 経由で repo process method を
/// ask する helper。 L0 portless 後の CLI → repo 操作の唯一の入口 (旧来の repo 直結 port QUIC は撤去)。
/// daemon が repo_path から path_key を正規化して当該 repo の
/// "control" channel を逆引きし、 dispatch_repo_method へ forward する (repo 直結 HTTP/QUIC を撤去)。
/// 戻り値は repo の応答 JSON (unison error frame `{"error":..}` は Err に変換)。
///
/// 既定 outer timeout = 35s (delete は tmux kill 等を含むが ≤35s)。 lane clone のような長い
/// operation は [`daemon_repo_request_with_timeout`] で個別に伸ばす。
pub async fn daemon_repo_request(
    daemon_port: u16,
    repo_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    daemon_repo_request_with_timeout(
        daemon_port,
        repo_path,
        method,
        payload,
        std::time::Duration::from_secs(35),
    )
    .await
}

/// [`daemon_repo_request`] の outer timeout を呼び出し側が指定する版。
///
/// `lane_create` は repo 側で git worktree clone (`new_sub_in`、 spawn_blocking) を含み
/// 数 10 sec かかり得るため、 MCP `quic_call_with_timeout("lane_create", .., 60s)` と揃えて 60s を
/// 渡す。 CLI 35s / MCP 60s の非対称だと、 大規模 repo で CLI だけ先に timeout → rollback 発火 →
/// repo が clone 完走 → orphan lane、 という race が起きる (moody-blues review #1)。 timeout を
/// 揃えることで CLI を MCP と同じ挙動にする (残余 race は MCP と同じ既知許容範囲)。
pub async fn daemon_repo_request_with_timeout(
    daemon_port: u16,
    repo_path: &str,
    method: &str,
    payload: serde_json::Value,
    timeout: std::time::Duration,
) -> Result<serde_json::Value> {
    // QUIC(rustls) は CryptoProvider の install が前提（install 済みなら no-op）
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let addr = format!("[::1]:{}", daemon_port);

    // connect → handshake → ask 全体を timeout で bound (caller 指定、 既定 35s)。
    let work = async {
        let client = connect_daemon(&addr).await?;
        let channel = client
            .open_channel("repo-proxy")
            .await
            .map_err(|e| anyhow::anyhow!("open repo-proxy channel 失敗: {}", e))?;
        // handshake: repo_path を渡す (daemon が path_key に正規化して control channel を逆引き)。
        channel
            .request::<serde_json::Value, serde_json::Value>(
                "subscribe",
                &serde_json::json!({ "repo_path": repo_path }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("repo-proxy handshake 失敗: {}", e))?;
        let resp: serde_json::Value = channel
            .request::<serde_json::Value, serde_json::Value>(method, &payload)
            .await
            .map_err(|e| anyhow::anyhow!("repo-proxy {} 失敗: {}", method, e))?;
        // unison は専用 error frame を持たず、 server Err を成功フレームに {"error":..} で
        // 詰めて返す（mcp.rs quic_call と同じ扱い）。 Err に変換する。
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            bail!("daemon repo-proxy error ({}): {}", method, err);
        }
        Ok::<serde_json::Value, anyhow::Error>(resp)
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(result) => result,
        Err(_) => bail!(
            "Daemon ({}) repo-proxy.{} が {:?} 以内に応答しませんでした",
            addr,
            method,
            timeout
        ),
    }
}

/// Daemon (`[::1]:<port>`) への QUIC 接続を張る。 channel open は呼び出し側の役目。
///
/// 戻り値の `ProtocolClient` は **channel を生かすために保持し続ける**こと
/// (drop すると connection ごと閉じる)。
async fn connect_daemon(addr: &str) -> Result<unison::ProtocolClient> {
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| anyhow::anyhow!("QUIC client build failed: {}", e))?;
    let client = unison::ProtocolClient::new(transport);
    client
        .connect(addr)
        .await
        .map_err(|e| anyhow::anyhow!("QUIC connect {} 失敗: {}", addr, e))?;
    Ok(client)
}

/// Daemon "daemon-repo" channel の `list_all_lanes` を 1 回 ask する (CLI 用 blocking)。
///
/// 応答は `{"repos":[{"repo_name":..,"repo_path":..,"lanes":[LaneInfo]}]}`
/// (`daemon::server::build_node_lanes`)。 repo ごとに束ねた **cross-project の lane 一覧**で、
/// repo-proxy (`lanes_list`) と違い **対象 repo の repo が起動していなくても答えが返る**のが要点:
/// 「その repo は registry に居ない」= 稼働 lane 0 件、という**答え**が得られる
/// (repo-proxy だと control channel 逆引き失敗 = error になり、「不明」と区別が付かない)。
///
/// timeout が既定 35s ではなく 10s なのは、これが**判定の前提**を取る ask だからで、
/// 取れなければ見送りを保留して人に告げる (待たせるより早く「不明」と言う方が良い)。
/// 同 channel の handler は handshake 不要 (subscribe しない one-shot request)。
pub fn daemon_lanes_snapshot_blocking(daemon_port: u16) -> Result<serde_json::Value> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let addr = format!("[::1]:{}", daemon_port);
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow::anyhow!("tokio runtime build failed: {}", e))?;
    rt.block_on(async {
        let work = async {
            // client は channel を生かすため ask 完了まで保持する。
            let client = connect_daemon(&addr).await?;
            let channel = client
                .open_channel("daemon-repo")
                .await
                .map_err(|e| anyhow::anyhow!("open daemon-repo channel 失敗: {}", e))?;
            let resp: serde_json::Value = channel
                .request::<serde_json::Value, serde_json::Value>(
                    "list_all_lanes",
                    &serde_json::json!({}),
                )
                .await
                .map_err(|e| anyhow::anyhow!("daemon-repo list_all_lanes 失敗: {}", e))?;
            if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                bail!("daemon-repo error (list_all_lanes): {}", err);
            }
            Ok::<serde_json::Value, anyhow::Error>(resp)
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), work).await {
            Ok(result) => result,
            Err(_) => bail!(
                "Daemon ({}) daemon-repo.list_all_lanes が 10 秒以内に応答しませんでした",
                addr
            ),
        }
    })
}

/// 同期版 (CLI 用入口)。 専用 runtime で `daemon_repo_request` を block_on する。
/// async context (flow.rs 等) からは runtime 内 runtime で panic するので `daemon_repo_request`
/// を直接 await すること。
pub fn daemon_repo_request_blocking(
    daemon_port: u16,
    repo_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow::anyhow!("tokio runtime build failed: {}", e))?;
    rt.block_on(daemon_repo_request(daemon_port, repo_path, method, payload))
}

/// L0 portless: target → repo_path（daemon repo-proxy handshake の安定 identifier）。
///
/// `resolve_target` の path 抽出版（旧 port 解決 `resolve_port_from_target` 撤去後の後継）。 repo port を
/// 使わず、 全 ResolvedTarget variant から repo path を取り出す（Running=repo_dir /
/// Configured=path / Cwd=path）。 repo が未起動でも path は決まるので、 旧 HTTP 経路の「起動してないと
/// 使えない」制約が消える。
pub fn resolve_repo_path_from_target(target: Option<&str>, config: &Config) -> Result<String> {
    Ok(match resolve::resolve_target(target, config)? {
        ResolvedTarget::Running { repo_dir, .. } => repo_dir,
        ResolvedTarget::Configured { path, .. } => path,
        ResolvedTarget::Cwd { path } => path,
    })
}

// tmux decoupling PR2: `resolve_pane_via_daemon`（label/pane_id → tmux pane 解決）は唯一の
// 呼び手だった `vp tmux` と dispatch 先 `tmux_resolve_pane` の撤去で退役。lane の宛先解決は
// lane address 直（`lane_nudge` / `lane_capture`）に一本化。
