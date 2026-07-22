//! プロセス発見モジュール
//!
//! TheWorld（port 32000）を単一の真実源として稼働中 project を発見する。
//!
//! ## データフロー
//!
//! ```text
//! project 起動（World が in-process で起こす）→ World の registry に登録
//! 問い合わせ → TheWorld Unison `registry.list` (QUIC :32000) → 返却
//! ```
//!
//! doc 44 P1 (fold-in) 以前は「SP が QUIC registry で自己登録し、切断で即時除去」
//! だったが、project が World と同一プロセスになり自己登録も切断も無くなった。
//!
//! doc 45 段 2: 問い合わせ transport を `GET /api/world/processes` から Unison
//! `registry.list` に差し替えた（`vp ps` が既に使っている面と同じ 1 本に寄せる）。

use crate::config::Config;
use crate::daemon::client::WorldControlClient;

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

/// TheWorld `registry.list` が返す Process エントリ
#[derive(Debug, serde::Deserialize)]
struct WorldProcessEntry {
    #[serde(default)]
    port: u16,
    #[serde(default)]
    pid: u32,
    project_path: String,
}

/// 全稼働中 project を取得
///
/// TheWorld (port 32000) に問い合わせ。project を起こすのは World 自身なので
/// World の registry が単一の真実源（doc 44 P1 fold-in 以前は SP の QUIC 自己登録が source）。
///
/// 返る `ProcessInfo` の `port` は常に 0、`pid` は World 自身のもの — どちらも
/// SP プロセス時代の遺構で、意味を持つのは `project_dir` だけ（doc 44 §5.3）。
pub async fn list() -> Vec<ProcessInfo> {
    query_world().await.unwrap_or_default()
}

/// project 1 件が抱える lane の集計。
#[derive(Debug, Clone, Copy, Default)]
pub struct LaneCounts {
    /// 登録されている lane 総数（PTY を持たない chat mode の lane も含む）。
    pub total: usize,
    /// `LaneState::Running` の lane 数。
    pub running: usize,
}

/// lane 一覧（`world-control.lanes/list` の返り値）を project 名ごとに集計する（純関数）。
///
/// doc 44 §5.3: fold-in で `vp ps` の PORT / PID 列が無意味化した（project は World と
/// 同一プロセスなので pid は全行 World 自身、port は不在の 0）。代わりに project の実体的な
/// 差である「何本のラインを抱え、そのうち動いているものがあるか」を出すための集計。
///
/// 各要素の想定 shape: `{"address": {"project": "<name>", ...}, "state": "running", ...}`。
/// 期待しない形（key 欠落 / 型違い）は**その lane を黙って飛ばす** — `vp ps` は表示系なので、
/// 1 件の形崩れで一覧全体を落とすより数え漏らす方が害が小さい。
///
/// ⚠️ 既知の制限: 集計キーは project **名**（`LaneAddress.project`）。これは lane 起動時に
/// 一度だけ解決されて凍結されるため、稼働中に `vp projects rename` すると `vp ps` 側の
/// 新名と一致せず、その project だけ LANES が `-`（idle 扱い）に落ちる。実害は表示のみで、
/// 該当 project を再起動すれば名前が再解決されて解消する。根治するなら join を path ベースに
/// する（`processes_list` は既に path を持つ）が、そこは transport 統一（doc 45）の範囲。
pub fn count_lanes_by_project_entries(
    lanes: &[serde_json::Value],
) -> std::collections::HashMap<String, LaneCounts> {
    let mut out: std::collections::HashMap<String, LaneCounts> = std::collections::HashMap::new();
    for lane in lanes {
        let Some(project) = lane
            .get("address")
            .and_then(|a| a.get("project"))
            .and_then(|p| p.as_str())
        else {
            continue;
        };
        let entry = out.entry(project.to_string()).or_default();
        entry.total += 1;
        if lane.get("state").and_then(|s| s.as_str()) == Some("running") {
            entry.running += 1;
        }
    }
    out
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

/// TheWorld に問い合わせ（Unison `registry.list`）
///
/// daemon 不在 / 接続失敗は None（caller の `list()` が空 Vec に落とす）。
/// retry=1 は「daemon が居ないことを素早く確定させたい」ため（旧 HTTP の 1s timeout 相当）。
async fn query_world() -> Option<Vec<ProcessInfo>> {
    let client = WorldControlClient::connect(crate::cli::world_port(), 1)
        .await
        .ok()?;
    let processes = client.processes_list().await.ok()?;

    Some(
        processes
            .into_iter()
            .filter_map(|v| serde_json::from_value::<WorldProcessEntry>(v).ok())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// project ごとに total / running を数える（doc 44 §5.3 の LANES / STATUS 列の土台）。
    #[test]
    fn counts_lanes_per_project_by_state() {
        let lanes = vec![
            serde_json::json!({"address": {"project": "alpha", "kind": "root"}, "state": "running"}),
            serde_json::json!({"address": {"project": "alpha", "kind": "performer"}, "state": "dead"}),
            serde_json::json!({"address": {"project": "beta",  "kind": "root"}, "state": "running"}),
        ];
        let counts = count_lanes_by_project_entries(&lanes);

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
            "居ない project は entry を作らない"
        );
    }

    /// 形が崩れた lane は飛ばし、健全な lane の集計は保つ。
    ///
    /// `vp ps` は表示系なので、1 件の形崩れで一覧全体を落とすより数え漏らす方が害が小さい。
    #[test]
    fn skips_malformed_lanes_without_dropping_the_rest() {
        let lanes = vec![
            serde_json::json!({"address": {"project": "alpha"}, "state": "running"}),
            serde_json::json!({"address": {}}), // project 欠落
            serde_json::json!({"state": "running"}), // address 欠落
            serde_json::json!({"address": {"project": 42}}), // project が文字列でない
        ];
        let counts = count_lanes_by_project_entries(&lanes);
        assert_eq!(counts.len(), 1);
        assert_eq!(counts["alpha"].total, 1);
    }

    /// 空入力でも panic せず空 map を返す。
    #[test]
    fn missing_or_empty_lanes_is_empty_map() {
        assert!(count_lanes_by_project_entries(&[]).is_empty());
    }
}
