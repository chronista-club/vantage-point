//! Closed-pipe regression: mem_1CgDnqNUYT1Z5eWFs9QA1H.
#![cfg(unix)]

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

#[test]
fn closed_stdout_does_not_panic_or_pollute_daemon_log() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("daemon.kdl.log");
    std::fs::write(&log, "daemon log sentinel\n").unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    // Close before spawn: no race with a small output fitting in the pipe buffer.
    drop(reader);
    let output = Command::new(env!("CARGO_BIN_EXE_vp"))
        .args(["db", "path"])
        .env("VP_PROFILE", "broken-pipe-test")
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("XDG_STATE_HOME", tmp.path().join("state"))
        .env("VP_DAEMON_LOG_FILE", &log)
        .env("RUST_LOG", "error")
        .env_remove("VP_LOG")
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(101), "{:?}", output);
    assert_eq!(output.status.signal(), Some(13), "{:?}", output);
    assert!(output.stderr.is_empty(), "{:?}", output);
    assert_eq!(
        std::fs::read_to_string(log).unwrap(),
        "daemon log sentinel\n"
    );
}

#[test]
fn terminal_panic_is_reported_on_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "terminal_panic_child", "--nocapture"])
        .env("VP_TERMINAL_PANIC_PROBE", "1")
        .env("XDG_CONFIG_HOME", tmp.path())
        .env("RUST_LOG", "error")
        .env_remove("VP_LOG")
        .env_remove("VP_DAEMON_LOG_FILE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("panic 発生: terminal panic probe"),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("terminal panic probe"));
}

#[test]
fn terminal_panic_child() {
    if std::env::var_os("VP_TERMINAL_PANIC_PROBE").is_none() {
        return;
    }
    // The hook and subscriber are process-global, so exercise them in a child.
    vantage_point::cli::init_tracing(
        vantage_point::cli::DebugMode::None,
        false,
        vantage_point::cli::TracingRole::Cli,
    );
    assert!(std::panic::catch_unwind(|| panic!("terminal panic probe")).is_err());
}
