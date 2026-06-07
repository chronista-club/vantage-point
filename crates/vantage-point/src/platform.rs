//! プラットフォーム抽象層 (Unix / Windows)
//!
//! Unix の `libc::kill` と `tokio::signal::unix` を crossplat 化する薄い wrapper。
//! Phase W0 (VP-87) で Windows ビルドを通すために導入、
//! Step 1 (Windows dogfood) で `windows` crate (0.62) 経由の本実装に移行。
//!
//! ## Windows 実装メモ
//! - `process_alive`: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `GetExitCodeProcess`
//!   で `STILL_ACTIVE` を判定。 OpenProcess 失敗は dead 扱い。
//! - `process_terminate` / `process_kill`: Windows に SIGTERM / SIGKILL の区別はない。
//!   両者とも `TerminateProcess(handle, 1)` で hard kill する (graceful な代替手段は
//!   別 API ─ WM_CLOSE 送信等 ─ で別途実装する必要があり、 現状は hard kill 1 本)。

/// PID が alive かを判定。
///
/// Unix では:
/// - `kill(pid, 0) == 0` → alive
/// - `EPERM` (権限なし) → プロセスは存在するので alive 扱い
/// - `ESRCH` (プロセスなし) → dead
///
/// Windows では:
/// - `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` でハンドル取得 → 失敗なら dead
/// - `GetExitCodeProcess` で exit code 取得 → `STILL_ACTIVE` (259) なら alive
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
        use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) if !h.is_invalid() => h,
                _ => return false,
            };
            let mut exit_code: u32 = 0;
            let query_ok = GetExitCodeProcess(handle, &mut exit_code).is_ok();
            let _ = CloseHandle(handle);
            // STILL_ACTIVE は NTSTATUS::STILL_ACTIVE (= 259 / 0x103)。 windows crate では
            // `Win32::Foundation::STILL_ACTIVE` (NTSTATUS) として export されている。
            query_ok && exit_code == STILL_ACTIVE.0 as u32
        }
    }
}

/// graceful terminate (Unix: SIGTERM、 Windows: TerminateProcess)。戻り値: 送信成功か。
///
/// 注: Windows に SIGTERM 相当の graceful signal はないため、 Unix の SIGTERM とは
/// 厳密に等価ではない。 console 子プロセスへの GenerateConsoleCtrlEvent や GUI の
/// WM_CLOSE 経路を入れる場合は callsite で別途扱う。
pub fn process_terminate(pid: u32) -> bool {
    #[cfg(unix)]
    {
        i32::try_from(pid).is_ok_and(|p| unsafe { libc::kill(p, libc::SIGTERM) == 0 })
    }
    #[cfg(windows)]
    {
        windows_terminate_process(pid)
    }
}

/// force kill (Unix: SIGKILL、 Windows: TerminateProcess)。戻り値: 送信成功か。
pub fn process_kill(pid: u32) -> bool {
    #[cfg(unix)]
    {
        i32::try_from(pid).is_ok_and(|p| unsafe { libc::kill(p, libc::SIGKILL) == 0 })
    }
    #[cfg(windows)]
    {
        windows_terminate_process(pid)
    }
}

/// Windows: PROCESS_TERMINATE ハンドルで `TerminateProcess(pid, 1)` を呼ぶ共通実装。
/// terminate / kill 両者から呼ばれる (Windows は SIGTERM / SIGKILL の区別なし)。
#[cfg(windows)]
fn windows_terminate_process(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    unsafe {
        let handle = match OpenProcess(PROCESS_TERMINATE, false, pid) {
            Ok(h) if !h.is_invalid() => h,
            _ => return false,
        };
        let ok = TerminateProcess(handle, 1).is_ok();
        let _ = CloseHandle(handle);
        ok
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
