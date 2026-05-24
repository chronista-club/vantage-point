//! Stand discovery API — `GET /api/stands` (doc 11 §4.1 PR-C)
//!
//! ## 役割
//!
//! VP install root の `.mise/tasks/vp/stand/{name}` に居る task ファイル群を
//! `mise tasks ls --json` 経由で discovery し、 stand 名と description を返す。
//! sidebar の `+ Add Wing` で stand dropdown 表示するための data source。
//!
//! ## キャッシュ
//!
//! `mise tasks ls --json` は ~100ms 程度の cost。 sidebar が頻繁に開閉される dogfood
//! を考えると毎回叩くのは hot 過ぎ、 process 全体で **TTL 30 秒の lazy cache** を持つ。
//! TTL 内なら cached vector を即返却、 expired なら再 scan + cache 更新。
//!
//! cache scope は process 全体 (per-SP)。 install root は process 起動後に変わらない
//! 前提なので per-project cache は不要、 全 caller で共有する。
//!
//! ## wire format
//!
//! ```json
//! {
//!   "stands": [
//!     {"name": "echoes", "description": "VP Stand: Echoes — ..."},
//!     {"name": "shell", "description": "VP Stand: bare login shell ..."},
//!     {"name": "tmux",  "description": "VP Stand: tmux session attached ..."}
//!   ]
//! }
//! ```
//!
//! `name` は `vp:stand:{name}` の `{name}` 部分そのまま (例: `"echoes"` / `"shell"` / `"tmux"`、 PR-pre2 で hd → echoes rename)、
//! sidebar が POST `/api/lanes` の `stand` field にそのまま使える形式。

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

/// Sidebar 側に返す stand 1 entry の schema。
#[derive(Debug, Clone, Serialize)]
pub struct StandInfo {
    /// `vp:stand:{name}` の name 部分 (例: `"echoes"` / `"shell"` / `"tmux"`)
    pub name: String,
    /// task ファイル先頭の `#MISE description="..."` の値 (空文字も許容)
    pub description: String,
}

/// `GET /api/stands` の response shape。
#[derive(Debug, Serialize)]
struct StandsResponse {
    stands: Vec<StandInfo>,
}

/// process 全体で共有する cache (TTL 30 秒)。
struct StandCache {
    cached_at: Instant,
    stands: Vec<StandInfo>,
}

static CACHE: LazyLock<Mutex<Option<StandCache>>> = LazyLock::new(|| Mutex::new(None));
const CACHE_TTL: Duration = Duration::from_secs(30);

/// `GET /api/stands` handler — TTL cache 経由で stand 一覧を返す。
pub async fn list_handler() -> impl IntoResponse {
    // fast path: cache 確認
    {
        let cache = CACHE.lock().expect("StandCache mutex poisoned");
        if let Some(c) = cache.as_ref()
            && c.cached_at.elapsed() < CACHE_TTL
        {
            return (
                StatusCode::OK,
                Json(StandsResponse {
                    stands: c.stands.clone(),
                }),
            )
                .into_response();
        }
    }

    // slow path: mise tasks ls --json で再 scan
    let stands = match list_available_stands().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "GET /api/stands: list_available_stands 失敗 (空リスト返却): {}",
                e
            );
            Vec::new()
        }
    };

    // cache 更新 (失敗時も Vec::new() で cache 化、 30 秒後に retry)
    {
        let mut cache = CACHE.lock().expect("StandCache mutex poisoned");
        *cache = Some(StandCache {
            cached_at: Instant::now(),
            stands: stands.clone(),
        });
    }

    (StatusCode::OK, Json(StandsResponse { stands })).into_response()
}

/// `mise tasks ls --json` を VP install root cwd で叩いて、 `vp:stand:` prefix の
/// task のみ filter して `StandInfo` に変換。
///
/// install root 解決失敗時は anyhow error を返す (caller は graceful degrade で空 list)。
async fn list_available_stands() -> anyhow::Result<Vec<StandInfo>> {
    let install_root = super::super::install_root::locate_install_root().ok_or_else(|| {
        anyhow::anyhow!("VP install root not found (locate_install_root returned None)")
    })?;

    let output = tokio::process::Command::new("mise")
        .args(["tasks", "ls", "--json"])
        .current_dir(install_root)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "mise tasks ls --json failed (exit={}): {}",
            output.status,
            stderr
        );
    }

    /// mise tasks ls --json の 1 entry — 必要な field だけ deserialize。
    #[derive(Deserialize)]
    struct MiseTask {
        name: String,
        #[serde(default)]
        description: String,
    }

    let tasks: Vec<MiseTask> = serde_json::from_slice(&output.stdout)?;
    let stands = tasks
        .into_iter()
        .filter_map(|t| {
            t.name
                .strip_prefix("vp:stand:")
                .map(|stand_name| StandInfo {
                    name: stand_name.to_string(),
                    description: t.description,
                })
        })
        .collect();
    Ok(stands)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mise --version` が動くか check。 CI (GitHub Actions ubuntu-latest 等) では mise 未 install
    /// なので、 test を graceful skip するための gate。 dev 環境 (mise 必須の VP toolchain) では真。
    fn mise_available() -> bool {
        std::process::Command::new("mise")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// install root 経由で `mise tasks ls --json` が走り、 vp:stand:* が 3 つ以上返ること。
    /// (PR-A で hd / shell / tmux 3 task を install root に置いた、 dogfood 中の追加 task は
    /// 環境依存だが 3 つ以上は保証)
    /// CI 環境 (mise 未 install) では skip。
    #[tokio::test]
    async fn list_available_stands_returns_pr_a_three_tasks() {
        if !mise_available() {
            eprintln!("skipping: mise not in PATH (CI 環境想定)");
            return;
        }
        let stands = list_available_stands()
            .await
            .expect("install root + mise が動く環境では成功するはず");
        let names: std::collections::HashSet<&str> =
            stands.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains("echoes"),
            "echoes stand 不在: got {:?}",
            names
        );
        assert!(names.contains("shell"), "shell stand 不在: got {:?}", names);
        assert!(names.contains("tmux"), "tmux stand 不在: got {:?}", names);
    }

    /// description が空文字でなく、 PR-A の `#MISE description=` が読まれていること。
    /// CI 環境 (mise 未 install) では skip。
    #[tokio::test]
    async fn stand_description_populated_from_task_metadata() {
        if !mise_available() {
            eprintln!("skipping: mise not in PATH (CI 環境想定)");
            return;
        }
        let stands = list_available_stands()
            .await
            .expect("install root + mise が動く環境では成功するはず");
        let echoes = stands
            .iter()
            .find(|s| s.name == "echoes")
            .expect("echoes 不在");
        assert!(
            echoes.description.contains("Echoes"),
            "echoes.description に Echoes 含まれない: {:?}",
            echoes.description
        );
    }
}
