//! spawn する子プロセス用の PATH 補強 + ロケール解決。
//!
//! SSOT は `vp_paths::spawn_env` に一本化した (vantage-point + vp-app 共有、 drift 源だった
//! vp-app 側レプリカを解消)。 本 module は既存 caller (`crate::spawn_env::augment_path` 等) の
//! 互換のため re-export のみ。 実装・doc・test は `vp_paths::spawn_env` が canonical。

pub use vp_paths::spawn_env::{
    augment_path, augment_path_env, augmented_spawn_path, resolve_utf8_locale, utf8_locale,
};
