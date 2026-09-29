//! マイクの生 sample を whisper の入力形（16 kHz mono f32）に直す（純粋関数）。
//!
//! マイクの形式は機材で違う（2026-09-30 の開発機で、内蔵と AirPods が 48 kHz・1 ch、
//! audio interface の Zenith 2 が 96 kHz・4 ch — `system_profiler SPAudioDataType`）。
//! whisper は 16 kHz mono しか受けないので、録音側は機材の形式のまま溜めて、認識の直前に
//! ここで一度だけ変換する。

/// whisper.cpp が要求する sample rate。
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// interleaved な `channels` ch・`rate` Hz の sample を、16 kHz mono に変換する。
///
/// - 多 ch は平均して mono にする（ch ごとの音量差は気にしない — 声が入っていれば足りる）
/// - resample は線形補間。認識用途では十分で、依存を増やさない
pub fn to_whisper_input(samples: &[f32], channels: u16, rate: u32) -> Vec<f32> {
    if samples.is_empty() || channels == 0 || rate == 0 {
        return Vec::new();
    }
    let mono: Vec<f32> = samples
        .chunks(usize::from(channels))
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    if rate == WHISPER_SAMPLE_RATE {
        return mono;
    }
    // 出力の i 番目 = 入力上の位置 i * rate / 16000 を、前後 2 点の線形補間で取る
    let step = f64::from(rate) / f64::from(WHISPER_SAMPLE_RATE);
    let out_len = (mono.len() as f64 / step).round() as usize;
    let last = mono.len() - 1;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * step;
            let idx = (pos.floor() as usize).min(last);
            let frac = (pos - idx as f64) as f32;
            mono[idx] * (1.0 - frac) + mono[(idx + 1).min(last)] * frac
        })
        .collect()
}

/// 録れた音が「無音」か（ピークが -80 dBFS 未満）。
///
/// マイクの許可が無いと、error ではなく無音が届くことがある（2026-09-27 に届いたほぼ無音の
/// 録音はこれを疑ったが、原因は確定していない）。そのまま認識に回すと「何も言っていない」と
/// 区別できないので、ここで弾いて許可の確認を促す。閾値の -80 dBFS は、その録音（ピーク
/// -100〜-102 dBFS）と、話し声（本人のボイスメモでピーク -2〜-6 dBFS）の間に置いた。
/// 暗騒音の実測はまだ無いので、誤って弾く録音が出たら下げる。
pub fn is_silent(samples: &[f32]) -> bool {
    /// -80 dBFS（= 10^(-80/20)）
    const SILENCE_PEAK: f32 = 1e-4;
    samples.iter().all(|s| s.abs() < SILENCE_PEAK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zero_and_empty_are_silent() {
        assert!(is_silent(&[]));
        assert!(is_silent(&[0.0; 48_000]));
        // -100 dBFS 程度（2026-09-27 に届いたほぼ無音の録音の水準）
        assert!(is_silent(&[1e-5, -1e-5, 0.0]));
    }

    #[test]
    fn room_noise_and_speech_are_not_silent() {
        // -60 dBFS 程度の暗騒音
        assert!(!is_silent(&[0.001, -0.001, 0.0]));
        // 話し声（-6 dBFS 程度）。負側のピークも拾う
        assert!(!is_silent(&[0.0, -0.5, 0.1]));
    }

    #[test]
    fn mono_16k_passes_through_unchanged() {
        let input = vec![0.0, 0.25, -0.5, 1.0];
        assert_eq!(to_whisper_input(&input, 1, 16_000), input);
    }

    #[test]
    fn stereo_is_averaged_to_mono() {
        // L/R の組が 1 frame。平均が mono の値になる
        let input = vec![1.0, 0.0, -1.0, -1.0, 0.5, 0.25];
        assert_eq!(to_whisper_input(&input, 2, 16_000), vec![0.5, -1.0, 0.375]);
    }

    #[test]
    fn downsampling_48k_keeps_duration() {
        // 48 kHz で 1 秒 → 16 kHz で 1 秒（= 16000 sample）
        let input = vec![0.1; 48_000];
        let out = to_whisper_input(&input, 1, 48_000);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|s| (s - 0.1).abs() < 1e-6));
    }

    #[test]
    fn upsampling_interpolates_between_samples() {
        // 8 kHz → 16 kHz: 元の sample の間に中間値が入る
        let out = to_whisper_input(&[0.0, 1.0, 0.0], 1, 8_000);
        assert_eq!(out.len(), 6);
        assert!((out[1] - 0.5).abs() < 1e-6, "0 と 1 の中間: {out:?}");
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_or_degenerate_input_yields_empty() {
        assert!(to_whisper_input(&[], 1, 48_000).is_empty());
        assert!(to_whisper_input(&[0.1, 0.2], 0, 48_000).is_empty());
        assert!(to_whisper_input(&[0.1, 0.2], 1, 0).is_empty());
    }
}
