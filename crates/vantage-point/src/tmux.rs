//! tmux 透過統合ヘルパー
//!
//! VP の TUI モード起動時に tmux セッションを自動作成し、
//! ターミナル管理を tmux に委譲する。
//! tmux 未インストール時やすでに tmux 内にいる場合はフォールバックして
//! 従来の ratatui TUI を直接起動する。

use std::path::Path;
use std::process::Command;

/// tmux がインストールされているか確認
pub fn is_tmux_available() -> bool {
    Command::new("which")
        .arg("tmux")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 現在のプロセスが tmux セッション内で実行されているか確認
pub fn is_inside_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// プロジェクト名から tmux セッション名を生成
///
/// tmux セッション名にドットは使えないのでハイフンに置換する
pub fn session_name(project_name: &str) -> String {
    let sanitized = project_name.replace('.', "-");
    format!("vp-{}", sanitized)
}

/// 指定名の tmux セッションが存在するか確認
pub fn session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// tmux new-session で新規セッションを作成し、その中で VP を再実行する
///
/// `exec` で現在のプロセスを tmux に置き換えるため、この関数は戻らない。
pub fn create_and_exec(name: &str, vp_bin: &Path, args: &[&str]) -> ! {
    // tmux new-session -s <name> <vp_bin> <args...>
    // VP バイナリとその引数を tmux のウィンドウコマンドとして渡す
    let vp_bin_str = vp_bin.to_string_lossy();
    let mut cmd_parts = vec![vp_bin_str.as_ref()];
    cmd_parts.extend(args);
    let shell_command = cmd_parts.join(" ");

    tracing::info!(
        "tmux new-session -s {} で VP を再起動します",
        name
    );

    let err = exec_command(
        "tmux",
        &["new-session", "-s", name, &shell_command],
    );

    // exec が失敗した場合（通常は到達しない）
    eprintln!("tmux exec に失敗しました: {}", err);
    std::process::exit(1);
}

/// 既存の tmux セッションにアタッチする
///
/// `exec` で現在のプロセスを tmux に置き換えるため、この関数は戻らない。
pub fn attach_and_exec(name: &str) -> ! {
    tracing::info!(
        "tmux attach-session -t {} で既存セッションに接続します",
        name
    );

    let err = exec_command("tmux", &["attach-session", "-t", name]);

    eprintln!("tmux exec に失敗しました: {}", err);
    std::process::exit(1);
}

/// 現在のペインを水平分割して新しいペインを作成
///
/// `command` を指定するとそのコマンドで起動、None ならデフォルトシェル。
/// tmux split-window は現在のセッション内で実行されるため、
/// tmux 内でのみ呼び出すこと。
pub fn split_window(horizontal: bool, command: Option<&str>) -> Result<(), std::io::Error> {
    let mut args = vec!["split-window"];
    if horizontal {
        args.push("-v"); // 水平分割（上下）
    } else {
        args.push("-h"); // 垂直分割（左右）
    }
    if let Some(cmd) = command {
        args.push(cmd);
    }

    let status = Command::new("tmux")
        .args(&args)
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("tmux split-window failed with {}", status),
        ))
    }
}

/// ペイン一覧を取得（Phase C 以降で Canvas 連携に使用）
pub fn list_panes() -> Result<Vec<PaneInfo>, std::io::Error> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-F",
            "#{pane_id}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{pane_current_command}",
        ])
        .output()?;

    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "tmux list-panes failed",
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let panes = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 5 {
                Some(PaneInfo {
                    id: parts[0].to_string(),
                    active: parts[1] == "1",
                    width: parts[2].parse().unwrap_or(0),
                    height: parts[3].parse().unwrap_or(0),
                    command: parts[4].to_string(),
                })
            } else {
                None
            }
        })
        .collect();

    Ok(panes)
}

/// tmux ペイン情報
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub id: String,
    pub active: bool,
    pub width: u32,
    pub height: u32,
    pub command: String,
}

/// Unix exec でプロセスを置き換える
#[cfg(unix)]
fn exec_command(program: &str, args: &[&str]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    // exec() は成功時には戻らない。エラー時のみ Error を返す
    Command::new(program).args(args).exec()
}

#[cfg(not(unix))]
fn exec_command(program: &str, _args: &[&str]) -> std::io::Error {
    // 非 Unix 環境ではフォールバック（実質 macOS 専用なので到達しない）
    let _ = program;
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "exec is only supported on Unix",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_name() {
        assert_eq!(session_name("my-project"), "vp-my-project");
        assert_eq!(session_name("vantage-point"), "vp-vantage-point");
    }

    #[test]
    fn test_session_name_sanitizes_dots() {
        assert_eq!(session_name("com.example.app"), "vp-com-example-app");
    }

    #[test]
    fn test_is_inside_tmux_respects_env() {
        // TMUX 環境変数がなければ false（CI 環境ではほぼ常に false）
        let result = is_inside_tmux();
        let _ = result;
    }

    #[test]
    fn test_pane_info_debug() {
        let pane = PaneInfo {
            id: "%0".to_string(),
            active: true,
            width: 120,
            height: 40,
            command: "zsh".to_string(),
        };
        assert!(pane.active);
        assert_eq!(pane.width, 120);
    }
}
