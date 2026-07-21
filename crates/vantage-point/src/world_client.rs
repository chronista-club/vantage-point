//! World daemon への one-shot control RPC (Unison `world-control`、 専用 OS thread 実行)。
//!
//! PR-D (control plane 一元化): CLI の projects.kdl 直書きを daemon 経由に移管する。
//!
//! ## doc 45 段 2 — transport を HTTP から Unison へ
//!
//! 旧実装は `reqwest::blocking` で `/api/world/projects/{sync,reload}` を叩いていた。
//! control plane は Unison に寄せる方針（doc 45: KDL schema / drift 検出 / MCP tool 合成が
//! 付いてくる）なので、transport を [`WorldControlClient`] に差し替えた。**呼び出し側の
//! 意味論（best-effort、daemon 不在なら None）は変えていない**。
//!
//! Unison client は async なので、sync caller のために **専用 OS thread + 短命 runtime** で
//! 実行する。これは旧 `reqwest::blocking` 時代からの構造をそのまま踏襲したもので、
//! async context (tokio runtime 内、例: World daemon の axum handler) から呼ばれても
//! nested runtime panic (「Cannot block the current thread from within an asynchronous
//! context」) を起こさない。新規 OS thread は tokio context 外なので安全。
//! daemon 不在は best-effort で None (= `vp projects` の Unison 経路とは別、sync/async
//! どちらの caller からも安全に呼べる)。

use crate::cli::world_port;
use crate::daemon::client::WorldControlClient;
use crate::projects_file::SyncOutcome;

/// world-control RPC を専用 OS thread の短命 runtime で実行し、 join して結果を返す。
///
/// daemon 不在 / 接続失敗 / RPC エラー / thread panic はすべて `None`（best-effort）。
/// retry を 1 にしているのは「daemon が居ないことを素早く確定させたい」ため
/// （呼び出し側はいずれも kdl 直操作へのフォールバックを持つ）。
fn world_control_blocking<T, F, Fut>(f: F) -> Option<T>
where
    F: FnOnce(WorldControlClient) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async move {
            let client = WorldControlClient::connect(world_port(), 1).await.ok()?;
            f(client).await.ok()
        })
    })
    .join()
    .ok()
    .flatten()
}

/// ghost project 除去を daemon に依頼する。
///
/// 成功時 `SyncOutcome`、 daemon 不在 / エラーは None (caller は kdl フォールバックに落とす)。
pub fn notify_world_sync() -> Option<SyncOutcome> {
    let removed = world_control_blocking(|client| async move { client.projects_sync().await })?;
    Some(SyncOutcome { removed })
}

/// 稼働中の daemon に projects.kdl の reload を依頼する (best-effort、結果は捨てる)。
///
/// VP-189: CLI が projects.kdl を書き換えても、 既に稼働している daemon は in-memory projects を
/// 保持したままで乖離する。 daemon が動いていなければ黙って無視してよい (= 次回 daemon 起動時の
/// `load_config` で projects.kdl が読まれるため取りこぼしにならない)。
pub fn notify_world_reload() {
    let _ = world_control_blocking(|client| async move { client.projects_reload().await });
}

/// 登録 project 一覧を daemon から取得する (`(name, path)` の組)。
///
/// doc 45 段 2: 旧 `GET /api/world/projects` の後継。daemon 不在は None
/// (caller は projects.kdl フォールバックに落とす)。
pub fn list_projects_blocking() -> Option<Vec<(String, String)>> {
    let projects = world_control_blocking(|client| async move { client.projects_list().await })?;
    Some(
        projects
            .iter()
            .filter_map(|p| {
                let name = p.get("name")?.as_str()?.to_string();
                let path = p.get("path")?.as_str()?.to_string();
                Some((name, path))
            })
            .collect(),
    )
}

/// 稼働中 project の snapshot を daemon から取得する (`registry.list`)。
///
/// doc 45 段 2: 旧 `GET /api/world/processes` の後継。daemon 不在は None。
/// 各要素は `{project_name, port, pid, project_path}`（fold-in 後は port=0 / pid=World 自身なので
/// 意味を持つのは name と path、doc 44 §5.3）。
pub fn list_processes_blocking() -> Option<Vec<serde_json::Value>> {
    world_control_blocking(|client| async move { client.processes_list().await })
}
