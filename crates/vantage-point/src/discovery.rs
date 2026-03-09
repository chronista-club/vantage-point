//! プロセス発見モジュール
//!
//! TheWorld API を第一ソース、HTTP スキャンをフォールバックとして
//! 稼働中 Process を発見する。running.json を使わない。
//!
//! ## データフロー
//!
//! ```text
//! 問い合わせ → TheWorld API (port 32000) → 成功 → 返却
//!                                        → 失敗 → HTTP スキャン (33000-33010) → 返却
//! ```

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
/// 1. TheWorld API (port 32000) に問い合わせ
/// 2. 失敗したら HTTP スキャン (33000-33010)
pub async fn list() -> Vec<ProcessInfo> {
    // 1. TheWorld API
    if let Some(processes) = query_world().await {
        return processes;
    }

    // 2. HTTP スキャンフォールバック
    scan_ports().await
}

/// プロジェクトディレクトリから Process を検索
pub async fn find_by_project(project_dir: &str) -> Option<ProcessInfo> {
    let canonical = Config::normalize_path(std::path::Path::new(project_dir));
    list().await.into_iter().find(|p| p.project_dir == canonical)
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

/// TheWorld に Process を登録
pub async fn register(port: u16, project_dir: &str, pid: u32, terminal_token: &str) {
    let client = build_client(2000);
    let url = format!("http://[::1]:{}/api/world/processes/register", WORLD_PORT);

    let body = serde_json::json!({
        "port": port,
        "project_dir": project_dir,
        "pid": pid,
        "terminal_token": terminal_token,
    });

    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!("TheWorld に Process 登録完了 (port={})", port);
        }
        Ok(resp) => {
            tracing::debug!(
                "TheWorld 登録失敗 (status={}): スキャンで発見される",
                resp.status()
            );
        }
        Err(_) => {
            tracing::debug!("TheWorld 未起動: スキャンで発見される");
        }
    }
}

/// TheWorld から Process を登録解除
pub async fn unregister(port: u16) {
    let client = build_client(2000);
    let url = format!(
        "http://[::1]:{}/api/world/processes/unregister",
        WORLD_PORT
    );

    let body = serde_json::json!({ "port": port });

    // ベストエフォート — 失敗してもヘルスモニターが補完
    let _ = client.post(&url).json(&body).send().await;
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

/// HTTP スキャンで Process を発見
async fn scan_ports() -> Vec<ProcessInfo> {
    let client = build_client(500);
    let mut processes = Vec::new();

    for port in PORT_RANGE_START..=PORT_RANGE_END {
        let url = format!("http://[::1]:{}/api/health", port);
        if let Ok(resp) = client.get(&url).send().await
            && resp.status().is_success()
            && let Ok(health) = resp.json::<HealthResponse>().await
        {
            processes.push(ProcessInfo {
                port,
                pid: health.pid,
                project_dir: health.project_dir,
                terminal_token: health.terminal_token,
            });
        }
    }

    processes
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

// ─── 同期ラッパー（CLI コマンドから使用）───────────────────

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
