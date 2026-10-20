//! macOS の bundle 起動コマンド。構築と実行を分け、GUI を立てずに検証する。

use std::path::Path;
use std::process::Command;

pub(crate) fn launch_services_command(
    bin: &Path,
    stdout_log: &Path,
    daemon_log: &Path,
) -> std::io::Result<Option<Command>> {
    // PATH の symlink や相対 VP_APP_BIN も、選ばれた実体の bundle に解決する。
    let bin = dunce::canonicalize(bin)?;
    let Some(macos) = bin.parent().filter(|p| p.ends_with("Contents/MacOS")) else {
        return Ok(None);
    };
    let Some(bundle) = macos.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    if bundle.extension().is_none_or(|ext| ext != "app") {
        return Ok(None);
    }

    // GUI を CLI の子として spawn すると、親の launchd job の QoS clamp も継承する。
    // LaunchServices に起動を頼み、stdout/stderr は従来のログへ追記する。
    // open(1) は呼び出し元の環境を GUI へ渡すので VP_PROFILE / XDG_* も保たれる。
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-a")
        .arg(bundle)
        .arg("--stdout")
        .arg(stdout_log)
        .arg("--stderr")
        .arg(stdout_log)
        .env("VP_DAEMON_LOG_FILE", daemon_log);
    Ok(Some(cmd))
}

#[cfg(test)]
mod tests {
    // task: mem_1CgDrXvGhTr9k3kqbV4M2N
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "vp-app-launch-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self(dunce::canonicalize(root).unwrap())
        }

        fn binary(&self, relative: &str) -> PathBuf {
            let bin = self.0.join(relative);
            std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
            std::fs::write(&bin, "test executable placeholder").unwrap();
            bin
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn bundle_uses_launch_services_and_preserves_log_paths() {
        let fixture = Fixture::new();
        let bin = fixture.binary("Vantage Point.app/Contents/MacOS/vp-app");
        let stdout = fixture.0.join("logs with spaces/app.stdout.log");
        let daemon = fixture.0.join("logs with spaces/daemon.kdl.log");
        let cmd = launch_services_command(&bin, &stdout, &daemon)
            .unwrap()
            .expect("bundle GUI must not be spawned directly under its parent's QoS clamp");
        assert_eq!(cmd.get_program(), OsStr::new("/usr/bin/open"));
        let bundle = fixture.0.join("Vantage Point.app");
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("-a"),
                bundle.as_os_str(),
                OsStr::new("--stdout"),
                stdout.as_os_str(),
                OsStr::new("--stderr"),
                stdout.as_os_str(),
            ]
        );
        assert_eq!(
            cmd.get_envs().collect::<Vec<_>>(),
            vec![(OsStr::new("VP_DAEMON_LOG_FILE"), Some(daemon.as_os_str()))]
        );
    }

    #[test]
    fn symlink_to_bundle_binary_opens_the_resolved_bundle() {
        let fixture = Fixture::new();
        let bin = fixture.binary("VantagePoint.app/Contents/MacOS/vp-app");
        let link = fixture.0.join("vp-app");
        std::os::unix::fs::symlink(&bin, &link).unwrap();
        let log = fixture.0.join("app.log");
        let cmd = launch_services_command(&link, &log, &log)
            .unwrap()
            .expect("symlinked installed binary must use LaunchServices too");
        assert_eq!(
            cmd.get_args().nth(1),
            Some(fixture.0.join("VantagePoint.app").as_os_str())
        );
    }

    #[test]
    fn development_binaries_keep_direct_spawn() {
        let fixture = Fixture::new();
        let log = fixture.0.join("app.log");
        for relative in [
            "target/release/vp-app",
            "not-a-bundle/Contents/MacOS/vp-app",
            "VantagePoint.app/other/vp-app",
        ] {
            let bin = fixture.binary(relative);
            assert!(launch_services_command(&bin, &log, &log).unwrap().is_none());
        }
    }
}
