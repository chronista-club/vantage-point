//! `vp restart-all` コマンドの実行ロジック
//!
//! TheWorld + 全 SP + Canvas + tmux セッションを一括再起動する。
//! 主に新バイナリへの切り替え時に使用。

use anyhow::Result;

use crate::cli::stop_process;
use crate::daemon::process as daemon;
use crate::discovery;
use crate::tmux;

/// `vp restart-all` を実行
pub fn execute() -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    // 1. 稼働中の全 Process を取得（再起動対象を記憶）
    let processes = discovery::list_blocking();
    if processes.is_empty() {
        println!("稼働中の Process はありません");
    } else {
        println!("🔄 {} 個の Process を停止中...", processes.len());
    }

    // 2. 全 SP を API 経由で停止
    rt.block_on(async {
        for proc in &processes {
            let name = project_name(&proc.project_dir);
            print!("  ⏹ {} (port {})... ", name, proc.port);
            match stop_process(proc.port).await {
                Ok(()) => println!("ok"),
                Err(e) => println!("error: {}", e),
            }
        }
    });

    // 3. Canvas を停止
    if let Some(pid) = crate::canvas::stop_canvas() {
        println!("  ⏹ Canvas (pid {})... ok", pid);
    }

    // 4. tmux セッションを kill
    if tmux::is_tmux_available() {
        let sessions = tmux::list_vp_sessions();
        for session in &sessions {
            print!("  ⏹ tmux:{} ... ", session);
            if tmux::kill_session(session) {
                println!("ok");
            } else {
                println!("skip");
            }
        }
    }

    // 5. TheWorld を停止
    if let Some(pid) = daemon::is_daemon_running() {
        print!("  ⏹ TheWorld (pid {})... ", pid);
        match daemon::stop_daemon(pid) {
            Ok(()) => println!("ok"),
            Err(e) => println!("error: {}", e),
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    println!();

    // 6. TheWorld を再起動
    println!("🚀 TheWorld を起動中...");
    if let Err(e) = daemon::ensure_daemon_running(crate::cli::WORLD_PORT) {
        eprintln!("✗ TheWorld 起動失敗: {}", e);
        return Err(e);
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    println!("  ✓ TheWorld ready (port {})", crate::cli::WORLD_PORT);

    // 7. 全 SP を再起動
    if !processes.is_empty() {
        println!("🚀 {} 個の SP を再起動中...", processes.len());
    }

    for proc in &processes {
        let name = project_name(&proc.project_dir);
        print!("  ▶ {}... ", name);
        if let Err(e) = crate::commands::start::spawn_sp_detached(&proc.project_dir, None) {
            eprintln!("⚠️  SP 起動失敗 ({}): {}", name, e);
        } else {
            println!("ok");
        }
    }

    if !processes.is_empty() {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    println!();
    println!("✅ 全体再起動完了");
    println!();
    println!("HD セッションは `vp hd start` で個別に再作成してください。");

    // 最終状態を表示
    let final_processes = discovery::list_blocking();
    if !final_processes.is_empty() {
        println!();
        for proc in &final_processes {
            let name = project_name(&proc.project_dir);
            println!("  ✓ {} (port {})", name, proc.port);
        }
    }

    Ok(())
}

/// project_dir からプロジェクト名を抽出
fn project_name(project_dir: &str) -> String {
    project_dir
        .rsplit('/')
        .next()
        .unwrap_or(project_dir)
        .to_string()
}
