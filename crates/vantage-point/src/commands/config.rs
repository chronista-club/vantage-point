//! `vp config` コマンドの実行ロジック

use anyhow::Result;

use crate::config::Config;

/// `vp config` を実行
///
/// daemon 接続時は API からプロジェクト一覧を取得。
/// 未接続時は config / projects.kdl にフォールバック。
pub fn execute(config: &Config) -> Result<()> {
    println!("Config file: {}", Config::config_path().display());
    println!();

    // daemon API からプロジェクト一覧を取得（フォールバック: projects.kdl）
    let (projects, source) = match fetch_projects_from_thedaemon() {
        Some(projects) => (projects, "daemon API"),
        None => {
            let projects: Vec<(String, String)> = config
                .projects
                .iter()
                .map(|p| (p.name.clone(), p.path.clone()))
                .collect();
            (projects, "projects.kdl (daemon offline)")
        }
    };

    println!("Source: {}", source);
    println!();

    if projects.is_empty() {
        println!("No projects registered.");
    } else {
        // 稼働中プロセスを取得
        let running = fetch_running_processes();

        println!("Registered projects:");
        println!("  #  NAME                STATUS    PATH");
        println!("  ─  ────                ──────    ────");
        for (i, (name, path)) in projects.iter().enumerate() {
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
        println!("● = SP running, ○ = stopped");
    }

    Ok(())
}

/// daemon からプロジェクト一覧を取得（Unison `daemon-control.projects/list`）
///
/// doc 45 段 2: 旧 `GET /api/daemon/projects` から差し替え。daemon 不在は None で、
/// caller が projects.kdl フォールバックに落とす（従来どおり）。
fn fetch_projects_from_thedaemon() -> Option<Vec<(String, String)>> {
    crate::daemon_client::list_projects_blocking()
}

/// daemon から稼働中プロセスのパス一覧を取得（Unison `registry.list`）
///
/// doc 45 段 2: 旧 `GET /api/daemon/processes` から差し替え。daemon 不在は空 Vec
/// （= 全 project が「停止」表示。表示系なので落とさない）。
fn fetch_running_processes() -> Vec<String> {
    crate::daemon_client::list_processes_blocking()
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p.get("project_path")?.as_str().map(String::from))
        .collect()
}
