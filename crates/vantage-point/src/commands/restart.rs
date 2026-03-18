//! `vp restart` コマンドの実行ロジック（restart-all から内部利用）
//!
//! SP サーバーを再起動する。tmux セッションの再起動は hd_cmd.rs が担当。

use anyhow::Result;

use crate::cli::stop_process;
use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};
use crate::tmux;

/// SP + tmux をセットで再起動（restart-all から呼ばれる）
pub fn execute(
    target: Option<&str>,
    _browser: bool,
    _headless: bool,
    config: &Config,
) -> Result<()> {
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
                "\u{2717} '{}' is not running. Use `vp sp start` instead.",
                name
            );
            return Ok(());
        }
        ResolvedTarget::Cwd { .. } => {
            println!("\u{2717} No running Process found for current directory.");
            println!("  Use `vp sp start` to start a new SP server.");
            return Ok(());
        }
    };

    // 1. SP を API 経由で停止
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        stop_process(port).await?;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        Ok::<(), anyhow::Error>(())
    })?;

    // 2. tmux セッションを kill（存在する場合）
    let session = tmux::session_name(&project_name);
    if tmux::is_tmux_available() && tmux::session_exists(&session) {
        print!("  ⏹ tmux:{}... ", session);
        if tmux::kill_session(&session) {
            println!("ok");
        } else {
            println!("skip");
        }
    }

    // 3. SP を再起動（detached subprocess として spawn）
    println!("\u{1f680} Starting SP...");
    if let Err(e) = crate::commands::start::spawn_sp_detached(&project_dir, None) {
        eprintln!("⚠️  SP 起動失敗: {}", e);
    }

    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("  ✓ {} (port {})", project_name, port);
    Ok(())
}
