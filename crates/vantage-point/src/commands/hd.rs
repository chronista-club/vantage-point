//! vp hd — Heaven's Door インスタンス管理
//!
//! Claude CLI の tmux セッションを管理する。
//! SP サーバーの管理は sp_cmd.rs に分離。

use anyhow::Result;
use clap::Subcommand;

use crate::config::Config;
use crate::tmux;

#[derive(Subcommand)]
pub enum HdCommands {
    /// HD インスタンスを起動（tmux + Claude CLI）
    Start {
        /// インスタンス名（例: kaizen, scroll-bug）。セッション名 = {project}-{id}-vp
        #[arg(long)]
        id: Option<String>,
        /// 作業ディレクトリ（省略時は cwd）
        #[arg(long)]
        cwd: Option<String>,
    },
    /// HD インスタンスを停止（tmux kill）
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

    // tmux セッション作成（既にあれば再利用）
    if tmux::session_exists(&session_name) {
        println!("✅ tmux セッション '{}' は既に存在します", session_name);
    } else {
        // ターミナルサイズ取得（デフォルトフォールバック付き）
        let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 40));
        create_tmux_session(
            &session_name,
            project_dir,
            cols,
            rows,
            sp_port.unwrap_or(33000),
        )?;
        println!("✅ tmux セッション '{}' を作成しました", session_name);
    }

    println!();
    if let Some(id) = id {
        println!("📖 HD インスタンス '{}' が準備できました", id);
        println!("   vp hd stop --id {}    — 停止", id);
    } else {
        println!("📖 HD インスタンスが準備できました");
        println!("   vp hd stop    — 停止");
    }

    Ok(())
}

/// HD インスタンスを停止
fn hd_stop(project_name: &str, id: Option<&str>) -> Result<()> {
    let session_name = tmux::session_name_with_id(project_name, id);

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
        let id_display = id.unwrap_or("(default)");
        println!("  {} {} (tmux: ✅)", id_display, session);
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

// =============================================================================
// tmux + Claude セッション作成 helper (refactor R1-2 で旧 commands/start.rs から移設)
// =============================================================================

fn tmux_session_exists(name: &str) -> bool {
    crate::tmux::tmux_command()
        .args(["has-session", "-t", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// tmux セッション作成（Claude CLI を中で起動、ステータスバー非表示）
///
/// `--continue` 付きで起動し、即死した場合は `--continue` なしでフォールバック。
fn create_tmux_session(
    name: &str,
    project_dir: &str,
    cols: u16,
    rows: u16,
    process_port: u16,
) -> Result<()> {
    let mut mise_envs = collect_mise_env(project_dir);
    // VP_PROCESS_PORT を注入 → CC が起動する MCP プロセスに自動伝播
    mise_envs.push(("VP_PROCESS_PORT".to_string(), process_port.to_string()));

    // まず --continue 付きで試行
    let created = try_create_tmux_claude(name, project_dir, cols, rows, &mise_envs, true)?;
    if !created {
        anyhow::bail!("tmux セッション作成に失敗: {}", name);
    }

    // セッションが即死していないか確認（claude --continue が壊れたセッションで落ちるケース）
    // lane パフォーマー環境ではセッション履歴がなく --continue が即死するため、十分に待つ
    std::thread::sleep(std::time::Duration::from_millis(1500));
    if !tmux_session_exists(name) {
        tracing::warn!("claude --continue が即死。--continue なしでフォールバック");
        let created = try_create_tmux_claude(name, project_dir, cols, rows, &mise_envs, false)?;
        if !created {
            anyhow::bail!("tmux セッション作成に失敗（フォールバック）: {}", name);
        }
    }

    if !mise_envs.is_empty() {
        tracing::info!("mise env: {} 変数を tmux セッションに注入", mise_envs.len());
    }

    // TUI が自前のヘッダー/フッターを持つため tmux ステータスバーを非表示
    let _ = crate::tmux::tmux_command()
        .args(["set-option", "-t", name, "status", "off"])
        .status();

    // VP-83 refinement 53 / Tier 1 Chain tuning:
    // ─ escape-time 0: Esc 応答即時化 (vi-like TUI 体験改善)
    // ─ focus-events on: terminal focus 変化を Claude CLI 等に forward
    //   → NSWindow become-key で TUI redraw → HD 入力 area の 2 行問題緩和の期待
    // ─ terminal-overrides *:Tc: 24-bit truecolor を 256 色にダウングレードさせない
    let _ = crate::tmux::tmux_command()
        .args(["set-option", "-t", name, "escape-time", "0"])
        .status();
    let _ = crate::tmux::tmux_command()
        .args(["set-option", "-t", name, "focus-events", "on"])
        .status();
    let _ = crate::tmux::tmux_command()
        .args(["set-option", "-ga", "terminal-overrides", ",*:Tc"])
        .status();

    // mise 環境変数を tmux セッションにも set-environment（後続ペイン用）
    for (key, value) in &mise_envs {
        let _ = crate::tmux::tmux_command()
            .args(["set-environment", "-t", name, key, value])
            .status();
    }

    Ok(())
}

/// tmux new-session で Claude CLI を起動（成功なら true）
fn try_create_tmux_claude(
    name: &str,
    project_dir: &str,
    cols: u16,
    rows: u16,
    mise_envs: &[(String, String)],
    with_continue: bool,
) -> Result<bool> {
    let mut args = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        name.to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
        "-c".to_string(),
        project_dir.to_string(),
    ];
    for (key, value) in mise_envs {
        args.push("-e".to_string());
        args.push(format!("{}={}", key, value));
    }
    // zsh -lc でラップ: tmux の直接 exec ではシェル初期化が走らず
    // claude が依存する PATH/環境変数が不足して即死するケースを回避
    let mut claude_cmd = "claude --dangerously-skip-permissions".to_string();
    if with_continue {
        claude_cmd.push_str(" --continue");
    }
    args.push("zsh".to_string());
    args.push("-lc".to_string());
    args.push(claude_cmd);

    let status = crate::tmux::tmux_command().args(&args).status()?;

    Ok(status.success())
}

/// mise env を project_dir で評価し、環境変数の (key, value) ペアを返す
///
/// mise が未インストール or .mise.toml がなければ空 Vec を返す（ベストエフォート）。
fn collect_mise_env(project_dir: &str) -> Vec<(String, String)> {
    let mise_bin = dirs::home_dir()
        .map(|h| h.join(".local/bin/mise"))
        .unwrap_or_else(|| "mise".into());

    let output = std::process::Command::new(&mise_bin)
        .args(["env", "--shell", "bash"])
        .current_dir(project_dir)
        .output();

    let Ok(output) = output else {
        return vec![];
    };
    if !output.status.success() {
        return vec![];
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // "export KEY=VALUE" → (KEY, VALUE)
            let line = line.strip_prefix("export ")?;
            let (key, value) = line.split_once('=')?;
            // クォート除去
            let value = value.trim_matches('\'').trim_matches('"');
            Some((key.to_string(), value.to_string()))
        })
        .collect()
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
        assert!(!is_own_session(
            "creo-memories-performer-vp",
            "creo",
            &others
        ));

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
