//! プロセス発見モジュール
//!
//! daemon（port 32000）を単一の真実源として稼働中 repo を発見する。
//!
//! ## データフロー
//!
//! ```text
//! repo 起動（daemon が in-process で起こす）→ daemon の registry に登録
//! 問い合わせ → daemon Unison `registry.list` (QUIC :32000) → 返却
//! ```
//!
//! doc 44 P1 (fold-in) 以前は「repo が QUIC registry で自己登録し、切断で即時除去」
//! だったが、repo が daemon と同一プロセスになり自己登録も切断も無くなった。
//!
//! doc 45 段 2: 問い合わせ transport を `GET /api/daemon/processes` から Unison
//! `registry.list` に差し替えた（`vp ps` が既に使っている面と同じ 1 本に寄せる）。

use crate::config::Config;
use crate::daemon::client::DaemonControlClient;

/// 発見された Process の情報
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessInfo {
    /// ポート番号
    pub port: u16,
    /// プロセス ID
    pub pid: u32,
    /// repoディレクトリ（正規化済み）
    pub repo_dir: String,
    /// Terminal チャネル認証トークン
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_token: Option<String>,
}

/// daemon `registry.list` が返す Process エントリ
#[derive(Debug, serde::Deserialize)]
struct DaemonProcessEntry {
    #[serde(default)]
    port: u16,
    #[serde(default)]
    pid: u32,
    repo_path: String,
}

/// 全稼働中 repo を取得
///
/// daemon (port 32000) に問い合わせ。repo を起こすのは Daemon 自身なので
/// daemon の registry が単一の真実源（doc 44 P1 fold-in 以前は repo の QUIC 自己登録が source）。
///
/// 返る `ProcessInfo` の `port` は常に 0、`pid` は Daemon 自身のもの — どちらも
/// repo プロセス時代の遺構で、意味を持つのは `repo_dir` だけ（doc 44 §5.3）。
pub async fn list() -> Vec<ProcessInfo> {
    query_daemon().await.unwrap_or_default()
}

/// repo 1 件が抱える lane の集計。
#[derive(Debug, Clone, Copy, Default)]
pub struct LaneCounts {
    /// 登録されている lane 総数（PTY を持たない chat mode の lane も含む）。
    pub total: usize,
    /// `LaneState::Running` の lane 数。
    pub running: usize,
}

/// lane 一覧（`daemon-control.lanes/list` の返り値）を repo 名ごとに集計する（純関数）。
///
/// doc 44 §5.3: fold-in で `vp ps` の PORT / PID 列が無意味化した（repo は daemon と
/// 同一プロセスなので pid は全行 Daemon 自身、port は不在の 0）。代わりに repo の実体的な
/// 差である「何本のラインを抱え、そのうち動いているものがあるか」を出すための集計。
///
/// 各要素の想定 shape: `{"address": {"repo": "<name>", ...}, "state": "running", ...}`。
/// 期待しない形（key 欠落 / 型違い）は**その lane を黙って飛ばす** — `vp ps` は表示系なので、
/// 1 件の形崩れで一覧全体を落とすより数え漏らす方が害が小さい。
///
/// ⚠️ 既知の制限: 集計キーは repo **名**（`LaneAddress.repo`）。これは lane 起動時に
/// 一度だけ解決されて凍結されるため、稼働中に `vp repos rename` すると `vp ps` 側の
/// 新名と一致せず、その repo だけ LANES が `-`（idle 扱い）に落ちる。実害は表示のみで、
/// 該当 repo を再起動すれば名前が再解決されて解消する。根治するなら join を path ベースに
/// する（`processes_list` は既に path を持つ）が、そこは transport 統一（doc 45）の範囲。
pub fn count_lanes_by_repo_entries(
    lanes: &[serde_json::Value],
) -> std::collections::HashMap<String, LaneCounts> {
    let mut out: std::collections::HashMap<String, LaneCounts> = std::collections::HashMap::new();
    for lane in lanes {
        let Some(repo) = lane
            .get("address")
            .and_then(|a| a.get("repo"))
            .and_then(|p| p.as_str())
        else {
            continue;
        };
        let entry = out.entry(repo.to_string()).or_default();
        entry.total += 1;
        if lane.get("state").and_then(|s| s.as_str()) == Some("running") {
            entry.running += 1;
        }
    }
    out
}

/// repoディレクトリから Process を検索
pub async fn find_by_repo(repo_dir: &str) -> Option<ProcessInfo> {
    let canonical = Config::normalize_path(std::path::Path::new(repo_dir));
    list().await.into_iter().find(|p| p.repo_dir == canonical)
}

/// 現在のワーキングディレクトリから Process を検索
///
/// cwd と一致するか、cwd が repo_dir のサブディレクトリならマッチ。
/// 複数マッチした場合は最も具体的な（パスが長い）ものを返す。
pub async fn find_for_cwd() -> Option<ProcessInfo> {
    let cwd = std::env::current_dir().ok()?;
    let cwd_str = Config::normalize_path(&cwd);

    let processes = list().await;

    processes
        .into_iter()
        .filter(|p| cwd_str == p.repo_dir || cwd_str.starts_with(&format!("{}/", p.repo_dir)))
        .max_by_key(|p| p.repo_dir.len())
}

/// daemon に問い合わせ（Unison `registry.list`）
///
/// daemon 不在 / 接続失敗は None（caller の `list()` が空 Vec に落とす）。
/// retry=1 は「daemon が居ないことを素早く確定させたい」ため（旧 HTTP の 1s timeout 相当）。
async fn query_daemon() -> Option<Vec<ProcessInfo>> {
    let client = DaemonControlClient::connect(crate::cli::daemon_port(), 1)
        .await
        .ok()?;
    let processes = client.processes_list().await.ok()?;

    Some(
        processes
            .into_iter()
            .filter_map(|v| serde_json::from_value::<DaemonProcessEntry>(v).ok())
            .map(|p| ProcessInfo {
                port: p.port,
                pid: p.pid,
                repo_dir: p.repo_path,
                terminal_token: None, // daemon は token を持たない — 必要なら health API で取得
            })
            .collect(),
    )
}

/// Terminal トークンを生成（UUID v4）
pub fn generate_terminal_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── Daemon uplink（退役）───────────────────────────────────
//
// doc 44 P1 (fold-in): repo → daemon の uplink（registry / canvas-ingest / control の
// 3 channel を 1 QUIC connection に集約したもの）は、repo プロセスの消滅とともに退役した。
// repo は daemon と同一プロセスの `Arc<AppState>` になったため、
//   - 自己登録 / heartbeat → `RepoRuntimes` の map エントリ
//   - canvas-ingest      → repo の TopicRouter を daemon が直接購読
//   - control 逆ルート    → `dispatch_repo_method` の直呼び
// にそれぞれ退化した。`run()` を外した時点で本ブロックは丸ごと到達不能になっている。

// ─── 同期ラッパー（CLI コマンドから使用）───────────────────
//
// resolve.rs / start.rs 等の同期関数から呼ぶための同期版。
// 内部で小さな tokio ランタイムを作成する。

/// 同期版: 全稼働中 Process を取得
pub fn list_blocking() -> Vec<ProcessInfo> {
    make_runtime().block_on(list())
}

/// 同期版: repoディレクトリから Process を検索
pub fn find_by_repo_blocking(repo_dir: &str) -> Option<ProcessInfo> {
    let canonical = Config::normalize_path(std::path::Path::new(repo_dir));
    let processes = list_blocking();
    processes.into_iter().find(|p| p.repo_dir == canonical)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// repo ごとに total / running を数える（doc 44 §5.3 の LANES / STATUS 列の土台）。
    #[test]
    fn counts_lanes_per_repo_by_state() {
        let lanes = vec![
            serde_json::json!({"address": {"repo": "alpha", "kind": "root"}, "state": "running"}),
            serde_json::json!({"address": {"repo": "alpha", "kind": "sub"}, "state": "dead"}),
            serde_json::json!({"address": {"repo": "beta",  "kind": "root"}, "state": "running"}),
        ];
        let counts = count_lanes_by_repo_entries(&lanes);

        let alpha = counts.get("alpha").expect("alpha");
        assert_eq!(
            (alpha.total, alpha.running),
            (2, 1),
            "dead は total にのみ数える"
        );
        let beta = counts.get("beta").expect("beta");
        assert_eq!((beta.total, beta.running), (1, 1));
        assert!(
            !counts.contains_key("gamma"),
            "居ない repo は entry を作らない"
        );
    }

    /// 形が崩れた lane は飛ばし、健全な lane の集計は保つ。
    ///
    /// `vp ps` は表示系なので、1 件の形崩れで一覧全体を落とすより数え漏らす方が害が小さい。
    #[test]
    fn skips_malformed_lanes_without_dropping_the_rest() {
        let lanes = vec![
            serde_json::json!({"address": {"repo": "alpha"}, "state": "running"}),
            serde_json::json!({"address": {}}),      // repo 欠落
            serde_json::json!({"state": "running"}), // address 欠落
            serde_json::json!({"address": {"repo": 42}}), // repo が文字列でない
        ];
        let counts = count_lanes_by_repo_entries(&lanes);
        assert_eq!(counts.len(), 1);
        assert_eq!(counts["alpha"].total, 1);
    }

    /// 空入力でも panic せず空 map を返す。
    #[test]
    fn missing_or_empty_lanes_is_empty_map() {
        assert!(count_lanes_by_repo_entries(&[]).is_empty());
    }
}
