//! vp hd — Heaven's Door インスタンス管理
//!
//! Claude CLI の tmux セッション + ccwire 登録を管理する。
//! SP サーバーの管理は sp_cmd.rs に分離。

use anyhow::Result;
use clap::Subcommand;

use crate::config::Config;
use crate::tmux;

#[derive(Subcommand)]
pub enum HdCommands {
    /// HD インスタンスを起動（tmux + Claude CLI + ccwire 登録）
    Start {
        /// インスタンス名（例: kaizen, scroll-bug）。セッション名 = {project}-{id}-vp
        #[arg(long)]
        id: Option<String>,
        /// 作業ディレクトリ（省略時は cwd）
        #[arg(long)]
        cwd: Option<String>,
    },
    /// HD インスタンスを停止（ccwire 解除 + tmux kill）
    Stop {
        /// 停止するインスタンス名
        #[arg(long)]
        id: Option<String>,
    },
    /// HD インスタンスを再起動（tmux kill → 再作成）
    Restart {
        /// 再起動するインスタンス名
        #[arg(long)]
        id: Option<String>,
    },
    /// HD インスタンス一覧
    List,
    /// HD インスタンスに TUI 接続（旧 vp tui）
    Attach {
        /// 接続するインスタンス名（省略時はデフォルトセッション）
        #[arg(long)]
        id: Option<String>,
        /// tmux セッション名を直接指定（--id より優先）
        #[arg(long, short = 's')]
        session: Option<String>,
    },
}

/// vp hd コマンドを実行
pub fn execute(cmd: HdCommands, config: &Config) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let project_name = crate::resolve::project_name_from_path(&cwd.to_string_lossy(), config);

    // Config から他プロジェクト名一覧を取得（prefix フィルタ用）
    let my_prefix = project_name.replace('.', "-");
    let other_prefixes: Vec<String> = config
        .projects
        .iter()
        .map(|p| {
            let name = crate::resolve::project_name_from_path(&p.path, config);
            name.replace('.', "-")
        })
        .filter(|name| *name != my_prefix)
        .collect();

    match cmd {
        HdCommands::Start { id, cwd: work_dir } => {
            let project_dir = work_dir.unwrap_or_else(|| cwd.to_string_lossy().to_string());
            hd_start(&project_name, id.as_deref(), &project_dir, config)
        }
        HdCommands::Stop { id } => hd_stop(&project_name, id.as_deref()),
        HdCommands::Restart { id } => {
            hd_restart(&project_name, id.as_deref(), &cwd.to_string_lossy(), config)
        }
        HdCommands::List => hd_list(&project_name, &other_prefixes),
        HdCommands::Attach { id, session } => {
            // --session 直接指定 > --id ベース解決 > cwd 自動検出
            let session_name = if let Some(s) = session {
                s
            } else {
                tmux::session_name_with_id(&project_name, id.as_deref())
            };
            hd_attach(&session_name, config)
        }
    }
}

/// HD インスタンスを起動
fn hd_start(
    project_name: &str,
    id: Option<&str>,
    project_dir: &str,
    _config: &Config,
) -> Result<()> {
    if !tmux::is_tmux_available() {
        anyhow::bail!("tmux が見つかりません。インストールしてください。");
    }

    let session_name = tmux::session_name_with_id(project_name, id);

    // SP サーバー稼働チェック（Warning のみ、エラーにはしない）
    let normalized = Config::normalize_path(std::path::Path::new(project_dir));
    let sp_port = crate::discovery::find_by_project_blocking(&normalized).map(|p| p.port);
    if sp_port.is_none() {
        eprintln!("⚠️  SP サーバーが未起動です。`vp sp start` で起動を推奨します。");
    }

    // Phase L7a: ccwire::cleanup_stale 呼出停止 (Mailbox 移行、stale は tmux session
    // 存在判定で derive する方針。ghost session は実害なく残る)
    // if let Err(e) = crate::ccwire::cleanup_stale() { ... }

    // tmux セッション作成（既にあれば再利用）
    if tmux::session_exists(&session_name) {
        println!("✅ tmux セッション '{}' は既に存在します", session_name);
    } else {
        // ターミナルサイズ取得（デフォルトフォールバック付き）
        let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
        crate::commands::start::create_tmux_session(
            &session_name,
            project_dir,
            cols,
            rows,
            sp_port.unwrap_or(33000),
        )?;
        println!("✅ tmux セッション '{}' を作成しました", session_name);
    }

    // Phase L7b: ccwire register 呼出停止 (Mailbox Router が daemon 側で
    // 保持、agent 起動時に自動 register する設計。soft degradation)
    // let tmux_target = format!("{}:0.0", session_name);
    // match crate::ccwire::register(&session_name, &tmux_target) { ... }

    println!();
    if let Some(id) = id {
        println!("📖 HD インスタンス '{}' が準備できました", id);
        println!("   vp hd attach --id {}  — TUI 接続", id);
        println!("   vp hd stop --id {}    — 停止", id);
    } else {
        println!("📖 HD インスタンスが準備できました");
        println!("   vp hd attach  — TUI 接続");
        println!("   vp hd stop    — 停止");
    }

    Ok(())
}

/// HD インスタンスを停止
fn hd_stop(project_name: &str, id: Option<&str>) -> Result<()> {
    let session_name = tmux::session_name_with_id(project_name, id);

    // Phase L7b: ccwire unregister 呼出停止
    // match crate::ccwire::unregister(&session_name) { ... }

    // tmux kill
    if tmux::session_exists(&session_name) {
        if tmux::kill_session(&session_name) {
            println!("✅ tmux セッション '{}' を削除しました", session_name);
        } else {
            eprintln!("⚠️  tmux セッション削除失敗");
        }
    } else {
        println!("ℹ️  tmux セッション '{}' は存在しません", session_name);
    }

    Ok(())
}

/// HD インスタンス一覧を表示
fn hd_list(project_name: &str, other_prefixes: &[String]) -> Result<()> {
    let sessions = tmux::list_vp_sessions();
    let prefix = project_name.replace('.', "-");

    println!("📖 HD インスタンス一覧:");
    println!();

    let mut found = false;
    for session in &sessions {
        // このプロジェクトに属するセッションのみ表示
        if !is_own_session(session, &prefix, other_prefixes) {
            continue;
        }
        found = true;

        // セッション名から ID を抽出
        let id = extract_id_from_session(session, &prefix);
        let registered = crate::ccwire::is_registered(session);

        let id_display = id.unwrap_or("(default)");
        let ccwire_icon = if registered { "✅" } else { "❌" };

        println!(
            "  {} {} (tmux: ✅, ccwire: {})",
            id_display, session, ccwire_icon
        );
    }

    if !found {
        println!("  (なし)");
    }

    // 他プロジェクトのセッションも表示
    let other_sessions: Vec<_> = sessions
        .iter()
        .filter(|s| !is_own_session(s, &prefix, other_prefixes))
        .collect();
    if !other_sessions.is_empty() {
        println!();
        println!("  他プロジェクト:");
        for session in other_sessions {
            println!("    {}", session);
        }
    }

    Ok(())
}

/// HD インスタンスを再起動（tmux kill → 再作成）
fn hd_restart(
    project_name: &str,
    id: Option<&str>,
    project_dir: &str,
    config: &Config,
) -> Result<()> {
    let session_name = tmux::session_name_with_id(project_name, id);
    println!("🔄 HD インスタンスを再起動します: {}", session_name);

    // 停止
    hd_stop(project_name, id)?;

    // tmux セッション完全停止を待つ（最大2秒）
    for _ in 0..20 {
        if !tmux::session_exists(&session_name) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // 再作成
    hd_start(project_name, id, project_dir, config)
}

/// HD インスタンスに TUI 接続
fn hd_attach(session_name: &str, _config: &Config) -> Result<()> {
    if !tmux::session_exists(session_name) {
        anyhow::bail!(
            "HD インスタンス (セッション: {}) が見つかりません。先に `vp hd start` してください。",
            session_name
        );
    }

    // TUI コンソールを起動
    crate::commands::tui_cmd::run_tui_console_blocking(session_name)
}

/// セッションがこのプロジェクトに属するか判定
///
/// `{prefix}-vp` または `{prefix}-{id}-vp` 形式にマッチ。
/// プロジェクト名にハイフンを含む場合（`creo-memories`）と
/// ID にハイフンを含む場合（`scroll-bug`）が構文的に区別できないため、
/// Config の全プロジェクト名から「より長いプロジェクト名」を除外リストとして使用する。
///
/// `other_prefixes` には自分以外のプロジェクト名（サニタイズ済み）を渡す。
pub fn is_own_session(session: &str, project_prefix: &str, other_prefixes: &[String]) -> bool {
    let default_name = format!("{}-vp", project_prefix);

    // デフォルトセッション: {prefix}-vp
    if session == default_name {
        return true;
    }

    // ID 付きセッション: {prefix}-{id}-vp
    if let Some(rest) = session.strip_prefix(&format!("{}-", project_prefix))
        && rest.ends_with("-vp")
        && rest.len() > 3
    {
        // 他プロジェクトのセッション名でないことを確認
        // 例: session="creo-memories-vp", prefix="creo" → rest="memories-vp"
        //     other_prefixes に "creo-memories" があれば除外
        for other in other_prefixes {
            if let Some(other_suffix) = other.strip_prefix(&format!("{}-", project_prefix)) {
                // rest が "{other_suffix}-vp" または "{other_suffix}-{id}-vp" で始まるなら除外
                let other_default = format!("{}-vp", other_suffix);
                let other_id_prefix = format!("{}-", other_suffix);
                if rest == other_default || rest.starts_with(&other_id_prefix) {
                    return false;
                }
            }
        }
        return true;
    }

    false
}

/// セッション名から ID 部分を抽出
///
/// `vantage-point-kaizen-vp` → Some("kaizen")
/// `vantage-point-vp` → None
fn extract_id_from_session<'a>(session: &'a str, project_prefix: &str) -> Option<&'a str> {
    let suffix = session.strip_prefix(project_prefix)?;
    // "-vp" を除去
    let suffix = suffix.strip_suffix("-vp")?;
    // 先頭の "-" を除去
    let id = suffix.strip_prefix('-')?;
    if id.is_empty() { None } else { Some(id) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_own_session() {
        let others = vec!["creo-memories".to_string()];

        // 自プロジェクトのデフォルトセッション
        assert!(is_own_session("creo-vp", "creo", &others));
        assert!(is_own_session("vantage-point-vp", "vantage-point", &[]));

        // 自プロジェクトの ID 付きセッション
        assert!(is_own_session("creo-kaizen-vp", "creo", &others));
        assert!(is_own_session(
            "vantage-point-kaizen-vp",
            "vantage-point",
            &[]
        ));

        // 別プロジェクト（prefix が部分一致するが別物）
        assert!(!is_own_session("creo-memories-vp", "creo", &others));
        assert!(!is_own_session("creo-memories-worker-vp", "creo", &others));

        // 無関係なセッション
        assert!(!is_own_session("fleetflow-vp", "creo", &others));
    }

    #[test]
    fn test_extract_id_from_session() {
        assert_eq!(
            extract_id_from_session("vantage-point-kaizen-vp", "vantage-point"),
            Some("kaizen")
        );
        assert_eq!(
            extract_id_from_session("vantage-point-vp", "vantage-point"),
            None
        );
        assert_eq!(
            extract_id_from_session("creo-scroll-bug-vp", "creo"),
            Some("scroll-bug")
        );
    }
}
