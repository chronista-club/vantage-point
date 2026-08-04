//! sidebar Hub 行の Login / Logout フロー。
//!
//! `auth:login` / `auth:logout` IPC を受けた event loop が blocking pool
//! （`tokio::task::spawn_blocking`）で呼ぶ。実行体は既存 CLI（`vp auth login|logout`）の
//! spawn — OAuth（browser + localhost callback + token 交換）の実装を GUI に複製しない。
//! 成功後の hub への反映は caller が `daemon-control.hub/reconnect` で行う
//! （表示の SSOT は daemon の**接続の auth 状態**であって file ではない）。
//!
//! ## 実行スレッド
//! rfd の同期ダイアログと `Command` は blocking なので async context から直接呼ばない
//! （`update_flow.rs` と同じ理由）。rfd の macOS 同期ダイアログは別スレッドから呼んでも
//! 内部で main queue に marshal されるため、main の event loop が回っていれば動く。

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::daemon_launcher::locate_vp_binary;

/// login / logout フローの二重起動ガード（ボタン連打 / browser 放置中の再 click 対策）。
static AUTH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// `vp auth login` の完了待ち上限。browser の OAuth をユーザーが放置した場合に
/// フロー（と in-flight ガード）が永久に塞がらないよう、超過で子プロセスを kill する。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// [`AUTH_IN_FLIGHT`] の RAII ガード。取得失敗 = 別フロー実行中。
/// drop で必ず解放するので、早期 return / panic 経路でもガードが残らない。
struct FlightGuard;

impl FlightGuard {
    fn acquire() -> Option<Self> {
        if AUTH_IN_FLIGHT.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(Self)
        }
    }
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        AUTH_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

/// Login フロー本体（blocking）。成功 = true（caller が `hub/reconnect` を撃つ）。
///
/// `vp auth login` は default browser で authorize URL を開き、localhost callback で
/// token を受けて `~/.vp/credentials.json` に保存して exit する。
pub fn run_login_blocking(target: &str) -> bool {
    let Some(_guard) = FlightGuard::acquire() else {
        tracing::info!("auth login: 既にフロー実行中のため click を無視");
        return false;
    };
    let vp = locate_vp_binary();
    tracing::info!(
        "auth login: `vp auth login --for {}` を起動 (browser OAuth): {}",
        target,
        vp.display()
    );
    // ⚠️ 宛先を必ず渡す。省略すると env `VP_OIDC_AUDIENCE` の既定に落ちて**別の席**へ
    // 保存され、「creo に Login したのに creo の token が増えない」になる。
    let child = Command::new(&vp)
        .args(["auth", "login", "--for", target])
        .stdin(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("auth login: `vp auth login` の起動に失敗: {}", e);
            return false;
        }
    };
    match wait_with_timeout(&mut child, LOGIN_TIMEOUT) {
        Some(true) => {
            tracing::info!("auth login: 完了 (credentials 保存済み)");
            true
        }
        Some(false) => {
            tracing::warn!("auth login: `vp auth login` が失敗 exit した");
            false
        }
        None => {
            tracing::warn!(
                "auth login: {}s 以内に完了しなかったため中断 (browser 放置?)",
                LOGIN_TIMEOUT.as_secs()
            );
            false
        }
    }
}

/// Logout フロー本体（blocking）。確認ダイアログ → `vp auth logout`。成功 = true。
pub fn run_logout_blocking(target: &str) -> bool {
    let Some(_guard) = FlightGuard::acquire() else {
        tracing::info!("auth logout: 既にフロー実行中のため click を無視");
        return false;
    };
    // 宛先ごとに何が起きるかを文面で正直に言い分ける（「全部消える」と「1 つ切れる」は別物）。
    let description = match target {
        "hub" => {
            "hub 向けの認証情報を削除します。hub federation は匿名接続に落ちます。よろしいですか？"
        }
        "creo" => {
            "creo-memories 向けの認証情報を削除します。ACTIONS の同期が止まります。よろしいですか？"
        }
        _ => {
            "保存済みの認証情報を**すべて**削除します。hub federation は匿名接続に落ち、ACTIONS の同期も止まります。よろしいですか？"
        }
    };
    let result = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Info)
        .set_title("Creo ID からログアウト")
        .set_description(description)
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show();
    if !matches!(result, rfd::MessageDialogResult::Ok) {
        tracing::info!("auth logout: ユーザーがキャンセル");
        return false;
    }
    let vp = locate_vp_binary();
    // 空文字 = 宛先指定なし = 全部捨てる（CLI の `--for` 省略と同じ意味）。
    let mut args: Vec<&str> = vec!["auth", "logout"];
    if !target.is_empty() {
        args.push("--for");
        args.push(target);
    }
    match Command::new(&vp).args(&args).status() {
        Ok(s) if s.success() => {
            tracing::info!("auth logout: 完了 (credentials 削除済み)");
            true
        }
        Ok(s) => {
            tracing::warn!("auth logout: `vp auth logout` が失敗 exit した: {}", s);
            false
        }
        Err(e) => {
            tracing::warn!("auth logout: `vp auth logout` の起動に失敗: {}", e);
            false
        }
    }
}

/// 子プロセスの終了を上限付きで待つ（超過で kill）。
///
/// `Some(success)` = 期限内に終了、`None` = timeout（kill 済み）。std に wait_timeout が
/// 無いため try_wait の poll loop（500ms 刻み — 人間の browser 操作待ちに十分な粒度）。
fn wait_with_timeout(child: &mut Child, limit: Duration) -> Option<bool> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            // try_wait 自体の失敗は稀（EBADF 等）— 成否不明のまま待ち続けるより失敗扱いで返す。
            Err(e) => {
                tracing::warn!("auth flow: 子プロセスの状態確認に失敗: {}", e);
                return Some(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flight_guard_blocks_second_acquire_and_releases_on_drop() {
        let first = FlightGuard::acquire();
        assert!(first.is_some());
        // 取得中は 2 本目が弾かれる（= ボタン連打で二重フローが走らない）。
        assert!(FlightGuard::acquire().is_none());
        drop(first);
        // drop で解放され、次のフローが取得できる。
        assert!(FlightGuard::acquire().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn wait_with_timeout_reports_exit_and_kills_on_deadline() {
        // 即終了（成功）を期限内に拾う。
        let mut ok = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        assert_eq!(
            wait_with_timeout(&mut ok, Duration::from_secs(5)),
            Some(true)
        );
        // 失敗 exit も success=false で返る。
        let mut ng = Command::new("sh").args(["-c", "exit 1"]).spawn().unwrap();
        assert_eq!(
            wait_with_timeout(&mut ng, Duration::from_secs(5)),
            Some(false)
        );
        // 期限超過は kill して None（ガードが永久に塞がらない）。
        let mut hang = Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap();
        assert_eq!(
            wait_with_timeout(&mut hang, Duration::from_millis(100)),
            None
        );
    }
}
