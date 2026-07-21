//! World daemon への projects mutation 通知 (HTTP blocking、 専用 OS thread 実行)。
//!
//! PR-D (control plane 一元化): CLI の projects.kdl 直書きを daemon 経由に移管する。
//! `reqwest::blocking` を**専用 OS thread**で実行することで、 async context (tokio runtime 内、
//! 例: World daemon の axum handler `world_sync_projects` や `start_process`) から呼ばれても
//! nested runtime panic (= `reqwest::blocking` が内部で runtime を作る際、 async context だと
//! 「Cannot block the current thread from within an asynchronous context」で落ちる) を回避する。
//! 新規 OS thread は tokio context 外なので安全。 sync context (CLI) からは単に別 thread で実行
//! されるだけ。 daemon 不在は best-effort で false/None (= `vp projects` の Unison 経路とは別、
//! sync/async どちらの caller からも安全に呼べる)。

use crate::cli::world_port;
use crate::projects_file::SyncOutcome;

/// blocking HTTP を専用 OS thread で実行し、 join して結果を返す。
///
/// tokio runtime 内 (async context) から `reqwest::blocking` を直接呼ぶと panic するため、
/// tokio context 外の新規 OS thread に退避する。 thread panic / join 失敗時は `T::default()`。
fn in_dedicated_thread<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + Default + 'static,
{
    std::thread::spawn(f).join().unwrap_or_default()
}

/// 2 秒 timeout の blocking client (daemon 不在を素早く検出)。
fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
}

fn url(path: &str) -> String {
    format!("http://[::1]:{}{}", world_port(), path)
}

/// ghost project 除去を daemon に依頼する。
///
/// 成功時 `SyncOutcome`、 daemon 不在 / エラーは None (caller は kdl フォールバックに落とす)。
pub fn notify_world_sync() -> Option<SyncOutcome> {
    in_dedicated_thread(move || {
        let c = client()?;
        let resp = c.post(url("/api/world/projects/sync")).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().ok()?;
        Some(SyncOutcome {
            removed: v
                .get("removed")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| e.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        })
    })
}
