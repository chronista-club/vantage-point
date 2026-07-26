//! resume 失敗の観測装置（F4、解剖 memory `cc-session-pointer-self-destruction`）
//!
//! tui type-ahead の `claude --resume 'X' … || vp lane resume-failed 'X' || claude …` chain の
//! 中継として呼ばれ、「resume が失敗して fresh に fallback した」事象を disk に残す。
//! 従来この fallback は無音で、復帰失敗の真因（session-in-use / transcript 消失 / …）を
//! 後から辿る証拠が一切残らなかった。
//!
//! - 置き場: `vp_state_dir()/log/resume_failures.log`（append-only、1 行 1 事象）
//! - 呼び手: `vp lane resume-failed <attempted>`（記録して**常に exit 1** = `||` chain の中継専用。
//!   shell group `{ …; }` を使わないのは fish 互換のため — slot の shell は user の login shell）

use std::io::Write;
use std::path::{Path, PathBuf};

/// state base 配下の log file path（純関数、テスト用に base 注入）
pub fn log_path_in(base: &Path) -> PathBuf {
    base.join("log").join("resume_failures.log")
}

/// 1 事象を追記する。形式: `ts=<rfc3339>\trepo=<p>\tlane=<l>\tattempted=<id|continue>`
pub fn append_in(base: &Path, repo: &str, lane: &str, attempted: &str) -> std::io::Result<()> {
    let path = log_path_in(base);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(
        f,
        "ts={}\trepo={}\tlane={}\tattempted={}",
        chrono::Utc::now().to_rfc3339(),
        repo,
        lane,
        attempted
    )
}

/// 本番 base（vp_state_dir）での append（`vp lane resume-failed` から呼ぶ）
pub fn append(repo: &str, lane: &str, attempted: &str) -> std::io::Result<()> {
    append_in(&crate::config::vp_state_dir(), repo, lane, attempted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 呼び出し = 1 行 append、既存行は保持される（append-only の担保）。
    #[test]
    fn append_writes_one_line_per_event() {
        let tmp = tempfile::tempdir().expect("tempdir");
        append_in(tmp.path(), "vp", "root", "abc-123").expect("append 1");
        append_in(tmp.path(), "vp", "root", "continue").expect("append 2");
        let body = std::fs::read_to_string(log_path_in(tmp.path())).expect("read");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "1 事象 1 行: {body}");
        assert!(
            lines[0].contains("repo=vp") && lines[0].contains("attempted=abc-123"),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("attempted=continue"), "{}", lines[1]);
    }
}
