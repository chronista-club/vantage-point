//! `vp config` コマンドの実行ロジック

use anyhow::Result;

use crate::config::Config;

/// `vp config` を実行
///
/// daemon 接続時は API からrepo 一覧を取得。
/// 未接続時は config / repos.kdl にフォールバック。
pub fn execute(config: &Config) -> Result<()> {
    println!("Config file: {}", Config::config_path().display());
    println!();

    // daemon API からrepo 一覧を取得（フォールバック: repos.kdl）
    let (repos, source) = match fetch_repos_from_thedaemon() {
        Some(repos) => (repos, "daemon API"),
        None => {
            let repos: Vec<(String, String)> = config
                .repos
                .iter()
                .map(|p| (p.name.clone(), p.path.clone()))
                .collect();
            (repos, "repos.kdl (daemon offline)")
        }
    };

    println!("Source: {}", source);
    println!();

    if repos.is_empty() {
        println!("No repos registered.");
    } else {
        // 稼働中プロセスを取得
        let running = fetch_running_processes();

        println!("Registered repos:");
        println!("  #  NAME                STATUS    PATH");
        println!("  ─  ────                ──────    ────");
        for (i, (name, path)) in repos.iter().enumerate() {
            let status = if running.iter().any(|r| r == path) {
                "●"
            } else {
                "○"
            };
            let path_display = if path.len() > 40 {
                format!("...{}", &path[path.len() - 37..])
            } else {
                path.clone()
            };
            println!("  {}  {:18}  {:>6}   {}", i + 1, name, status, path_display);
        }
        println!();
        println!("● = repo running, ○ = stopped");
    }

    Ok(())
}

/// daemon からrepo 一覧を取得（Unison `daemon-control.repos/list`）
///
/// doc 45 段 2: 旧 `GET /api/daemon/repos` から差し替え。daemon 不在は None で、
/// caller が repos.kdl フォールバックに落とす（従来どおり）。
fn fetch_repos_from_thedaemon() -> Option<Vec<(String, String)>> {
    crate::daemon_client::list_repos_blocking()
}

/// daemon から稼働中プロセスのパス一覧を取得（Unison `registry.list`）
///
/// doc 45 段 2: 旧 `GET /api/daemon/processes` から差し替え。daemon 不在は空 Vec
/// （= 全 repo が「停止」表示。表示系なので落とさない）。
fn fetch_running_processes() -> Vec<String> {
    crate::daemon_client::list_processes_blocking()
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("repo_path")?.as_str().map(String::from))
        .collect()
}
