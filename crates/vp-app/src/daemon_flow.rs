//! 設定ページの「daemon を再起動」フロー（doc 59 P1）。
//!
//! 1. macOS ネイティブ確認ダイアログ（rfd）で**何が落ちて何が戻るか**を出す
//! 2. OK なら `vp daemon restart` を spawn する
//!
//! ## なぜ確認が要るか
//!
//! doc 44 P1 fold-in 以降、repo は daemon プロセス内の `Arc<AppState>` なので、
//! **daemon を止めると全 repo = 全 lane の claude が一緒に落ちる**。旧「gentle（daemon だけ
//! 止めて repo は温存）」は repo が別プロセスだった時代の挙動で、今は成立しない。
//! GUI の押しやすい場所に置く以上、押す前に代償が見えている必要がある。
//!
//! 会話自体は `cc_session` の `--resume` で次回 spawn 時に継がれる（「プロセスは死ぬが
//! コンテキストは蘇る」）ので、ダイアログではその点も併せて伝える。
//!
//! ## 実行スレッド
//!
//! rfd の同期ダイアログと `Command` は blocking なので async context から直接呼ばない
//! （`update_flow.rs` / `repo_dialog.rs` と同じ理由 = event loop = main thread を塞がない）。
//!
//! ⚠️ `update_flow` と違い **GUI を relaunch しない**。GUI は daemon の再接続を待つだけで
//! 生き残る（daemon は落として上げるが、vp-app 自身は無関係）。

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crate::daemon_launcher::locate_vp_binary;

/// 再起動フローが実行中かのガード。ボタン連打で `vp daemon restart` が二重に走るのを防ぐ
/// （rfd ダイアログ表示中の追加 click 対策。`update_flow` の `UPDATE_IN_FLIGHT` と同型）。
static RESTART_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// 確認ダイアログの文面。**代償（全 lane が落ちる）を先に、回復（会話は戻る）を後に**置く
/// — 順序を逆にすると「戻るなら平気」と読み飛ばされる。
fn confirm_message() -> String {
    "daemon を再起動すると、すべての lane のプロセス（claude 等）が一緒に落ちます。\n\n\
     会話は失われません — 次に lane を開いたときに前回の続きから復帰します。\n\n\
     再起動しますか？"
        .to_string()
}

/// 確認ダイアログ → `vp daemon restart` を専用スレッドで実行する。
///
/// 二重起動中なら何もしない（`false` を返す）。呼び手は event loop。
pub fn spawn_daemon_restart() -> bool {
    if RESTART_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        tracing::info!("daemon restart: 既に実行中なので無視");
        return false;
    }
    let spawned = thread::Builder::new()
        .name("daemon-restart".into())
        .spawn(run_restart_flow);
    if spawned.is_err() {
        // スレッドすら立たなかった場合はガードを戻す（次の click を殺さない）。
        RESTART_IN_FLIGHT.store(false, Ordering::SeqCst);
        tracing::warn!("daemon restart: スレッド起動に失敗");
        return false;
    }
    true
}

/// フロー本体（専用スレッドで実行）。
fn run_restart_flow() {
    let confirmed = matches!(
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("daemon を再起動")
            .set_description(confirm_message())
            .set_buttons(rfd::MessageButtons::OkCancel)
            .show(),
        rfd::MessageDialogResult::Ok
    );
    if !confirmed {
        tracing::info!("daemon restart: ユーザーがキャンセル");
        RESTART_IN_FLIGHT.store(false, Ordering::SeqCst);
        return;
    }

    let vp = locate_vp_binary();
    tracing::info!("daemon restart: 実行 vp={}", vp.display());
    // `vp daemon restart` は ownership-agnostic（実 port holder を停止 → LaunchAgent 優先で
    // 起動）なので、brew の LaunchAgent 常駐でも dev 起動でも同じ 1 コマンドで足りる。
    match Command::new(&vp).args(["daemon", "restart"]).status() {
        Ok(status) if status.success() => {
            tracing::info!("daemon restart: 完了");
        }
        Ok(status) => {
            tracing::warn!("daemon restart: 非ゼロ終了 status={status}");
        }
        Err(e) => {
            tracing::warn!("daemon restart: spawn 失敗 vp={} err={e}", vp.display());
        }
    }
    RESTART_IN_FLIGHT.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_message_states_cost_before_recovery() {
        // ⚠️ 順序が意味を持つ（代償 → 回復）。逆になると「戻るなら平気」と読み飛ばされる。
        let m = confirm_message();
        let cost = m.find("落ちます").expect("代償の記述が要る");
        let recovery = m.find("失われません").expect("回復の記述が要る");
        assert!(cost < recovery, "代償を先に書くこと: {m}");
    }

    #[test]
    fn confirm_message_mentions_lane_processes() {
        // 「daemon を再起動」だけでは何が落ちるか伝わらない（doc 44 P1 fold-in の意味論）。
        assert!(confirm_message().contains("lane"));
    }
}
