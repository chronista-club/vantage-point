//! Shell の自動判定 — Lead pane で login shell を起動して `~/.zshrc` 等を読む。
//!
//! 関連 memory: `mem_1CaSiJkD9HATDY2srrv6D4` (VP Observability Stack) と同 sprint。
//!
//! ## 方針
//!
//! - **macOS**: `$SHELL` (大半のユーザは `/bin/zsh`) を **`-l` (login shell)** で起動
//!   → `~/.zshenv` → `~/.zprofile` → `~/.zshrc` 連鎖が読まれて、mise / volta / nvm 等の
//!   PATH を rc 経由で取り込む。Terminal.app / iTerm2 の慣例と一致。
//! - **Windows**: **git-bash** (`C:\Program Files\Git\bin\bash.exe`) を最優先。
//!   WSL の `C:\Windows\System32\bash.exe` は path lower-case で除外 (名前衝突を避ける)。
//!   git-bash 不在なら pwsh.exe → powershell.exe → cmd.exe へフォールバック。
//! - **Linux**: `$SHELL` > `/bin/bash`。
//!
//! Mode 1 (`crate::terminal::spawn_shell` — local portable-pty) と
//! Mode 2 (`crate::ws_terminal::connect_daemon_terminal` — TheWorld daemon WS 経由)
//! の両方から使える共通モジュール。

use std::path::Path;

/// Shell binary を決定する。
///
/// 優先順位:
/// 1. `VP_SHELL` env (明示 override、`mise run win:wsl` 等の経路)
/// 2. `SHELL` env (Unix 通常、macOS は `/bin/zsh` が入ってる)
/// 3. macOS フォールバック: `/bin/zsh`
/// 4. Linux フォールバック: `/bin/bash`
/// 5. Windows: git-bash 標準 install path → PATH 内 bash.exe (System32 除外)
///    → pwsh.exe → powershell.exe → cmd.exe
pub fn detect_shell() -> String {
    if let Ok(explicit) = std::env::var("VP_SHELL") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return s;
        }
    }

    #[cfg(target_os = "macos")]
    {
        "/bin/zsh".to_string()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "/bin/bash".to_string()
    }
    #[cfg(windows)]
    {
        // 1. git-bash の標準 install path を最優先 (Git for Windows)
        let git_bash_candidates = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        for path in &git_bash_candidates {
            if Path::new(path).exists() {
                return (*path).to_string();
            }
        }
        // 2. PATH から bash.exe (Git for Windows が PATH 設定してる場合)。
        //    ただし WSL の C:\Windows\System32\bash.exe は除外。
        if let Some(p) = find_in_path("bash.exe") {
            let p_lower = p.to_string_lossy().to_lowercase();
            if !p_lower.contains(r"\windows\system32\") {
                return p.to_string_lossy().into_owned();
            }
        }
        // 3. PowerShell (pwsh > powershell)
        for shell in &["pwsh.exe", "powershell.exe"] {
            if let Some(p) = find_in_path(shell) {
                return p.to_string_lossy().into_owned();
            }
        }
        // 4. cmd.exe (COMSPEC)
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
}

/// Shell basename に応じた default 起動引数。
/// - bash 系 (zsh/bash/sh/fish/dash/ksh): `-l` (login shell として起動)
/// - PowerShell (pwsh/powershell): `-NoLogo` (起動 banner 抑制、interactive は PTY なら自動)
/// - cmd.exe / その他: 引数なし
///
/// `VP_SHELL_ARGS` env が set されていれば最優先 (POSIX shell-words split)。
pub fn detect_shell_args(shell: &str) -> Vec<String> {
    if let Ok(explicit) = std::env::var("VP_SHELL_ARGS") {
        return shell_words::split(&explicit).unwrap_or_default();
    }
    let basename = Path::new(shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    match basename.as_str() {
        // POSIX shells: login で起動 (~/.zprofile, ~/.zshrc 等の rc 連鎖を読む)
        "zsh" | "bash" | "sh" | "fish" | "dash" | "ksh" => vec!["-l".to_string()],
        // PowerShell: NoLogo で起動 banner 抑制
        "pwsh" | "powershell" => vec!["-NoLogo".to_string()],
        // cmd.exe / その他はデフォルト引数なし
        _ => Vec::new(),
    }
}

/// Windows: PATH 環境変数から実行可能ファイルを探す簡易 `which`。
#[cfg(windows)]
fn find_in_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in path_var.split(';') {
        let candidate = Path::new(dir).join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_shell_args_zsh() {
        // VP_SHELL_ARGS が set されてるとそれが優先される (test 並列実行で競合しないよう unsafe save/restore は省略)
        let original = std::env::var("VP_SHELL_ARGS").ok();
        unsafe {
            std::env::remove_var("VP_SHELL_ARGS");
        }

        assert_eq!(detect_shell_args("/bin/zsh"), vec!["-l"]);
        assert_eq!(detect_shell_args("/bin/bash"), vec!["-l"]);
        assert_eq!(detect_shell_args("/usr/local/bin/fish"), vec!["-l"]);
        assert_eq!(
            detect_shell_args(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            vec!["-NoLogo"]
        );
        assert_eq!(
            detect_shell_args(r"C:\Windows\System32\cmd.exe"),
            Vec::<String>::new()
        );

        if let Some(v) = original {
            unsafe {
                std::env::set_var("VP_SHELL_ARGS", v);
            }
        }
    }
}
