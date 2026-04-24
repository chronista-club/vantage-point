//! プラットフォーム抽象 (vp-db 内版)
//!
//! vantage-point crate の `platform.rs` と同じ実装を vp-db 内に持たせることで
//! vantage-point 本体への依存を断つ。コードが重複するが、vp-db が使う関数は
//! これだけ (process alive/terminate/kill) なので影響は最小。

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
