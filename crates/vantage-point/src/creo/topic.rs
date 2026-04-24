//! Topic namespace — canonical 4-part + alias table (R0 skeleton)
//!
//! Canonical: `{scope}/{capability}/{category}/{detail}`
//! - scope: `project` / `user` / `system`
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
        ("pp.route", "project/pp/command/route"),
        ("sc.item.added", "project/sc/state/item-added"),
        ("sc.item.updated", "project/sc/state/item-updated"),
        ("hd.message", "project/hd/notify/message"),
        ("hd.session.started", "project/hd/lifecycle/session-started"),
        ("user.click", "user/user/command/click"),
        ("user.focus", "user/user/state/focus-changed"),
        ("build.done", "project/ge/state/build-done"),
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
            a.get("pp.route").map(String::as_str),
            Some("project/pp/command/route")
        );
        assert_eq!(
            a.get("sc.item.added").map(String::as_str),
            Some("project/sc/state/item-added")
        );
        assert_eq!(
            a.get("hd.session.started").map(String::as_str),
            Some("project/hd/lifecycle/session-started")
        );
    }

    #[test]
    fn canonical_shape_check() {
        assert!(looks_canonical("project/pp/command/route"));
        assert!(looks_canonical("user/user/state/focus-changed"));
        assert!(!looks_canonical("pp.route"));
        assert!(!looks_canonical("project/pp"));
        assert!(!looks_canonical(""));
    }
}
