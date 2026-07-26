//! daemon への one-shot control RPC (Unison `daemon-control`、 専用 OS thread 実行)。
//!
//! PR-D (control plane 一元化): CLI の repos.kdl 直書きを daemon 経由に移管する。
//!
//! ## doc 45 段 2 — transport を HTTP から Unison へ
//!
//! 旧実装は `reqwest::blocking` で `/api/daemon/repos/{sync,reload}` を叩いていた。
//! control plane は Unison に寄せる方針（doc 45: KDL schema / drift 検出 / MCP tool 合成が
//! 付いてくる）なので、transport を [`DaemonControlClient`] に差し替えた。**呼び出し側の
//! 意味論（best-effort、daemon 不在なら None）は変えていない**。
//!
//! Unison client は async なので、sync caller のために **専用 OS thread + 短命 runtime** で
//! 実行する。これは旧 `reqwest::blocking` 時代からの構造をそのまま踏襲したもので、
//! async context (tokio runtime 内、例: daemon の axum handler) から呼ばれても
//! nested runtime panic (「Cannot block the current thread from within an asynchronous
//! context」) を起こさない。新規 OS thread は tokio context 外なので安全。
//! daemon 不在は best-effort で None (= `vp repos` の Unison 経路とは別、sync/async
//! どちらの caller からも安全に呼べる)。

use crate::cli::daemon_port;
use crate::daemon::client::DaemonControlClient;
use crate::repos_file::SyncOutcome;

/// daemon-control RPC を専用 OS thread の短命 runtime で実行し、 join して結果を返す。
///
/// daemon 不在 / 接続失敗 / RPC エラー / thread panic はすべて `None`（best-effort）。
/// retry を 1 にしているのは「daemon が居ないことを素早く確定させたい」ため
/// （呼び出し側はいずれも kdl 直操作へのフォールバックを持つ）。
fn daemon_control_blocking<T, F, Fut>(f: F) -> Option<T>
where
    F: FnOnce(DaemonControlClient) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async move {
            let client = DaemonControlClient::connect(daemon_port(), 1).await.ok()?;
            f(client).await.ok()
        })
    })
    .join()
    .ok()
    .flatten()
}

/// ghost repo 除去を daemon に依頼する。
///
/// 成功時 `SyncOutcome`、 daemon 不在 / エラーは None (caller は kdl フォールバックに落とす)。
pub fn notify_daemon_sync() -> Option<SyncOutcome> {
    let removed = daemon_control_blocking(|client| async move { client.repos_sync().await })?;
    Some(SyncOutcome { removed })
}

/// 稼働中の daemon に repos.kdl の reload を依頼する (best-effort、結果は捨てる)。
///
/// VP-189: CLI が repos.kdl を書き換えても、 既に稼働している daemon は in-memory repos を
/// 保持したままで乖離する。 daemon が動いていなければ黙って無視してよい (= 次回 daemon 起動時の
/// `load_config` で repos.kdl が読まれるため取りこぼしにならない)。
pub fn notify_daemon_reload() {
    let _ = daemon_control_blocking(|client| async move { client.repos_reload().await });
}

/// 登録 repo 一覧を daemon から取得する (`(name, path)` の組)。
///
/// doc 45 段 2: 旧 `GET /api/daemon/repos` の後継。daemon 不在は None
/// (caller は repos.kdl フォールバックに落とす)。
pub fn list_repos_blocking() -> Option<Vec<(String, String)>> {
    let repos = daemon_control_blocking(|client| async move { client.repos_list().await })?;
    Some(
        repos
            .iter()
            .filter_map(|p| {
                let name = p.get("name")?.as_str()?.to_string();
                let path = p.get("path")?.as_str()?.to_string();
                Some((name, path))
            })
            .collect(),
    )
}

/// 稼働中 repo の snapshot を daemon から取得する (`registry.list`)。
///
/// doc 45 段 2: 旧 `GET /api/daemon/processes` の後継。daemon 不在は None。
/// 各要素は `{repo_name, port, pid, repo_path}`（fold-in 後は port=0 / pid=Daemon 自身なので
/// 意味を持つのは name と path、doc 44 §5.3）。
pub fn list_processes_blocking() -> Option<Vec<serde_json::Value>> {
    daemon_control_blocking(|client| async move { client.processes_list().await })
}

/// 見送り判定を帳簿に記録し、反映後の滞留一覧を得る（doc 44 §7.5）。
///
/// daemon 不在 / RPC 失敗は `None`。呼び出し側（`vp lane cleanup`）は滞留の注記を
/// 諦めて続行する — **帳簿に書けないことは見送りを止める理由にならない**
/// （止める理由になるのは稼働状況が不明な時だけ、§7.5「不明は無いに畳まない」）。
pub fn farewell_observe_blocking(
    repo_path: &str,
    observations: &[crate::host::ledger::FarewellObservation],
) -> Option<Vec<crate::host::ledger::FarewellEntry>> {
    let path = repo_path.to_string();
    let observations = observations.to_vec();
    daemon_control_blocking(move |client| async move {
        client.farewell_observe(&path, &observations).await
    })
}

/// 実際に見送った lane を帳簿に記録する（doc 44 §7.5）。記録件数、失敗は `None`。
pub fn farewell_reclaimed_blocking(
    repo_path: &str,
    lanes: &[crate::host::ledger::FarewellObservation],
) -> Option<usize> {
    let path = repo_path.to_string();
    let lanes = lanes.to_vec();
    daemon_control_blocking(
        move |client| async move { client.farewell_reclaimed(&path, &lanes).await },
    )
}

/// 帳簿の見送り記録を新しい順に読む（`vp lane history`）。daemon 不在は `None`。
pub fn farewell_log_blocking(
    repo_path: &str,
    limit: usize,
) -> Option<Vec<crate::host::ledger::FarewellEntry>> {
    let path = repo_path.to_string();
    daemon_control_blocking(move |client| async move { client.farewell_log(&path, limit).await })
}
