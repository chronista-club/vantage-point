//! test 専用: process-global な env var を差し替える test の直列化 + RAII 復元。
//!
//! `vp_state_dir()` は呼び出しの都度 `$XDG_STATE_HOME` を読む (cache しない) ため、 これを
//! 差し替える test が parallel runner で並ぶと、 別 test の `set_var` / `remove_var` が
//! 割り込んで読み書きの向き先が入れ替わる (= intermittent failure)。
//!
//! **ロックは env var ごとに crate 全体で 1 個でなければ意味がない**。 module ごとに
//! `static ENV_LOCK` を置くと、 Rust の static は module ごとに独立した実体なので、
//! 名前とコメントが同じでも互いに排他しない (実際に一度そう書いて flake を作った)。
//! 本 module がその唯一の置き場。
//!
//! 使い方:
//! ```ignore
//! let state = test_env::state_dir();          // sync test
//! let state = test_env::state_dir_async().await;  // async test (.await 越しに保持可)
//! // …test 本体… guard の drop で env が復元され、 lock が解放される
//! ```

use std::ffi::OsString;

/// `XDG_STATE_HOME` を触る test を直列化する crate 唯一のロック。
///
/// `tokio::sync::Mutex` なのは async test が `.await` 越しに guard を保持するため
/// (`clippy::await_holding_lock` を踏まない)。 sync test は `blocking_lock` で待つ。
static STATE_DIR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `XDG_STATE_HOME` を tempdir に向けている間だけ生きる guard。
///
/// drop で env を元に戻し、 その後ロックを解放する (順序は field 宣言順)。
pub(crate) struct StateDirGuard {
    tmp: tempfile::TempDir,
    prev: Option<OsString>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl StateDirGuard {
    /// 差し替え先の state dir (= `vp_state_dir()` の親)。
    pub(crate) fn path(&self) -> &std::path::Path {
        self.tmp.path()
    }

    fn enter(lock: tokio::sync::MutexGuard<'static, ()>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: STATE_DIR_LOCK 保持中 = この env var を触るのは本 guard だけ。
        unsafe { std::env::set_var("XDG_STATE_HOME", tmp.path()) };
        Self {
            tmp,
            prev,
            _lock: lock,
        }
    }
}

impl Drop for StateDirGuard {
    fn drop(&mut self) {
        // SAFETY: enter と同じくロック保持中 (guard の drop 中なので _lock はまだ生きている)。
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }
}

/// sync test 用。 tokio runtime の外から呼ぶこと (`blocking_lock` は runtime 内で panic)。
pub(crate) fn state_dir() -> StateDirGuard {
    StateDirGuard::enter(STATE_DIR_LOCK.blocking_lock())
}

/// async test 用。 同じ `STATE_DIR_LOCK` を待つので sync test とも直列化される。
pub(crate) async fn state_dir_async() -> StateDirGuard {
    StateDirGuard::enter(STATE_DIR_LOCK.lock().await)
}
