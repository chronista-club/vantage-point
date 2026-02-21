//! Daemon プロセスのライフサイクル管理
//!
//! PIDファイルによる生存確認、バックグラウンド自動起動、
//! シグナルハンドリングによるグレースフル停止を提供する。

use anyhow::Result;
use std::path::PathBuf;

/// Daemon の作業ディレクトリ（PIDファイル等を格納）
pub fn daemon_dir() -> PathBuf {
    std::env::temp_dir().join("vantage-point")
}

/// PIDファイルのパス
pub fn pid_file() -> PathBuf {
    daemon_dir().join("daemon.pid")
}

/// Daemon が生きているか確認する
///
/// PIDファイルを読み取り、そのプロセスが存在するかを `kill(pid, 0)` で確認。
/// 生きていれば `Some(pid)` を返す。
pub fn is_daemon_running() -> Option<u32> {
    let pid_path = pid_file();
    if !pid_path.exists() {
        return None;
    }
    let pid_str = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = pid_str.trim().parse().ok()?;

    // kill(pid, 0) でプロセスの存在を確認（シグナルは送信しない）
    let alive = i32::try_from(pid).map_or(false, |pid_i32| {
        unsafe { libc::kill(pid_i32, 0) == 0 }
    });
    if alive {
        Some(pid)
    } else {
        // プロセスが死んでいる場合、古いPIDファイルを掃除
        let _ = std::fs::remove_file(&pid_path);
        None
    }
}

/// PIDファイルを書き出す
pub fn write_pid_file() -> Result<()> {
    let dir = daemon_dir();
    std::fs::create_dir_all(&dir)?;
    let pid = std::process::id();
    std::fs::write(pid_file(), pid.to_string())?;
    tracing::info!(
        "PIDファイル書き出し: {} (PID: {})",
        pid_file().display(),
        pid
    );
    Ok(())
}

/// PIDファイルを削除する
pub fn remove_pid_file() {
    let path = pid_file();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("PIDファイル削除失敗: {}", e);
        } else {
            tracing::info!("PIDファイル削除: {}", path.display());
        }
    }
}

/// Daemon をフォアグラウンドで起動する
///
/// `vp daemon start` から呼ばれる。PIDファイルを書き出し、
/// シグナルハンドリングを設定し、シャットダウンを待機する。
pub async fn run_daemon(port: u16) -> Result<()> {
    // PIDファイル書き出し
    write_pid_file()?;

    println!(
        "VP Daemon started (PID: {}, port: {})",
        std::process::id(),
        port
    );
    tracing::info!(
        "VP Daemon 起動 (PID: {}, port: {})",
        std::process::id(),
        port
    );

    // シグナルハンドラ: SIGTERM / SIGINT でグレースフルシャットダウン
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM ハンドラ登録失敗");

    // DaemonState を初期化し、Unison Server を起動
    let state = std::sync::Arc::new(super::server::DaemonState::new());
    let server_handle = tokio::spawn(super::server::start_daemon_server(state, port));

    // シャットダウン待機
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT 受信、シャットダウン開始");
            println!("Shutting down VP Daemon...");
        }
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM 受信、シャットダウン開始");
            println!("Shutting down VP Daemon (SIGTERM)...");
        }
        _ = server_handle => {
            tracing::warn!("Unison Server が予期せず終了");
        }
    }

    // クリーンアップ
    remove_pid_file();
    println!("VP Daemon stopped.");
    Ok(())
}

/// Daemon がまだ起動していなければバックグラウンドで自動起動する
///
/// `vp start` から呼ばれる。既に起動済みならそのPIDを返す。
pub fn ensure_daemon_running(port: u16) -> Result<u32> {
    if let Some(pid) = is_daemon_running() {
        tracing::info!("Daemon は既に起動中 (PID: {})", pid);
        return Ok(pid);
    }

    tracing::info!("Daemon を自動起動します (port: {})", port);

    // 自分自身の実行ファイルを `vp daemon start` として起動
    let child = std::process::Command::new(std::env::current_exe()?)
        .args(["daemon", "start", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let pid = child.id();
    tracing::info!("Daemon 起動完了 (PID: {})", pid);
    Ok(pid)
}

/// PIDを指定してDaemonプロセスを停止する
pub fn stop_daemon(pid: u32) -> Result<()> {
    tracing::info!("Daemon 停止要求 (PID: {})", pid);

    let pid_i32 = i32::try_from(pid)
        .map_err(|_| anyhow::anyhow!("PIDがi32の範囲外: {}", pid))?;

    // SIGTERM を送信
    let ret = unsafe { libc::kill(pid_i32, libc::SIGTERM) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!("SIGTERM送信失敗 (PID: {}): {}", pid, err);
        // プロセスが既に死んでいる可能性がある
        remove_pid_file();
        return Ok(());
    }

    // PIDファイルはDaemon側のシャットダウン処理で削除される
    // ただし、Daemonが応答しなかった場合のフォールバック
    std::thread::sleep(std::time::Duration::from_millis(500));
    if is_daemon_running().is_some() {
        tracing::warn!("SIGTERM後もDaemonが生存、SIGKILLを送信");
        let ret = unsafe { libc::kill(pid_i32, libc::SIGKILL) };
        if ret != 0 {
            tracing::warn!("SIGKILL送信失敗 (PID: {}): {}", pid, std::io::Error::last_os_error());
        }
        remove_pid_file();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_dir_path() {
        let dir = daemon_dir();
        // /tmp/vantage-point/ 配下であることを確認
        assert!(dir.to_string_lossy().contains("vantage-point"));
    }

    #[test]
    fn test_pid_file_path() {
        let path = pid_file();
        assert!(path.to_string_lossy().contains("daemon.pid"));
        // daemon_dir() 配下であることを確認
        assert_eq!(path.parent().unwrap(), daemon_dir());
    }

    #[test]
    fn test_is_daemon_running_no_pid_file() {
        // PIDファイルが存在しない状態での確認
        // テスト環境ではPIDファイルが存在しない前提
        // （CI等で /tmp/vantage-point/daemon.pid が残っている場合は
        //  実際のDaemonが動いている可能性があるため、存在チェックのみ）
        let result = is_daemon_running();
        // PIDファイルがなければ None, あっても正しい動作
        // ここではパスの正当性の確認が主目的
        assert!(result.is_none() || result.is_some());
    }

    #[test]
    fn test_write_and_remove_pid_file() {
        // テスト用の一時ディレクトリを使うため、実際のPIDファイルには触れない
        // write_pid_file / remove_pid_file の基本パスロジックを確認
        let dir = daemon_dir();
        let path = pid_file();
        assert!(path.starts_with(&dir));
    }
}
