//! in-app update の適用フロー。
//!
//! sidebar footer の「更新する」ボタン（`update:apply` IPC）を Rust 側で受けて:
//!   1. macOS ネイティブ確認ダイアログ（rfd）を出す
//!   2. OK なら self-update を実行する
//!      - Homebrew cask 管理下なら `brew upgrade --cask vantage-point`（brew の帳簿を狂わせない）
//!      - direct .dmg なら既存 self-update エンジン `vp update`
//!   3. `vp daemon restart`（ownership-agnostic、#763）で daemon を新 binary に入れ替える
//!   4. 新しい .app を relaunch して自身を終了する
//!
//! ## 実行スレッド
//! rfd の同期ダイアログと外部プロセスは blocking なので、`repo_dialog.rs` と同じく
//! 専用スレッドで回す（event loop = main thread を塞がない）。rfd の macOS 同期ダイアログは
//! 別スレッドから呼んでも内部で main queue に marshal されるため、main の event loop が
//! 回っていれば動く（`spawn_add_repo_picker` と同型）。
//!
//! ## 安全性
//! 本フローは実 daemon / .app を差し替え、daemon restart で全 repo を rolling させる
//! 破壊的操作。実機での実行はユーザーの明示的なボタン click ＋ 確認ダイアログ OK が gate で、
//! それ以外では一切走らない。純粋な判定・コマンド構築・文言は unit test で、実 apply（brew
//! upgrade / daemon restart / relaunch）は log で観測する。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crate::daemon_launcher::locate_vp_binary;

/// 更新フローが実行中かのガード。「更新する」CTA の連打で破壊的フローが二重に
/// 走るのを防ぐ（rfd ダイアログ表示中の追加 click 対策）。フロー完了 / キャンセル /
/// spawn 失敗で false に戻す（relaunch 成功時はプロセスごと終了するので戻さなくてよい）。
static UPDATE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// self-update の配送チャネル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    /// Homebrew cask 管理下。`brew upgrade --cask` に委譲して brew の帳簿と整合させる。
    Brew,
    /// direct .dmg 配布。既存の self-update エンジン `vp update` を使う。
    Direct,
}

/// 現在の環境から update チャネルを判定する。
///
/// 判定基準は spec（mem_1Cd1DDx3M7hRoCcfBoqCru）どおり Homebrew Caskroom の痕跡有無。
/// macOS 以外は cask が存在しないので常に Direct。
pub fn detect_channel() -> UpdateChannel {
    // Homebrew cask は macOS(Apple Silicon prefix) のみ。他 OS では痕跡の無い path を
    // 渡して常に Direct に落とす（detect_channel_at を全 platform で「使用」させる意図も兼ねる）。
    #[cfg(target_os = "macos")]
    let caskroom = Path::new("/opt/homebrew/Caskroom/vantage-point");
    #[cfg(not(target_os = "macos"))]
    let caskroom = Path::new("");
    detect_channel_at(caskroom)
}

/// `caskroom` path の存在で brew / direct を判定する純粋関数。
fn detect_channel_at(caskroom: &Path) -> UpdateChannel {
    if caskroom.exists() {
        UpdateChannel::Brew
    } else {
        UpdateChannel::Direct
    }
}

/// チャネルごとの self-update コマンド `(program, args)` を構築する。
///
/// - Brew: `brew upgrade --cask vantage-point`（PATH 解決）
/// - Direct: `<vp> update`（既存 self-update エンジン）
fn self_update_command(channel: UpdateChannel, vp_binary: &Path) -> (PathBuf, Vec<String>) {
    match channel {
        UpdateChannel::Brew => (
            PathBuf::from("brew"),
            vec![
                "upgrade".to_string(),
                "--cask".to_string(),
                "vantage-point".to_string(),
            ],
        ),
        UpdateChannel::Direct => (vp_binary.to_path_buf(), vec!["update".to_string()]),
    }
}

/// 確認ダイアログの本文を作る純粋関数。
fn confirm_message(version: &str) -> String {
    format!(
        "Vantage Point を v{version} に更新します。\n\n\
         アプリと常駐 daemon を新しいバージョンに入れ替えて再起動します。\n\
         進行中のセッションは自動的に復帰します。"
    )
}

/// 現在の .app bundle path を `current_exe` から遡って求める。
fn current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    app_bundle_of(&exe)
}

/// `exe` path から `.app` bundle root を遡って探す純粋関数。
///
/// macOS の bundle 構造 `Foo.app/Contents/MacOS/<exe>` を前提に、拡張子 `.app` の
/// 祖先ディレクトリを返す。bundle 外（dev binary 等）では None。
fn app_bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(|p| p.to_path_buf())
}

/// 「更新する」ボタン → 確認 → 適用フローを専用スレッドで起動する。
///
/// `version` は検知済みの latest version（ダイアログ文言用）。
pub fn spawn_update_flow(version: String) {
    // 二重起動ガード: 既にフローが走っていれば無視（CTA 連打 / ダイアログ表示中の再 click 対策）。
    if UPDATE_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        tracing::info!("in-app update: 既に更新フロー実行中のため click を無視");
        return;
    }
    let spawned = thread::Builder::new()
        .name("update-flow".into())
        .spawn(move || {
            run_update_flow(version);
            // フローが return した = キャンセル / 失敗。再度更新可能にする
            // （成功時は relaunch_and_exit がプロセスごと終了するのでここには来ない）。
            UPDATE_IN_FLIGHT.store(false, Ordering::SeqCst);
        });
    if let Err(e) = spawned {
        UPDATE_IN_FLIGHT.store(false, Ordering::SeqCst);
        tracing::warn!("in-app update: flow スレッド起動失敗: {}", e);
    }
}

/// 適用フロー本体（専用スレッドで実行）。
fn run_update_flow(version: String) {
    // 1. 確認ダイアログ（キャンセルなら何もしない）。
    let result = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Info)
        .set_title("Vantage Point を更新")
        .set_description(confirm_message(&version))
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show();
    match result {
        rfd::MessageDialogResult::Ok => {}
        _ => {
            tracing::info!("in-app update: ユーザーがキャンセル (version={})", version);
            return;
        }
    }

    let vp = locate_vp_binary();
    let channel = detect_channel();
    tracing::info!(
        "in-app update: 適用開始 version={} channel={:?} vp={}",
        version,
        channel,
        vp.display()
    );

    // 2. self-update（.app / binary の差し替え）。
    let (program, args) = self_update_command(channel, &vp);
    if !run_step("self-update", &program, &args) {
        notify_failure("更新のダウンロードに失敗しました");
        return;
    }

    // 3. daemon restart（ownership-agnostic、#763）。実 port holder ベースで
    //    daemon を新 binary に入れ替える。repo は rolling、lane は --resume で復帰。
    if !run_step(
        "daemon-restart",
        &vp,
        &["daemon".to_string(), "restart".to_string()],
    ) {
        notify_failure("daemon の再起動に失敗しました");
        return;
    }

    // 4. 新 GUI を relaunch して自身を終了する。
    relaunch_and_exit();
}

/// 外部コマンドを 1 ステップ実行し、成否を返す（log 付き）。
fn run_step(label: &str, program: &Path, args: &[String]) -> bool {
    tracing::info!(
        "in-app update step [{}]: {} {:?}",
        label,
        program.display(),
        args
    );
    // GUI (.app) を Finder / Dock / launchd 経由で起動するとプロセスの PATH が最小集合
    // (/usr/bin:/bin:...) になり、brew (/opt/homebrew/bin) 等の user-installed tool を
    // 見つけられず spawn が失敗する (#498/#501)。daemon_launcher.rs の spawn 同様、
    // augmented PATH を注入して brew / vp を確実に解決する。
    match Command::new(program)
        .args(args)
        .env("PATH", vp_paths::spawn_env::augmented_spawn_path())
        .status()
    {
        Ok(s) if s.success() => {
            tracing::info!("in-app update step [{}]: 成功", label);
            true
        }
        Ok(s) => {
            tracing::warn!("in-app update step [{}]: 失敗 exit={:?}", label, s.code());
            false
        }
        Err(e) => {
            tracing::warn!("in-app update step [{}]: spawn 失敗: {}", label, e);
            false
        }
    }
}

/// 更新失敗を native 通知でユーザーに知らせる。
fn notify_failure(msg: &str) {
    if let Err(e) = notify_rust::Notification::new()
        .summary("Vantage Point 更新")
        .body(msg)
        .show()
    {
        tracing::warn!("in-app update: 失敗通知の表示に失敗: {}", e);
    }
}

/// 新しい .app を detached で起動し、自身を終了する。
///
/// `update_capability::restart_app` と同型: `open <app>` を親から切り離して起動し、
/// 現プロセスを終了する。bundle path が取れない（dev binary 等）場合は relaunch を
/// 諦めて終了のみ（次回は手動起動）。
fn relaunch_and_exit() -> ! {
    match current_app_bundle() {
        Some(app) => {
            tracing::info!("in-app update: relaunch → {}", app.display());
            // 現プロセス終了と競合しないよう detached で open。
            if let Err(e) = Command::new("open").arg(&app).spawn() {
                tracing::warn!("in-app update: relaunch spawn 失敗: {}", e);
            }
        }
        None => {
            tracing::warn!("in-app update: .app bundle 不明 (dev binary?) — relaunch 省略");
        }
    }
    // 差し替え済みの新 binary で起動し直すため、旧 GUI プロセスを終了する。
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_channel_brew_when_caskroom_exists() {
        // 実在する dir（temp）を caskroom に見立てる → Brew。
        let dir = std::env::temp_dir();
        assert_eq!(detect_channel_at(&dir), UpdateChannel::Brew);
    }

    #[test]
    fn detect_channel_direct_when_caskroom_absent() {
        let missing = std::env::temp_dir().join("vp-nonexistent-caskroom-должно-отсутствовать");
        assert_eq!(detect_channel_at(&missing), UpdateChannel::Direct);
    }

    #[test]
    fn self_update_command_brew_is_cask_upgrade() {
        let (p, args) = self_update_command(UpdateChannel::Brew, Path::new("/usr/local/bin/vp"));
        assert_eq!(p, PathBuf::from("brew"));
        assert_eq!(args, vec!["upgrade", "--cask", "vantage-point"]);
    }

    #[test]
    fn self_update_command_direct_uses_vp_binary() {
        let vp = Path::new("/opt/homebrew/bin/vp");
        let (p, args) = self_update_command(UpdateChannel::Direct, vp);
        assert_eq!(p, vp.to_path_buf());
        assert_eq!(args, vec!["update"]);
    }

    #[test]
    fn app_bundle_of_walks_to_dot_app() {
        let exe = Path::new("/Applications/VantagePoint.app/Contents/MacOS/vp-app");
        assert_eq!(
            app_bundle_of(exe),
            Some(PathBuf::from("/Applications/VantagePoint.app"))
        );
    }

    #[test]
    fn app_bundle_of_none_outside_bundle() {
        let exe = Path::new("/Users/x/.cargo/bin/vp-app");
        assert_eq!(app_bundle_of(exe), None);
    }

    #[test]
    fn confirm_message_mentions_version() {
        let m = confirm_message("0.47.0");
        assert!(m.contains("0.47.0"), "ダイアログ文言に version が無い: {m}");
    }
}
