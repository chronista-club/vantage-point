//! `vp restart` コマンドの実行ロジック

use anyhow::Result;

use crate::cli::{parse_debug_env, scan_instances, stop_stand};

/// `vp restart` を実行
pub fn execute(port: u16, browser: bool, headless: bool) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Get current instance info before stopping
        let instances = scan_instances().await;
        let instance = instances.iter().find(|i| i.port == port);

        let _project_dir = match instance {
            Some(inst) => inst.project_dir.clone().unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            }),
            None => {
                println!(
                    "✗ No Stand running on port {}. Use `vp start` instead.",
                    port
                );
                return Ok(());
            }
        };

        println!("🔄 Restarting vp on port {}...", port);
        println!("   Project: {}", _project_dir);

        // Stop the Stand
        stop_stand(port).await?;

        // Wait a moment for port to be released
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        println!("🚀 Starting Stand...");
        Ok::<(), anyhow::Error>(())
    })?;

    // Get project_dir for starting (need to get it again outside async block)
    let rt2 = tokio::runtime::Runtime::new()?;
    let project_dir = rt2.block_on(async {
        // Read from persisted state file
        let state_path = crate::config::config_dir()
            .join("state")
            .join(format!("{}.json", port));

        if let Ok(data) = std::fs::read_to_string(&state_path)
            && let Ok(state) = serde_json::from_str::<serde_json::Value>(&data)
            && let Some(dir) = state.get("project_dir").and_then(|v| v.as_str())
        {
            return dir.to_string();
        }

        std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });

    // Determine debug mode from env
    let debug_mode = parse_debug_env().unwrap_or_default();

    // Create CapabilityConfig (no MIDI on restart)
    let cap_config = crate::stand::CapabilityConfig {
        project_dir,
        midi_config: None,
        bonjour_port: Some(port), // Bonjour広告を有効化
    };

    if headless || browser {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let server_handle = tokio::spawn(async move {
                crate::stand::run(port, false, debug_mode, cap_config).await
            });

            if browser {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let url = format!("http://localhost:{}", port);
                tracing::info!("Opening in browser: {}", url);
                let _ = open::that(&url);
            }

            server_handle.await?
        })
    } else {
        // WebView mode
        let server_thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async { crate::stand::run(port, false, debug_mode, cap_config).await })
        });

        std::thread::sleep(std::time::Duration::from_millis(300));

        let webview_result = crate::webview::run_webview(port);

        match webview_result {
            Ok(()) => tracing::info!("WebView closed"),
            Err(e) => tracing::error!("WebView error: {}", e),
        }

        drop(server_thread);
        Ok(())
    }
}
