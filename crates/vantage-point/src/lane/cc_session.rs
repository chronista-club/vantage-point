//! lane ごとの CC session id 永続化 (R3-b、 設計 mem_1CbXZyCiqrdgteGhRFDaHW)
//!
//! `claude --continue` は「cwd の最新 session」を拾うため、 background session が
//! 居ると Agent View dashboard を開いて send-keys が詰まる (CC 2.1 罠)。
//! 特定 session を `--resume <id>` で指名すれば構造的に回避できる — その id を
//! lane 単位で保持するのが本 module。
//!
//! - **書き手**: `vp wire hook-check` (SessionStart で自 session_id を記録)。
//!   spawn 時に旧 session は `agents --json` に出ない (死んでいる) ため、
//!   生きているうちに hook で自己申告させる — 収穫経路を Phase A poll から
//!   変更した理由 (設計メモからの実装修正、 R3-b PR 参照)
//! - **読み手**: `vp lane last-session` (echoes task が spawn 時に呼ぶ) /
//!   GET /api/lanes の lazy populate (可視化、 performer_status と同じ前例)
//! - 置き場: `vp_state_dir()/cc_sessions/<project>__<lane>` (1 lane 1 file 1 行)

use std::path::{Path, PathBuf};

/// file 名に使えない文字を潰す (path traversal / separator 防止)
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect()
}

/// state base dir 配下の session file path (純関数、 テスト用に base 注入)
pub fn session_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("cc_sessions")
        .join(format!("{}__{}", sanitize(project), sanitize(lane)))
}

/// session id を記録する (上書き、 1 行)
pub fn record_in(base: &Path, project: &str, lane: &str, session_id: &str) -> std::io::Result<()> {
    let path = session_file_in(base, project, lane);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, session_id)
}

/// 最後に記録された session id を返す。
///
/// 無い / 空 / uuid 形式外 (英数とハイフン以外を含む) は None — 壊れた file を
/// `--resume` に渡さない + echoes task の single-quote 埋め込みを quote 安全にする。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<String> {
    let raw = std::fs::read_to_string(session_file_in(base, project, lane)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// 本番 base (vp_state_dir) での record (hook-check から呼ぶ)
pub fn record(project: &str, lane: &str, session_id: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, session_id)
}

/// 本番 base (vp_state_dir) での last (`vp lane last-session` / lazy populate から呼ぶ)
pub fn last(project: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_sanitizes_project_and_lane() {
        let p = session_file_in(Path::new("/base"), "creo.memories", "conductor");
        assert_eq!(p, Path::new("/base/cc_sessions/creo.memories__conductor"));
        // path separator は - に潰す (traversal 防止)
        let p = session_file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/cc_sessions/a-b__..-evil"));
    }

    #[test]
    fn record_and_last_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "0196-session-id").expect("record");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("0196-session-id")
        );
        // 未記録 lane は None
        assert_eq!(last_in(tmp.path(), "vp", "w1"), None);
        // 上書き (最新が勝つ)
        record_in(tmp.path(), "vp", "conductor", "newer-id").expect("record 2");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("newer-id")
        );
    }

    #[test]
    fn last_rejects_garbage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("cc_sessions");
        std::fs::create_dir_all(&dir).unwrap();
        // 空白のみ / quote 等の uuid 形式外は None (壊れた file を resume に渡さない)
        std::fs::write(dir.join("vp__conductor"), "  \n").unwrap();
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        std::fs::write(dir.join("vp__conductor"), "abc'; rm -rf /'").unwrap();
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        // 正常な uuid 形式は通る (trim 済み)
        std::fs::write(dir.join("vp__conductor"), "0196-abc\n").unwrap();
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("0196-abc")
        );
    }
}
