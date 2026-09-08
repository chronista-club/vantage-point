//! test 専用: process-global な env var を差し替える test の直列化 + RAII 復元。
//!
//! `SessionState::save()` は `vp_paths::vp_state_dir()` = `$XDG_STATE_HOME` を呼び出しの都度読む
//! （cache しない）。これを差し替える test が parallel runner で並ぶと、別 test の `set_var` /
//! `remove_var` が割り込んで書き先が入れ替わる（= intermittent failure）。Rust 2024 で `set_var`
//! が unsafe になった理由そのもの（`lane/title.rs` の test note も参照）。
//!
//! **ロックは env var ごとに crate 全体で 1 個**。module ごとに static を置くと実体が別になり
//! 排他しない。本 module がその唯一の置き場。server crate の `vantage_point::test_env` と同型。
//!
//! 使い方:
//! ```ignore
//! let state = crate::test_env::state_dir();   // guard の drop で env 復元 → lock 解放
//! ```

use std::ffi::OsString;

/// `XDG_STATE_HOME` を触る test を直列化する crate 唯一のロック。
static STATE_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `XDG_STATE_HOME` を tempdir に向けている間だけ生きる guard。
/// drop で env を元に戻し、その後ロックを解放する（順序は field 宣言順）。
pub(crate) struct StateDirGuard {
    tmp: tempfile::TempDir,
    prev: Option<OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl StateDirGuard {
    /// 差し替え先の state dir（= `vp_state_dir()` の親）。
    #[allow(dead_code)]
    pub(crate) fn path(&self) -> &std::path::Path {
        self.tmp.path()
    }
}

impl Drop for StateDirGuard {
    fn drop(&mut self) {
        // SAFETY: ロック保持中（guard の drop 中なので _lock はまだ生きている）= この env var を
        // 触るのは本 guard だけ。
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }
}

/// sync test 用。前の test が panic して poison していても続行する（env は drop で復元済）。
pub(crate) fn state_dir() -> StateDirGuard {
    let lock = STATE_DIR_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("XDG_STATE_HOME");
    // SAFETY: STATE_DIR_LOCK 保持中 = この env var を触るのは本 guard だけ。
    unsafe { std::env::set_var("XDG_STATE_HOME", tmp.path()) };
    StateDirGuard {
        tmp,
        prev,
        _lock: lock,
    }
}
