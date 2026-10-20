//! Startup policy regression: mem_1CgDnqNUYT1Z5eWFs9QA1H.
//! Run only startup configuration in child processes; never dispatch a daemon command.

use super::{Cli, Commands, Parser, configure_process};
use std::process::{Command, Output};

fn probe(
    args: &[&str],
    daemon: bool,
    log: Option<&str>,
    panic: bool,
) -> (Output, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", "startup_tests::startup_child", "--nocapture"])
        .env("VP_STARTUP_ARGS", serde_json::to_string(args).unwrap())
        .env("VP_STARTUP_DAEMON", daemon.to_string())
        .env("VP_STARTUP_PANIC", panic.to_string())
        .env("VP_PROFILE", "startup-test")
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_STATE_HOME", tmp.path().join("state"))
        .env("RUST_LOG", "error")
        .env_remove("VP_LOG");
    if let Some(log) = log {
        child.env("VP_DAEMON_LOG_FILE", log);
    } else {
        child.env_remove("VP_DAEMON_LOG_FILE");
    }
    (child.output().unwrap(), tmp)
}

#[test]
fn startup_policy_distinguishes_foreground_daemon_from_cli() {
    let cases: &[(&[&str], bool)] = &[
        (&["vp", "daemon"], true),
        (&["vp", "daemon", "--port", "32123"], true),
        (&["vp", "daemon", "start", "--port", "32123"], true),
        (&["vp"], false),
        (&["vp", "ps"], false),
        (&["vp", "mcp"], false),
        (&["vp", "daemon", "status"], false),
        (&["vp", "daemon", "stop"], false),
        (&["vp", "daemon", "restart"], false),
        (&["vp", "daemon", "processes", "--watch"], false),
        (&["vp", "daemon", "discover"], false),
        (&["vp", "daemon", "install"], false),
        (&["vp", "daemon", "uninstall"], false),
    ];
    for (args, daemon) in cases {
        let (output, _tmp) = probe(args, *daemon, Some("inherited-daemon.log"), false);
        assert!(output.status.success(), "{args:?}: {output:?}");
    }
    for log in [None, Some(" ")] {
        let (output, _tmp) = probe(&["vp", "daemon"], true, log, false);
        assert!(output.status.success(), "{log:?}: {output:?}");
    }
}

#[test]
fn cli_panic_does_not_append_to_inherited_daemon_log() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("daemon.kdl.log");
    std::fs::write(&log, "daemon log sentinel\n").unwrap();
    let (output, _child_tmp) = probe(&["vp", "daemon", "status"], false, log.to_str(), true);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("panic 発生: startup panic probe"),
        "{output:?}"
    );
    assert_eq!(
        std::fs::read_to_string(log).unwrap(),
        "daemon log sentinel\n"
    );
}

#[test]
fn startup_child() {
    let Ok(args) = std::env::var("VP_STARTUP_ARGS") else {
        return;
    };
    let args: Vec<String> = serde_json::from_str(&args).unwrap();
    let command = Cli::try_parse_from(args)
        .unwrap()
        .command
        .unwrap_or(Commands::Ps);
    let daemon = std::env::var("VP_STARTUP_DAEMON").unwrap() == "true";
    let inherited_log = std::env::var("VP_DAEMON_LOG_FILE").ok();
    configure_process(&command);

    // Query without replacing the installed disposition. All process-global changes
    // are confined to this subprocess, not the parent test runner.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut action) },
        0
    );
    assert_eq!(
        action.sa_sigaction,
        if daemon { libc::SIG_IGN } else { libc::SIG_DFL }
    );
    if daemon {
        let expected = inherited_log
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| {
                vantage_point::config::vp_log_dir()
                    .join("daemon.kdl.log")
                    .to_str()
                    .unwrap()
                    .to_owned()
            });
        assert_eq!(std::env::var("VP_DAEMON_LOG_FILE").unwrap(), expected);
        // Exercise a real SIGPIPE: the daemon startup policy must leave us alive.
        assert_eq!(unsafe { libc::raise(libc::SIGPIPE) }, 0);
    } else {
        assert!(std::env::var_os("VP_DAEMON_LOG_FILE").is_none());
    }
    if std::env::var("VP_STARTUP_PANIC").unwrap() == "true" {
        vantage_point::cli::init_tracing(vantage_point::cli::DebugMode::None, false);
        assert!(std::panic::catch_unwind(|| panic!("startup panic probe")).is_err());
    }
}
