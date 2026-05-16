//! VP の config / data パス解決 (VP-192)。
//!
//! `vantage_point::config` の `vp_config_dir()` / `vp_data_dir()` と **同一定義**。
//! vp-app は `vantage-point` crate (SurrealDB / wry / midir 等を含む重量 crate) に
//! 意図的に依存しない方針 (`client.rs` の lite struct 参照) のため、 2 つの軽量
//! ヘルパーだけをここに複製する。
//!
//! ## パス方針 (canonical: `vantage_point::config` モジュール doc)
//!
//! | 種別 | API | macOS | Linux | Windows |
//! |------|-----|-------|-------|---------|
//! | config | `vp_config_dir()` | `~/Library/Application Support/vp/` | `~/.config/vp/` | `%APPDATA%\vp\` |
//! | data   | `vp_data_dir()`   | `~/Library/Application Support/vp/` | `~/.local/share/vp/` | `%LOCALAPPDATA%\vp\` |
//!
//! 設定ファイル (vp-app.toml) は config、 セッション状態 (session-state.json) は
//! 生成データなので data 配下に置く。

use std::path::PathBuf;

/// VP の config ディレクトリ (OS 別)。
///
/// `dirs::config_dir()` に OS 判定を委ね、 末尾に `vp` を付ける。
/// `dirs` が None を返す環境では `$HOME/.config` を fallback に使う。
pub fn vp_config_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vp")
}

/// VP の data ディレクトリ (OS 別)。
///
/// `dirs::data_local_dir()` に OS 判定を委ね、 末尾に `vp` を付ける。
/// `dirs` が None を返す環境では `$HOME/.local/share` を fallback に使う。
pub fn vp_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vp")
}

/// 旧 config/data パスから新パスへの冪等なデータ移行 (VP-192)。
///
/// `vantage_point::config::migrate_legacy_paths` と同一ロジック。 vp-app は
/// `vantage-point` crate に依存しないため複製している。 daemon (vp-cli) 側と
/// vp-app の双方が起動時に呼ぶが、 **コピー先に既存データがあれば skip** する
/// 冪等設計なので二重実行しても安全。 旧データは残す (move ではなく copy)。
///
/// `run()` 冒頭で 1 回呼ぶこと。
pub fn migrate_legacy_paths() {
    if let Some(home) = dirs::home_dir() {
        let legacy_config = home.join(".config").join("vp");
        migrate_dir_if_needed(&legacy_config, &vp_config_dir(), "config");
    }
    if let Some(cfg) = dirs::config_dir() {
        let legacy_data = cfg.join("vantage");
        migrate_dir_if_needed(&legacy_data, &vp_data_dir(), "data");
    }
}

/// `legacy` ディレクトリの中身を `target` にコピーする (冪等ヘルパー)。
fn migrate_dir_if_needed(legacy: &std::path::Path, target: &std::path::Path, label: &str) {
    if legacy == target {
        return;
    }
    if !legacy.is_dir() {
        return;
    }
    if dir_has_entries(target) {
        return;
    }
    tracing::info!(
        "VP-192 path migration ({}): {} → {}",
        label,
        legacy.display(),
        target.display()
    );
    if let Err(e) = copy_dir_recursive(legacy, target) {
        tracing::warn!(
            "VP-192 path migration ({}) 失敗 ({} → {}): {} — 旧パスのまま継続",
            label,
            legacy.display(),
            target.display(),
            e
        );
    }
}

/// ディレクトリが存在し、 かつ 1 つ以上のエントリを持つか。
fn dir_has_entries(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// `src` の中身を `dst` に再帰コピーする。 `dst` は無ければ作成。
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vp_config_dir_ends_with_vp() {
        assert!(
            vp_config_dir().ends_with("vp"),
            "vp_config_dir は 'vp' で終わるべき"
        );
    }

    #[test]
    fn test_vp_data_dir_ends_with_vp() {
        assert!(
            vp_data_dir().ends_with("vp"),
            "vp_data_dir は 'vp' で終わるべき"
        );
    }

    #[test]
    fn test_migrate_dir_if_needed_idempotent() {
        // VP-192: 新パスに既存データがあれば旧データで上書きしない (冪等)
        let tmp = std::env::temp_dir().join(format!("vp192app_mig_{}", std::process::id()));
        let legacy = tmp.join("legacy");
        let target = tmp.join("target");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("data.txt"), "OLD").unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("data.txt"), "NEW").unwrap();

        migrate_dir_if_needed(&legacy, &target, "test");
        migrate_dir_if_needed(&legacy, &target, "test");
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "NEW",
            "既存データがあれば移行 skip (冪等)"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_migrate_dir_if_needed_copies_to_empty_target() {
        // VP-192: 新パスが空なら旧データをコピーし、旧データは残す
        let tmp = std::env::temp_dir().join(format!("vp192app_migc_{}", std::process::id()));
        let legacy = tmp.join("legacy");
        let target = tmp.join("target");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("data.txt"), "OLD").unwrap();

        migrate_dir_if_needed(&legacy, &target, "test");
        assert_eq!(
            std::fs::read_to_string(target.join("data.txt")).unwrap(),
            "OLD"
        );
        assert!(legacy.join("data.txt").exists(), "旧データは残る (copy)");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
