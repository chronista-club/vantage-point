//! LPD8 ピン（8 枠・色 8・`dimGain`）の **app 全体で 1 組**の正本（doc 81）。
//!
//! 同じ profile の GUI は 1 プロセス・複数 window（doc 75）。LPD8 は 1 台で pad に文字が無く、指は位置で
//! 覚えるので、どの window が前面でも同じ pad は同じ lane を指す必要がある。そこでピンは window の session
//! （`session.<N>.json`）でなく、state zone の `midi-pins.json` 1 つに置く。
//!
//! - 読み: 起動時に 1 度 [`MidiPins::load`]。file が無ければ旧 window 別の値から 1 組を採用して即書く
//!   （以後 session file は旧 field を出さないので、書かずに終わると移行元が消える）。
//! - 書き: [`MidiPins::set`] が file に書き、全 window へ配る印（[`MidiPins::take_broadcast`]）を立てる。
//!   配るのは全 window を持つ event loop の役目（`app::run`）。
//!
//! 中身の形（`{ slots, colors, dimGain }`）は webview が検証する。Rust が見るのは object であること（IPC では大きさも、`sidebar_ipc`）だけ。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session_state::SessionState;

/// state zone 内の file 名。window slot と無関係な app 全体の設定なので `session.*` と別にする。
const FILE_NAME: &str = "midi-pins.json";

/// app 全体のピン。1 プロセスに 1 つ（`AppResources` が持つ）。
#[derive(Debug)]
pub struct MidiPins {
    path: PathBuf,
    /// 保存値。未設定は `Null`（webview は既定値で表示する）。
    value: Value,
    /// `set` 後、まだ全 window に配っていない。
    broadcast_pending: bool,
}

impl MidiPins {
    /// `vp_state_dir()` から読む（= `session.json` と同じ場所）。
    pub fn load() -> Self {
        Self::load_in(&vp_paths::vp_state_dir())
    }

    /// `dir` から読む。file が無ければ旧 window 別の値を採用して書く。壊れた file は未設定扱い。
    pub fn load_in(dir: &Path) -> Self {
        let path = dir.join(FILE_NAME);
        let stored = std::fs::read_to_string(&path)
            .ok()
            .map(|text| serde_json::from_str::<Value>(&text));
        let value = match stored {
            Some(Ok(value)) if value.is_object() => value,
            Some(_) => {
                tracing::warn!("{} を読めないため未設定で起動します", path.display());
                Value::Null
            }
            None => match SessionState::legacy_midi_navigation_in(dir) {
                Some(legacy) => {
                    tracing::info!("window 別の MIDI ピンを {} へ移しました", path.display());
                    write_atomic(&path, &legacy);
                    legacy
                }
                None => Value::Null,
            },
        };
        Self {
            path,
            value,
            broadcast_pending: false,
        }
    }

    /// 今の保存値（webview へ配る形そのもの）。
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// 保存して、全 window へ配る印を立てる。
    pub fn set(&mut self, value: Value) {
        write_atomic(&self.path, &value);
        self.value = value;
        self.broadcast_pending = true;
    }

    /// 配る印が立っていれば下ろして値を返す（event loop が全 window の webview へ撃つ）。
    pub fn take_broadcast(&mut self) -> Option<&Value> {
        std::mem::take(&mut self.broadcast_pending).then_some(&self.value)
    }
}

/// tmp → rename の atomic write（`SessionState::save` と同じ規律）。失敗は warn で続行。
fn write_atomic(path: &Path, value: &Value) {
    let result = (|| -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if let Err(e) = result {
        tracing::warn!("MIDI ピンの保存に失敗 ({}): {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pins(color: &str) -> Value {
        json!({"slots":[{"path":"/vp","address":"vp/root"},null,null,null,null,null,null,null],
            "colors":[color,"#639af5","#b78aee","#f0b45a","#9be15d","#ff7f6e","#4fd18b","#e05fd0"],
            "dimGain":0.25})
    }

    /// 落ちるべき壊し方 = 保存先を window に戻す（window ごとに別の値が見える）。
    /// window B で保存した値を、window A も（後から開いた window も）同じ 1 つの file から読む。
    #[test]
    fn a_save_from_any_window_is_what_every_window_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = MidiPins::load_in(dir.path());
        assert!(app.value().is_null());

        // window B の sidebar / Editor からの保存
        app.set(pins("#123456"));
        assert_eq!(app.take_broadcast(), Some(&pins("#123456")));
        assert_eq!(app.take_broadcast(), None, "配る印は 1 度だけ");

        // 次回起動（どの window slot で開いても読む先は同じ）
        assert_eq!(MidiPins::load_in(dir.path()).value(), &pins("#123456"));
        // window の session file には載らない
        let window_a: SessionState =
            serde_json::from_value(json!({"midi_navigation": pins("#abcdef")})).unwrap();
        assert!(
            serde_json::to_value(window_a)
                .unwrap()
                .get("midi_navigation")
                .is_none(),
            "window の file にピンを書く経路は無い"
        );
    }

    /// 移行: 共通の保存が無ければ slot 0 を優先、無ければ index の小さい非 null。採用値は即 file に書く。
    #[test]
    fn migrates_once_from_slot_zero_first_then_the_first_saved_window() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: Value| {
            std::fs::write(dir.path().join(name), body.to_string()).unwrap()
        };
        write(
            "session.3.json",
            json!({"midi_navigation": pins("#333333")}),
        );
        write(
            "session.2.json",
            json!({"midi_navigation": pins("#222222")}),
        );
        write("session.json", json!({"midi_navigation": null}));
        write(
            "session.json.tmp",
            json!({"midi_navigation": pins("#999999")}),
        );
        assert_eq!(MidiPins::load_in(dir.path()).value(), &pins("#222222"));

        write("session.json", json!({"midi_navigation": pins("#000000")}));
        // 既に移した（file がある）ので、旧値が変わっても読み直さない
        assert_eq!(MidiPins::load_in(dir.path()).value(), &pins("#222222"));
        std::fs::remove_file(dir.path().join(FILE_NAME)).unwrap();
        assert_eq!(MidiPins::load_in(dir.path()).value(), &pins("#000000"));
    }

    #[test]
    fn nothing_saved_anywhere_is_unset_and_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("session.json"), "{}").unwrap();
        assert!(MidiPins::load_in(dir.path()).value().is_null());
        assert!(!dir.path().join(FILE_NAME).exists());
    }
}
