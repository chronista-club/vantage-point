//! in-app update の適用フロー。
//!
//! sidebar footer の「更新する」ボタン（`update:apply` IPC）を Rust 側で受けて:
//!   1. macOS ネイティブ確認ダイアログ（rfd）を出す
//!   2. OK なら self-update を実行する
//!      - Homebrew cask 管理下なら `brew upgrade --cask vantage-point`（brew の帳簿を狂わせない。
//!        daemon の入れ替えも cask postflight の `vp daemon restart --if-running` が担う）
//!      - direct .dmg なら既存 self-update エンジン `vp update`
//!   3. 後半（Direct の daemon restart + 新 .app の relaunch）は detached helper（`sh -c`）に
//!      委譲し、GUI は即終了する
//!
//! ## 後半を helper に委譲する理由（v0.57 実機 2026-07-30）
//! 旧設計は daemon restart の完了を GUI 内スレッドで待ってから relaunch していた。しかし
//! Brew channel では cask postflight と二重の daemon restart で待ちが約 50 秒に伸び、その間に
//! 旧 GUI が終了（手動 quit 等）すると relaunch に到達しない =「更新したのにアプリが戻らない」。
//! 後半を GUI プロセスの生存に依存させないため、setsid で独立した helper が「旧 GUI の終了を
//! 待つ → (Direct のみ) daemon restart → open」を行う。旧 GUI の終了を待つのは、`open` が
//! 稼働中 instance を activate するだけで新規起動しないため。動きは `vp_log_dir()/
//! update-helper.log` で観測できる。
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
/// spawn 失敗で false に戻す（成功時は helper へ委譲後プロセスごと終了するので戻さなくてよい）。
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
/// `version` は検知済みの latest version（ダイアログ文言用）。`on_phase` は進行状態の通知
/// （true = 適用中 / false = キャンセル・失敗で通常に戻す）。sidebar の「更新中…」表示に使う。
pub fn spawn_update_flow(version: String, on_phase: impl Fn(bool) + Send + 'static) {
    // 二重起動ガード: 既にフローが走っていれば無視（CTA 連打 / ダイアログ表示中の再 click 対策）。
    if UPDATE_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        tracing::info!("in-app update: 既に更新フロー実行中のため click を無視");
        return;
    }
    let spawned = thread::Builder::new()
        .name("update-flow".into())
        .spawn(move || {
            // ダイアログ表示中も含めフロー全体を「適用中」として見せる
            // （= UPDATE_IN_FLIGHT ガードで click が無視される区間と一致させる）。
            on_phase(true);
            run_update_flow(version);
            // フローが return した = キャンセル / 失敗。再度更新可能にする
            // （成功時は helper へ委譲後プロセスごと終了するのでここには来ない）。
            on_phase(false);
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

    // 2. self-update（.app / binary の差し替え）。GUI 内で blocking 実行するのはここまで
    //    （失敗をユーザーに notify できる最後の地点でもある）。
    let (program, args) = self_update_command(channel, &vp);
    if !run_step("self-update", &program, &args) {
        notify_failure("更新のダウンロードに失敗しました");
        return;
    }

    // 3. 後半（Direct の daemon restart + relaunch）は detached helper に委譲して即終了。
    //    GUI 内で完了を待つ設計は GUI の寿命に人質に取られる（module doc 参照）。
    handoff_to_helper_and_exit(channel, &vp);
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

/// path を POSIX sh の single-quote で安全に囲む純粋関数（`'` は `'\''` に割る）。
fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

/// 親（旧 GUI）の終了待ちループの上限回数。0.2s × 150 = 30s。
/// 超えたら諦めて進む（`open` が activate に化けるだけで害はない）。
const PARENT_WAIT_MAX_TICKS: u32 = 150;

/// 更新後半を担う detached helper の sh script を組む純粋関数。
///
/// script は「親 (parent_pid = 旧 GUI) の終了を待つ → (Direct のみ) `vp daemon restart` →
/// `open <app>`」。channel / bundle 有無で不要な段を落とす:
/// - Brew: daemon restart は cask postflight（`vp daemon restart --if-running`）が実施済み
///   のため行わない（v0.57 実機で二重 restart がフローを約 50s に引き延ばした反省）
/// - app が None（dev binary / Linux 等 bundle 外）は relaunch を省略
/// - Brew + app None はやる仕事が無いので script 自体不要 = None
fn post_update_script(
    channel: UpdateChannel,
    parent_pid: u32,
    vp: &Path,
    app: Option<&Path>,
) -> Option<String> {
    let restart_daemon = channel == UpdateChannel::Direct;
    if !restart_daemon && app.is_none() {
        return None;
    }
    let mut script = format!(
        "echo \"[update-helper] start parent={parent_pid}\"\n\
         i=0\n\
         while kill -0 {parent_pid} 2>/dev/null; do\n\
         \x20 i=$((i+1))\n\
         \x20 [ \"$i\" -ge {PARENT_WAIT_MAX_TICKS} ] && break\n\
         \x20 sleep 0.2\n\
         done\n"
    );
    if restart_daemon {
        script.push_str(&format!("{} daemon restart\n", sh_quote(vp)));
    }
    if let Some(app) = app {
        script.push_str(&format!("open {}\n", sh_quote(app)));
    }
    script.push_str("echo \"[update-helper] done\"\n");
    Some(script)
}

/// 更新フロー後半を detached helper に委譲し、自身（旧 GUI）を終了する。
///
/// helper は setsid で完全独立させる（daemon_launcher の spawn と同型）ため、旧 GUI が
/// いつ・どう消えてもフローは完走する。出力は `vp_log_dir()/update-helper.log` に append。
#[cfg(unix)]
fn handoff_to_helper_and_exit(channel: UpdateChannel, vp: &Path) -> ! {
    match post_update_script(
        channel,
        std::process::id(),
        vp,
        current_app_bundle().as_deref(),
    ) {
        Some(script) => {
            let log_path = vp_paths::vp_log_dir().join("update-helper.log");
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path);
            // stderr も同じ log へ（vp / open の失敗理由こそ観測したい）。clone 失敗時のみ null。
            let (stdout, stderr) = match log {
                Ok(f) => {
                    let err = f
                        .try_clone()
                        .map(std::process::Stdio::from)
                        .unwrap_or_else(|_| std::process::Stdio::null());
                    (std::process::Stdio::from(f), err)
                }
                Err(_) => (std::process::Stdio::null(), std::process::Stdio::null()),
            };
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(&script)
                .env("PATH", vp_paths::spawn_env::augmented_spawn_path())
                .stdin(std::process::Stdio::null())
                .stdout(stdout)
                .stderr(stderr);
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    cmd.pre_exec(|| {
                        if libc::setsid() == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            match cmd.spawn() {
                Ok(child) => {
                    tracing::info!(
                        "in-app update: 後半を helper に委譲 (pid={}, log={})",
                        child.id(),
                        log_path.display()
                    );
                    drop(child); // wait しない → 独立稼働
                }
                Err(e) => tracing::warn!("in-app update: helper spawn 失敗: {}", e),
            }
        }
        None => {
            tracing::warn!("in-app update: .app bundle 不明 (dev binary?) — 後半 helper 省略");
        }
    }
    // 差し替え済みの新 binary で起動し直すため、旧 GUI プロセスを終了する。
    std::process::exit(0);
}

/// Windows: sh が無いため従来どおり GUI 内で daemon restart まで行い終了する
/// （relaunch は元々 `open` 依存 = macOS 専用だったので据え置き）。
#[cfg(windows)]
fn handoff_to_helper_and_exit(channel: UpdateChannel, vp: &Path) -> ! {
    if channel == UpdateChannel::Direct {
        run_step(
            "daemon-restart",
            vp,
            &["daemon".to_string(), "restart".to_string()],
        );
    }
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

    #[test]
    fn sh_quote_wraps_plain_path() {
        assert_eq!(
            sh_quote(Path::new("/Applications/VantagePoint.app")),
            "'/Applications/VantagePoint.app'"
        );
    }

    #[test]
    fn sh_quote_escapes_single_quote() {
        assert_eq!(sh_quote(Path::new("/tmp/o'brien")), r"'/tmp/o'\''brien'");
    }

    #[test]
    fn post_update_script_brew_skips_daemon_restart() {
        // Brew は cask postflight が daemon restart 済み → helper は open のみ。
        let s = post_update_script(
            UpdateChannel::Brew,
            1234,
            Path::new("/opt/homebrew/bin/vp"),
            Some(Path::new("/Applications/VantagePoint.app")),
        )
        .expect("Brew + app で script が組まれるべき");
        assert!(
            !s.contains("daemon restart"),
            "Brew に daemon restart は不要: {s}"
        );
        assert!(
            s.contains("open '/Applications/VantagePoint.app'"),
            "open が無い: {s}"
        );
        assert!(s.contains("kill -0 1234"), "親 PID 待ちが無い: {s}");
    }

    #[test]
    fn post_update_script_direct_restarts_daemon_before_open() {
        let s = post_update_script(
            UpdateChannel::Direct,
            1234,
            Path::new("/opt/homebrew/bin/vp"),
            Some(Path::new("/Applications/VantagePoint.app")),
        )
        .expect("Direct + app で script が組まれるべき");
        let restart = s
            .find("'/opt/homebrew/bin/vp' daemon restart")
            .expect("daemon restart が無い");
        let open = s.find("open '").expect("open が無い");
        assert!(
            restart < open,
            "daemon restart は open より前であるべき: {s}"
        );
    }

    #[test]
    fn post_update_script_direct_without_app_restarts_only() {
        // dev binary / Linux 等 bundle 外: relaunch は省略し daemon restart のみ。
        let s = post_update_script(
            UpdateChannel::Direct,
            1234,
            Path::new("/usr/local/bin/vp"),
            None,
        )
        .expect("Direct はapp 無しでも script が組まれるべき");
        assert!(s.contains("daemon restart"), "daemon restart が無い: {s}");
        assert!(!s.contains("open "), "app 不明なのに open がある: {s}");
    }

    #[test]
    fn post_update_script_brew_without_app_is_none() {
        // Brew + bundle 不明はやる仕事が無い → helper 自体不要。
        assert_eq!(
            post_update_script(
                UpdateChannel::Brew,
                1234,
                Path::new("/opt/homebrew/bin/vp"),
                None
            ),
            None
        );
    }

    #[test]
    fn post_update_script_parent_wait_has_upper_bound() {
        let s = post_update_script(
            UpdateChannel::Direct,
            9,
            Path::new("/usr/local/bin/vp"),
            None,
        )
        .unwrap();
        assert!(
            s.contains(&PARENT_WAIT_MAX_TICKS.to_string()),
            "親待ちループに上限が無い: {s}"
        );
    }
}
