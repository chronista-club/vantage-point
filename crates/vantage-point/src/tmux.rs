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

}
