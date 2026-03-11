//! `vp restart-all` コマンドの実行ロジック
//!
//! TheWorld + 全 Process + Canvas + tmux セッションを一括再起動する。
//! tmux セッションと Process はライフサイクルを共有し、セットで再起動される。
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

    // 2. 全 Process を API 経由で停止
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

    // 4. tmux セッションを kill（Process とライフサイクル共有）
    if tmux::is_tmux_available() {
        let sessions = tmux::list_vp_sessions();
        for session in &sessions {
            // 自分自身のセッションは最後に kill（restart-all 実行中のセッション）
            if tmux::is_inside_tmux() {
                if let Ok(current) = std::env::var("TMUX") {
                    // TMUX 環境変数から現在のセッション名を取れないが、
                    // restart-all は通常 tmux 外 or 別セッションから呼ぶ想定
                    let _ = current;
                }
            }
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

    // 7. 全 Process を tmux セッション付きで再起動
    if !processes.is_empty() {
        println!("🚀 {} 個の Process を再起動中...", processes.len());
    }

    let vp_bin = std::env::current_exe().unwrap_or_else(|_| "vp".into());

    for proc in &processes {
        let name = project_name(&proc.project_dir);
        let session = tmux::session_name(&name);
        print!("  ▶ {} (tmux:{})... ", name, session);

        if tmux::is_tmux_available() {
            // tmux セッション + TUI + Process をセットで起動
            match tmux::create_detached(
                &session,
                &vp_bin,
                &["start", "--project-dir", &proc.project_dir],
            ) {
                Ok(()) => println!("ok"),
                Err(e) => {
                    // tmux 起動失敗 → headless フォールバック
                    println!("tmux error: {}, falling back to headless", e);
                    spawn_headless(&vp_bin, &proc.project_dir);
                }
            }
        } else {
            // tmux なし → headless で起動
            spawn_headless(&vp_bin, &proc.project_dir);
            println!("ok (headless)");
        }
    }

    if !processes.is_empty() {
        // Process + TUI が起動するのを待つ
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    println!();
    println!("✅ 全体再起動完了");

    // 最終状態を表示
    let final_processes = discovery::list_blocking();
    if !final_processes.is_empty() {
        println!();
        for proc in &final_processes {
            let name = project_name(&proc.project_dir);
            let session = tmux::session_name(&name);
            let tmux_status = if tmux::session_exists(&session) {
                "tmux"
            } else {
                "headless"
            };
            println!("  ✓ {} (port {}, {})", name, proc.port, tmux_status);
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

/// headless で Process を起動（tmux フォールバック用）
fn spawn_headless(vp_bin: &std::path::Path, project_dir: &str) {
    let _ = std::process::Command::new(vp_bin)
        .args(["start", "--headless", "--project-dir", project_dir])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
