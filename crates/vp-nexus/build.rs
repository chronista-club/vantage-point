//! vp-nexus build script — build 時の git_sha + built_at を env var に埋め込む。
//!
//! 設計方針: `chrono` / `time` / `vergen` 等の crate を増やさず、 shell out
//! (= `git` + `date`) で値を取得する。 vp-nexus crate の
//! 「workspace deps のみで完結」 という独立性方針 (= PR #462) に揃える。
//!
//! ## 痛点 1-2 の解消 (= dogfood 3 / PR #466、 SDG v2 supersede input)
//!
//! - **痛点 1 (dirty 出ない)**: `git diff --quiet --ignore-submodules` で
//!   working tree dirty を check、 dirty なら `<sha>-dirty` suffix を付ける
//!   (= Linux kbuild 慣行と一致)。
//! - **痛点 2 (rebuild trigger 不足)**: `.git/HEAD` と現在 branch の ref path を
//!   動的に解決して `cargo:rerun-if-changed` で監視。 branch 切替 / 同 branch
//!   内 commit を検知して env var を再生成。
//!
//! ## fallback
//!
//! - git command 不在 / `.git` 不在 → `git_sha = "unknown"`、 suffix なし
//! - detached HEAD → ref path watch は skip、 HEAD watch のみで gracefully degrade

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let git_sha_raw = capture("git", &["rev-parse", "--short=12", "HEAD"]);
    let git_sha = if git_sha_raw != "unknown" && is_dirty() {
        format!("{git_sha_raw}-dirty")
    } else {
        git_sha_raw
    };
    println!("cargo:rustc-env=NEXUS_GIT_SHA={git_sha}");

    // RFC3339 UTC (= "YYYY-MM-DDTHH:MM:SSZ")。 macOS / Linux 共通の date 形式。
    let built_at = capture("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]);
    println!("cargo:rustc-env=NEXUS_BUILT_AT={built_at}");

    println!("cargo:rerun-if-changed=build.rs");
    emit_git_rerun_triggers();
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

/// working tree が dirty かを返す。 `git diff --quiet --ignore-submodules` の
/// exit code で判定 (= exit 0 clean / 1 dirty)。 失敗時は false (= 安全側、
/// dirty 扱いしない)。
fn is_dirty() -> bool {
    Command::new("git")
        .args(["diff", "--quiet", "--ignore-submodules"])
        .status()
        .ok()
        .map(|s| !s.success())
        .unwrap_or(false)
}

/// `.git/HEAD` と現在 branch の ref path を rebuild trigger に登録する。
///
/// - HEAD watch: branch 切替で再 build
/// - ref watch: 同 branch 内 commit で再 build
///
/// detached HEAD / `.git` 不在は gracefully degrade (= HEAD のみ or 出力なし)。
fn emit_git_rerun_triggers() {
    let Some(git_dir) = locate_git_dir() else {
        return;
    };

    let head_path = git_dir.join("HEAD");
    if !head_path.exists() {
        return;
    }
    println!("cargo:rerun-if-changed={}", head_path.display());

    // HEAD content から `ref: refs/heads/<branch>` を resolve、 ref path も watch
    if let Some(ref_relpath) = read_head_ref(&head_path) {
        let ref_path = git_dir.join(&ref_relpath);
        if ref_path.exists() {
            println!("cargo:rerun-if-changed={}", ref_path.display());
        }
    }
    // detached HEAD (= HEAD が直接 commit sha を保持) なら ref watch は skip、
    // HEAD watch のみで gracefully degrade
}

/// vp-nexus crate (= `crates/vp-nexus/`) から 2 階層上の `.git` directory を返す。
/// workspace root に `.git` がある前提 (= 通常の cargo workspace 配置)。
fn locate_git_dir() -> Option<PathBuf> {
    // CARGO_MANIFEST_DIR = crates/vp-nexus、 そこから ../../.git
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let git_dir = Path::new(&manifest).join("..").join("..").join(".git");
    if git_dir.exists() {
        Some(git_dir)
    } else {
        None
    }
}

/// `.git/HEAD` から `ref: refs/heads/<branch>` を resolve。
/// detached HEAD (= HEAD が直接 sha を保持) は None。
fn read_head_ref(head_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(head_path).ok()?;
    content.trim().strip_prefix("ref: ").map(|s| s.to_string())
}
