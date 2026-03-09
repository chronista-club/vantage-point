//! Retained メッセージストア
//!
//! MQTT の retained message に相当する機能。
//! Topic ごとに最新のメッセージを保持し、新規接続時に最新状態を配信する。
//! `state` および `command` カテゴリのトピックが retained 対象。

use std::collections::HashMap;
use std::time::Instant;

use crate::protocol::ProcessMessage;

use super::topic::TopicPattern;

/// Retained メッセージのエントリ
#[derive(Debug, Clone)]
struct RetainedEntry {
    /// 保持しているメッセージ
    message: ProcessMessage,
    /// 保存時刻
    stored_at: Instant,
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
    pub fn set(&mut self, topic: &str, msg: ProcessMessage) {
        self.store.insert(
            topic.to_string(),
            RetainedEntry {
                message: msg,
                stored_at: Instant::now(),
            },
        );
    }

    /// トピックに保存されたメッセージを取得
    pub fn get(&self, topic: &str) -> Option<&ProcessMessage> {
        self.store.get(topic).map(|e| &e.message)
    }

    /// パターンに一致する全エントリを返す
    pub fn get_matching(&self, pattern: &TopicPattern) -> Vec<(&str, &ProcessMessage)> {
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
    pub fn remove(&mut self, topic: &str) -> Option<ProcessMessage> {
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
    use crate::protocol::{Content, ProcessMessage};

    /// テスト用の Show メッセージを生成
    fn make_show(pane_id: &str, text: &str) -> ProcessMessage {
        ProcessMessage::Show {
            pane_id: pane_id.to_string(),
            content: Content::Markdown(text.to_string()),
            append: false,
            title: None,
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
        store.set("process/paisley-park/command/show/main", msg);

        let retrieved = store.get("process/paisley-park/command/show/main");
        assert!(retrieved.is_some());
        match retrieved.unwrap() {
            ProcessMessage::Show { pane_id, .. } => {
                assert_eq!(pane_id, "main");
            }
            _ => panic!("Show メッセージを期待"),
        }
    }

    #[test]
    fn test_set_overwrites() {
        let mut store = RetainedStore::new();
        store.set(
            "process/terminal/state/ready",
            ProcessMessage::TerminalReady,
        );
        store.set(
            "process/terminal/state/ready",
            ProcessMessage::TerminalExited,
        );

        let msg = store.get("process/terminal/state/ready").unwrap();
        assert!(matches!(msg, ProcessMessage::TerminalExited));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_get_nonexistent() {
        let store = RetainedStore::new();
        assert!(store.get("process/debug/log").is_none());
    }

    #[test]
    fn test_remove() {
        let mut store = RetainedStore::new();
        store.set(
            "process/terminal/state/ready",
            ProcessMessage::TerminalReady,
        );
        assert_eq!(store.len(), 1);

        let removed = store.remove("process/terminal/state/ready");
        assert!(removed.is_some());
        assert!(store.is_empty());
    }

    #[test]
    fn test_remove_nonexistent() {
        let mut store = RetainedStore::new();
        let removed = store.remove("process/debug/log");
        assert!(removed.is_none());
    }

    #[test]
    fn test_clear() {
        let mut store = RetainedStore::new();
        store.set(
            "process/terminal/state/ready",
            ProcessMessage::TerminalReady,
        );
        store.set(
            "process/paisley-park/command/show/main",
            make_show("main", "Hi"),
        );
        assert_eq!(store.len(), 2);

        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn test_get_matching_exact() {
        let mut store = RetainedStore::new();
        store.set(
            "process/paisley-park/command/show/main",
            make_show("main", "A"),
        );
        store.set(
            "process/paisley-park/command/show/side",
            make_show("side", "B"),
        );
        store.set(
            "process/terminal/state/ready",
            ProcessMessage::TerminalReady,
        );

        // Paisley Park の command 配下を全取得
        let pattern = TopicPattern::parse("process/paisley-park/command/#");
        let results = store.get_matching(&pattern);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_get_matching_single_wildcard() {
        let mut store = RetainedStore::new();
        store.set(
            "process/terminal/state/ready",
            ProcessMessage::TerminalReady,
        );
        store.set(
            "process/heavens-door/state/session-list",
            ProcessMessage::SessionList {
                sessions: vec![],
                active_id: None,
            },
        );
        store.set(
            "process/paisley-park/command/show/main",
            make_show("main", "X"),
        );

        // 全 capability の state を取得
        let pattern = TopicPattern::parse("process/+/state/#");
        let results = store.get_matching(&pattern);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_get_matching_no_results() {
        let mut store = RetainedStore::new();
        store.set(
            "process/terminal/state/ready",
            ProcessMessage::TerminalReady,
        );

        let pattern = TopicPattern::parse("process/debug/#");
        let results = store.get_matching(&pattern);
        assert!(results.is_empty());
    }

    #[test]
    fn test_default() {
        let store = RetainedStore::default();
        assert!(store.is_empty());
    }
}
