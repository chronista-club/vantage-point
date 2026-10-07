//! Codex 固有の session helper（thread id の検証 + CLI path 解決）。
//!
//! codex（OpenAI Codex CLI）の会話単位は **thread**（id は UUID）。会話 id の SSOT は
//! doc 40 で [`super::session_registry`]（`SessionEntry.conversation`）に統合され、per-lane
//! state file の store 役（record / last / clear）は doc 40 PR-2 で退役した（codex は RpcHost =
//! [`crate::conversation::codex_host`] が `session_registry::set_conversation` で registry 直結に記録する）。
//!
//! 本 module に残るのは codex 固有部だけ:
//! - [`is_valid_thread_id`]: `resume '<id>'` への injection 防壁（registry の write 側検証も使う）
//! - [`codex_cli_path`]: launchd の細い PATH 対策の CLI path 解決（`session_store::resolve_cli` 委譲）

use std::path::{Path, PathBuf};

/// thread id の正規形（英数 + ハイフン、非空 = UUID を包含）。
///
/// `resume '<id>'` の single-quote 埋め込みが shell injection にならないための防壁
/// （registry の write 側検証 `session_registry::is_valid_conversation` の codex arm も本関数を使う）。
pub(crate) fn is_valid_thread_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// codex の実行パスを解決する（launchd の細い PATH 対策、`session_store::resolve_cli` 委譲）。
///
/// brew cask（`/opt/homebrew/bin/codex`）が主経路。Windows は Codex デスクトップアプリ同梱の
/// CLI（[`codex_desktop_cli`]）— こちらは PATH に載らない。gui（[`crate::conversation::codex_host`]）の
/// turn spawn が使うため crate 内公開。
pub(crate) fn codex_cli_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut well_known = vec![
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from(format!("{home}/.local/bin/codex")),
    ];
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        well_known.extend(codex_desktop_cli(Path::new(&local_app_data)));
    }
    super::session_store::resolve_cli("codex", &well_known)
}

/// Codex デスクトップアプリ（Windows）が展開する `OpenAI/Codex/bin/<hash>/codex.exe`。
/// hash は版ごとに変わり、更新直後は旧版の dir も残るので最も新しい codex.exe を選ぶ。
fn codex_desktop_cli(local_app_data: &Path) -> Option<PathBuf> {
    std::fs::read_dir(local_app_data.join("OpenAI").join("Codex").join("bin"))
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path().join("codex.exe")))
        .filter_map(|exe| Some((exe.metadata().ok()?.modified().ok()?, exe)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, exe)| exe)
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

    /// Codex デスクトップ（Windows）は `bin/<hash>/codex.exe` に CLI を置き、hash は版ごとに変わる。
    /// codex.exe を持たない hash dir（rg.exe だけ等）は飛ばし、複数あれば新しい方を取る。
    #[test]
    fn desktop_cli_picks_newest_codex_exe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(codex_desktop_cli(tmp.path()), None, "未インストールは None");

        let bin = tmp.path().join("OpenAI").join("Codex").join("bin");
        let only_rg = bin.join("aaa");
        let old = bin.join("bbb");
        let new = bin.join("ccc");
        for dir in [&only_rg, &old, &new] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(only_rg.join("rg.exe"), "").unwrap();
        let now = std::time::SystemTime::now();
        for (dir, age) in [(&old, 3600), (&new, 0)] {
            let file = std::fs::File::create(dir.join("codex.exe")).unwrap();
            file.set_modified(now - std::time::Duration::from_secs(age))
                .unwrap();
        }

        assert_eq!(codex_desktop_cli(tmp.path()), Some(new.join("codex.exe")));
    }
}
