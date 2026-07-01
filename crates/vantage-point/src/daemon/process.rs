//! Daemon プロセスのライフサイクル管理
//!
//! PIDファイルによる生存確認、バックグラウンド自動起動、
//! シグナルハンドリングによるグレースフル停止を提供する。

use anyhow::Result;
use std::path::{Path, PathBuf};

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
        // GUI/launchd 起動の最小 PATH が daemon → SP へ伝播するのを spawn 最上流で断つ。
        .env("PATH", crate::spawn_env::augmented_spawn_path())
        // LANG も PATH と対称に補強。 launchd の C ロケール伝播を daemon spawn 最上流で断ち、
        // 子 PtySlot の utf8_locale() がこの LANG を継承して UTF-8 に解決できるようにする。
        .env("LANG", crate::spawn_env::utf8_locale())
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

    // u32 → i32 overflow は Unix の kill(2) で扱えないため早期 Err
    // (Phase W1 で Windows 向け OpenProcess/TerminateProcess を導入する際は
    //  Windows DWORD に合わせてこの制限を外す)
    if i32::try_from(pid).is_err() {
        anyhow::bail!("PID {} が i32 範囲外 — 停止対象として不正", pid);
    }

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

// ============================================================================
// L1 lifecycle (Phase C ⑤): macOS LaunchAgent 常駐化（login always-on + crash 自動再起動）
// ============================================================================
//
// `vp daemon start` は foreground blocking、`ensure_daemon_running` は bg auto-spawn のみで、
// login 永続も crash 自動復帰も無い。LaunchAgent（10+ 年 stable な launchd 機構、SMAppService
// = macOS 13+ は L3）で TheWorld を always-on にする。plist を `~/Library/LaunchAgents/` に
// 置くだけで次回 login から `RunAtLoad` が起こし、`KeepAlive=true` が crash を自動再起動する。

/// LaunchAgent の Label（plist の Label key + launchctl の service 名）。
pub const LAUNCH_AGENT_LABEL: &str = "club.chronista.vantage-point.daemon";

/// XML 特殊文字を escape する（plist の動的文字列＝path / PATH env に `&` 等が混じっても壊さない）。
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// LaunchAgent plist の中身を生成する（純関数: data → calculation、env/fs に触れない）。
///
/// - `ProgramArguments` = `[<vp_binary>, world, --port, <port>]`（`ensure_daemon_running` と同形）
/// - `RunAtLoad=true`（login always-on）/ `KeepAlive=true`（crash 自動再起動）
/// - `EnvironmentVariables.PATH` = `path_env`（GUI/launchd の痩せた PATH では mise/claude が
///   解決不能なので augmented を渡す。SSOT は [`crate::spawn_env::augmented_spawn_path`]）
/// - `EnvironmentVariables.LANG` / `LC_CTYPE` = `lang`（launchd は LANG を剥がして C ロケールに
///   するため、配下の tmux client が utf8=0 → 日本語など CJK が `_` 化する。UTF-8 ロケールを
///   焼いて断つ。SSOT は [`crate::spawn_env::utf8_locale`]。PATH/TERM と同根の launchd env stripping）
/// - `StandardOut/ErrPath` = `log_dir` 配下（daemon は `vp world` 経路で自前 file log を持たないため、
///   crash-restart loop の調査用に launchd へ stdout/stderr を捕捉させる）
pub fn generate_launch_agent_plist(
    vp_binary: &Path,
    port: u16,
    path_env: &str,
    lang: &str,
    log_dir: &Path,
) -> String {
    let exec = xml_escape(&vp_binary.to_string_lossy());
    let path_env = xml_escape(path_env);
    let lang = xml_escape(lang);
    let out = xml_escape(&log_dir.join("daemon.launchd.out.log").to_string_lossy());
    let err = xml_escape(&log_dir.join("daemon.launchd.err.log").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCH_AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exec}</string>
        <string>world</string>
        <string>--port</string>
        <string>{port}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path_env}</string>
        <key>LANG</key>
        <string>{lang}</string>
        <key>LC_CTYPE</key>
        <string>{lang}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#
    )
}

/// LaunchAgent plist のパス（`~/Library/LaunchAgents/<label>.plist`）。
#[cfg(target_os = "macos")]
pub fn launch_agent_plist_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join("Library/LaunchAgents")
            .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
    })
}

/// LaunchAgent を install する（idempotent）。plist を書き出し、launchctl で即時 load する。
///
/// plist を `~/Library/LaunchAgents/` に置くだけで**次回 login から** `RunAtLoad` が起こすが、
/// 今すぐ有効化するため bootout(既存)→bootstrap で reload する。bootstrap が非ゼロ（既 load 等）でも
/// plist は配置済なので install は成功扱い（次回 login で確実に拾う＝degradation 設計）。
#[cfg(target_os = "macos")]
pub fn install_launch_agent(vp_binary: &Path, port: u16) -> Result<PathBuf> {
    use anyhow::Context;

    let plist_path =
        launch_agent_plist_path().context("home dir 解決失敗（LaunchAgent path 不明）")?;
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("LaunchAgents dir 作成失敗: {}", parent.display()))?;
    }

    // StandardOut/ErrPath の親（log dir）を先に作る（launchd は dir を作らない）。
    let log_dir = crate::config::vp_log_dir();
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        tracing::warn!("log dir 作成失敗（launchd log は出ないかも）: {}", e);
    }

    let path_env = crate::spawn_env::augmented_spawn_path();
    let lang = crate::spawn_env::utf8_locale();
    let content = generate_launch_agent_plist(vp_binary, port, &path_env, &lang, &log_dir);
    std::fs::write(&plist_path, content)
        .with_context(|| format!("plist 書き出し失敗: {}", plist_path.display()))?;

    // launchctl で即時 reload（modern API: bootout→bootstrap、gui/<uid> domain）。
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let service_target = format!("{domain}/{LAUNCH_AGENT_LABEL}");
    // 既存 load を bootout（未 load なら error → 無視）してから bootstrap で立て直す。
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &service_target])
        .output();
    let out = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_path.to_string_lossy()])
        .output()
        .context("launchctl bootstrap 実行失敗")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        tracing::warn!(
            "launchctl bootstrap 非ゼロ（plist は配置済、次回 login で RunAtLoad が拾う）: {}",
            stderr.trim()
        );
    }
    Ok(plist_path)
}

/// LaunchAgent を uninstall する（idempotent）。launchctl で unload し plist を削除する。
#[cfg(target_os = "macos")]
pub fn uninstall_launch_agent() -> Result<()> {
    use anyhow::Context;

    let uid = unsafe { libc::getuid() };
    let service_target = format!("gui/{uid}/{LAUNCH_AGENT_LABEL}");
    // 未 load でも error は無視（idempotent）。
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &service_target])
        .output();
    if let Some(plist_path) = launch_agent_plist_path()
        && plist_path.exists()
    {
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("plist 削除失敗: {}", plist_path.display()))?;
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
    fn test_generate_launch_agent_plist_shape() {
        let plist = generate_launch_agent_plist(
            Path::new("/usr/local/bin/vp"),
            32000,
            "/opt/homebrew/bin:/usr/bin",
            "ja_JP.UTF-8",
            Path::new("/tmp/vp-log"),
        );
        // Label / 常駐 key / ProgramArguments / port / PATH / LANG / log path を含む。
        assert!(plist.contains("<string>club.chronista.vantage-point.daemon</string>"));
        assert!(plist.contains("<string>/usr/local/bin/vp</string>"));
        assert!(plist.contains("<string>world</string>"));
        assert!(plist.contains("<string>32000</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("/opt/homebrew/bin:/usr/bin"));
        // CJK blackout 対策: LANG / LC_CTYPE が UTF-8 ロケールで焼かれている。
        assert!(plist.contains("<key>LANG</key>"));
        assert!(plist.contains("<key>LC_CTYPE</key>"));
        assert!(plist.contains("<string>ja_JP.UTF-8</string>"));
        assert!(plist.contains("daemon.launchd.out.log"));
    }

    #[test]
    fn test_launch_agent_plist_xml_escapes() {
        // path / PATH に XML 特殊文字が混じっても plist を壊さない（生の & を残さない）。
        let plist = generate_launch_agent_plist(
            Path::new("/Users/a&b/vp"),
            32000,
            "/x<y>/bin",
            "en_US.UTF-8",
            Path::new("/tmp/l"),
        );
        assert!(plist.contains("/Users/a&amp;b/vp"));
        assert!(plist.contains("/x&lt;y&gt;/bin"));
        assert!(!plist.contains("a&b/vp"), "生の & が残ってはいけない");
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
