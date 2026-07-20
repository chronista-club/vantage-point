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

use crate::cli::{PORT_RANGE_END, PORT_RANGE_START, world_port};
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

/// ポートが利用可能かバインドして確認 (wildcard で test、 dual-stack)
///
/// 旧実装: `[::1]` (loopback specific) で test していたが、 既存 SP が `[::]` (wildcard)
/// で bound してる場合、 specific bind は OS の dual-stack 仕様で **success してしまう**
/// (= false positive、 「available」 判定 → actual SP bind で EADDRINUSE) という bug 発生
/// (bikeboy 2026-04-29 観測)。
///
/// 修正: SP server と同じ wildcard (`[::]`) で test bind 試行、
/// + TCP connect 経由で「listening 中の何か」 検出 (wildcard bind 不可なケース対応)。
fn is_port_available(port: u16) -> bool {
    use std::net::{Ipv6Addr, SocketAddrV6, TcpListener};
    // 1. wildcard bind を試行 (= SP が実際に bind するのと同じ場所)
    let v6_wild = TcpListener::bind(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0)).is_ok();
    let v4_wild = TcpListener::bind(("0.0.0.0", port)).is_ok();
    if !v6_wild || !v4_wild {
        return false;
    }
    // 2. listening している process が無いか念のため connect 経由で再確認
    std::net::TcpStream::connect_timeout(
        &format!("[::1]:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(50),
    )
    .is_err()
}

/// TheWorld API に問い合わせ
async fn query_world() -> Option<Vec<ProcessInfo>> {
    let client = build_client(1000);
    let url = format!("http://[::1]:{}/api/world/processes", world_port());

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

/// Terminal トークンを生成（UUID v4）
pub fn generate_terminal_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── World uplink（退役）───────────────────────────────────
//
// doc 44 P1 (fold-in): SP → TheWorld の uplink（registry / canvas-ingest / control の
// 3 channel を 1 QUIC connection に集約したもの）は、SP プロセスの消滅とともに退役した。
// project は World と同一プロセスの `Arc<AppState>` になったため、
//   - 自己登録 / heartbeat → `ProjectRuntimes` の map エントリ
//   - canvas-ingest      → project の TopicRouter を World が直接購読
//   - control 逆ルート    → `dispatch_process_method` の直呼び
// にそれぞれ退化した。`run()` を外した時点で本ブロックは丸ごと到達不能になっている。

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

/// 短命のランタイムを作成（同期ラッパー用）
fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime")
}
