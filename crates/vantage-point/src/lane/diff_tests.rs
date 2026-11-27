// mem_1ChSjZ8945Y4MsvU4F2qEf
use super::*;
use crate::flow::{DiffTier, classify_diff};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}
fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-b", "main"]);
    git(d.path(), &["config", "user.email", "test@example.com"]);
    git(d.path(), &["config", "user.name", "Test"]);
    fs::write(d.path().join("a.rs"), "base\n").unwrap();
    git(d.path(), &["add", "."]);
    git(d.path(), &["commit", "-m", "base"]);
    git(
        d.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(
        d.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    d
}

#[test]
fn diff_stats_include_committed_staged_unstaged_and_untracked() {
    let d = repo();
    let dir = d.path();
    assert!(collect_diff_stats(dir, "origin/main").unwrap().is_empty());
    fs::write(dir.join("a.rs"), "base\ncommitted\n").unwrap();
    git(dir, &["commit", "-am", "change"]);
    fs::write(dir.join("a.rs"), "base\ncommitted\nstaged\n").unwrap();
    git(dir, &["add", "a.rs"]);
    fs::write(dir.join("a.rs"), "base\ncommitted\nstaged\ndirty\n").unwrap();
    fs::write(dir.join("日本語\tnew.rs"), "one\ntwo").unwrap();
    fs::write(dir.join(".gitignore"), "ignored\n").unwrap();
    fs::write(dir.join("ignored"), "not counted").unwrap();
    let stats = collect_diff_stats(dir, "origin/main").unwrap();
    assert_eq!(stats.len(), 3);
    let a = stats.iter().find(|s| s.path == "a.rs").unwrap();
    assert_eq!((a.insertions, a.deletions), (3, 0));
    assert_eq!(
        stats
            .iter()
            .find(|s| s.path == "日本語\tnew.rs")
            .unwrap()
            .insertions,
        2
    );
    assert_eq!(classify_diff(&stats).unwrap().tier, DiffTier::B);
}

#[test]
fn moving_sensitive_file_to_docs_still_requires_c() {
    let d = repo();
    let dir = d.path();
    fs::write(dir.join("AGENTS.md"), "rules\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "rules"]);
    git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    fs::create_dir(dir.join("docs")).unwrap();
    git(dir, &["mv", "AGENTS.md", "docs/old.md"]);
    let stats = collect_diff_stats(dir, "origin/main").unwrap();
    assert!(stats.iter().any(|s| s.path == "AGENTS.md"));
    assert_eq!(classify_diff(&stats).unwrap().tier, DiffTier::C);
}

#[test]
fn missing_base_is_error_not_empty_diff() {
    let d = repo();
    assert!(collect_diff_stats(d.path(), "origin/missing").is_err());
}

#[test]
fn binary_and_empty_untracked_files_are_changes() {
    let d = repo();
    fs::write(d.path().join("picture.bin"), [0, 1, 2]).unwrap();
    fs::write(d.path().join("empty.rs"), "").unwrap();
    let stats = collect_diff_stats(d.path(), "origin/main").unwrap();
    assert_eq!(stats.len(), 2);
    assert!(
        stats
            .iter()
            .find(|s| s.path == "picture.bin")
            .unwrap()
            .binary
    );
}

#[test]
fn classification_uses_lane_base_config_and_works_from_subdirectory() {
    let d = repo();
    let dir = d.path();
    fs::create_dir(dir.join(".vp")).unwrap();
    fs::write(dir.join(".vp/sub-files.kdl"), "base-ref \"nightly\"\n").unwrap();
    fs::write(dir.join("AGENTS.md"), "rules\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "nightly"]);
    git(dir, &["update-ref", "refs/remotes/origin/nightly", "HEAD"]);
    fs::create_dir(dir.join("src")).unwrap();
    fs::write(dir.join("src/new.rs"), "small\n").unwrap();
    let c = classify_worktree(&dir.join("src")).unwrap().unwrap();
    assert_eq!(c.tier, DiffTier::A);
    assert_eq!(c.changed_files, 1);
    let status = sub_status(dir);
    let json = serde_json::to_value(status).unwrap();
    assert_eq!(json["diff_tier"], "A");
    assert!(!json["tier_reasons"].as_array().unwrap().is_empty());
}
