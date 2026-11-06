//! 起動時のログ詳細度と子プロセスへの環境継承を、daemon を起動せず検証する。
//! Spec: mem_1CgEApaZrUnceK1YdA1fAX。

use super::{Cli, Commands, Parser, configure_process};
use std::process::Command;

fn probe(args: &[&str], settings: Option<&str>, env: &[(&str, &str)], level: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config/vp-logging-test");
    std::fs::create_dir_all(&config).unwrap();
    if let Some(level) = settings {
        std::fs::write(
            config.join("settings.kdl"),
            format!("log-level \"{level}\"\n"),
        )
        .unwrap();
    }
    let log = tmp.path().join("daemon.log");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "logging_tests::logging_child", "--nocapture"])
        .env("VP_LOGGING_ARGS", serde_json::to_string(args).unwrap())
        .env("VP_PROFILE", "logging-test")
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("XDG_STATE_HOME", tmp.path().join("state"))
        .env("VP_DAEMON_LOG_FILE", &log)
        .env_remove("RUST_LOG")
        .env_remove("VP_LOG")
        .env_remove("VANTAGE_DEBUG")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}, {env:?}: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let logs = if log.exists() {
        assert!(stderr.is_empty(), "{stderr}");
        std::fs::read_to_string(log).unwrap()
    } else {
        stderr.into_owned()
    };
    for (marker, enabled) in [
        ("logging-error", true),
        ("logging-warn", level != "error"),
        ("logging-info", matches!(level, "info" | "debug")),
        ("logging-debug", level == "debug"),
    ] {
        assert_eq!(logs.contains(marker), enabled, "{args:?}, {env:?}: {logs}");
    }
}

#[test]
fn cli_defaults_to_warn_even_with_daemon_settings() {
    for args in [
        vec!["vp"],
        vec!["vp", "mcp"],
        vec!["vp", "daemon", "status"],
    ] {
        for settings in [None, Some("debug")] {
            probe(&args, settings, &[], "warn");
        }
    }
}

#[test]
fn explicit_logging_env_overrides_cli_default() {
    for (env, level) in [
        (vec![("VP_LOG", "info")], "info"),
        (vec![("RUST_LOG", "vantage_point=debug")], "debug"),
        (vec![("VANTAGE_DEBUG", "simple")], "info"),
        (vec![("VANTAGE_DEBUG", "detail")], "debug"),
        (
            vec![("VP_LOG", "warn"), ("RUST_LOG", "vantage_point=debug")],
            "warn",
        ),
        (
            vec![
                ("RUST_LOG", "vantage_point=error"),
                ("VANTAGE_DEBUG", "detail"),
            ],
            "error",
        ),
    ] {
        probe(&["vp", "daemon", "status"], Some("debug"), &env, level);
    }
}

#[test]
fn daemon_keeps_info_default_and_settings_override() {
    for args in [vec!["vp", "daemon"], vec!["vp", "daemon", "start"]] {
        probe(&args, None, &[], "info");
        probe(&args, Some("debug"), &[], "debug");
        probe(&args, Some("invalid"), &[], "info");
    }
}

#[test]
fn daemon_logging_overrides_do_not_change_child_rust_log() {
    for (env, level) in [
        (vec![("VP_LOG", "info")], "info"),
        (vec![("VANTAGE_DEBUG", "simple")], "info"),
        (vec![("VANTAGE_DEBUG", "detail")], "debug"),
        (vec![("RUST_LOG", "vantage_point=warn")], "warn"),
        (
            vec![("VP_LOG", "info"), ("RUST_LOG", "vantage_point=warn")],
            "info",
        ),
    ] {
        probe(&["vp", "daemon"], Some("error"), &env, level);
    }
}

#[test]
fn logging_child() {
    let Ok(args) = std::env::var("VP_LOGGING_ARGS") else {
        return;
    };
    let args: Vec<String> = serde_json::from_str(&args).unwrap();
    let command = Cli::try_parse_from(args)
        .unwrap()
        .command
        .unwrap_or(Commands::Ps);
    let inherited = std::env::var("RUST_LOG").ok();
    let role = configure_process(&command);
    vantage_point::cli::init_tracing(
        vantage_point::cli::parse_debug_env().unwrap_or_default(),
        false,
        role,
    );
    tracing::error!(target: "vantage_point", "logging-error");
    tracing::warn!(target: "vantage_point", "logging-warn");
    tracing::info!(target: "vantage_point", "logging-info");
    tracing::debug!(target: "vantage_point", "logging-debug");

    // PTY / chat と同じ OS の環境継承を使う。期待値の指定以外、環境を加工しない。
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "logging_tests::inherited_env_child",
            "--nocapture",
        ])
        .env(
            "VP_EXPECTED_RUST_LOG",
            serde_json::to_string(&inherited).unwrap(),
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn inherited_env_child() {
    let Ok(expected) = std::env::var("VP_EXPECTED_RUST_LOG") else {
        return;
    };
    let expected: Option<String> = serde_json::from_str(&expected).unwrap();
    assert_eq!(std::env::var("RUST_LOG").ok(), expected);
}
