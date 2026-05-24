use super::config;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// crossplat symlink (Unix: `symlink`。Windows: file/dir を `is_dir()` で判別)。
/// Windows で symlink を張るには Developer Mode もしくは管理者権限が必要。
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
    }
    #[cfg(windows)]
    {
        if src.is_dir() {
            std::os::windows::fs::symlink_dir(src, dst)
        } else {
            std::os::windows::fs::symlink_file(src, dst)
        }
    }
}

/// Create a new wing environment
pub fn new_wing(name: &str, branch: &str, force: bool) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;
    let wing_dir = setup_wing(name, branch, &repo_root, force)?;
    println!("{}", wing_dir.display());
    Ok(())
}

/// Phase 4-X: SP-friendly wrapper. `repo_root` を明示的に受け取り、 wing dir の `PathBuf` を返す。
/// stdout への print なし、 lib call として完結。 SP server (lanes.rs) から直接呼ぶ用。
pub fn new_wing_in(
    repo_root: &Path,
    name: &str,
    branch: &str,
    force: bool,
) -> Result<PathBuf, String> {
    setup_wing(name, branch, repo_root, force)
}

/// Phase 4-X: SP-friendly remove。 repo_root を明示的に受け取り、 project-local 新 path +
/// legacy global path の dual-read で wing dir を解決して削除する。
///
/// project-local lane refactor PR 1: `repo_name: &str` → `repo_root: &Path` に signature
/// 変更。 caller (sidebar 経由 DELETE 等) は state.project_dir を直接渡せる。
pub fn remove_wing_in(repo_root: &Path, name: &str) -> Result<(), String> {
    config::validate_wing_name(name)?;
    let Some(wing_dir) = find_wing_dir_dual(repo_root, name) else {
        let repo_name = repo_root.file_name().and_then(|n| n.to_str()).unwrap_or("");
        return Err(format!(
            "wing not found: '{name}' (looked in {}/.vp/lanes/, legacy global path with prefix '{repo_name}-')",
            repo_root.display()
        ));
    };
    fs::remove_dir_all(&wing_dir).map_err(|e| e.to_string())
}

/// Fork current dirty state into a new wing environment
pub fn fork_wing(name: &str, branch: &str, force: bool) -> Result<(), String> {
    let repo_root = config::find_repo_root().map_err(|e| e.to_string())?;

    // Capture dirty state as a diff BEFORE creating the wing
    let diff = capture_dirty_diff(&repo_root)?;

    let wing_dir = setup_wing(name, branch, &repo_root, force)?;

    // Apply the captured diff to the wing
    if let Some(patch) = diff {
        eprintln!("dirty state を適用中...");
        apply_patch(&wing_dir, &patch)?;
    } else {
        eprintln!("フォークする未コミット変更はありません。");
    }

    println!("{}", wing_dir.display());
    Ok(())
}

/// Common wing setup: clone, symlink, branch, post-setup.
/// Returns the wing directory path.
///
/// project-local lane refactor PR 1: 新 lane の配置先を `<repo_root>/.vp/lanes/<name>` に
/// 切替。 旧 `<wings_dir>/<repo>-<name>` (global path + repo prefix) は CLI dual-read で
/// 読めるが、 新規作成は project-local 一本。 parent repo の `.gitignore` に `.vp/` を
/// best-effort で追記して nested git clone を隠蔽する。
fn setup_wing(name: &str, branch: &str, repo_root: &Path, force: bool) -> Result<PathBuf, String> {
    config::validate_wing_name(name)?;

    let remote_url = config::get_remote_url().map_err(|e| e.to_string())?;
    let cfg = config::load_config(repo_root)?;

    // parent repo の .gitignore に .vp/ を追記 (idempotent、 best-effort)。 失敗しても
    // wing 作成は続行する (= user が手動で .gitignore 編集する fallback path 残す)。
    if let Err(e) = config::ensure_vp_gitignored(repo_root) {
        eprintln!("⚠ .gitignore への .vp/ 追記失敗 (続行): {e}");
    }

    let wings_dir = config::project_lanes_dir(repo_root);
    let wing_dir = wings_dir.join(name);

    // Check existing wing (新 path のみ。 legacy global path との conflict は dual-read 経由で
    // user に見える + 別 path なので衝突しない)
    if wing_dir.exists() {
        if !force {
            return Err(format!(
                "ウィング '{name}' は既に存在します ({})。上書きするには --force を指定してください。",
                wing_dir.display()
            ));
        }
        eprintln!("既存ウィングを削除: {}", wing_dir.display());
        fs::remove_dir_all(&wing_dir).map_err(|e| e.to_string())?;
    }

    // Clone
    fs::create_dir_all(&wings_dir).map_err(|e| e.to_string())?;
    eprintln!("{} にクローン中...", wing_dir.display());
    let repo_root_str = repo_root
        .to_str()
        .ok_or("リポジトリルートのパスが有効な UTF-8 ではありません")?;
    let wing_dir_str = wing_dir
        .to_str()
        .ok_or("ウィングディレクトリのパスが有効な UTF-8 ではありません")?;
    run_git(&["clone", "--depth", "1", repo_root_str, wing_dir_str])?;

    // Set remote to GitHub URL
    run_git_in(&wing_dir, &["remote", "set-url", "origin", &remote_url])?;

    // Symlinks
    for file in &cfg.symlinks {
        let src = repo_root.join(file);
        let dst = wing_dir.join(file);
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            // Remove existing file from clone (if it exists) before symlinking
            let _ = fs::remove_file(&dst);
            symlink(&src, &dst).map_err(|e| e.to_string())?;
            eprintln!("  symlink: {file}");
        }
    }

    // Copies
    for file in &cfg.copies {
        let src = repo_root.join(file);
        let dst = wing_dir.join(file);
        if src.exists() {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(&src, &dst).map_err(|e| e.to_string())?;
            eprintln!("  copy: {file}");
        }
    }

    // Symlink patterns
    for pattern in &cfg.symlink_patterns {
        let matches =
            glob::glob(&format!("{}/{pattern}", repo_root.display())).map_err(|e| e.to_string())?;

        for entry in matches.flatten() {
            // Skip .git directory
            if entry.to_str().is_some_and(|s| s.contains("/.git/")) {
                continue;
            }
            if let Ok(rel) = entry.strip_prefix(repo_root) {
                let dst = wing_dir.join(rel);
                if !dst.exists() {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    symlink(&entry, &dst).map_err(|e| e.to_string())?;
                    eprintln!("  symlink (pattern): {}", rel.display());
                }
            }
        }
    }

    // Create branch
    run_git_in(&wing_dir, &["checkout", "-b", branch])?;

    // Post-setup
    if let Some(cmd) = &cfg.post_setup {
        eprintln!("実行中: {cmd}");
        let status = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&wing_dir)
            .status()
            .map_err(|e| e.to_string())?;

        if !status.success() {
            return Err(format!("post-setup 失敗: {cmd}"));
        }
    }

    // project-local lane refactor PR 4a: PR #429 の `claude_trust::pre_grant_trust` 削除。
    // wing dir は `<repo>/.vp/lanes/<name>` に置かれ、 parent repo (= `<repo>`) の
    // `hasTrustDialogAccepted: true` が claude 側で **hierarchical 継承** されるので
    // pre-grant は不要 (2026-05-24 実証、 nested `.git/` でも継承)。
    Ok(wing_dir)
}

/// List all wing environments (dual-read: cwd repo の project-local + legacy global)。
///
/// project-local lane refactor PR 1: cwd が git repo の場合、 `<repo>/.vp/lanes/` を
/// 先に列挙し、 続けて legacy global path も列挙する (= 移行期の overview)。
pub fn list_wings() -> Result<(), String> {
    // 1. cwd の project-local
    if let Ok(repo_root) = config::find_repo_root() {
        let pl_dir = config::project_lanes_dir(&repo_root);
        if pl_dir.exists()
            && let Ok(entries) = fs::read_dir(&pl_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let branch = get_branch(&path).unwrap_or_else(|| "-".to_string());
                println!("{name}\t{branch}\t{}", path.display());
            }
        }
    }

    // 2. legacy global (PR 4 cleanup で削除予定)
    let Ok(wings_dir) = config::wings_dir() else {
        return Ok(());
    };
    if !wings_dir.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(&wings_dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let branch = get_branch(&path).unwrap_or_else(|| "-".to_string());
        println!("{name}\t{branch}\t{}", path.display());
    }

    Ok(())
}

/// disk 上で発見された Wing 環境 1 件 (lane Wing dir の structured view、 SP /api/lanes 用)。
///
/// PtySlot 起動の有無は問わない (= disk 存在のみ示す)。 lanes.rs:list_handler で
/// in-memory LanePool に居ない Wing を `LaneState::Inactive` として merge する時の中間 type。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InactiveWingEntry {
    /// repo prefix を剥がした wing 名 (`<repo_name>-<name>` → `<name>`)
    pub name: String,
    /// 絶対 path
    pub path: String,
    /// `git branch --show-current` の結果。 取れない時 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// repo に紐づく Wing dir を disk scan して返す (SP /api/lanes 用)。
///
/// project-local lane refactor PR 1: `repo_name: &str` → `repo_root: &Path` に signature
/// 変更し、 dual-read で両 path を列挙する:
/// 1. `<repo_root>/.vp/lanes/<name>` (= 新 path、 prefix 不要)
/// 2. `<wings_dir>/<repo_name>-<name>` (= legacy global path、 PR 4 cleanup で削除)
///
/// 重複時 (= 新旧両方に同名 dir): project-local 優先 (legacy 側を skip)。
///
/// 「基本は通らない防御パス」: 通常 lane clone は POST /api/lanes 経由で生成され、 同 session 内なら
/// LanePool に登録されている。 ただし vp-app crash 後の残骸 / 別 session での `vp lane new` 等で
/// disk に存在するが LanePool に居ない Wing が出ることがあり、 それを sidebar に inactive 状態で
/// surface するため。 click で activate (= POST /api/lanes に cwd 指定で attach) する想定。
///
/// fail-soft (= 防御パスのため read error は空 Vec 扱い)。
pub fn list_wings_for_repo(repo_root: &Path) -> Vec<InactiveWingEntry> {
    let mut out = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. project-local: <repo>/.vp/lanes/<name>
    let pl_dir = config::project_lanes_dir(repo_root);
    if let Ok(entries) = fs::read_dir(&pl_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = entry.file_name();
            let dir_name = dir_name.to_string_lossy().into_owned();
            seen_names.insert(dir_name.clone());
            out.push(InactiveWingEntry {
                name: dir_name,
                path: path.to_string_lossy().into_owned(),
                branch: get_branch(&path),
            });
        }
    }

    // 2. legacy global: <wings_dir>/<repo_name>-<name> (PR 4 cleanup で削除予定)
    let Some(repo_name) = repo_root.file_name().and_then(|n| n.to_str()) else {
        return out;
    };
    if repo_name.is_empty() {
        return out;
    }
    let Ok(wings_dir) = config::wings_dir() else {
        return out;
    };
    if !wings_dir.exists() {
        return out;
    }
    let Ok(entries) = fs::read_dir(&wings_dir) else {
        return out;
    };
    let prefix = format!("{repo_name}-");
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name();
        let dir_name = dir_name.to_string_lossy();
        let Some(stripped) = dir_name.strip_prefix(&prefix) else {
            continue;
        };
        if stripped.is_empty() {
            continue;
        }
        // 新 path 側に同名 lane があれば legacy は skip (project-local 優先)
        if seen_names.contains(stripped) {
            continue;
        }
        out.push(InactiveWingEntry {
            name: stripped.to_string(),
            path: path.to_string_lossy().into_owned(),
            branch: get_branch(&path),
        });
    }
    out
}

/// Print the path to a wing (dual-read: project-local 優先、 legacy global fallback)。
pub fn wing_path(name: &str) -> Result<(), String> {
    // cwd の repo を起点に dual-read
    if let Ok(repo_root) = config::find_repo_root()
        && let Some(found) = find_wing_dir_dual(&repo_root, name)
    {
        println!("{}", found.display());
        return Ok(());
    }
    // cwd が git repo でない場合: legacy global path のみ ad-hoc lookup
    if let Ok(wings_dir) = config::wings_dir() {
        let direct = wings_dir.join(name);
        if direct.is_dir() {
            println!("{}", direct.display());
            return Ok(());
        }
    }
    Err(format!(
        "ウィング '{name}' が見つかりません。`vp lane ls` で一覧を確認してください。"
    ))
}

/// Remove a wing environment (dual-read: project-local 優先、 legacy global fallback)。
pub fn remove_wing(name: Option<&str>, all: bool, force: bool) -> Result<(), String> {
    if all {
        if !force {
            return Err("--all には --force が必要です（誤削除防止）".into());
        }
        let mut removed_any = false;
        // 1. cwd の project-local 全削除
        if let Ok(repo_root) = config::find_repo_root() {
            let pl_dir = config::project_lanes_dir(&repo_root);
            if pl_dir.exists() {
                fs::remove_dir_all(&pl_dir).map_err(|e| e.to_string())?;
                eprintln!("project-local ウィング全削除: {}", pl_dir.display());
                removed_any = true;
            }
        }
        // 2. legacy global 全削除 (PR 4 cleanup で削除予定)
        if let Ok(wings_dir) = config::wings_dir()
            && wings_dir.exists()
        {
            fs::remove_dir_all(&wings_dir).map_err(|e| e.to_string())?;
            eprintln!("legacy global ウィング全削除: {}", wings_dir.display());
            removed_any = true;
        }
        if !removed_any {
            eprintln!("削除対象のウィングはありませんでした");
        }
        return Ok(());
    }

    let name = name.ok_or("ウィング名を指定するか --all --force を使用してください")?;
    config::validate_wing_name(name)?;

    // dual-read で発見した path を削除 (cwd が git repo でなければ legacy のみ)
    let found = if let Ok(repo_root) = config::find_repo_root() {
        find_wing_dir_dual(&repo_root, name)
    } else if let Ok(wings_dir) = config::wings_dir() {
        let direct = wings_dir.join(name);
        if direct.is_dir() { Some(direct) } else { None }
    } else {
        None
    };
    let Some(wing_dir) = found else {
        return Err(format!(
            "ウィング '{name}' が見つかりません。`vp lane ls` で一覧を確認してください。"
        ));
    };
    fs::remove_dir_all(&wing_dir).map_err(|e| e.to_string())?;
    eprintln!("削除: {}", wing_dir.display());
    Ok(())
}

/// Show status of all wing environments (dual-read: cwd repo の project-local + legacy global)。
pub fn status_wings() -> Result<(), String> {
    let mut found = false;

    // 1. cwd の project-local
    if let Ok(repo_root) = config::find_repo_root() {
        let pl_dir = config::project_lanes_dir(&repo_root);
        if pl_dir.exists()
            && let Ok(entries) = fs::read_dir(&pl_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || !path.join(".git").exists() {
                    continue;
                }
                found = true;
                print_wing_status_row(&path, &entry.file_name().to_string_lossy());
            }
        }
    }

    // 2. legacy global
    if let Ok(wings_dir) = config::wings_dir()
        && wings_dir.exists()
        && let Ok(entries) = fs::read_dir(&wings_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join(".git").exists() {
                continue;
            }
            found = true;
            print_wing_status_row(&path, &entry.file_name().to_string_lossy());
        }
    }

    if !found {
        eprintln!("ウィングはありません。`vp lane new <name> <branch>` で作成できます。");
    }

    Ok(())
}

/// Remove wings whose branch is merged into main (dual-read 両 path 対象)
pub fn cleanup_wings(force: bool) -> Result<(), String> {
    let mut to_remove: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut kept: Vec<(String, String)> = Vec::new();

    // 1. cwd の project-local
    if let Ok(repo_root) = config::find_repo_root() {
        let pl_dir = config::project_lanes_dir(&repo_root);
        if pl_dir.exists()
            && let Ok(entries) = fs::read_dir(&pl_dir)
        {
            for entry in entries.flatten() {
                classify_wing_for_cleanup(entry, &mut to_remove, &mut kept);
            }
        }
    }

    // 2. legacy global
    if let Ok(wings_dir) = config::wings_dir()
        && wings_dir.exists()
        && let Ok(entries) = fs::read_dir(&wings_dir)
    {
        for entry in entries.flatten() {
            classify_wing_for_cleanup(entry, &mut to_remove, &mut kept);
        }
    }

    if to_remove.is_empty() && kept.is_empty() {
        eprintln!("クリーンアップ対象はありません。");
        return Ok(());
    }
    if to_remove.is_empty() {
        eprintln!("クリーンアップ対象はありません。");
        for (name, reason) in &kept {
            eprintln!("  保持: {name} ({reason})");
        }
        return Ok(());
    }

    for (name, _) in &to_remove {
        eprintln!("  削除可能: {name} (マージ済み)");
    }
    for (name, reason) in &kept {
        eprintln!("  保持: {name} ({reason})");
    }

    if !force {
        eprintln!("\n実際に削除するには `vp lane cleanup --force` を実行してください。");
        return Ok(());
    }

    for (name, path) in &to_remove {
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
        eprintln!("  削除: {name}");
    }

    eprintln!("{} ウィングを削除しました。", to_remove.len());
    Ok(())
}

/// `status_wings` 内の 1 wing 行表示 helper (project-local / legacy 両 path で共有)
fn print_wing_status_row(path: &Path, name: &str) {
    let branch = get_branch(path).unwrap_or_else(|| "-".to_string());
    let changes = count_changes(path);
    let ahead_behind = get_ahead_behind(path);
    let last_commit = get_last_commit(path);
    let changes_str = if changes > 0 {
        format!("{changes} files")
    } else {
        "clean".to_string()
    };
    println!("{name}\t{branch}\t{changes_str}\t{ahead_behind}\t{last_commit}");
}

/// `cleanup_wings` 内の 1 wing 分類 helper (project-local / legacy 両 path で共有)
fn classify_wing_for_cleanup(
    entry: fs::DirEntry,
    to_remove: &mut Vec<(String, std::path::PathBuf)>,
    kept: &mut Vec<(String, String)>,
) {
    let path = entry.path();
    if !path.is_dir() || !path.join(".git").exists() {
        return;
    }
    let name = entry.file_name().to_string_lossy().to_string();
    let _ = run_git_in(&path, &["fetch", "--quiet"]);
    if is_branch_merged(&path) {
        to_remove.push((name, path));
    } else {
        let changes = count_changes(&path);
        let reason = if changes > 0 {
            format!("アクティブ ({changes} files changed)")
        } else {
            "未マージ".to_string()
        };
        kept.push((name, reason));
    }
}

/// dual-read で wing dir を解決する: project-local 優先、 legacy global 2 form (直 / `<repo>-<name>` prefix) fallback。
///
/// project-local lane refactor PR 1: lane の lookup を 1 箇所に集約。 wing_path / remove_wing / remove_wing_in が共有。
fn find_wing_dir_dual(repo_root: &Path, name: &str) -> Option<PathBuf> {
    // 1. project-local: <repo>/.vp/lanes/<name>
    let project_local = config::project_lanes_dir(repo_root).join(name);
    if project_local.is_dir() {
        return Some(project_local);
    }
    // 2. legacy global path (PR 4 で削除予定)
    let wings_dir = config::wings_dir().ok()?;
    let direct = wings_dir.join(name);
    if direct.is_dir() {
        return Some(direct);
    }
    let repo_name = repo_root.file_name().and_then(|n| n.to_str())?;
    let prefixed = wings_dir.join(format!("{repo_name}-{name}"));
    if prefixed.is_dir() {
        return Some(prefixed);
    }
    None
}

// --- helpers ---

/// Capture uncommitted changes (staged + unstaged + untracked) as a combined diff.
/// Returns None if there are no changes.
fn capture_dirty_diff(repo_root: &Path) -> Result<Option<String>, String> {
    // Staged + unstaged tracked changes
    let tracked = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    if !tracked.status.success() {
        return Err("git diff HEAD に失敗しました".to_string());
    }

    let diff = String::from_utf8_lossy(&tracked.stdout).to_string();

    // Untracked files — generate diff with --no-index /dev/null <file>
    let untracked = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;

    let mut full_diff = diff;

    if untracked.status.success() {
        for file in String::from_utf8_lossy(&untracked.stdout).lines() {
            let file = file.trim();
            if file.is_empty() {
                continue;
            }
            // Use git diff --no-index to generate a proper patch (handles binary, no-newline, etc.)
            let file_diff = Command::new("git")
                .args(["diff", "--no-index", "--", "/dev/null", file])
                .current_dir(repo_root)
                .output()
                .ok();
            if let Some(output) = file_diff {
                // --no-index exits 1 when files differ (expected), only skip on spawn failure
                let patch = String::from_utf8_lossy(&output.stdout);
                if !patch.is_empty() {
                    // Rewrite paths: /dev/null → a/<file>, <file> → b/<file>
                    for line in patch.lines() {
                        if line.starts_with("+++ ") && !line.contains("/dev/null") {
                            full_diff.push_str(&format!("+++ b/{file}\n"));
                        } else if line.starts_with("--- /dev/null") {
                            full_diff.push_str("--- /dev/null\n");
                        } else if line.starts_with("diff --git") {
                            full_diff.push_str(&format!("diff --git a/{file} b/{file}\n"));
                        } else {
                            full_diff.push_str(line);
                            full_diff.push('\n');
                        }
                    }
                }
            }
        }
    }

    if full_diff.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(full_diff))
    }
}

/// Apply a unified diff patch to a directory
fn apply_patch(wing_dir: &Path, patch: &str) -> Result<(), String> {
    let mut child = Command::new("git")
        .args(["apply", "--allow-empty", "-"])
        .current_dir(wing_dir)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(patch.as_bytes())
            .map_err(|e| e.to_string())?;
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git apply failed: {stderr}"));
    }

    Ok(())
}

fn run_git(args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {stderr}", args.join(" ")));
    }
    Ok(())
}

fn run_git_in(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {stderr}", args.join(" ")));
    }
    Ok(())
}

fn count_changes(dir: &std::path::Path) -> usize {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .count(),
        _ => 0,
    }
}

fn get_ahead_behind(dir: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let parts: Vec<&str> = s.split('\t').collect();
            if parts.len() == 2 {
                let ahead: i32 = parts[0].parse().unwrap_or(0);
                let behind: i32 = parts[1].parse().unwrap_or(0);
                match (ahead, behind) {
                    (0, 0) => "up-to-date".to_string(),
                    (a, 0) => format!("↑{a}"),
                    (0, b) => format!("↓{b}"),
                    (a, b) => format!("↑{a}↓{b}"),
                }
            } else {
                "-".to_string()
            }
        }
        _ => "local".to_string(),
    }
}

fn get_last_commit(dir: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["log", "--oneline", "-1"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "-".to_string(),
    }
}

/// Check if HEAD in the wing dir is merged into origin/main (or origin/master).
///
/// A wing is "merged" only if:
///   1. HEAD is an ancestor of origin/<main> (merge-base --is-ancestor), AND
///   2. The wing has diverged (has at least 1 local commit beyond origin/<main>)
///
/// This prevents false positives on freshly-created wings (HEAD == origin/main).
fn is_branch_merged(wing_dir: &std::path::Path) -> bool {
    for branch in &["main", "master"] {
        let remote_ref = format!("origin/{branch}");

        // Check if HEAD is ancestor of remote main
        let ancestor = Command::new("git")
            .args(["merge-base", "--is-ancestor", "HEAD", &remote_ref])
            .current_dir(wing_dir)
            .output()
            .ok();
        if !matches!(ancestor, Some(ref o) if o.status.success()) {
            continue;
        }

        // Guard: skip if HEAD is exactly the same commit as origin/main
        // (freshly created wing that hasn't diverged yet)
        let head_sha = git_rev_parse(wing_dir, "HEAD");
        let remote_sha = git_rev_parse(wing_dir, &remote_ref);
        if head_sha == remote_sha {
            continue;
        }

        return true;
    }
    false
}

fn git_rev_parse(dir: &std::path::Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(dir)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn get_branch(dir: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(dir)
        .output()
        .ok()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() {
            None
        } else {
            Some(branch)
        }
    } else {
        None
    }
}

// ── Phase 5-D D1: Wing status (struct 返却、 SP API exposure 用) ───────────

/// Wing workspace の git 状態 snapshot。 `wing_status(path)` で取得、
/// `/api/lanes` の LaneInfo に embed して sidebar に表示する。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WingStatus {
    /// 現在のブランチ (detached HEAD 時 None)
    pub branch: Option<String>,
    /// `git status --short` の non-empty lines (= 変更ファイル数、 0 なら clean)
    pub dirty_count: usize,
    /// upstream に対する ahead commit 数 (upstream 無い時 0)
    pub ahead: u32,
    /// upstream に対する behind commit 数
    pub behind: u32,
    /// upstream tracking 自体があるか (`local` / detached の判別用)
    pub has_upstream: bool,
    /// 最新 commit `{sha} {message}` (`git log --oneline -1`)、 取得失敗時 "-"
    pub last_commit: String,
    /// origin/main (or origin/master) に merge 済みで cleanup 候補か
    pub is_merged: bool,
}

/// Wing workspace dir から status snapshot を取得 (SP API 用)。 git 関連 subprocess を
/// 5-7 個並列に呼ぶので 1 回 ~50-100ms 程度。 多数 wing 時は SP 側で並列化検討。
pub fn wing_status(dir: &Path) -> WingStatus {
    let branch = get_branch(dir);
    let dirty_count = count_changes(dir);
    let (ahead, behind, has_upstream) = get_ahead_behind_counts(dir);
    let last_commit = get_last_commit(dir);
    let is_merged = is_branch_merged(dir);
    WingStatus {
        branch,
        dirty_count,
        ahead,
        behind,
        has_upstream,
        last_commit,
        is_merged,
    }
}

/// `get_ahead_behind` の数値版 ── upstream tracking が無いと `(0, 0, false)` を返す。
fn get_ahead_behind_counts(dir: &Path) -> (u32, u32, bool) {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(dir)
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let parts: Vec<&str> = s.split('\t').collect();
            if parts.len() == 2 {
                let ahead: u32 = parts[0].parse().unwrap_or(0);
                let behind: u32 = parts[1].parse().unwrap_or(0);
                (ahead, behind, true)
            } else {
                (0, 0, false)
            }
        }
        _ => (0, 0, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    // --- test helpers ---

    /// Create a unique temp dir per test to avoid parallel test collisions
    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lane-cmd-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Initialize a git repo with an initial commit in the given directory.
    /// Configures local user.name/email to avoid system config dependency.
    fn git_init_with_commit(dir: &std::path::Path) {
        Cmd::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap();
        // initial commit
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(dir)
            .output()
            .unwrap();
        // Ensure we are on 'main'
        Cmd::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    // --- capture_dirty_diff ---

    #[test]
    fn capture_dirty_diff_no_changes_returns_none() {
        let repo = test_dir("dirty-diff-clean");
        git_init_with_commit(&repo);

        let result = capture_dirty_diff(&repo).unwrap();
        assert!(result.is_none(), "clean repo should return None");

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn capture_dirty_diff_tracked_change_returns_some() {
        let repo = test_dir("dirty-diff-tracked");
        git_init_with_commit(&repo);

        // tracked ファイルを変更
        fs::write(repo.join("README.md"), "# modified\n").unwrap();

        let result = capture_dirty_diff(&repo).unwrap();
        assert!(result.is_some(), "tracked change should return Some diff");
        let diff = result.unwrap();
        assert!(
            diff.contains("README.md"),
            "diff should mention the changed file"
        );

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn capture_dirty_diff_untracked_file_included() {
        let repo = test_dir("dirty-diff-untracked");
        git_init_with_commit(&repo);

        // untracked ファイルを追加
        fs::write(repo.join("new_file.txt"), "hello world\n").unwrap();

        let result = capture_dirty_diff(&repo).unwrap();
        assert!(result.is_some(), "untracked file should produce Some diff");
        let diff = result.unwrap();
        assert!(
            diff.contains("new_file.txt"),
            "diff should include the untracked file"
        );

        let _ = fs::remove_dir_all(&repo);
    }

    // --- is_branch_merged ---

    /// bare repo → clone 構成で origin/main を持つウィングを作る
    fn setup_merged_wing_repos(base: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        // 1. bare repo（origin の代替）を作成
        //    --initial-branch=main で CI runner (init.defaultBranch=master 可能性) でも
        //    origin/HEAD が main に固定されるようにする
        let bare = base.join("bare.git");
        fs::create_dir_all(&bare).unwrap();
        Cmd::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .current_dir(&bare)
            .output()
            .unwrap();

        // 2. bare を clone してメイン repo を作る
        let main_repo = base.join("main_repo");
        Cmd::new("git")
            .args(["clone", bare.to_str().unwrap(), main_repo.to_str().unwrap()])
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // initial commit を main_repo で作り、bare に push
        fs::write(main_repo.join("README.md"), "# init\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        // main ブランチにリネーム
        Cmd::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "-u", "origin", "main"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // 3. wing repo を bare から clone
        let wing_repo = base.join("wing");
        Cmd::new("git")
            .args(["clone", bare.to_str().unwrap(), wing_repo.to_str().unwrap()])
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&wing_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&wing_repo)
            .output()
            .unwrap();

        (main_repo, wing_repo)
    }

    #[test]
    fn is_branch_merged_returns_true_after_merge() {
        let base = test_dir("merged-true");
        let (main_repo, wing_repo) = setup_merged_wing_repos(&base);

        // wing で feature ブランチを作りコミット
        Cmd::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(&wing_repo)
            .output()
            .unwrap();
        fs::write(wing_repo.join("feature.txt"), "feature work\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&wing_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "feature commit"])
            .current_dir(&wing_repo)
            .output()
            .unwrap();

        // wing の feature を bare に push
        Cmd::new("git")
            .args(["push", "origin", "feature"])
            .current_dir(&wing_repo)
            .output()
            .unwrap();

        // main_repo で feature を main に merge して push
        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["merge", "origin/feature", "--no-edit"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        // main にさらに1コミット追加して origin/main を feature より先に進める
        fs::write(main_repo.join("extra.txt"), "extra\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["commit", "-m", "post-merge commit"])
            .current_dir(&main_repo)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&main_repo)
            .output()
            .unwrap();

        // wing が fetch して origin/main を最新化
        // wing の HEAD は feature のまま（origin/main より古い）
        Cmd::new("git")
            .args(["fetch", "origin"])
            .current_dir(&wing_repo)
            .output()
            .unwrap();

        // wing HEAD は origin/main の祖先 + 分岐あり → merged = true
        assert!(
            is_branch_merged(&wing_repo),
            "merged feature branch should return true"
        );

        let _ = fs::remove_dir_all(&base);
    }

    // --- find_wing_dir_dual (project-local lane refactor PR 1) ---

    /// 共通 fixture: temp 領域に偽 repo + project-local lane dir を作る (.git なし、 dir 検出のみ)
    fn setup_dual_fixture(slug: &str) -> (PathBuf, PathBuf) {
        let repo = test_dir(&format!("dual-{slug}"));
        let pl = repo.join(".vp").join("lanes");
        fs::create_dir_all(&pl).unwrap();
        (repo, pl)
    }

    #[test]
    #[serial_test::serial(vp_lanes_env)]
    fn find_wing_dir_dual_prefers_project_local() {
        // 同名 lane が新旧両 path に居れば project-local を返す
        let (repo, pl) = setup_dual_fixture("prefer-pl");
        let pl_wing = pl.join("foo");
        fs::create_dir_all(&pl_wing).unwrap();

        // legacy global path にも同名 (`<repo>-foo`) を仕込む
        let global = test_dir("dual-prefer-pl-global");
        fs::create_dir_all(&global).unwrap();
        let repo_name = repo.file_name().unwrap().to_string_lossy().into_owned();
        let legacy_wing = global.join(format!("{repo_name}-foo"));
        fs::create_dir_all(&legacy_wing).unwrap();
        // SAFETY: テストプロセス内シングルスレッドで env を握る。 並列テスト同士の干渉は test_dir
        // の unique slug + 各 test が serial に env を上書きするため許容。
        unsafe {
            std::env::set_var("VP_LANES_DIR", &global);
        }

        let resolved = find_wing_dir_dual(&repo, "foo");
        assert_eq!(resolved.as_deref(), Some(pl_wing.as_path()));

        unsafe {
            std::env::remove_var("VP_LANES_DIR");
        }
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&global);
    }

    #[test]
    #[serial_test::serial(vp_lanes_env)]
    fn find_wing_dir_dual_falls_back_to_legacy_direct() {
        // project-local に無い + legacy global の直 dir に居る場合
        let (repo, _pl) = setup_dual_fixture("legacy-direct");
        let global = test_dir("dual-legacy-direct-global");
        fs::create_dir_all(&global).unwrap();
        let legacy_wing = global.join("bar"); // prefix なしの直 dir
        fs::create_dir_all(&legacy_wing).unwrap();
        unsafe {
            std::env::set_var("VP_LANES_DIR", &global);
        }

        let resolved = find_wing_dir_dual(&repo, "bar");
        assert_eq!(resolved.as_deref(), Some(legacy_wing.as_path()));

        unsafe {
            std::env::remove_var("VP_LANES_DIR");
        }
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&global);
    }

    #[test]
    #[serial_test::serial(vp_lanes_env)]
    fn find_wing_dir_dual_falls_back_to_legacy_prefixed() {
        // project-local に無い + legacy direct に無い + legacy prefix にある
        let (repo, _pl) = setup_dual_fixture("legacy-prefix");
        let global = test_dir("dual-legacy-prefix-global");
        fs::create_dir_all(&global).unwrap();
        let repo_name = repo.file_name().unwrap().to_string_lossy().into_owned();
        let legacy_wing = global.join(format!("{repo_name}-baz"));
        fs::create_dir_all(&legacy_wing).unwrap();
        unsafe {
            std::env::set_var("VP_LANES_DIR", &global);
        }

        let resolved = find_wing_dir_dual(&repo, "baz");
        assert_eq!(resolved.as_deref(), Some(legacy_wing.as_path()));

        unsafe {
            std::env::remove_var("VP_LANES_DIR");
        }
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&global);
    }

    #[test]
    #[serial_test::serial(vp_lanes_env)]
    fn find_wing_dir_dual_returns_none_when_nowhere() {
        let (repo, _pl) = setup_dual_fixture("none");
        let global = test_dir("dual-none-global");
        fs::create_dir_all(&global).unwrap();
        unsafe {
            std::env::set_var("VP_LANES_DIR", &global);
        }

        let resolved = find_wing_dir_dual(&repo, "missing");
        assert!(resolved.is_none());

        unsafe {
            std::env::remove_var("VP_LANES_DIR");
        }
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&global);
    }

    // --- list_wings_for_repo (dual-read 後の挙動) ---

    #[test]
    #[serial_test::serial(vp_lanes_env)]
    fn list_wings_for_repo_lists_both_paths_with_dedup() {
        let (repo, pl) = setup_dual_fixture("list-both");
        // project-local: foo (with .git for branch detect)
        fs::create_dir_all(pl.join("foo").join(".git")).unwrap();
        // project-local: shared (同名で legacy 側にも置く → project-local 優先)
        fs::create_dir_all(pl.join("shared").join(".git")).unwrap();

        let global = test_dir("dual-list-both-global");
        fs::create_dir_all(&global).unwrap();
        let repo_name = repo.file_name().unwrap().to_string_lossy().into_owned();
        // legacy: <repo>-bar + <repo>-shared (shared は project-local 側が勝つ)
        fs::create_dir_all(global.join(format!("{repo_name}-bar"))).unwrap();
        fs::create_dir_all(global.join(format!("{repo_name}-shared"))).unwrap();
        // 関係ない repo の lane は出ない
        fs::create_dir_all(global.join("other-repo-baz")).unwrap();
        unsafe {
            std::env::set_var("VP_LANES_DIR", &global);
        }

        let mut listed: Vec<String> = list_wings_for_repo(&repo)
            .into_iter()
            .map(|e| e.name)
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["bar", "foo", "shared"]);

        // shared の path は project-local 側であること
        let shared = list_wings_for_repo(&repo)
            .into_iter()
            .find(|e| e.name == "shared")
            .expect("shared が出ない");
        assert!(
            shared.path.contains("/.vp/lanes/shared"),
            "shared は project-local が勝つべき: {}",
            shared.path
        );

        unsafe {
            std::env::remove_var("VP_LANES_DIR");
        }
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&global);
    }

    #[test]
    #[serial_test::serial(vp_lanes_env)]
    fn list_wings_for_repo_handles_missing_project_local_dir() {
        // <repo>/.vp/lanes が存在しなくても legacy global は読める
        let repo = test_dir("list-no-pl");
        fs::create_dir_all(&repo).unwrap();
        let global = test_dir("list-no-pl-global");
        fs::create_dir_all(&global).unwrap();
        let repo_name = repo.file_name().unwrap().to_string_lossy().into_owned();
        fs::create_dir_all(global.join(format!("{repo_name}-only-legacy"))).unwrap();
        unsafe {
            std::env::set_var("VP_LANES_DIR", &global);
        }

        let listed: Vec<String> = list_wings_for_repo(&repo)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(listed, vec!["only-legacy"]);

        unsafe {
            std::env::remove_var("VP_LANES_DIR");
        }
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&global);
    }

    #[test]
    fn is_branch_merged_returns_false_when_head_equals_origin_main() {
        let base = test_dir("merged-false-fresh");
        let (_, wing_repo) = setup_merged_wing_repos(&base);

        // wing に local commit なし（HEAD == origin/main）
        // false-positive ガード: is_branch_merged は false を返すべき
        assert!(
            !is_branch_merged(&wing_repo),
            "fresh wing (HEAD == origin/main) should return false"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
