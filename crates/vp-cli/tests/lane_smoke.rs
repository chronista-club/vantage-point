//! `vp lane` subcommand の end-to-end smoke test (VP-13 sub-scope B)。
//!
//! lane refactor 5 PR で大量変更した CLI 経路の regression net。 read-only / pure CLI
//! command を覆い、 setup-heavy な `lane new` / `lane fork` (= git clone + remote 要)
//! は別 PR で integration test 化を検討。
//!
//! ## fixture 方針
//!
//! - `tempfile::TempDir` で隔離 fixture (= test 並列実行で衝突しない)
//! - `git init` + initial commit + `.claude/sub-files.kdl` placeholder で最小 sub 環境
//! - `<repo>/.vp/lanes/<name>/.git/HEAD` を仕込んで `list_subs_for_repo` が拾うかを test
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

/// `<repo>/.vp/lanes/<name>/.git/HEAD` を仕込んで sub として認識される状態にする
/// (= `list_subs_for_repo` が disk scan で拾う、 actual git clone は不要)。
fn arm_sub_dir(repo: &Path, name: &str) {
    let sub = repo.join(".vp").join("lanes").join(name);
    fs::create_dir_all(sub.join(".git")).unwrap();
    fs::write(sub.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
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
fn vp_lane_ls_shows_armed_sub_dir() {
    // <repo>/.vp/lanes/<name>/ を仕込めば ls に出る
    let repo = setup_minimal_repo();
    arm_sub_dir(repo.path(), "smoke-target");
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
    arm_sub_dir(repo.path(), "found-sub");
    // PathBuf::join はパス区切りに OS 既定を使う（Unix: /、Windows: \）。
    // Windows では結合部が `.vp\lanes\found-sub` となるため期待値を分岐する。
    #[cfg(not(windows))]
    let expected = ".vp/lanes/found-sub";
    #[cfg(windows)]
    let expected = ".vp\\lanes\\found-sub";
    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "path", "found-sub"])
        .current_dir(repo.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(expected));
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
    arm_sub_dir(repo.path(), "removable");
    let sub_dir = repo.path().join(".vp/lanes/removable");
    assert!(sub_dir.exists(), "事前条件: sub dir 存在");

    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "rm", "removable"])
        .current_dir(repo.path())
        .assert()
        .success();

    assert!(!sub_dir.exists(), "rm 後: sub dir 消滅");
}

#[test]
fn vp_lane_rm_all_without_force_errors() {
    let repo = setup_minimal_repo();
    arm_sub_dir(repo.path(), "guard-test");
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
        .env("VP_TEST_NO_RUNNING_LANES", "1")
        .current_dir(repo.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("クリーンアップ対象はありません"));
}

/// 回帰固定（doc 44 P3）: `--force` は worktree だけでなく **共有 `.git` の branch も掃除する**。
///
/// P3 の Host 移管で `git branch -d` が **一度も実行されない**状態になっていた。
/// `remove_sub_workspace` が worktree ディレクトリごと消すため、その後に
/// `get_branch(&path)` を呼ぶと `git` の cwd が無く `output()` が Err → 常に `None` に落ちる。
/// 修正は branch 名を `LaneFacts` に削除前から持たせること。
///
/// **この e2e が要る理由**: 単体テスト（`branch_cannot_be_read_after_ground_is_removed`）は
/// 「消えた dir から引けない」という**前提**を固定するだけで、`cleanup_subs` が実際に
/// facts 側を読んでいることは見ていない。将来また `get_branch(&path)` に戻しても単体は通る。
/// ここは**壊れた経路そのもの**（cleanup --force → 親 repo の branch 一覧）を直接見る。
#[test]
fn vp_lane_cleanup_force_removes_merged_branch_from_shared_git() {
    let repo = setup_minimal_repo();
    let root = repo.path();
    let git = |args: &[&str], cwd: &std::path::Path| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git 実行");
    };

    // origin を用意（merged 判定は origin/<default> を見るため remote が要る）
    let origin = tempfile::tempdir().unwrap();
    git(&["init", "--quiet", "--bare"], origin.path());
    git(
        &["remote", "add", "origin", &origin.path().to_string_lossy()],
        root,
    );
    git(&["push", "--quiet", "origin", "main"], root);
    git(&["remote", "set-head", "origin", "main"], root);

    // lane worktree を作り、そこで 1 commit 積む
    let lane_dir = root.join(".vp/lanes/done");
    git(
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feat-done",
            &lane_dir.to_string_lossy(),
        ],
        root,
    );
    fs::write(lane_dir.join("x.txt"), "x\n").unwrap();
    git(&["add", "."], &lane_dir);
    git(&["commit", "--quiet", "-m", "work"], &lane_dir);

    // main に取り込んで push（= Host が「見送ってよい」と判定する状態）
    git(
        &["merge", "--quiet", "--no-ff", "feat-done", "-m", "m"],
        root,
    );
    git(&["push", "--quiet", "origin", "main"], root);
    git(&["fetch", "--quiet", "origin"], root);

    let branches_before = String::from_utf8(
        std::process::Command::new("git")
            .args(["branch", "--list", "feat-done"])
            .current_dir(root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        branches_before.contains("feat-done"),
        "事前条件: branch が存在する"
    );

    Command::cargo_bin("vp")
        .unwrap()
        .args(["lane", "cleanup", "--force"])
        .env("VP_TEST_NO_RUNNING_LANES", "1")
        .current_dir(root)
        .assert()
        .success();

    assert!(!lane_dir.exists(), "worktree dir は消える");

    let branches_after = String::from_utf8(
        std::process::Command::new("git")
            .args(["branch", "--list", "feat-done"])
            .current_dir(root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        branches_after.trim().is_empty(),
        "merge 済 branch は共有 .git からも消える（never-fire 回帰の固定）: {branches_after:?}"
    );
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
