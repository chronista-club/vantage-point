//! 音声入力（push-to-talk → whisper.cpp で認識）。
//!
//! 流れ: webview の🎙ボタン → IPC `voice:start` / `voice:stop` → [`recorder`] がマイクから
//! 溜める → [`audio`] で 16 kHz mono に直す → [`stt`] で認識 → push `voice:text` で
//! 押した session の入力欄に入れる（送信はユーザーが Enter で行う）。
//!
//! 認識は Mac の中で完結する（外部 API に音声を送らない）。

pub mod audio;
pub mod recorder;
pub mod stt;

/// 録音を止めて認識する（重い — `spawn_blocking` の中で呼ぶ）。
pub fn finish(recording: recorder::Recording) -> Result<String, String> {
    let captured = recording.stop()?;
    let samples = audio::to_whisper_input(&captured.samples, captured.channels, captured.rate);
    if audio::is_silent(&samples) {
        return Err(
            "マイクの音が入っていません。システム設定 → プライバシーとセキュリティ → \
                    マイク で VantagePoint を許可してください"
                .to_string(),
        );
    }
    stt::transcribe(&samples)
}
