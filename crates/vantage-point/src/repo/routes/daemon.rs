//! daemon control の core 関数群（doc 45 段 4 で HTTP route は全廃）。
//!
//! かつては daemon の REST API（`/api/daemon/repos*` / `/api/daemon/processes*` /
//! `/api/daemon/lanes*`）の axum handler を置いていた。control plane は Unison
//! "daemon-control" channel に一本化したので（doc 45）、本 module は**入口を持たない
//! 実装だけ**になった: [`apply_repo_update`] / [`collect_lanes`] /
//! [`resolve_create_lane_args`] の 3 つを `daemon::server::handle_daemon_control` が呼ぶ。
//!
//! ## なぜ「入口が消えても関数は残る」のか
//!
//! これらは元々 route handler の中にしか無かった orchestration（update の rename+enabled 合成 /
//! lane の filter+sort / create の default 導出）で、Unison に同じ面を出す時に二重実装に
//! なりかけたものを段 1 で 1 実装へ畳んだ。畳んであったおかげで段 4 の HTTP 撤去は
//! **handler の殻を剥がすだけ**で済み、振る舞いは 1 行も動いていない。
//!
//! なお `routes/` に非 HTTP の core 関数が残るのは本 module が最初ではない
//! （`routes/lanes.rs` / `routes/stands.rs` / `routes/wire.rs` / `routes/delegation.rs` も
//! HTTP 撤去後に core / dispatch 実装だけを保持している）。

use crate::capability::RepoManagerCapability;

/// repo の部分更新（rename / enabled）を適用する — Unison `repos/update` の実体。
///
/// `name` / `enabled` はどちらも任意で、指定されたものだけを順に適用する。
/// **どちらも未指定なら `Err("No fields to update")`** — 「何も指定しない update」は
/// 呼び出し側のバグなので黙って成功にしない（旧 HTTP の 400 と同じ意味論を保つ）。
pub(crate) async fn apply_repo_update(
    daemon: &RepoManagerCapability,
    path: &str,
    name: Option<&str>,
    enabled: Option<bool>,
) -> Result<(), String> {
    let mut updated = false;

    if let Some(new_name) = name {
        daemon
            .rename_repo(path, new_name)
            .await
            .map_err(|e| e.to_string())?;
        updated = true;
    }

    if let Some(enabled) = enabled {
        daemon
            .set_repo_enabled(path, enabled)
            .await
            .map_err(|e| e.to_string())?;
        updated = true;
    }

    if updated {
        Ok(())
    } else {
        Err("No fields to update".to_string())
    }
}

/// lane 作成の省略時 default を導出する (= calc) — Unison `lanes/create` の実体。
///
/// repo create_handler と parity: branch 未指定 → `<user>/<name>` derive、
/// agent 未指定 → config の `default_agent` → `claude`。返り値は `(branch, agent)`。
pub(crate) fn resolve_create_lane_args(
    path: &str,
    name: &str,
    branch: Option<&str>,
    agent: Option<&str>,
) -> (String, String) {
    let repo_root = std::path::PathBuf::from(path);
    let branch = branch
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| super::lanes::derive_default_branch(&repo_root, name));
    let agent = agent.map(|s| s.to_string()).unwrap_or_else(|| {
        crate::config::Config::load()
            .map(|c| c.default_agent_or_claude().to_string())
            .unwrap_or_else(|_| "claude".to_string())
    });
    (branch, agent)
}

// doc 44 P1 (fold-in): daemon_register_process / daemon_unregister_process は撤去。
// repo は Daemon 自身が起こすため「外から自己登録される」経路が存在しない。

// doc 44 P1 (fold-in): POST /api/daemon/refresh (daemon_refresh) は撤去。
// 呼び出し元はゼロで、中身の `refresh_process_status`（PID liveness）が
// fold-in で無意味化したため（pid が全 repo 共通の Daemon 自身）。

// =============================================================================
// Lane Registry — Phase 1c: agent (Conversation on Claude CLI) が lane console を引く
// =============================================================================

/// Phase 1c: Lane filter query
///
/// Unison `lanes/list` の payload field からそのまま deserialize する
/// （旧 HTTP は同名の query string `?repo=&lane=&agent=`）。全 field 省略可 = 無フィルタ。
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct LanesQuery {
    /// Repo name filter (LaneAddress.repo)
    pub repo: Option<String>,
    /// Lane name filter — Conductor は "root"、 Performer は name (例: "sub")
    pub lane: Option<String>,
    /// Agent kind filter — "claude" or "shell"
    pub agent: Option<String>,
}

/// lane registry を filter + sort して返す — Unison `lanes/list` の実体。
///
/// 順序: repo 名昇順 → 同 repo 内は開発起点 (root) 先 → 続いて Performer (created_at 昇順)。
///
/// 全 repo の Lane を flatten して返すので、`vp ps` / sidebar が見る一覧はここが源。
/// disconnect した repo の Lane は registry から消えるので、応答 = Currents 限定。
pub(crate) async fn collect_lanes(
    daemon: &RepoManagerCapability,
    query: &LanesQuery,
) -> Vec<crate::repo::lanes_state::LaneInfo> {
    let lane_registry = daemon.lane_registry_ref();
    let registry = lane_registry.read().await;

    // 全 repo の Lane を flatten + filter (repo / lane / agent)
    let mut lanes: Vec<crate::repo::lanes_state::LaneInfo> = registry
        .values()
        .flatten()
        .filter(|l| query.repo.as_deref().is_none_or(|p| l.address.repo == p))
        .filter(|l| {
            // doc 44 P2: 旧 kind 分岐（conductor は "root"、performer は name と照合）は
            // フラット化で name 一本の比較に畳まれた（開発起点の name が予約名 "root"）。
            query.lane.as_deref().is_none_or(|n| l.address.name == n)
        })
        .filter(|l| {
            // doc 11 PR-B: l.agent は String 化、 query.agent と直接比較 (wire 上は新 agent 名のみ accept)。
            query.agent.as_deref().is_none_or(|s| l.agent == s)
        })
        .cloned()
        .collect();

    lanes.sort_by(|a, b| {
        use std::cmp::Ordering;
        a.address.repo.cmp(&b.address.repo).then_with(|| {
            // doc 44 P2: 開発起点を先頭に置く表示順（旧 kind 比較の後継、`LanePool::list` と同型）
            match (a.address.is_root(), b.address.is_root()) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => a.created_at.cmp(&b.created_at),
            }
        })
    });

    lanes
}

// doc 45 段 4: 本 module の inline test（`daemon_list_repos` / `daemon_list_processes` の
// axum oneshot smoke）は route ごと撤去した。残った 3 関数の振る舞いは
// `daemon/server.rs` の daemon-control テスト群が Unison 入口から固定する。
