//! claude 固有の session helper（会話 id の検証 + transcript 探索）。
//!
//! ⚠️ **doc 40 で会話 id の SSOT は [`super::session_registry`]（`SessionEntry.conversation`）
//! に統合された**。かつて本 module が持っていた per-lane state file の store 役
//! （record / last / clear）は doc 40 PR-2 で退役した（one-shot migration で全 lane の会話 id を
//! registry へ移設済み。旧書き手 = hook 直書きは「root の label に追従しない」ラベル乖離バグの
//! 発生源だった — doc 40 §1-1。hook は SP への報告者に降格済み）。
//!
//! 本 module に残るのは claude 固有部だけ:
//! - [`is_valid_session_id`]: `--resume '<id>'` への injection 防壁（registry の write 側
//!   dispatch [`super::session_registry`] も使う）
//! - [`transcript_path`] / [`transcript_exists`]: `~/.claude/projects` 走査（resume の
//!   pre-flight / transcript replay 源の解決）

use std::path::PathBuf;

/// session id の正規形 (英数+ハイフン、 非空)。 `--resume '<id>'` の single-quote 埋め込みが
/// shell injection にならないための防壁（registry の write 側検証
/// `session_registry::is_valid_conversation` の claude arm も本関数を使う）。
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
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

    /// claude 版の会話 id 検証規則: 英数 + ハイフンのみ（`_` / 空 / injection 形は不可）。
    /// `--resume '<id>'` の single-quote 埋め込み防壁の核。
    #[test]
    fn session_id_validation_rejects_underscore_and_injection() {
        assert!(is_valid_session_id("good-id"));
        assert!(is_valid_session_id("94427c81-1234-4abc"));
        assert!(!is_valid_session_id(""), "空は不可");
        assert!(!is_valid_session_id("has_underscore"), "_ は不可");
        assert!(!is_valid_session_id("bad id'; rm"), "quote 破りは reject");
    }
}
