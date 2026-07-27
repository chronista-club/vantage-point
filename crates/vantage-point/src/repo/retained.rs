//! Retained メッセージストア
//!
//! MQTT の retained message に相当する機能。
//! Topic ごとに最新のメッセージを保持し、新規接続時に最新状態を配信する。
//! `state` および `command` カテゴリのトピックが retained 対象。

use std::collections::HashMap;

use crate::protocol::RepoMessage;

use super::topic::TopicPattern;

/// Retained メッセージのエントリ
#[derive(Debug, Clone)]
struct RetainedEntry {
    /// 保持しているメッセージ
    message: RepoMessage,
}

/// Topic ごとに最新メッセージを保持するストア
#[derive(Debug)]
pub struct RetainedStore {
    store: HashMap<String, RetainedEntry>,
}

impl RetainedStore {
    /// 空のストアを作成
    pub fn new() -> Self {
        Self {
            store: HashMap::new(),
        }
    }

    /// メッセージを保存（同じトピックは上書き）
    pub fn set(&mut self, topic: &str, msg: RepoMessage) {
        self.store
            .insert(topic.to_string(), RetainedEntry { message: msg });
    }

    /// トピックに保存されたメッセージを取得
    pub fn get(&self, topic: &str) -> Option<&RepoMessage> {
        self.store.get(topic).map(|e| &e.message)
    }

    /// パターンに一致する全エントリを返す
    pub fn get_matching(&self, pattern: &TopicPattern) -> Vec<(&str, &RepoMessage)> {
        use super::topic::TopicPath;

        self.store
            .iter()
            .filter(|(key, _)| {
                let path = TopicPath::parse(key);
                path.matches(pattern)
            })
            .map(|(key, entry)| (key.as_str(), &entry.message))
            .collect()
    }

    /// 指定トピックのエントリを削除
    pub fn remove(&mut self, topic: &str) -> Option<RepoMessage> {
        self.store.remove(topic).map(|e| e.message)
    }

    /// 全エントリを削除
    pub fn clear(&mut self) {
        self.store.clear();
    }

    /// 保存されているエントリ数
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// ストアが空かどうか
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

impl Default for RetainedStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Content, RepoMessage};

    /// テスト用の Show メッセージを生成
    fn make_show(pane_id: &str, text: &str) -> RepoMessage {
        RepoMessage::Show {
            pane_id: pane_id.to_string(),
            content: Content::Markdown(text.to_string()),
            append: false,
            title: None,
            lane: None,
            scope: None,
        }
    }

    #[test]
    fn test_new_store_is_empty() {
        let store = RetainedStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_set_and_get() {
        let mut store = RetainedStore::new();
        let msg = make_show("main", "# Hello");
        store.set("repo/board/command/show/main", msg);

        let retrieved = store.get("repo/board/command/show/main");
        assert!(retrieved.is_some());
        match retrieved.unwrap() {
            RepoMessage::Show { pane_id, .. } => {
                assert_eq!(pane_id, "main");
            }
            _ => panic!("Show メッセージを期待"),
        }
    }

    #[test]
    fn test_set_overwrites() {
        let mut store = RetainedStore::new();
        store.set("repo/terminal/state/ready", RepoMessage::TerminalReady);
        store.set("repo/terminal/state/ready", RepoMessage::TerminalExited);

        let msg = store.get("repo/terminal/state/ready").unwrap();
        assert!(matches!(msg, RepoMessage::TerminalExited));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_get_nonexistent() {
        let store = RetainedStore::new();
        assert!(store.get("repo/debug/log").is_none());
    }

    #[test]
    fn test_remove() {
        let mut store = RetainedStore::new();
        store.set("repo/terminal/state/ready", RepoMessage::TerminalReady);
        assert_eq!(store.len(), 1);

        let removed = store.remove("repo/terminal/state/ready");
        assert!(removed.is_some());
        assert!(store.is_empty());
    }

    #[test]
    fn test_remove_nonexistent() {
        let mut store = RetainedStore::new();
        let removed = store.remove("repo/debug/log");
        assert!(removed.is_none());
    }

    #[test]
    fn test_clear() {
        let mut store = RetainedStore::new();
        store.set("repo/terminal/state/ready", RepoMessage::TerminalReady);
        store.set("repo/board/command/show/main", make_show("main", "Hi"));
        assert_eq!(store.len(), 2);

        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn test_get_matching_exact() {
        let mut store = RetainedStore::new();
        store.set("repo/board/command/show/main", make_show("main", "A"));
        store.set("repo/board/command/show/side", make_show("side", "B"));
        store.set("repo/terminal/state/ready", RepoMessage::TerminalReady);

        // Board の command 配下を全取得
        let pattern = TopicPattern::parse("repo/board/command/#");
        let results = store.get_matching(&pattern);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_get_matching_single_wildcard() {
        let mut store = RetainedStore::new();
        store.set("repo/terminal/state/ready", RepoMessage::TerminalReady);
        store.set(
            "repo/conversation/state/session-list",
            RepoMessage::SessionList {
                sessions: vec![],
                active_id: None,
            },
        );
        store.set("repo/board/command/show/main", make_show("main", "X"));

        // 全 capability の state を取得
        let pattern = TopicPattern::parse("repo/+/state/#");
        let results = store.get_matching(&pattern);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_get_matching_no_results() {
        let mut store = RetainedStore::new();
        store.set("repo/terminal/state/ready", RepoMessage::TerminalReady);

        let pattern = TopicPattern::parse("repo/debug/#");
        let results = store.get_matching(&pattern);
        assert!(results.is_empty());
    }

    #[test]
    fn test_default() {
        let store = RetainedStore::default();
        assert!(store.is_empty());
    }
}
