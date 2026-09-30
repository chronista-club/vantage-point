//! event handler: 音声入力（push-to-talk。録音の開始 / 停止 → 認識 → 入力欄へ返す）。
//!
//! webview の🎙は `voice:stop` を送った後「認識中」で待つ。**どの経路でも必ず
//! `voice:text` か `voice:error` を 1 つ返す**こと — 返さないと入力欄が編集不可のまま残る。

use tao::event_loop::EventLoopProxy;

use super::boot::Boot;
use super::state::{ActiveVoice, UiState};
use crate::events::AppEvent;
use crate::webview::push_main;

pub(super) fn start(ui: &mut UiState, boot: &Boot, lane: String, session: u32) {
    // 同じ欄からの二重の start（押し直し）は無視
    if ui
        .voice
        .as_ref()
        .is_some_and(|active| active.lane == lane && active.session == session)
    {
        return;
    }
    // 別の欄で押された = 前の録音はもう押されていない（ポインタは 1 つ）。pointerup の
    // 取りこぼし等で残った録音なので捨てる（drop で録音 thread が止まる）。前の欄には
    // 中断を返して🎙を戻す — 断る側に倒すと、残った録音が全部の欄を塞ぎ続ける。
    if let Some(stale) = ui.voice.take() {
        push_main::voice_error(
            &boot.webview,
            stale.lane,
            stale.session,
            "録音を中断しました".to_string(),
        );
    }
    ui.voice = Some(ActiveVoice {
        lane,
        session,
        recording: crate::voice::recorder::start(),
    });
}

pub(super) fn stop(
    ui: &mut UiState,
    boot: &Boot,
    proxy: &EventLoopProxy<AppEvent>,
    lane: String,
    session: u32,
) {
    let active = match ui.voice.take() {
        Some(active) if active.lane == lane && active.session == session => active,
        other => {
            // 別の欄の録音は続けたまま、この欄には「始まっていない」を返して🎙を戻す
            ui.voice = other;
            push_main::voice_error(
                &boot.webview,
                lane,
                session,
                "録音が始まっていません".to_string(),
            );
            return;
        }
    };
    let proxy = proxy.clone();
    boot.rt_handle.spawn_blocking(move || {
        let started = std::time::Instant::now();
        let result = crate::voice::finish(active.recording);
        tracing::info!(
            "voice: 認識 {} ms ({})",
            started.elapsed().as_millis(),
            if result.is_ok() { "ok" } else { "error" }
        );
        let _ = proxy.send_event(AppEvent::VoiceResult {
            lane: active.lane,
            session: active.session,
            result,
        });
    });
}

pub(super) fn result(boot: &Boot, lane: String, session: u32, result: Result<String, String>) {
    match result {
        Ok(text) => push_main::voice_text(&boot.webview, lane, session, text),
        Err(message) => {
            tracing::warn!("voice: {message}");
            push_main::voice_error(&boot.webview, lane, session, message)
        }
    }
}
