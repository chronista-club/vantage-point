//! vp-nexus build script — build 時の git_sha + built_at を env var に埋め込む。
//!
//! 設計方針: `chrono` / `time` / `vergen` 等の crate を増やさず、 shell out
//! (= `git rev-parse` + `date -u`) で値を取得する。 vp-nexus crate の
//! 「workspace deps のみで完結」 という独立性方針 (= PR #462) に揃える。
//!
//! - 取得失敗時は `"unknown"` フォールバック (= source tarball build / git 無し
//!   環境 / Windows でも build 自体は成立させる)。
//! - rebuild trigger は build.rs 自身の変更のみ。 厳密な git HEAD 追跡は
//!   後続 task で改善可能 (= 現状 dogfood 規約検証が主目的、 yagni)。

use std::process::Command;

fn main() {
    let git_sha = capture("git", &["rev-parse", "--short=12", "HEAD"]);
    println!("cargo:rustc-env=NEXUS_GIT_SHA={git_sha}");

    // RFC3339 UTC (= "YYYY-MM-DDTHH:MM:SSZ")。 macOS / Linux 共通の date 形式。
    let built_at = capture("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]);
    println!("cargo:rustc-env=NEXUS_BUILT_AT={built_at}");

    println!("cargo:rerun-if-changed=build.rs");
}

/// shell command を実行し stdout を trim して返す。 失敗時は "unknown"。
fn capture(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
