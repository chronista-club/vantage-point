//! lane ごとの CC session id 永続化 (R3-b、 設計 mem_1CbXZyCiqrdgteGhRFDaHW)
//!
//! `claude --continue` は「cwd の最新 session」を拾うため、 background session が
//! 居ると Agent View dashboard を開いて send-keys が詰まる (CC 2.1 罠)。
//! 特定 session を `--resume <id>` で指名すれば構造的に回避できる。
//!
//! ⚠️ **doc 40 で会話 id の SSOT は [`super::session_registry`]（`SessionEntry.conversation`）
//! に統合された**。本 store は移行 bridge（registry load 時の read-only backfill）としてだけ
//! 読まれ、新規の書き手は居ない（旧書き手 = hook 直書きは「root の label に追従しない」
//! ラベル乖離バグの発生源だった — doc 40 §1-1。hook は SP への報告者に降格済み）。
//! **PR-2 で record / last / clear の store 役は退役**し、本 module に残るのは claude 固有部
//! （[`transcript_path`] / [`transcript_exists`] / [`is_valid_session_id`]）のみになる。
//!
//! - 置き場: `vp_state_dir()/cc_sessions/<project>__<lane>` (1 lane 1 file 1 行)
//! - 共通機構（record / last / clear + 検証防壁）は [`super::session_store`] に委譲

use std::path::{Path, PathBuf};

use super::session_store::SessionStore;

/// session id の正規形 (英数+ハイフン、 非空)。 書き込み・読み出しの両側で同じ検証を
/// 使い、 state file が常に正規形であることを保証する (moody 指摘 #1)。
/// doc 40: registry の write 側 dispatch（`session_registry::is_valid_conversation`）も使う。
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

const STORE: SessionStore = SessionStore::new("cc_sessions", is_valid_session_id);

/// state base dir 配下の session file path (純関数、 テスト用に base 注入)
pub fn session_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    STORE.file_in(base, project, lane)
}

/// session id を記録する (上書き、 1 行)。 形式外は**書かずに** Ok（session_store の共通原則）。
pub fn record_in(base: &Path, project: &str, lane: &str, session_id: &str) -> std::io::Result<()> {
    STORE.record_in(base, project, lane, session_id)
}

/// 最後に記録された session id を返す（無い / 形式外は None — 壊れた file を `--resume` に渡さない）。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<String> {
    STORE.last_in(base, project, lane)
}

/// 記録を消す (未記録なら no-op)。
///
/// 「素の新規 session を張る」 (= fresh) の意味を state 側で表現する唯一の手段。
/// 消した後は `last` が None になるので:
/// - chat engine は `--resume` 無しで spawn → `SessionInit` で新 id を書き戻す
/// - transcript replay-on-attach は再生対象を失う (= 前の会話を映さない)
///
/// 旧 session の transcript 自体は `~/.claude/projects/` に残る (指す矢印を捨てるだけ)。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    STORE.clear_in(base, project, lane)
}

/// 本番 base (vp_state_dir) での record (hook-check から呼ぶ)
pub fn record(project: &str, lane: &str, session_id: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, session_id)
}

/// 本番 base (vp_state_dir) での clear (fresh restart から呼ぶ)
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base (vp_state_dir) での last (`vp lane last-session` / lazy populate から呼ぶ)
pub fn last(project: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}

/// claude の session transcript file path を引く（`~/.claude/projects/*/<id>.jsonl`）。
///
/// claude は cwd 由来の encoded dir 名で session を分けるため、 全 project dir を走査する
/// （encoding 形式に依存しない = 堅牢）。 N は project 数（数百程度、 boot でなく切替 / attach 時のみ）。
/// 不正 id / home 不明 / 実体なしは None。
pub fn transcript_path(session_id: &str) -> Option<PathBuf> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    let projects = dirs::home_dir()?.join(".claude").join("projects");
    let target = format!("{session_id}.jsonl");
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&target))
        .find(|p| p.exists())
}

/// claude の session transcript が実在するか。
///
/// doc 33 C2: chat engine を `--resume <id>` で立てる前の pre-flight。 stale / phantom な
/// cc_session id（実体が消えた session）で resume すると headless claude が
/// "No conversation found" で即エラーになる（TUI の `|| claude` fallback に相当する
/// ものが headless には無い）ため、 存在しない id は resume に渡さず fresh spawn に倒す。
/// Act I ⇄ II 切替の live session は transcript が disk にあるので resume が継続する。
pub fn transcript_exists(session_id: &str) -> bool {
    transcript_path(session_id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// dir 名が cc_sessions であること + sanitize が効くこと（共通機構との結線 smoke。
    /// record/last/clear の共通挙動そのものは session_store のテストが持つ）。
    #[test]
    fn file_lives_under_cc_sessions_with_sanitize() {
        let p = session_file_in(Path::new("/base"), "creo.memories", "conductor");
        assert_eq!(p, Path::new("/base/cc_sessions/creo-memories__conductor"));
        let p = session_file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/cc_sessions/a-b__---evil"));
    }

    /// claude 版の検証規則: 英数 + ハイフンのみ（`_` は不可）。
    #[test]
    fn session_id_validation_rejects_underscore_and_injection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "good-id").expect("record");
        record_in(tmp.path(), "vp", "conductor", "has_underscore").expect("形式外は no-op");
        record_in(tmp.path(), "vp", "conductor", "bad id'; rm").expect("形式外は no-op");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("good-id"),
            "正常な記録が保持される"
        );
    }
}
