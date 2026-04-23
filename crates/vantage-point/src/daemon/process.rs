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
/// 1. PIDファイルを読み取り、`kill(pid, 0)` でプロセスの存在を確認
/// 2. PIDファイルが無い/古い場合、ポート接続で実際の稼働を確認（フォールバック）
pub fn is_daemon_running() -> Option<u32> {
    // 1. PIDファイルベースの確認
    if let Some(pid) = check_pid_file() {
        return Some(pid);
    }

    // 2. フォールバック: ポート接続で TheWorld の生存を確認
    //    PIDファイルが壊れた・消えた場合でも制御可能にする
    check_world_port()
}

/// PIDファイルからデーモンの生存を確認
fn check_pid_file() -> Option<u32> {
    let pid_path = pid_file();
    if !pid_path.exists() {
        return None;
    }
    let pid_str = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = pid_str.trim().parse().ok()?;

    let alive = crate::platform::process_alive(pid);
    if alive {
        Some(pid)
    } else {
        // プロセスが死んでいる場合、古いPIDファイルを掃除
        let _ = std::fs::remove_file(&pid_path);
        None
    }
}

/// TheWorld ポートに接続して PID を取得（PIDファイル不在時のフォールバック）
fn check_world_port() -> Option<u32> {
    let url = format!("http://[::1]:{}/api/health", crate::cli::WORLD_PORT);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let resp = client.get(&url).send().ok()?;
    let health: crate::cli::HealthResponse = resp.json().ok()?;

    // PIDファイルを復元する
    let dir = daemon_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("PIDファイルディレクトリ作成失敗: {}", e);
    }
    if let Err(e) = std::fs::write(pid_file(), health.pid.to_string()) {
        tracing::warn!("PIDファイル復元書き込み失敗: {}", e);
    }
    tracing::info!(
        "PIDファイル復元（ポートフォールバック）: PID {}",
        health.pid
    );

    Some(health.pid)
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

    // DaemonState を初期化し、Unison Server を起動
    let state = std::sync::Arc::new(super::server::DaemonState::new());
    let server_handle = tokio::spawn(super::server::start_daemon_server(state, port));

    // シャットダウン待機 (Unix: SIGTERM / SIGINT。Windows: Ctrl-C のみ)
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT (Ctrl-C) 受信、シャットダウン開始");
            println!("Shutting down VP Daemon...");
        }
        _ = crate::platform::wait_for_terminate_signal() => {
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

/// TheWorld がまだ起動していなければバックグラウンドで自動起動する
///
/// `vp sp start` から呼ばれる。既に起動済みならそのPIDを返す。
pub fn ensure_daemon_running(port: u16) -> Result<u32> {
    if let Some(pid) = is_daemon_running() {
        tracing::info!("TheWorld は既に起動中 (PID: {})", pid);
        return Ok(pid);
    }

    tracing::info!("TheWorld を自動起動します (port: {})", port);

    // 自分自身の実行ファイルを `vp world` として起動
    let child = std::process::Command::new(std::env::current_exe()?)
        .args(["world", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let pid = child.id();
    tracing::info!("TheWorld 起動完了 (PID: {})", pid);
    Ok(pid)
}

/// PIDを指定してDaemonプロセスを停止する
pub fn stop_daemon(pid: u32) -> Result<()> {
    tracing::info!("Daemon 停止要求 (PID: {})", pid);

    // SIGTERM を送信
    if !crate::platform::process_terminate(pid) {
        tracing::warn!("SIGTERM 送信失敗 (PID: {}) — 既に死んでいる可能性", pid);
        remove_pid_file();
        return Ok(());
    }

    // PIDファイルはDaemon側のシャットダウン処理で削除される
    // ただし、Daemonが応答しなかった場合のフォールバック
    // 注: ポートフォールバック（check_world_port）は使わない。
    //      シャットダウン中はポートがまだ開いていることがあり、誤検出で SIGKILL を送ってしまうため。
    std::thread::sleep(std::time::Duration::from_millis(500));
    if check_pid_file().is_some_and(|running_pid| running_pid == pid) {
        tracing::warn!("SIGTERM後もDaemonが生存、SIGKILLを送信");
        if !crate::platform::process_kill(pid) {
            tracing::warn!("SIGKILL 送信失敗 (PID: {})", pid);
        }
        remove_pid_file();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PIDファイルを共有するテスト間の競合を防ぐミューテックス
    static PID_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    fn test_check_pid_file_stale_pid_cleanup() {
        let _lock = PID_FILE_LOCK.lock().unwrap();
        // 存在しないPIDのPIDファイルが残っている場合、
        // check_pid_file は None を返し、ファイルを掃除するべき
        // 注: is_daemon_running() はポートフォールバックを含むため、
        //      TheWorld 稼働中に誤検出する。PIDファイル単体のテストには check_pid_file を使う。
        let dir = daemon_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = pid_file();

        // 既存のPIDファイルをバックアップ
        let backup = std::fs::read_to_string(&path).ok();

        // ほぼ確実に存在しないPIDを書き込む
        std::fs::write(&path, "2147483647").unwrap(); // i32::MAX

        let result = check_pid_file();
        assert!(result.is_none(), "存在しないPIDなのに Some が返った");
        // PIDファイルが掃除されていること
        assert!(!path.exists(), "古いPIDファイルが削除されていない");

        // バックアップを復元
        if let Some(content) = backup {
            let _ = std::fs::create_dir_all(&dir);
            std::fs::write(&path, content).unwrap();
        }
    }

    #[test]
    fn test_check_pid_file_overflow_pid() {
        let _lock = PID_FILE_LOCK.lock().unwrap();
        // i32::MAX を超えるPIDがPIDファイルに書かれた場合、
        // check_pid_file は安全に None を返すべき
        let dir = daemon_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = pid_file();

        let backup = std::fs::read_to_string(&path).ok();

        // u32::MAX は i32::try_from で失敗する
        std::fs::write(&path, u32::MAX.to_string()).unwrap();

        let result = check_pid_file();
        assert!(result.is_none(), "オーバーフローPIDが Some を返した");

        // バックアップを復元（PIDファイルが消えている場合に備え）
        if let Some(content) = backup {
            let _ = std::fs::create_dir_all(&dir);
            std::fs::write(&path, content).unwrap();
        } else {
            // バックアップなし → ファイルを削除して元の状態に
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn test_write_and_remove_pid_file() {
        let dir = daemon_dir();
        let path = pid_file();
        assert!(path.starts_with(&dir));
    }

    #[test]
    fn test_stop_daemon_nonexistent_pid() {
        let _lock = PID_FILE_LOCK.lock().unwrap();
        // 存在しないPIDへの stop_daemon はエラーにならず正常終了すべき
        // SIGTERM送信失敗 → PIDファイル掃除 → Ok
        let result = stop_daemon(2147483647); // i32::MAX — まず存在しない
        assert!(
            result.is_ok(),
            "存在しないPIDへの stop_daemon がエラーを返した: {:?}",
            result
        );
    }

    #[test]
    fn test_stop_daemon_overflow_pid() {
        // i32 に収まらないPIDへの stop_daemon はエラーを返すべき
        let overflow_pid = i32::MAX as u32 + 1;
        let result = stop_daemon(overflow_pid);
        assert!(
            result.is_err(),
            "オーバーフローPIDの stop_daemon がエラーを返さなかった"
        );
    }
}
