#![cfg(target_os = "macos")]
// mem_1CgDiXr51YQzyg6Hoo4wp7: 実 launchctl と user の HOME を使わない install 回帰テスト。

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn install_reports_bootstrap_failure() {
    let home = tempfile::tempdir().unwrap();
    let launchctl = home.path().join("launchctl");
    std::fs::write(
        &launchctl,
        "#!/bin/sh\ncase \"$1\" in\nprint) echo 'Could not find service' >&2; exit 113;;\nbootstrap) echo 'Bootstrap failed: 5: Input/output error' >&2; exit 5;;\n*) exit 0;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&launchctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "isolated_install_worker", "--nocapture"])
        .env("VP_INSTALL_TEST_HOME", home.path())
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("PATH", home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn isolated_install_worker() {
    let Some(home) = std::env::var_os("VP_INSTALL_TEST_HOME") else {
        return;
    };
    assert_eq!(std::env::var_os("HOME").unwrap(), home);
    assert_eq!(std::env::var_os("PATH").unwrap(), home);
    let result =
        vantage_point::daemon::process::install_launch_agent(std::path::Path::new("/test/vp"), 0);
    assert!(result.is_err(), "bootstrap failure must not report success");
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("Bootstrap failed: 5"), "{error}");
    assert!(error.contains("launchctl bootstrap"), "{error}");
}
