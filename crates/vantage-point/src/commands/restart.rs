//! `vp restart` コマンドの実行ロジック（restart-all から内部利用）
//!
//! SP サーバーを再起動する。claude は SP の PtySlot の子なので、SP 停止 = lane 停止、
//! 再起動後は conductor spawn が `--resume` で会話を継ぐ（tmux decoupling PR2）。

use anyhow::Result;

use crate::cli::stop_process;
use crate::config::Config;
use crate::resolve::{self, ResolvedTarget};

/// SP を再起動（restart-all から呼ばれる）
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

    // 2. SP を再起動（detached subprocess として spawn）
    // tmux decoupling PR2: 旧 step 2 (tmux session kill) は退役 — claude は SP の子なので
    // step 1 の SP 停止で完全に落ち、 再起動後の conductor spawn が --resume で会話を継ぐ。
    println!("\u{1f680} Starting SP...");
    if let Err(e) = crate::commands::sp::spawn_sp_detached(&project_dir, None) {
        eprintln!("⚠️  SP 起動失敗: {}", e);
    }

    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("  ✓ {} (port {})", project_name, port);
    Ok(())
}
