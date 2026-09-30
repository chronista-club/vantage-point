//! マイク録音（cpal = CoreAudio）。既定の入力デバイスから、機材の形式のまま溜める。
//!
//! 録音は専用の thread に閉じ込める。デバイスを開く処理（初回はマイクの許可ダイアログが
//! 絡む — どれだけ止まるかは未実測）を event loop から外すため。外（event loop）が持つのは
//! 止める合図と結果の受け口だけ。

use std::sync::mpsc;

/// 録音で溜まったもの（機材の形式のまま。16 kHz への変換は [`super::audio`]）。
pub struct Captured {
    pub samples: Vec<f32>,
    pub channels: u16,
    pub rate: u32,
}

/// 録音中の 1 本。[`Recording::stop`] で止めて中身を受け取る。
pub struct Recording {
    stop_tx: mpsc::Sender<()>,
    done_rx: mpsc::Receiver<Result<Captured, String>>,
}

impl Recording {
    /// 録音を止めて、溜まった音を返す。stream の後始末を待つので event loop では呼ばない
    /// （`spawn_blocking` の中で呼ぶ）。
    pub fn stop(self) -> Result<Captured, String> {
        let _ = self.stop_tx.send(());
        self.done_rx
            .recv()
            .map_err(|_| "録音 thread が応答しませんでした".to_string())?
    }
}

/// 既定の入力デバイスで録音を始める。すぐ返る（デバイスを開くのは録音 thread 側）。
///
/// デバイスを開けなかった場合の error は [`Recording::stop`] で返る — 開くのを待つと、
/// 初回のマイク許可ダイアログの間 event loop が止まる恐れがあるため。
pub fn start() -> Recording {
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("vp-voice-recorder".into())
        .spawn(move || {
            let result = record_until_stopped(&stop_rx);
            let _ = done_tx.send(result);
        })
        .expect("録音 thread の起動");
    Recording { stop_tx, done_rx }
}

#[cfg(target_os = "macos")]
fn record_until_stopped(stop_rx: &mpsc::Receiver<()>) -> Result<Captured, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::{Arc, Mutex};

    let device = cpal::default_host()
        .default_input_device()
        .ok_or("入力デバイスが見つかりません（マイクが接続されていない）")?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("マイクの形式を取得できません: {e}"))?;
    let channels = config.channels();
    let rate = config.sample_rate();
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let on_error = |e| tracing::warn!("voice: 録音 stream の error: {e}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let buf = Arc::clone(&buf);
            device.build_input_stream(
                config.into(),
                move |data: &[f32], _: &_| buf.lock().unwrap().extend_from_slice(data),
                on_error,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let buf = Arc::clone(&buf);
            device.build_input_stream(
                config.into(),
                move |data: &[i16], _: &_| {
                    buf.lock()
                        .unwrap()
                        .extend(data.iter().map(|s| f32::from(*s) / 32768.0))
                },
                on_error,
                None,
            )
        }
        other => return Err(format!("未対応のマイク形式です: {other:?}")),
    }
    .map_err(|e| format!("マイクを開けません: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("録音を開始できません: {e}"))?;

    // 止める合図（または送り手の消滅）まで待つ
    let _ = stop_rx.recv();
    drop(stream);
    let samples = std::mem::take(&mut *buf.lock().unwrap());
    Ok(Captured {
        samples,
        channels,
        rate,
    })
}

#[cfg(not(target_os = "macos"))]
fn record_until_stopped(stop_rx: &mpsc::Receiver<()>) -> Result<Captured, String> {
    let _ = stop_rx.recv();
    Err("音声入力は macOS のみ対応です".to_string())
}
