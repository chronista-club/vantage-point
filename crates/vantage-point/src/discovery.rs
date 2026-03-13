//! プロセス発見モジュール
//!
//! TheWorld API（port 32000）を単一の真実源として稼働中 Process を発見する。
//! SP は QUIC "registry" チャネルで自己登録し、切断時に即時除去される。
//!
//! ## データフロー
//!
//! ```text
//! SP 起動 → QUIC "registry" チャネルで TheWorld に自己登録
//! 問い合わせ → TheWorld HTTP API (port 32000) → 返却
//! SP 停止/切断 → TheWorld が即時除去
//! ```

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::cli::{PORT_RANGE_END, PORT_RANGE_START, WORLD_PORT};
use crate::config::Config;

/// 発見された Process の情報
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessInfo {
    /// ポート番号
    pub port: u16,
    /// プロセス ID
    pub pid: u32,
    /// プロジェクトディレクトリ（正規化済み）
    pub project_dir: String,
    /// Terminal チャネル認証トークン
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_token: Option<String>,
}

/// TheWorld API のレスポンス
#[derive(Debug, serde::Deserialize)]
struct WorldProcessesResponse {
    processes: Vec<WorldProcessEntry>,
}

/// TheWorld が返す Process エントリ
#[derive(Debug, serde::Deserialize)]
struct WorldProcessEntry {
    port: u16,
    pid: u32,
    project_path: String,
}

/// Health API のレスポンス
#[derive(Debug, serde::Deserialize)]
struct HealthResponse {
    pid: u32,
    project_dir: String,
    #[serde(default)]
    terminal_token: Option<String>,
}

/// HTTP クライアントを生成（短タイムアウト）
fn build_client(timeout_ms: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 全稼働中 Process を取得
///
/// TheWorld API (port 32000) に問い合わせ。
/// SP は QUIC registry チャネルで自己登録するため、TheWorld が単一の真実源。
pub async fn list() -> Vec<ProcessInfo> {
    query_world().await.unwrap_or_default()
}

/// プロジェクトディレクトリから Process を検索
pub async fn find_by_project(project_dir: &str) -> Option<ProcessInfo> {
    let canonical = Config::normalize_path(std::path::Path::new(project_dir));
    list()
        .await
        .into_iter()
        .find(|p| p.project_dir == canonical)
}

/// 現在のワーキングディレクトリから Process を検索
///
/// cwd と一致するか、cwd が project_dir のサブディレクトリならマッチ。
/// 複数マッチした場合は最も具体的な（パスが長い）ものを返す。
pub async fn find_for_cwd() -> Option<ProcessInfo> {
    let cwd = std::env::current_dir().ok()?;
    let cwd_str = Config::normalize_path(&cwd);

    let processes = list().await;

    processes
        .into_iter()
        .filter(|p| cwd_str == p.project_dir || cwd_str.starts_with(&format!("{}/", p.project_dir)))
        .max_by_key(|p| p.project_dir.len())
}

/// 空きポートを検索（バインドテストのみ、ファイル不使用）
pub fn find_available_port() -> Option<u16> {
    (PORT_RANGE_START..=PORT_RANGE_END).find(|&port| is_port_available(port))
}

/// ポートが利用可能かバインドして確認（IPv6 + IPv4）
fn is_port_available(port: u16) -> bool {
    use std::net::{Ipv6Addr, SocketAddrV6, TcpListener};
    TcpListener::bind(SocketAddrV6::new(Ipv6Addr::LOCALHOST, port, 0, 0)).is_ok()
        && TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// TheWorld API に問い合わせ
async fn query_world() -> Option<Vec<ProcessInfo>> {
    let client = build_client(1000);
    let url = format!("http://[::1]:{}/api/world/processes", WORLD_PORT);

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let body = resp.json::<WorldProcessesResponse>().await.ok()?;

    Some(
        body.processes
            .into_iter()
            .map(|p| ProcessInfo {
                port: p.port,
                pid: p.pid,
                project_dir: p.project_path,
                terminal_token: None, // TheWorld は token を持たない — 必要なら health API で取得
            })
            .collect(),
    )
}

/// 特定ポートの Process から terminal_token を取得
pub async fn fetch_terminal_token(port: u16) -> Option<String> {
    let client = build_client(1000);
    let url = format!("http://[::1]:{}/api/health", port);

    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    let health = resp.json::<HealthResponse>().await.ok()?;
    health.terminal_token
}

/// Terminal トークンを生成（UUID v4）
pub fn generate_terminal_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── QUIC Registry 登録（TheWorld 永続接続）───────────────

/// TheWorld に QUIC "registry" チャネルで接続し、自己登録 + heartbeat を維持する
///
/// 切断時は自動的に再接続を試みる。shutdown_token がキャンセルされるまでループ。
/// TheWorld 側の registry チャネルハンドラが切断を検知 → running_processes から即時除去。
pub fn spawn_registry_keepalive(
    port: u16,
    project_dir: &str,
    pid: u32,
    terminal_token: &str,
    shutdown: CancellationToken,
) {
    let project_name = std::path::Path::new(project_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // config からプロジェクト名を解決（ディレクトリ名がデフォルト）
    let project_name = if let Ok(config) = Config::load() {
        let normalized = Config::normalize_path(std::path::Path::new(project_dir));
        config
            .projects
            .iter()
            .find(|p| Config::normalize_path(std::path::Path::new(&p.path)) == normalized)
            .map(|p| p.name.clone())
            .unwrap_or(project_name)
    } else {
        project_name
    };

    // tmux セッション名を付与（tmux 環境下なら `{project}-vp` 形式）
    let tmux_session = if crate::tmux::is_tmux_available() {
        Some(crate::tmux::session_name(&project_name))
    } else {
        None
    };

    let agent_card = serde_json::json!({
        "project_name": project_name,
        "port": port,
        "project_dir": project_dir,
        "pid": pid,
        "terminal_token": terminal_token,
        "tmux_session": tmux_session,
    });

    tokio::spawn(async move {
        loop {
            // TheWorld に QUIC 接続
            match connect_and_register(&agent_card).await {
                Ok(conn) => {
                    tracing::info!(
                        "Registry: QUIC 登録成功 (project={}, port={})",
                        project_name,
                        port
                    );

                    // Heartbeat ループ（15秒間隔）
                    // conn（ProtocolClient + UnisonChannel）はこのスコープで保持
                    let mut interval = tokio::time::interval(Duration::from_secs(15));
                    interval.tick().await; // 最初の tick をスキップ

                    loop {
                        tokio::select! {
                            _ = interval.tick() => {
                                if conn.channel
                                    .request("heartbeat", serde_json::json!({}))
                                    .await
                                    .is_err()
                                {
                                    tracing::warn!(
                                        "Registry: heartbeat 失敗 → 再接続"
                                    );
                                    break; // 外側ループで再接続
                                }
                            }
                            _ = shutdown.cancelled() => {
                                // グレースフル unregister
                                let _ = conn.channel
                                    .request("unregister", serde_json::json!({}))
                                    .await;
                                tracing::info!("Registry: QUIC 登録解除 (shutdown)");
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("Registry: TheWorld 接続失敗 ({}), 5秒後にリトライ", e);
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                        _ = shutdown.cancelled() => return,
                    }
                }
            }
        }
    });
}

/// QUIC 接続の所有権を保持する構造体
///
/// `ProtocolClient` が drop されると QUIC 接続も切れるため、
/// チャネルと一緒に保持する必要がある。
struct RegistryConnection {
    /// QUIC 接続の所有権（drop されないように保持）
    _client: unison::ProtocolClient,
    /// registry チャネル（heartbeat / unregister に使用）
    channel: unison::UnisonChannel,
}

/// TheWorld の "registry" チャネルに接続し、register リクエストを送信する
async fn connect_and_register(
    agent_card: &serde_json::Value,
) -> Result<RegistryConnection, String> {
    let client = unison::ProtocolClient::new_default()
        .map_err(|e| format!("QUIC client 作成失敗: {}", e))?;

    let addr = format!("[::1]:{}", WORLD_PORT);
    client
        .connect(&addr)
        .await
        .map_err(|e| format!("TheWorld 接続失敗: {}", e))?;

    let channel = client
        .open_channel("registry")
        .await
        .map_err(|e| format!("registry チャネルオープン失敗: {}", e))?;

    // register リクエスト送信
    let resp = channel
        .request("register", agent_card.clone())
        .await
        .map_err(|e| format!("register リクエスト失敗: {}", e))?;

    if resp.get("error").is_some() {
        return Err(format!("register 拒否: {}", resp));
    }

    Ok(RegistryConnection {
        _client: client,
        channel,
    })
}

// ─── 同期ラッパー（CLI コマンドから使用）───────────────────
//
// resolve.rs / start.rs 等の同期関数から呼ぶための同期版。
// 内部で小さな tokio ランタイムを作成する。

/// 同期版: 全稼働中 Process を取得
pub fn list_blocking() -> Vec<ProcessInfo> {
    make_runtime().block_on(list())
}

/// 同期版: プロジェクトディレクトリから Process を検索
pub fn find_by_project_blocking(project_dir: &str) -> Option<ProcessInfo> {
    let canonical = Config::normalize_path(std::path::Path::new(project_dir));
    let processes = list_blocking();
    processes.into_iter().find(|p| p.project_dir == canonical)
}

/// 同期版: 現在のワーキングディレクトリから Process を検索
pub fn find_for_cwd_blocking() -> Option<ProcessInfo> {
    make_runtime().block_on(find_for_cwd())
}

/// 同期版: terminal_token を取得
pub fn fetch_terminal_token_blocking(port: u16) -> Option<String> {
    make_runtime().block_on(fetch_terminal_token(port))
}

/// 短命のランタイムを作成（同期ラッパー用）
fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
}
