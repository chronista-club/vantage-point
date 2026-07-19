//! Codex 固有の session helper（thread id の検証 + CLI path 解決）。
//!
//! codex（OpenAI Codex CLI）の会話単位は **thread**（id は UUID）。会話 id の SSOT は
//! doc 40 で [`super::session_registry`]（`SessionEntry.conversation`）に統合され、per-lane
//! state file の store 役（record / last / clear）は doc 40 PR-2 で退役した（codex は RpcHost =
//! [`crate::echoes::codex_host`] が `session_registry::set_conversation` で registry 直結に記録する）。
//!
//! 本 module に残るのは codex 固有部だけ:
//! - [`is_valid_thread_id`]: `resume '<id>'` への injection 防壁（registry の write 側検証も使う）
//! - [`codex_cli_path`]: launchd の細い PATH 対策の CLI path 解決（`session_store::resolve_cli` 委譲）

use std::path::PathBuf;

/// thread id の正規形（英数 + ハイフン、非空 = UUID を包含）。
///
/// `resume '<id>'` の single-quote 埋め込みが shell injection にならないための防壁
/// （registry の write 側検証 `session_registry::is_valid_conversation` の codex arm も本関数を使う）。
pub(crate) fn is_valid_thread_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// codex の実行パスを解決する（launchd の細い PATH 対策、`session_store::resolve_cli` 委譲）。
///
/// brew cask（`/opt/homebrew/bin/codex`）が主経路。Act II（[`crate::echoes::codex_host`]）の
/// turn spawn が使うため crate 内公開。
pub(crate) fn codex_cli_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    super::session_store::resolve_cli(
        "codex",
        &[
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from(format!("{home}/.local/bin/codex")),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// thread id 検証: UUID 形は通し、injection 形 / `_` / 空は拒否（cc_session と同規則）。
    /// `resume '<id>'` の single-quote 埋め込み防壁の核。
    #[test]
    fn thread_id_validation_is_uuid_shaped() {
        assert!(is_valid_thread_id("0196f9a2-1234-4abc-9def-0123456789ab"));
        assert!(!is_valid_thread_id(""), "空は不可");
        assert!(!is_valid_thread_id("has_underscore"), "_ は不可");
        assert!(!is_valid_thread_id("a'; rm -rf /"), "quote 破りは reject");
    }
}
