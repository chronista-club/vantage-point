//! Topic namespace — canonical 4-part + alias table (R0 skeleton)
//!
//! Canonical: `{scope}/{capability}/{category}/{detail}`
//! - scope: `repo` / `user` / `system`
//! - category: `state` / `command` / `lifecycle` / `error` / `notify`
//!
//! Alias は永久互換、canonical は拡張のため slot を増やす余地を残す。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Topic string。runtime 層 (VP-74) で alias 解決 + canonical validation する。
pub type Topic = String;

/// Alias エントリ: short → canonical。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicAlias {
    pub short: String,
    pub canonical: String,
}

/// Seed alias set。`docs/design/06-creoui-draft.md` §6.3 を実体化。
pub fn default_aliases() -> HashMap<String, String> {
    [
        ("board.route", "repo/board/command/route"),
        ("sc.item.added", "repo/sc/state/item-added"),
        ("sc.item.updated", "repo/sc/state/item-updated"),
        // PR-pre2 (VP-118): hd → echoes rename (Heaven's Door → Echoes)
        ("conversation.message", "repo/conversation/notify/message"),
        (
            "conversation.session.started",
            "repo/conversation/lifecycle/session-started",
        ),
        ("user.click", "user/user/command/click"),
        ("user.focus", "user/user/state/focus-changed"),
        ("build.done", "repo/runner/state/build-done"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect()
}

/// 簡易 canonical shape チェック (4 slash-segment 以上)。
/// 完全 validation は runtime 層で実施予定。
pub fn looks_canonical(topic: &str) -> bool {
    topic.split('/').filter(|s| !s.is_empty()).count() >= 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_contain_seed_entries() {
        let a = default_aliases();
        assert_eq!(
            a.get("board.route").map(String::as_str),
            Some("repo/board/command/route")
        );
        assert_eq!(
            a.get("sc.item.added").map(String::as_str),
            Some("repo/sc/state/item-added")
        );
        assert_eq!(
            a.get("conversation.session.started").map(String::as_str),
            Some("repo/conversation/lifecycle/session-started")
        );
    }

    #[test]
    fn canonical_shape_check() {
        assert!(looks_canonical("repo/board/command/route"));
        assert!(looks_canonical("user/user/state/focus-changed"));
        assert!(!looks_canonical("board.route"));
        assert!(!looks_canonical("repo/board"));
        assert!(!looks_canonical(""));
    }
}
