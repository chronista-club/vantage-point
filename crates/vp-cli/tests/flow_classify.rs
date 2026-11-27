// mem_1ChSjZ8945Y4MsvU4F2qEf
use std::{fs, path::Path, process::Command};

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repo(dir: &Path, base: &str) {
    fs::create_dir_all(dir.join(".vp")).unwrap();
    fs::write(
        dir.join(".vp/sub-files.kdl"),
        format!("base-ref \"{base}\"\n"),
    )
    .unwrap();
    fs::write(dir.join(".gitignore"), ".vp/\n").unwrap();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "base"]);
    git(dir, &["update-ref", "refs/remotes/origin/nightly", "HEAD"]);
    git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
}

#[test]
fn classify_uses_target_repo_even_with_foreign_git_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target");
    let foreign = tmp.path().join("foreign");
    repo(&target, "nightly");
    repo(&foreign, "main");
    fs::write(target.join("AGENTS.md"), "dangerous change\n").unwrap();
    git(&target, &["add", "."]);
    git(&target, &["commit", "-m", "rules"]);
    git(&target, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    // Test both configured base-ref and origin/HEAD fallback. Neither may consult foreign refs.
    for configured in [true, false] {
        if !configured {
            fs::remove_file(target.join(".vp/sub-files.kdl")).unwrap();
            git(
                &target,
                &[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/nightly",
                ],
            );
        }
        for env in [
            None,
            Some("GIT_DIR"),
            Some("GIT_COMMON_DIR"),
            Some("GIT_WORK_TREE"),
            Some("GIT_INDEX_FILE"),
        ] {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_vp"));
            cmd.current_dir(&foreign)
                .args(["flow", "classify", "--format", "json", "--path"])
                .arg(&target);
            if let Some(name) = env {
                let value = match name {
                    "GIT_WORK_TREE" => foreign.clone(),
                    "GIT_INDEX_FILE" => foreign.join(".git/index"),
                    _ => foreign.join(".git"),
                };
                cmd.env(name, value);
            }
            let out = cmd.output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(
                json["tier"], "C",
                "configured={configured}, env={env:?}: {json}"
            );
            assert_eq!(json["changed_files"], 1);
        }
    }
}
