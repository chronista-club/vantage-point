//! `vp restart` コマンドの実行ロジック
//!
//! Process + tmux セッションをセットで再起動する。
//! tmux セッションが存在すれば kill → 再作成、なければ headless で再起動。

use anyhow::Result;

use crate::cli::stop_process;
use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};
use crate::tmux;

/// `vp restart` を実行
pub fn execute(target: Option<&str>, browser: bool, headless: bool, config: &Config) -> Result<()> {
    // ターゲット解決 — 実行中の Process を探す
    let resolved = resolve::resolve_target(target, config)?;

    let (port, project_dir, project_name) = match resolved {
        ResolvedTarget::Running {
            port,
            name,
            project_dir,
        } => {
            println!("\u{1f504} Restarting: {} (port {})", name, port);
            (port, project_dir, name.to_string())
        }
        ResolvedTarget::Configured { name, .. } => {
            println!(
                "\u{2717} '{}' is not running. Use `vp start {}` instead.",
                name, name
            );
            return Ok(());
        }
        ResolvedTarget::Cwd { .. } => {
            println!("\u{2717} No running Process found for current directory.");
            println!("  Use `vp start` to start a new Process.");
            return Ok(());
        }
    };

    // 1. Process を API 経由で停止
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        stop_process(port).await?;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        Ok::<(), anyhow::Error>(())
    })?;

    // 2. tmux セッションを kill（存在する場合）
    let session = tmux::session_name(&project_name);
    let had_tmux = if tmux::is_tmux_available() && tmux::session_exists(&session) {
        print!("  ⏹ tmux:{}... ", session);
        if tmux::kill_session(&session) {
            println!("ok");
        } else {
            println!("skip");
        }
        true
    } else {
        false
    };

    // 3. 再起動
    println!("\u{1f680} Starting Process...");
    let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());

    if headless || browser {
        // headless / browser: 従来通り
        let args = vec!["start", "--project-dir"];
        let pd = project_dir.clone();
        if headless {
            spawn_headless(&vp_bin, &pd);
        } else {
            // browser モード
            let _ = std::process::Command::new(&vp_bin)
                .args(["start", "--browser", "--project-dir", &pd])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        let _ = args; // suppress warning

        // Process ready 待ち
        std::thread::sleep(std::time::Duration::from_secs(2));
        println!("  ✓ {} (port {}, headless)", project_name, port);
        Ok(())
    } else if had_tmux && tmux::is_tmux_available() {
        // tmux セッションを再作成（detached）
        match tmux::create_detached(&session, &vp_bin, &["start", "--project-dir", &project_dir]) {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_secs(2));
                println!("  ✓ {} (port {}, tmux:{})", project_name, port, session);
            }
            Err(e) => {
                println!("  ⚠ tmux 起動失敗: {}, headless にフォールバック", e);
                spawn_headless(&vp_bin, &project_dir);
                std::thread::sleep(std::time::Duration::from_secs(2));
                println!("  ✓ {} (port {}, headless)", project_name, port);
            }
        }
        Ok(())
    } else if tmux::is_tmux_available() {
        // tmux はあるけどセッションがなかった → 新規作成
        match tmux::create_detached(&session, &vp_bin, &["start", "--project-dir", &project_dir]) {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_secs(2));
                println!("  ✓ {} (port {}, tmux:{})", project_name, port, session);
            }
            Err(e) => {
                println!("  ⚠ tmux 起動失敗: {}, headless にフォールバック", e);
                spawn_headless(&vp_bin, &project_dir);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        Ok(())
    } else {
        // tmux なし → headless
        spawn_headless(&vp_bin, &project_dir);
        std::thread::sleep(std::time::Duration::from_secs(2));
        println!("  ✓ {} (port {}, headless)", project_name, port);
        Ok(())
    }
}

/// headless で Process を起動
fn spawn_headless(vp_bin: &std::path::Path, project_dir: &str) {
    let _ = std::process::Command::new(vp_bin)
        .args(["start", "--headless", "--project-dir", project_dir])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
