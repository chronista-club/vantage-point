//! プラットフォーム抽象層 (Unix / Windows)
//!
//! Unix の `libc::kill` と `tokio::signal::unix` を crossplat 化する薄い wrapper。
//! Phase W0 (VP-87) で Windows ビルドを通すために導入。
//!
//! ## Windows 対応状況
//! - 現状 Windows 版は stub (always false / no-op) — Phase W1 以降で
//!   `windows-sys` 経由の `OpenProcess` + `TerminateProcess` で本実装予定。
//! - vp-shell (Phase W1) で daemon を Windows で実際に動かす段で必要になる。

/// PID が alive かを判定 (Unix: `kill(pid, 0)` + EPERM 判定)。Windows は現状 false。
///
/// Unix では:
/// - `kill(pid, 0) == 0` → alive
/// - `EPERM` (権限なし) → プロセスは存在するので alive 扱い
/// - `ESRCH` (プロセスなし) → dead
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid_i32) = i32::try_from(pid) else {
            return false;
        };
        let ret = unsafe { libc::kill(pid_i32, 0) };
        if ret == 0 {
            true
        } else {
            let err = std::io::Error::last_os_error();
            err.raw_os_error() == Some(libc::EPERM)
        }
    }
    #[cfg(windows)]
    {
        let _ = pid;
        false
    }
}

/// graceful terminate (Unix: SIGTERM)。戻り値: 送信成功か。
pub fn process_terminate(pid: u32) -> bool {
    #[cfg(unix)]
    {
        i32::try_from(pid).is_ok_and(|p| unsafe { libc::kill(p, libc::SIGTERM) == 0 })
    }
    #[cfg(windows)]
    {
        let _ = pid;
        false
    }
}

/// force kill (Unix: SIGKILL)。戻り値: 送信成功か。
pub fn process_kill(pid: u32) -> bool {
    #[cfg(unix)]
    {
        i32::try_from(pid).is_ok_and(|p| unsafe { libc::kill(p, libc::SIGKILL) == 0 })
    }
    #[cfg(windows)]
    {
        let _ = pid;
        false
    }
}

/// 終了シグナルを非同期で待ち受ける。
/// Unix: SIGTERM。Windows: Ctrl-C を代替イベントに (SIGTERM 相当なし)。
#[cfg(unix)]
pub async fn wait_for_terminate_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(e) => {
            tracing::warn!("SIGTERM handler install 失敗: {}", e);
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(windows)]
pub async fn wait_for_terminate_signal() {
    // Windows に SIGTERM はないので Ctrl-C を代替イベントに
    let _ = tokio::signal::ctrl_c().await;
}
