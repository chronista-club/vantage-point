//! `vp app` コマンドの実行ロジック

use anyhow::Result;

use crate::cli::{find_vantage_point_app, which_vp};

/// `vp app` を実行
pub fn execute(port: u16, no_conductor: bool) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Conductor Standが稼働しているか確認
        if !no_conductor {
            let conductor_url = format!("http://localhost:{}/api/health", port);
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()?;

            let conductor_running = client
                .get(&conductor_url)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);

            if !conductor_running {
                println!("🎭 Starting Conductor Stand on port {}...", port);
                // バックグラウンドでConductorを起動
                let vp_path = which_vp().ok_or_else(|| anyhow::anyhow!("vp binary not found"))?;

                std::process::Command::new(&vp_path)
                    .args(["conductor", "-p", &port.to_string()])
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("Failed to start conductor: {}", e))?;

                // 起動を待つ
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
        }

        // VantagePoint.app を起動
        let app_path = find_vantage_point_app();
        match app_path {
            Some(path) => {
                println!("🚀 Opening VantagePoint.app...");
                std::process::Command::new("open")
                    .arg(&path)
                    .spawn()
                    .map_err(|e| anyhow::anyhow!("Failed to open app: {}", e))?;
                println!("✓ VantagePoint.app started");
                Ok(())
            }
            None => {
                eprintln!("✗ VantagePoint.app not found");
                eprintln!("  Expected locations:");
                eprintln!("    - /Applications/VantagePoint.app");
                eprintln!("    - ~/Applications/VantagePoint.app");
                eprintln!("    - ~/repos/vantage-point-mac/VantagePoint/VantagePoint.app (dev)");
                std::process::exit(1);
            }
        }
    })
}
