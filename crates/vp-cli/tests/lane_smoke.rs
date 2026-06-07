//! `vp lane` subcommand の end-to-end smoke test (VP-13 sub-scope B)。
//!
//! lane refactor 5 PR で大量変更した CLI 経路の regression net。 read-only / pure CLI
//! command を覆い、 setup-heavy な `lane new` / `lane fork` (= git clone + remote 要)
//! は別 PR で integration test 化を検討。
//!
//! ## fixture 方針
//!
//! - `tempfile::TempDir` で隔離 fixture (= test 並列実行で衝突しない)
//! - `git init` + initial commit + `.claude/performer-files.kdl` placeholder で最小 performer 環境
//! - `<repo>/.vp/lanes/<name>/.git/HEAD` を仕込んで `list_performers_for_repo` が拾うかを test
//! - `assert_cmd` で `Command::cargo_bin("vp")` を current_dir 指定で起動

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// 最小 git repo fixture を作る (= `vp lane` の find_repo_root が拾える状態)。
/// initial commit を持たないので push 系は失敗するが、 read-only path には十分。
fn setup_minimal_repo() -> TempDir {
    let tmp = tempfile::tempdir().expect("tempdir 作成失敗");
    let repo_path = tmp.path();
    std::process::Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(repo_path)
        .status()
        .expect("git init 失敗");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .status()
        .expect("git config user.email 失敗");
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(repo_path)
        .status()
        .expect("git config user.name 失敗");
    fs::write(repo_path.join("README.md"), "# test\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(repo_path)
        .status()
        .expect("git add 失敗");
    std::process::Command::new("git")
        .args(["commit", "--quiet", "-m", "initial"])
        .current_dir(repo_path)
        .status()
        .expect("git commit 失敗");
    tmp
}

/// `<repo>/.vp/lanes/<name>/.git/HEAD` を仕込んで performer として認識される状態にする
/// (= `list_performers_for_repo` が disk scan で拾う、 actual git clone は不要)。
fn arm_performer_dir(repo: &Path, name: &str) {
    let performer = repo.join(".vp").join("lanes").join(name);
    fs::create_dir_all(performer.join(".git")).unwrap();
    fs::write(
        performer.join(".git").join("HEAD"),
        "ref: refs/heads/main\n",
    )
    .unwrap();
}

// --- top-level CLI ---

#[test]
fn vp_help_exits_zero() {
    Command::cargo_bin("vp")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Vantage Point"));
}

#[test]
fn vp_lane_help_lists_subcommands() {
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("new"))
        .stdout(predicate::str::contains("ls"))
        .stdout(predicate::str::contains("rm"))
        .stdout(predicate::str::contains("path"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("cleanup"));
}

// --- vp lane ls ---

#[test]
fn vp_lane_ls_in_non_git_dir_exits_zero_silently() {
    // git repo でない dir では find_repo_root が早期 return、 空出力 + exit 0
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "ls"])
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::eq(""));
}

#[test]
fn vp_lane_ls_in_empty_repo_exits_zero_silently() {
    // git repo だが .vp/lanes/ 不在 → 空出力 + exit 0
    let repo = setup_minimal_repo();
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "ls"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(predicate::eq(""));
}

#[test]
fn vp_lane_ls_shows_armed_performer_dir() {
    // <repo>/.vp/lanes/<name>/ を仕込めば ls に出る
    let repo = setup_minimal_repo();
    arm_performer_dir(repo.path(), "smoke-target");
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "ls"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("smoke-target"));
}

// --- vp lane path ---

#[test]
fn vp_lane_path_nonexistent_exits_nonzero() {
    let repo = setup_minimal_repo();
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "path", "nonexistent"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("見つかりません"));
}

#[test]
fn vp_lane_path_existing_prints_absolute_path() {
    let repo = setup_minimal_repo();
    arm_performer_dir(repo.path(), "found-performer");
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "path", "found-performer"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(".vp/lanes/found-performer"));
}

// --- vp lane rm ---

#[test]
fn vp_lane_rm_nonexistent_exits_nonzero() {
    let repo = setup_minimal_repo();
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "rm", "nonexistent"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("見つかりません"));
}

#[test]
fn vp_lane_rm_existing_removes_dir() {
    let repo = setup_minimal_repo();
    arm_performer_dir(repo.path(), "removable");
    let performer_dir = repo.path().join(".vp/lanes/removable");
    assert!(performer_dir.exists(), "事前条件: performer dir 存在");

    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "rm", "removable"])
        .current_dir(repo.path())
        .assert()
        .success();

    assert!(!performer_dir.exists(), "rm 後: performer dir 消滅");
}

#[test]
fn vp_lane_rm_all_without_force_errors() {
    let repo = setup_minimal_repo();
    arm_performer_dir(repo.path(), "guard-test");
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "rm", "--all"])
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));
}

// --- vp lane status ---

#[test]
fn vp_lane_status_empty_shows_help_hint() {
    let repo = setup_minimal_repo();
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "status"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("パフォーマーはありません"));
}

// --- vp lane cleanup ---

#[test]
fn vp_lane_cleanup_dryrun_in_empty_repo() {
    // --force なしの cleanup は dry-run + 削除候補表示。 .vp/lanes/ 不在なら
    // 「クリーンアップ対象はありません」 を返す
    let repo = setup_minimal_repo();
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "cleanup"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("クリーンアップ対象はありません"));
}

// --- name validation ---

#[test]
fn vp_lane_rm_with_invalid_name_errors() {
    let repo = setup_minimal_repo();
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "rm", "../escape"])
        .current_dir(repo.path())
        .assert()
        .failure();
}
