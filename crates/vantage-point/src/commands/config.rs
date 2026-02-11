//! `vp config` コマンドの実行ロジック

use anyhow::Result;

use crate::config::Config;

/// `vp config` を実行
pub fn execute(config: &Config) -> Result<()> {
    // Show configuration
    println!("Config file: {}", Config::config_path().display());
    println!();

    if config.projects.is_empty() {
        println!("No projects registered.");
        println!();
        println!("Add projects to your config file:");
        println!("  [[projects]]");
        println!("  name = \"my-project\"");
        println!("  path = \"/path/to/project\"");
    } else {
        println!("Registered projects:");
        println!("  #  NAME                PORT    PATH");
        println!("  ─  ────                ────    ────");
        for (i, project) in config.projects.iter().enumerate() {
            let port_str = project
                .port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string());
            // Shorten long paths
            let path_display = if project.path.len() > 40 {
                format!("...{}", &project.path[project.path.len() - 37..])
            } else {
                project.path.clone()
            };
            println!(
                "  {}  {:18}  {:>5}   {}",
                i + 1,
                project.name,
                port_str,
                path_display
            );
        }
        println!();
        println!("Usage: vp start <#> or vp start -C /path/to/project (# starts from 1)");
    }

    Ok(())
}
