//! CoreMIDI 通知 thread — daemon の process が「後から生えた MIDI port」を見られるようにする。
//!
//! ⚠️ CoreMIDI は process 内に endpoint 一覧の cache を持ち、その更新通知を
//! **process で最初に `MIDIClientCreate` した時点の current run loop** にだけ届ける。
//! daemon は tokio の worker thread で midir を呼ぶので run loop が回っておらず、
//! 一覧は最初の client 作成の瞬間で凍る。midistaged の仮想 port は lease のたびに
//! daemon 起動**後**に作られるので、凍った一覧からは毎回見えなかった（2026-10-14 実測、
//! `docs/design/midi-device-handoff.md`「daemon が仮想ポートを見られる条件」）。
//!
//! そこで daemon の MIDI 初期化の**最初**に専用 thread を 1 本立て、その thread で
//! process 最初の client を作って run loop を回し続ける。以後はどの thread の midir 列挙も
//! 最新の一覧を返す。
//!
//! - 順序が要（2 番目以降の client では効かない — 実測済み）。起動済みかを [`STARTED`] に持ち、
//!   daemon の MIDI 入口（devices）が [`warn_if_not_started`] で確かめる
//! - `vp midi …` CLI は対象外（daemon 経路だけ）
//! - macOS 以外は何もしない（run loop 依存は CoreMIDI 固有）

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// 通知 thread を起動済みか（起動の成否ではなく「呼んだか」— 失敗は起動時に warn 済み）。
static STARTED: OnceLock<()> = OnceLock::new();

/// 未起動の warn を 1 回に絞る（入口は列挙のたびに通るので、毎回出すと log が埋まる）。
static WARNED: AtomicBool = AtomicBool::new(false);

/// CoreMIDI 通知 thread を起動する（冪等）。**daemon の最初の midir 呼び出しより前に**呼ぶこと。
///
/// thread が client を作り終えるまで待ってから返す（返った時点で「最初の client」が確定している）。
pub fn start() {
    STARTED.get_or_init(spawn_notify_thread);
}

/// 起動済みか。
pub fn is_started() -> bool {
    STARTED.get().is_some()
}

/// daemon の MIDI 入口で呼ぶ。未起動なら 1 度だけ warn する。
///
/// panic にしないのは、test（`attach_fleet_inputs` を直接呼ぶ）と短命 CLI が同じ関数を
/// 通るため。warn が出たら daemon の起動順が崩れている（＝仮想 port が見えなくなる）。
pub fn warn_if_not_started(entry: &str) {
    if !is_started() && !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!(
            "🧲 CoreMIDI 通知 thread が未起動のまま MIDI に触れました（{entry}）— \
             後から生えた port（midistaged の仮想 port 等）が見えません。\
             coremidi_notify::start() を daemon の MIDI 初期化の先頭に置いてください"
        );
    }
}

#[cfg(target_os = "macos")]
fn spawn_notify_thread() {
    use std::time::Duration;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopRun();
    }

    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<bool>(1);
    let spawned = std::thread::Builder::new()
        .name("coremidi-notify".into())
        .spawn(move || {
            // この client が process 最初の MIDIClientCreate になり、通知の宛先 run loop が
            // この thread に決まる。drop すると宛先が消えるので thread の寿命いっぱい保持する。
            let client = midir::MidiInput::new("vp-coremidi-notify");
            let _ = ready_tx.send(client.is_ok());
            let Ok(_client) = client else { return };
            loop {
                // SAFETY: 引数なしの CoreFoundation 呼び出し。この thread の run loop を回すだけ。
                unsafe { CFRunLoopRun() };
                // source が一時的に空になると即 return しうる。busy loop を避けて回し直す。
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("🧲 CoreMIDI 通知 thread を起動できません: {e}");
        return;
    }
    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(true) => tracing::info!("🧲 CoreMIDI 通知 thread 起動（後から生えた port を追従）"),
        Ok(false) => tracing::warn!("🧲 CoreMIDI 通知 thread: MIDI client を作れません"),
        Err(_) => tracing::warn!("🧲 CoreMIDI 通知 thread: client 作成の完了を確認できません"),
    }
}

#[cfg(not(target_os = "macos"))]
fn spawn_notify_thread() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_is_idempotent() {
        start();
        start();
        assert!(is_started());
        // 起動後は入口の確認が静か（panic しない）
        warn_if_not_started("test");
    }
}
