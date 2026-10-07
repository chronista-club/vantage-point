use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::Path;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for part in ["config/vp", "repo/.git", "repo/.vp/lanes/demo/.git"] {
        std::fs::create_dir_all(dir.path().join(part)).unwrap();
    }
    let repo = std::path::PathBuf::from(vantage_point::config::Config::normalize_path(
        &dir.path().join("repo"),
    ));
    std::fs::write(
        dir.path().join("config/vp/repos.kdl"),
        format!(
            "repo name=\"example\" path={}\n",
            serde_json::to_string(repo.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    dir
}
fn cmd(dir: &Path, cwd: &str) -> Command {
    let mut cmd = Command::cargo_bin("vp").unwrap();
    cmd.env_remove("VP_PROFILE")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("VP_REPO", "wrong-inherited-repo")
        .env("VP_LANE", "wrong-inherited-lane")
        .current_dir(dir.join(cwd));
    cmd
}
fn run(dir: &Path, cwd: &str, args: &[&str]) -> Value {
    let result = cmd(dir, cwd).args(args).assert().success();
    serde_json::from_slice(&result.get_output().stdout).unwrap()
}
#[test]
fn lane_url_cli_shares_the_ui_store_and_uses_cwd_even_with_inherited_env() {
    let tmp = fixture();
    let d = tmp.path();
    let first = run(
        d,
        "repo/.vp/lanes/demo",
        &[
            "lane",
            "url",
            "set",
            "preview",
            "http://localhost:5173",
            "--label",
            "UI",
        ],
    );
    assert_eq!(first["lane"], "demo");
    assert_eq!(first["entries"][0]["id"], "preview");
    let next = run(
        d,
        "repo",
        &[
            "lane",
            "url",
            "set",
            "preview",
            "http://localhost:5174",
            "--lane",
            "demo",
        ],
    );
    assert_eq!(next["entries"].as_array().unwrap().len(), 1);
    assert_eq!(next["entries"][0]["label"], "UI");
    assert!(
        run(d, "repo", &["lane", "url", "list"])["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let got = run(
        d,
        "repo",
        &["lane", "url", "list", "--lane", "example/demo"],
    );
    assert_eq!(got["entries"], next["entries"]);
    let path = d.join("state/vp/lane-local-urls.json");
    let stored: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let repo = std::path::PathBuf::from(vantage_point::config::Config::normalize_path(
        &d.join("repo"),
    ));
    let key = serde_json::to_string(&(repo.to_str().unwrap(), "demo")).unwrap();
    assert_eq!(stored[&key], next["entries"]);
    assert!(
        run(
            d,
            "repo",
            &["lane", "url", "rm", "preview", "--lane", "demo"]
        )["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    cmd(d, "repo")
        .args(["lane", "url", "list", "--lane", "typo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Lane"));
}
