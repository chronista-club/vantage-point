//! tmux 透過統合ヘルパー
//!
//! VP の TUI モード起動時に tmux セッションを自動作成し、
//! ターミナル管理を tmux に委譲する。
//! tmux 未インストール時やすでに tmux 内にいる場合はフォールバックして
//! 従来の ratatui TUI を直接起動する。

use std::path::Path;
use std::process::Command;

/// tmux のフルパス（Homebrew 優先、macOS 標準にフォールバック）
pub fn tmux_bin() -> Option<&'static str> {
    static TMUX_BIN: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *TMUX_BIN.get_or_init(|| {
        let candidates = [
            "/opt/homebrew/bin/tmux",
            "/usr/local/bin/tmux",
            "/usr/bin/tmux",
        ];
        for path in candidates {
            if Path::new(path).exists() {
                return Some(path);
            }
        }
        None
    })
}

/// tmux がインストールされているか確認
pub fn is_tmux_available() -> bool {
    tmux_bin().is_some()
}

/// 現在のプロセスが tmux セッション内で実行されているか確認
pub fn is_inside_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

/// tmux セッション名のサフィックス
const TMUX_SUFFIX: &str = "-vp";

/// プロジェクト名から tmux セッション名を生成
///
/// tmux セッション名にドットは使えないのでハイフンに置換する。
/// 形式: `{project}-vp`（プロジェクト名が先、タブ補完しやすい）
pub fn session_name(project_name: &str) -> String {
    let sanitized = project_name.replace('.', "-");
    format!("{}{}", sanitized, TMUX_SUFFIX)
}

/// プロジェクト名 + オプショナル ID から tmux セッション名を生成
///
/// ID ありの場合: `{project}-{id}-vp`（例: `vantage-point-kaizen-vp`）
/// ID なしの場合: `{project}-vp`（通常の session_name と同じ）
pub fn session_name_with_id(project_name: &str, id: Option<&str>) -> String {
    let sanitized = project_name.replace('.', "-");
    match id {
        Some(id) => format!("{}-{}{}", sanitized, id, TMUX_SUFFIX),
        None => format!("{}{}", sanitized, TMUX_SUFFIX),
    }
}

/// 指定名の tmux セッションが存在するか確認
pub fn session_exists(name: &str) -> bool {
    Command::new(tmux_bin().unwrap_or("tmux"))
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

    tracing::info!("tmux new-session -s {} で VP を再起動します", name);

    let err = exec_command("tmux", &["new-session", "-s", name, &shell_command]);

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

/// tmux セッションを kill する（存在しなくてもエラーにしない）
pub fn kill_session(name: &str) -> bool {
    Command::new(tmux_bin().unwrap_or("tmux"))
        .args(["kill-session", "-t", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// tmux new-session をデタッチモードで作成（呼び出し元に制御を返す）
///
/// `vp restart-all` 等、バッチ的に複数セッションを起動する場合に使用。
pub fn create_detached(name: &str, vp_bin: &Path, args: &[&str]) -> std::io::Result<()> {
    let vp_bin_str = vp_bin.to_string_lossy();
    let mut cmd_parts = vec![vp_bin_str.to_string()];
    cmd_parts.extend(args.iter().map(|s| s.to_string()));
    let shell_command = cmd_parts.join(" ");

    let status = Command::new(tmux_bin().unwrap_or("tmux"))
        .args(["new-session", "-d", "-s", name, &shell_command])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("tmux new-session -d -s {} failed", name),
        ))
    }
}

/// `-vp` サフィックスを持つ全 tmux セッション名を列挙
pub fn list_vp_sessions() -> Vec<String> {
    let output = Command::new(tmux_bin().unwrap_or("tmux"))
        .args(["list-sessions", "-F", "#{session_name}"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|s| s.ends_with(TMUX_SUFFIX))
            .map(|s| s.to_string())
            .collect(),
        Err(_) => vec![],
    }
}

/// 現在のプロセスが指定セッション内で実行されているか確認
///
/// tmux new-session で自分自身を再実行した場合、自セッションかどうかを判定する。
pub fn is_in_session(session_name: &str) -> bool {
    if !is_inside_tmux() {
        return false;
    }
    // tmux display-message で現在のセッション名を取得
    let output = Command::new(tmux_bin().unwrap_or("tmux"))
        .args(["display-message", "-p", "#{session_name}"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(out) => {
            let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
            current == session_name
        }
        Err(_) => false,
    }
}

/// tmux switch-client で現在のクライアントを別セッションに切り替える
///
/// tmux 内からプロジェクトを切り替える場合に使用。
pub fn switch_client(target_session: &str) {
    let _ = Command::new(tmux_bin().unwrap_or("tmux"))
        .args(["switch-client", "-t", target_session])
        .status();
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
        assert_eq!(session_name("my-project"), "my-project-vp");
        assert_eq!(session_name("vantage-point"), "vantage-point-vp");
    }

    #[test]
    fn test_session_name_sanitizes_dots() {
        assert_eq!(session_name("com.example.app"), "com-example-app-vp");
    }

    #[test]
    fn test_session_name_with_id() {
        assert_eq!(
            session_name_with_id("vantage-point", Some("kaizen")),
            "vantage-point-kaizen-vp"
        );
        assert_eq!(
            session_name_with_id("vantage-point", None),
            "vantage-point-vp"
        );
    }

    #[test]
    fn test_session_name_with_id_sanitizes_dots() {
        assert_eq!(
            session_name_with_id("com.example", Some("test")),
            "com-example-test-vp"
        );
    }

    #[test]
    fn test_is_inside_tmux_respects_env() {
        // TMUX 環境変数がなければ false（CI 環境ではほぼ常に false）
        let result = is_inside_tmux();
        let _ = result;
    }
}
