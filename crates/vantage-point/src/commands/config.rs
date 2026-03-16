//! `vp config` コマンドの実行ロジック

use anyhow::Result;

use crate::config::Config;

/// `vp config` を実行
///
/// TheWorld 接続時は API からプロジェクト一覧を取得。
/// 未接続時は config.toml にフォールバック。
pub fn execute(config: &Config) -> Result<()> {
    println!("Config file: {}", Config::config_path().display());
    println!();

    // TheWorld API からプロジェクト一覧を取得（フォールバック: config.toml）
    let (projects, source) = match fetch_projects_from_theworld() {
        Some(projects) => (projects, "TheWorld API"),
        None => {
            let projects: Vec<(String, String)> = config
                .projects
                .iter()
                .map(|p| (p.name.clone(), p.path.clone()))
                .collect();
            (projects, "config.toml (TheWorld offline)")
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

/// TheWorld API からプロジェクト一覧を取得
fn fetch_projects_from_theworld() -> Option<Vec<(String, String)>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;

    let url = format!("http://[::1]:{}/api/world/projects", crate::cli::WORLD_PORT);
    let resp = client.get(&url).send().ok()?;
    let json: serde_json::Value = resp.json().ok()?;

    let projects = json.get("projects")?.as_array()?;
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

/// TheWorld API から稼働中プロセスのパス一覧を取得
fn fetch_running_processes() -> Vec<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();

    let url = format!(
        "http://[::1]:{}/api/world/processes",
        crate::cli::WORLD_PORT
    );
    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let json: serde_json::Value = match resp.json() {
        Ok(j) => j,
        Err(_) => return vec![],
    };

    json.get("processes")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.get("project_path")?.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
