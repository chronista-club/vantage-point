//! shell interpreter の探索 — Windows で git-bash を見つける共有ロジック。
//!
//! vantage-point (stand_spawner: stand script を bash 経由で exec) と vp-app (shell_detect:
//! Conductor pane の login shell) の双方が git-bash の実在 path を必要とする。 両者が別実装を
//! 持つと drift するため本 crate に集約する (spawn_env と同じ SSOT 方針)。

#[cfg(windows)]
use std::path::PathBuf;

/// Git for Windows (git-bash) の `bash.exe` を探す。 見つからなければ `None`。
///
/// 探索順:
/// 1. 標準 install path (`C:\Program Files\Git\bin\bash.exe` / `Program Files (x86)`)
/// 2. PATH 上の `bash.exe` ── ただし **WSL stub を除外**:
///    - `C:\Windows\System32\bash.exe` (WSL launcher)
///    - `...\WindowsApps\bash.exe` (Microsoft Store の WSL app execution alias)
///    どちらも native binary を Linux 環境で動かす stub で、 git-bash とは別物。 これらを掴むと
///    stand script が WSL 内で動いてしまい cwd / path / tool 解決が全て破綻する。
///
/// non-Windows では常に `None` (呼び出し側は Unix path を別途使う)。
#[cfg(windows)]
pub fn find_git_bash() -> Option<PathBuf> {
    use std::path::Path;

    const STANDARD: [&str; 2] = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    for path in STANDARD {
        if Path::new(path).exists() {
            return Some(PathBuf::from(path));
        }
    }

    let path_var = std::env::var("PATH").ok()?;
    for dir in path_var.split(';') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join("bash.exe");
        if candidate.exists() && !is_wsl_stub(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// non-Windows では git-bash 概念が無いので常に `None`。
#[cfg(not(windows))]
pub fn find_git_bash() -> Option<std::path::PathBuf> {
    None
}

/// `bash.exe` の path が WSL stub (System32 / WindowsApps) か判定する。
#[cfg(windows)]
fn is_wsl_stub(path: &std::path::Path) -> bool {
    let lower = path.to_string_lossy().to_lowercase();
    lower.contains(r"\windows\system32\") || lower.contains(r"\windowsapps\")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn wsl_stubs_are_excluded() {
        assert!(is_wsl_stub(Path::new(r"C:\Windows\System32\bash.exe")));
        assert!(is_wsl_stub(Path::new(
            r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\bash.exe"
        )));
    }

    #[test]
    fn git_bash_is_not_a_stub() {
        assert!(!is_wsl_stub(Path::new(r"C:\Program Files\Git\bin\bash.exe")));
    }
}
