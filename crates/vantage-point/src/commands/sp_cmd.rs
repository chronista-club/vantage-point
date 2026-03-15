//! vp sp — セッション環境管理（tmux + ccwire）
//!
//! SP サーバー（vp start）とは独立したセッション環境のライフサイクル管理。
//! tmux セッションの作成/削除、ccwire 登録/解除を担当。

use anyhow::Result;
use clap::Subcommand;

use crate::config::Config;
use crate::tmux;

#[derive(Subcommand)]
pub enum SpCommands {
    /// セッション環境を作成（tmux + ccwire 登録）
    Start,
    /// セッション環境を停止（ccwire 解除 + tmux kill）
    Stop,
    /// セッション状態を表示
    Status,
    /// セッションに接続（vp tui 経由）
    Attach,
}

/// vp sp コマンドを実行
pub fn execute(cmd: SpCommands, config: &Config) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_name = crate::resolve::project_name_from_path(
        &cwd.to_string_lossy(),
        config,
    );
    let session_name = tmux::session_name(&project_name);
    let project_dir = cwd.to_string_lossy().to_string();

    match cmd {
        SpCommands::Start => sp_start(&session_name, &project_dir),
        SpCommands::Stop => sp_stop(&session_name),
        SpCommands::Status => sp_status(&session_name),
        SpCommands::Attach => sp_attach(&session_name, config),
    }
}

/// セッション環境を作成
fn sp_start(session_name: &str, project_dir: &str) -> Result<()> {
    if !tmux::is_tmux_available() {
        anyhow::bail!("tmux が見つかりません。インストールしてください。");
    }

    // ゴーストセッションを掃除（tmux が消えてるのに ccwire に残ってるエントリ）
    if let Err(e) = crate::ccwire::cleanup_stale() {
        eprintln!("⚠️  ccwire ゴースト掃除失敗: {}", e);
    }

    // tmux セッション作成（既にあれば再利用）
    if tmux::session_exists(session_name) {
        println!("✅ tmux セッション '{}' は既に存在します", session_name);
    } else {
        match std::process::Command::new("tmux")
            .args(["new-session", "-d", "-s", session_name, "-c", project_dir])
            .status()
        {
            Ok(s) if s.success() => {
                println!("✅ tmux セッション '{}' を作成しました", session_name);
            }
            _ => {
                anyhow::bail!("tmux セッション作成に失敗しました");
            }
        }
    }

    // ccwire 登録
    let tmux_target = format!("{}:0.0", session_name);
    match crate::ccwire::register(session_name, &tmux_target) {
        Ok(()) => {
            println!("✅ ccwire 登録完了: {}", session_name);
        }
        Err(e) => {
            eprintln!("⚠️  ccwire 登録失敗（続行）: {}", e);
        }
    }

    println!("\n🏔️ セッション環境 '{}' が準備できました", session_name);
    println!("   vp sp attach  — 接続");
    println!("   vp tui         — TUI コンソールで接続");

    Ok(())
}

/// セッション環境を停止
fn sp_stop(session_name: &str) -> Result<()> {
    // ccwire 解除
    match crate::ccwire::unregister(session_name) {
        Ok(()) => println!("✅ ccwire 解除: {}", session_name),
        Err(e) => eprintln!("⚠️  ccwire 解除失敗: {}", e),
    }

    // tmux kill
    if tmux::session_exists(session_name) {
        if tmux::kill_session(session_name) {
            println!("✅ tmux セッション '{}' を削除しました", session_name);
        } else {
            eprintln!("⚠️  tmux セッション削除失敗");
        }
    } else {
        println!("ℹ️  tmux セッション '{}' は存在しません", session_name);
    }

    Ok(())
}

/// セッション状態を表示
fn sp_status(session_name: &str) -> Result<()> {
    println!("🏔️ セッション: {}", session_name);

    // tmux 状態
    if tmux::session_exists(session_name) {
        // ペイン情報を取得
        let output = std::process::Command::new("tmux")
            .args([
                "list-panes", "-t", session_name,
                "-F", "#{pane_id} #{pane_current_command} #{pane_width}x#{pane_height}",
            ])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let panes = String::from_utf8_lossy(&out.stdout);
                let pane_count = panes.lines().count();
                println!("   tmux:   ✅ alive ({} panes)", pane_count);
                for line in panes.lines() {
                    println!("           {}", line);
                }
            }
            _ => {
                println!("   tmux:   ✅ alive");
            }
        }
    } else {
        println!("   tmux:   ❌ not found");
    }

    // ccwire 状態（ccwire.rs の公開関数を使用）
    if crate::ccwire::is_registered(session_name) {
        println!("   ccwire: ✅ registered");
    } else {
        println!("   ccwire: ❌ not registered");
    }

    Ok(())
}

/// セッションに接続（vp tui 経由）
fn sp_attach(session_name: &str, config: &Config) -> Result<()> {
    if !tmux::session_exists(session_name) {
        anyhow::bail!("セッション '{}' が見つかりません。先に vp sp start してください。", session_name);
    }

    // vp tui に委譲
    crate::commands::tui_cmd::execute(Some(session_name.to_string()), config)
}
