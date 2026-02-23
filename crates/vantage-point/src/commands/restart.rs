//! `vp restart` コマンドの実行ロジック

use anyhow::Result;

use crate::cli::{parse_debug_env, stop_stand};
use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};

/// `vp restart` を実行
pub fn execute(target: Option<&str>, browser: bool, headless: bool, config: &Config) -> Result<()> {
    // ターゲット解決 — 実行中の Stand を探す
    let resolved = resolve::resolve_target(target, config)?;

    let (port, project_dir) = match resolved {
        ResolvedTarget::Running {
            port,
            name,
            project_dir,
        } => {
            println!("\u{1f504} Restarting: {} (port {})", name, port);
            (port, project_dir)
        }
        ResolvedTarget::Configured { name, .. } => {
            println!(
                "\u{2717} '{}' is not running. Use `vp start {}` instead.",
                name, name
            );
            return Ok(());
        }
        ResolvedTarget::Cwd { .. } => {
            println!("\u{2717} No running Stand found for current directory.");
            println!("  Use `vp start` to start a new Stand.");
            return Ok(());
        }
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        stop_stand(port).await?;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        println!("\u{1f680} Starting Stand...");
        Ok::<(), anyhow::Error>(())
    })?;

    // デバッグモード
    let debug_mode = parse_debug_env().unwrap_or_default();

    // CapabilityConfig（再起動時は MIDI なし）
    let project_dir_for_name = project_dir.clone();
    let cap_config = crate::stand::CapabilityConfig {
        project_dir,
        midi_config: None,
        bonjour_port: Some(port),
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
        // Daemon + ネイティブターミナルモード
        let daemon_port = crate::daemon::client::DAEMON_QUIC_PORT;
        match crate::daemon::process::ensure_daemon_running(daemon_port) {
            Ok(pid) => tracing::info!("Daemon ready (PID: {})", pid),
            Err(e) => tracing::warn!("Daemon 自動起動失敗: {}", e),
        }

        let server_thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create runtime");
            rt.block_on(async { crate::stand::run(port, false, debug_mode, cap_config).await })
        });

        std::thread::sleep(std::time::Duration::from_millis(500));

        let project_name = std::path::Path::new(&project_dir_for_name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("default")
            .to_string();

        let webview_result =
            crate::terminal_window::run_terminal_with_daemon(daemon_port, &project_name);

        match webview_result {
            Ok(()) => tracing::info!("Terminal window closed"),
            Err(e) => tracing::error!("Terminal window error: {}", e),
        }

        drop(server_thread);
        Ok(())
    }
}
