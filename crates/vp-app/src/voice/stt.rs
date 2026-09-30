//! 音声認識（whisper.cpp を whisper-rs 経由で vp-app の中で動かす）。
//!
//! モデルは初回の認識で読み込み、以後は使い回す（large-v3-turbo で読み込み約 2 秒、
//! 読み込み済みなら 2〜22 秒の発話 12 本で中央値 0.9 秒・最大 1.5 秒 — 2026-09-30 に
//! whisper-cli で実測。vp-app 内での値は未実測）。
//! 読み込んだモデル（file で約 1.6 GB）は app 終了まで持ち続ける。

use std::path::PathBuf;

/// 使うモデルの file 名（whisper.cpp 公式の ggml 形式）。
pub const MODEL_FILE: &str = "ggml-large-v3-turbo.bin";

/// モデルの置き場所: `~/.local/share/vp/models/`（dev profile は `vp-dev`）。
/// 最初の版はダウンロードの仕組みを持たないので、ユーザーが手で置く。
pub fn model_path() -> PathBuf {
    vp_paths::vp_data_dir().join("models").join(MODEL_FILE)
}

/// 読み込み済みのモデル。認識は 1 本ずつなので Mutex で直列にする。
#[cfg(target_os = "macos")]
static MODEL: std::sync::Mutex<Option<whisper_rs::WhisperContext>> = std::sync::Mutex::new(None);

/// process 終了時（`exit()`）にモデルを手放す。
///
/// ⚠️ whisper.cpp の Metal backend は、`exit()` の C++ 後片付けで「GPU 上の資源が全部
/// 解放済みか」を assert し、残っていれば abort する（`ggml-metal-device.m` の
/// `GGML_ASSERT([rsets->data count] == 0)`）。Rust の `static` は終了時に drop されないので、
/// 放っておくとモデルが資源を握ったまま assert に当たり、終了のたびに SIGABRT で落ちる
/// （2026-09-30、認識 3 本を流した test process で実際に踏み、この解放で exit 0 に戻った）。
/// `atexit` は登録の逆順に走るので、Metal の後片付け（モデル読み込み中に登録される）より
/// 後に登録すれば、それより先に走る。
#[cfg(target_os = "macos")]
extern "C" fn unload_at_exit() {
    // 認識の途中で終了した場合は lock が取れない — その時は諦める（待つと終了が止まる）
    if let Ok(mut guard) = MODEL.try_lock() {
        guard.take();
    }
}

/// 16 kHz mono の音を日本語として認識する。重い処理なので event loop では呼ばない。
#[cfg(target_os = "macos")]
pub fn transcribe(samples: &[f32]) -> Result<String, String> {
    use std::sync::Once;
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

    static LOG_HOOK: Once = Once::new();
    // whisper.cpp は既定で stderr に大量の log を出すので tracing に流す
    LOG_HOOK.call_once(whisper_rs::install_logging_hooks);

    let mut guard = MODEL
        .lock()
        .map_err(|_| "認識モデルの lock が壊れています")?;
    if guard.is_none() {
        let path = model_path();
        if !path.exists() {
            return Err(format!(
                "認識モデルがありません。{} を置いてください",
                path.display()
            ));
        }
        let path_str = path
            .to_str()
            .ok_or("モデルの path が UTF-8 ではありません")?;
        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .map_err(|e| format!("認識モデルを読み込めません: {e}"))?;
        *guard = Some(ctx);
        // 読み込みの**後**に登録する（理由は `unload_at_exit`）。読み込みは process で 1 回だけ
        static UNLOAD_HOOK: Once = Once::new();
        UNLOAD_HOOK.call_once(|| {
            // SAFETY: 引数も戻り値も無い extern "C" fn を渡すだけ
            if unsafe { libc::atexit(unload_at_exit) } != 0 {
                tracing::warn!("voice: 終了時のモデル解放を登録できませんでした");
            }
        });
    }
    let ctx = guard.as_ref().expect("直前で読み込み済み");

    let mut state = ctx
        .create_state()
        .map_err(|e| format!("認識の準備に失敗しました: {e}"))?;
    // whisper-cli の既定（beam 5）に揃える — 2026-09-30 の比較はこの設定で測った
    let mut params = FullParams::new(SamplingStrategy::BeamSearch {
        beam_size: 5,
        patience: -1.0,
    });
    params.set_language(Some("ja"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, samples)
        .map_err(|e| format!("認識に失敗しました: {e}"))?;
    let text: String = state.as_iter().map(|seg| seg.to_string()).collect();
    Ok(text.trim().to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn transcribe(samples: &[f32]) -> Result<String, String> {
    let _ = samples;
    Err("音声入力は macOS のみ対応です".to_string())
}
