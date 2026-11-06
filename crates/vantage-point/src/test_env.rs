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

/// `XDG_STATE_HOME` 以外の env var を差し替える test を直列化する crate 唯一のロック。
///
/// var ごとに分けない（分けると「module ごとの static」と同じ落とし穴 = 名前が同じでも排他しない）。
/// 差し替える test は少ないので 1 本で足りる。
static ENV_VAR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 任意の env var を差し替えている間だけ生きる guard（sync test 用）。drop で元に戻す。
///
/// 例: `GIT_ALLOW_PROTOCOL` は allowlist なので、残すと以降の test の https / ssh を塞ぐ。
pub(crate) struct EnvVarGuard {
    key: &'static str,
    prev: Option<OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: ENV_VAR_LOCK 保持中（_lock は field 宣言順で後に drop）= この var を触るのは本 guard だけ。
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// sync test 用。`key` を `value` に差し替え、guard の drop で復元する。
pub(crate) fn set_var(key: &'static str, value: &str) -> EnvVarGuard {
    let lock = ENV_VAR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var_os(key);
    // SAFETY: ENV_VAR_LOCK 保持中 = この経路で env を触るのは本 guard だけ。
    unsafe { std::env::set_var(key, value) };
    EnvVarGuard {
        key,
        prev,
        _lock: lock,
    }
}
