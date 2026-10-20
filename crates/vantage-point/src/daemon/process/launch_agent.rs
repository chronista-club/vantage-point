//! LaunchAgent の登録解除・再登録・起動確認。OS と時計の境界をテストで差し替える。

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const UNLOAD_TIMEOUT: Duration = Duration::from_secs(10);
const UNLOAD_INTERVAL: Duration = Duration::from_millis(100);
const BOOTSTRAP_ATTEMPTS: usize = 3;
const RETRY_INTERVAL: Duration = Duration::from_millis(500);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(15);

trait Runtime {
    fn launchctl(&mut self, args: &[&str]) -> std::io::Result<Output>;
    fn healthy(&mut self, port: u16, timeout: Duration) -> bool;
    fn elapsed(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
}

struct System {
    started: Instant,
    client: reqwest::blocking::Client,
}

impl Runtime for System {
    fn launchctl(&mut self, args: &[&str]) -> std::io::Result<Output> {
        Command::new("launchctl").args(args).output()
    }

    fn healthy(&mut self, port: u16, timeout: Duration) -> bool {
        self.client
            .get(format!("http://[::1]:{port}/api/health"))
            .timeout(timeout.min(Duration::from_secs(2)))
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<crate::cli::HealthResponse>())
            .is_ok_and(|health| health.status == "ok" && health.pid > 0)
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub(super) fn reload(domain: &str, plist: &Path, port: u16) -> Result<()> {
    // client を作れない場合は bootout より前に失敗する。
    let mut runtime = System {
        started: Instant::now(),
        client: reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .context("daemon health client 作成失敗")?,
    };
    reload_with(&mut runtime, domain, plist, port)
}

fn reload_with(runtime: &mut impl Runtime, domain: &str, plist: &Path, port: u16) -> Result<()> {
    reload_inner(runtime, domain, plist, port).map_err(|error| {
        // path に空白や単引用符があっても、通常の shell に貼れる復旧コマンドにする。
        let quoted_plist = plist.to_string_lossy().replace('\'', "'\\''");
        anyhow::anyhow!(
            "{error:#}\n復旧: VP の外の terminal で `launchctl bootstrap {domain} '{quoted_plist}'` を実行し、`vp daemon status` で確認してください。"
        )
    })
}

fn reload_inner(runtime: &mut impl Runtime, domain: &str, plist: &Path, port: u16) -> Result<()> {
    let target = format!("{domain}/{}", super::LAUNCH_AGENT_LABEL);
    // 未登録時の非ゼロは許容し、実際に外れたかは print で確かめる。
    runtime
        .launchctl(&["bootout", &target])
        .context("launchctl bootout 実行失敗")?;
    let deadline = runtime.elapsed() + UNLOAD_TIMEOUT;
    loop {
        let out = runtime
            .launchctl(&["print", &target])
            .context("launchctl print 実行失敗")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // 権限エラー等を「未登録」と誤認して bootstrap しない。
            if out.status.code() == Some(113) && stderr.contains("Could not find service") {
                break;
            }
            bail!("launchctl print で登録解除を確認できません: {stderr}");
        }
        let remaining = deadline.saturating_sub(runtime.elapsed());
        if remaining.is_zero() {
            bail!("LaunchAgent の登録解除が 10s 以内に完了しませんでした: {target}");
        }
        runtime.sleep(remaining.min(UNLOAD_INTERVAL));
    }

    for attempt in 1..=BOOTSTRAP_ATTEMPTS {
        let out = runtime
            .launchctl(&["bootstrap", domain, &plist.to_string_lossy()])
            .context("launchctl bootstrap 実行失敗")?;
        if out.status.success() {
            break;
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if attempt == BOOTSTRAP_ATTEMPTS {
            bail!(
                "launchctl bootstrap が {attempt} 回失敗しました: {}",
                stderr.trim()
            );
        }
        tracing::warn!(attempt, "launchctl bootstrap を再試行: {}", stderr.trim());
        runtime.sleep(RETRY_INTERVAL);
    }

    let deadline = runtime.elapsed() + HEALTH_TIMEOUT;
    loop {
        let remaining = deadline.saturating_sub(runtime.elapsed());
        if remaining.is_zero() {
            bail!("daemon が 15s 以内に health 応答しませんでした (port {port})");
        }
        if runtime.healthy(port, remaining) {
            return Ok(());
        }
        runtime.sleep(
            deadline
                .saturating_sub(runtime.elapsed())
                .min(RETRY_INTERVAL),
        );
    }
}

#[cfg(test)]
mod tests {
    // mem_1CgDiXr51YQzyg6Hoo4wp7: OS 呼び出しと時計を差し替え、順序・失敗・待ち上限を固定する。
    use super::*;
    use std::collections::VecDeque;
    use std::os::unix::process::ExitStatusExt;

    struct Fake {
        commands: VecDeque<(&'static str, i32, &'static str)>,
        health: VecDeque<bool>,
        elapsed: Duration,
        sleeps: Vec<Duration>,
        health_calls: usize,
    }

    impl Fake {
        fn new(commands: Vec<(&'static str, i32, &'static str)>, health: Vec<bool>) -> Self {
            Self {
                commands: commands.into(),
                health: health.into(),
                elapsed: Duration::ZERO,
                sleeps: vec![],
                health_calls: 0,
            }
        }

        fn run(&mut self) -> Result<()> {
            reload_with(
                self,
                "gui/123",
                Path::new("/test user's/daemon.plist"),
                32123,
            )
        }
    }

    impl Runtime for Fake {
        fn launchctl(&mut self, args: &[&str]) -> std::io::Result<Output> {
            let (command, code, stderr) = self.commands.pop_front().expect("unexpected launchctl");
            assert_eq!(args[0], command);
            if command == "bootstrap" {
                assert_eq!(args, ["bootstrap", "gui/123", "/test user's/daemon.plist"]);
            } else {
                assert_eq!(
                    args[1],
                    format!("gui/123/{}", super::super::LAUNCH_AGENT_LABEL)
                );
            }
            Ok(Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: vec![],
                stderr: stderr.as_bytes().to_vec(),
            })
        }

        fn healthy(&mut self, port: u16, timeout: Duration) -> bool {
            assert_eq!(port, 32123);
            assert!(timeout > Duration::ZERO && timeout <= HEALTH_TIMEOUT);
            self.health_calls += 1;
            self.health.pop_front().expect("unexpected health probe")
        }

        fn elapsed(&self) -> Duration {
            self.elapsed
        }

        fn sleep(&mut self, duration: Duration) {
            self.elapsed += duration;
            self.sleeps.push(duration);
        }
    }

    fn absent() -> (&'static str, i32, &'static str) {
        ("print", 113, "Could not find service")
    }

    #[test]
    fn waits_for_unload_retries_bootstrap_and_waits_for_health() {
        let mut runtime = Fake::new(
            vec![
                ("bootout", 0, ""),
                ("print", 0, ""),
                ("print", 0, ""),
                absent(),
                ("bootstrap", 5, "Bootstrap failed: 5: Input/output error"),
                ("bootstrap", 0, ""),
            ],
            vec![false, true],
        );
        runtime.run().unwrap();
        assert!(runtime.commands.is_empty());
        assert!(runtime.health.is_empty());
        assert_eq!(
            runtime.sleeps,
            [
                UNLOAD_INTERVAL,
                UNLOAD_INTERVAL,
                RETRY_INTERVAL,
                RETRY_INTERVAL
            ]
        );
    }

    #[test]
    fn bootstrap_exhaustion_returns_error_and_recovery_without_health() {
        let mut runtime = Fake::new(
            vec![
                ("bootout", 0, ""),
                absent(),
                ("bootstrap", 5, "Bootstrap failed: 5"),
                ("bootstrap", 5, "Bootstrap failed: 5"),
                ("bootstrap", 5, "Bootstrap failed: 5"),
            ],
            vec![],
        );
        let error = runtime.run().unwrap_err().to_string();
        assert!(error.contains("Bootstrap failed: 5"), "{error}");
        assert!(
            error.contains("launchctl bootstrap gui/123 '/test user'\\''s/daemon.plist'"),
            "{error}"
        );
        assert!(runtime.commands.is_empty());
        assert_eq!(runtime.sleeps, [RETRY_INTERVAL, RETRY_INTERVAL]);
        assert_eq!(runtime.health_calls, 0);
    }

    #[test]
    fn unload_timeout_does_not_bootstrap() {
        let mut commands = vec![("bootout", 0, "")];
        commands.extend(std::iter::repeat_n(("print", 0, ""), 101));
        let mut runtime = Fake::new(commands, vec![]);
        let error = runtime.run().unwrap_err().to_string();
        assert!(error.contains("登録解除が 10s"), "{error}");
        assert_eq!(runtime.elapsed, UNLOAD_TIMEOUT);
        assert!(runtime.commands.is_empty());
        assert_eq!(runtime.health_calls, 0);
    }

    #[test]
    fn print_permission_error_is_not_service_absence() {
        let mut runtime = Fake::new(
            vec![
                ("bootout", 1, "Operation not permitted"),
                ("print", 1, "Operation not permitted"),
            ],
            vec![],
        );
        let error = runtime.run().unwrap_err().to_string();
        assert!(error.contains("Operation not permitted"), "{error}");
        assert_eq!(runtime.elapsed, Duration::ZERO);
        assert_eq!(runtime.health_calls, 0);
    }

    #[test]
    fn fresh_install_accepts_absent_job() {
        let mut runtime = Fake::new(
            vec![
                ("bootout", 113, "Could not find service"),
                absent(),
                ("bootstrap", 0, ""),
            ],
            vec![true],
        );
        runtime.run().unwrap();
        assert!(runtime.commands.is_empty());
        assert_eq!(runtime.health_calls, 1);
        assert!(runtime.sleeps.is_empty());
    }

    #[test]
    fn successful_bootstrap_without_health_is_an_error() {
        let mut runtime = Fake::new(
            vec![("bootout", 0, ""), absent(), ("bootstrap", 0, "")],
            vec![false; 30],
        );
        let error = runtime.run().unwrap_err().to_string();
        assert!(error.contains("health 応答"), "{error}");
        assert!(error.contains("launchctl bootstrap gui/123"), "{error}");
        assert_eq!(runtime.elapsed, HEALTH_TIMEOUT);
        assert!(runtime.health.is_empty());
    }
}
