//! VP install root の runtime 解決 (doc 11 PR-D / Z 系統)
//!
//! ## 役割
//!
//! `.mise/tasks/vp/stand/{name}` task ファイルが居る dir を runtime で resolve する。
//! `mise run vp:stand:hd` 等を spawn する時の cwd 引数に使う ─ mise の filesystem
//! cascade (cwd → / の上方向 lookup) で task が必ず見つかるよう、 spawn cwd を
//! VP install root に固定する設計 (doc 11 §3.5 PR-D 改訂)。
//!
//! ## なぜ runtime 解決 (build-time const ではダメか)
//!
//! 配置 scenario が複数あり、 binary location から install root を逆算する logic が必要:
//!
//! | scenario | binary 位置 | install root |
//! |---|---|---|
//! | dev (cargo run / mise run vp:app) | `target/release/vp` | repo root (= `target/release/vp` の親 2 つ上) |
//! | cargo install | `~/.cargo/bin/vp` | (binary の隣接で task 不在 → fallback 必要) |
//! | Mac App Store / .app bundle | `<app>/Contents/MacOS/vp` | `<app>/Contents/Resources/` |
//! | dogfood (CARGO_MANIFEST_DIR 利用) | (build script 中) | `CARGO_MANIFEST_DIR/../..` |
//!
//! ## 解決優先順位
//!
//! 1. env `VP_INSTALL_ROOT` (test / override / 配布パッケージング script 用)
//! 2. `current_exe()` から walk-up: `.mise/tasks/vp/stand/` を持つ祖先 dir
//!    - dev: target/release/vp → ... → repo root に到達
//! 3. Mac .app bundle: binary が `Contents/MacOS/` 配下なら `Contents/Resources/` を試す
//! 4. CARGO_MANIFEST_DIR (cargo test / dev 環境のみ、 prod では unset)
//! 5. None (caller が graceful degrade)
//!
//! ## per-project override は廃止
//!
//! doc 11 §3.8 の cascade design (project local > workspace > global) は mise の
//! filesystem-tree cascade が sibling project 間で機能しない設計上の罠で破綻。
//! Z 系統では VP install root に **single source of truth**、 per-project の差し込みは
//! task script 内 `case "$VP_PROJECT"` 分岐 = explicit dispatch で対応する。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// VP install root を runtime で resolve、 1 process lifetime で cache。
///
/// caller (`stand_spawner::build_stand_command`) が `Some(root)` を spawn cwd として使う。
/// `None` の時は caller が `project_dir` を fallback 使用 (= 旧挙動 + 機能劣化)、 ただし
/// 大半の deployment では `current_exe` walk-up で見つかる前提。
pub fn locate_install_root() -> Option<&'static Path> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE.get_or_init(locate_install_root_uncached).as_deref()
}

fn locate_install_root_uncached() -> Option<PathBuf> {
    // 1. env override
    if let Ok(p) = std::env::var("VP_INSTALL_ROOT") {
        let path = PathBuf::from(p);
        if has_stand_tasks(&path) {
            tracing::debug!(
                "VP install root: env VP_INSTALL_ROOT で解決 path={}",
                path.display()
            );
            return Some(path);
        } else {
            tracing::warn!(
                "VP install root: VP_INSTALL_ROOT='{}' に .mise/tasks/vp/stand/ が無い、 次の経路を試行",
                path.display()
            );
        }
    }

    // 2. current_exe() walk-up
    if let Ok(exe) = std::env::current_exe()
        && let Some(found) = walk_up_for_stand_tasks(&exe)
    {
        tracing::debug!(
            "VP install root: current_exe walk-up で解決 path={}",
            found.display()
        );
        return Some(found);
    }

    // 3. CARGO_MANIFEST_DIR (cargo test / cargo run 経由の起動時のみ存在)
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        // crates/vantage-point/Cargo.toml の親 2 つ上 = workspace root
        if let Some(workspace_root) = PathBuf::from(manifest).parent().and_then(Path::parent)
            && has_stand_tasks(workspace_root)
        {
            tracing::debug!(
                "VP install root: CARGO_MANIFEST_DIR で解決 path={}",
                workspace_root.display()
            );
            return Some(workspace_root.to_path_buf());
        }
    }

    tracing::warn!(
        "VP install root: 解決失敗 ─ env VP_INSTALL_ROOT / current_exe walk-up / CARGO_MANIFEST_DIR 全 fallback で .mise/tasks/vp/stand/ が見つからない。 lane spawn は project_dir を mise cwd に使う旧挙動に degrade"
    );
    None
}

/// `start` から root (`/`) 方向に親を辿り、 `.mise/tasks/vp/stand/` を持つ最初の dir を返す。
///
/// Mac .app bundle の特殊 case: `<bundle>/Contents/MacOS/vp` を起点とする時、 walk-up の
/// 途中で `Contents/MacOS/` を踏むので、 そこで `Contents/Resources/` を sibling として確認する。
fn walk_up_for_stand_tasks(start: &Path) -> Option<PathBuf> {
    let mut cur = start.parent()?;
    loop {
        if has_stand_tasks(cur) {
            return Some(cur.to_path_buf());
        }
        // Mac .app bundle: <bundle>/Contents/MacOS/vp の場合、 Resources を試す
        if cur.ends_with("Contents/MacOS")
            && let Some(contents) = cur.parent()
        {
            let resources = contents.join("Resources");
            if has_stand_tasks(&resources) {
                return Some(resources);
            }
        }
        let parent = cur.parent()?;
        if parent == cur {
            // / に到達 (parent と self が同じ = filesystem root)
            return None;
        }
        cur = parent;
    }
}

fn has_stand_tasks(dir: &Path) -> bool {
    dir.join(".mise")
        .join("tasks")
        .join("vp")
        .join("stand")
        .is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CARGO_MANIFEST_DIR 経路での解決 (cargo test 環境では必ず動く)。
    #[test]
    fn locate_install_root_finds_workspace_root_in_cargo_test() {
        let root = locate_install_root().expect("CARGO_MANIFEST_DIR fallback で必ず解決するはず");
        // workspace root に `.mise/tasks/vp/stand/` がある (PR-A で導入済)
        assert!(root.join(".mise/tasks/vp/stand").is_dir());
    }

    /// VP_INSTALL_ROOT env で override 経路 (test では cache 経由で 1 回しか効かないので separate test)。
    #[test]
    fn has_stand_tasks_detects_workspace_root() {
        // CARGO_MANIFEST_DIR の親 2 つ上 = workspace root
        let manifest = env!("CARGO_MANIFEST_DIR");
        let workspace_root = std::path::PathBuf::from(manifest)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(has_stand_tasks(&workspace_root));
        assert!(!has_stand_tasks(&workspace_root.join("nonexistent-subdir")));
    }

    /// walk-up で workspace root に到達できる (cargo test の場合、 binary は target/.../deps/ 配下)。
    #[test]
    fn walk_up_finds_workspace_root_from_test_binary() {
        let exe = std::env::current_exe().expect("current_exe");
        let found = walk_up_for_stand_tasks(&exe);
        assert!(
            found.is_some(),
            "test binary {} から walk-up で workspace root に到達できるはず",
            exe.display()
        );
        let found = found.unwrap();
        assert!(found.join(".mise/tasks/vp/stand").is_dir());
    }
}
