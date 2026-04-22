//! TopicAlias runtime table — VP-73 creo::topic の死に型 TopicAlias 構造体を wire する。
//!
//! Worker C (rust-reviewer) Finding M-1 の対応: `TopicAlias` が定義だけで未使用だった状況を、
//! Phase A の alias 解決 runtime で本稼働させる。

use std::collections::HashMap;

use crate::creo::{TopicAlias, default_aliases};

/// Alias 解決テーブル。short → canonical の双方向近似 (resolve は one-way)。
pub struct AliasTable {
    map: HashMap<String, String>,
}

impl AliasTable {
    /// 空 table。
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// VP-73 `default_aliases()` seed 済み table。
    pub fn seeded() -> Self {
        Self {
            map: default_aliases(),
        }
    }

    /// alias を 1 件登録。
    pub fn insert(&mut self, alias: TopicAlias) {
        self.map.insert(alias.short, alias.canonical);
    }

    /// short を canonical に解決。found なら Some、否なら None (alias 対象外として扱う)。
    pub fn resolve(&self, short: &str) -> Option<String> {
        self.map.get(short).cloned()
    }

    /// 登録済み件数 (debugging / metric 用)。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// テーブル空判定。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for AliasTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_contains_hd_message() {
        let t = AliasTable::seeded();
        assert_eq!(
            t.resolve("hd.message").as_deref(),
            Some("project/hd/notify/message")
        );
    }

    #[test]
    fn seeded_contains_user_click() {
        let t = AliasTable::seeded();
        assert_eq!(
            t.resolve("user.click").as_deref(),
            Some("user/user/command/click")
        );
    }

    #[test]
    fn insert_and_resolve_roundtrip() {
        let mut t = AliasTable::new();
        t.insert(TopicAlias {
            short: "foo.bar".into(),
            canonical: "project/foo/state/bar".into(),
        });
        assert_eq!(
            t.resolve("foo.bar").as_deref(),
            Some("project/foo/state/bar")
        );
        assert!(t.resolve("unknown").is_none());
    }

    #[test]
    fn empty_table() {
        let t = AliasTable::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(t.resolve("anything").is_none());
    }
}
