//! TopicRouter: Hub → Topic 振り分けルーター
//!
//! Hub からの ProcessMessage を Topic パスに変換し、
//! パターンマッチする subscriber に配信する。
//! Retained 対象（state/command）のメッセージは最新値を保持し、
//! 新規 subscribe 時に初期配信する。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::{RwLock, mpsc};

use crate::process::retained::RetainedStore;
use crate::process::topic::{TopicPath, TopicPattern};
use crate::protocol::ProcessMessage;

/// Topic ベースのメッセージルーター
pub struct TopicRouter {
    /// Retained メッセージストア（state/command の最新値を保持）
    retained: Arc<RwLock<RetainedStore>>,
    /// アクティブな subscriber 一覧
    subscribers: Arc<RwLock<Vec<TopicSubscription>>>,
    /// subscriber ID の採番カウンター
    next_id: AtomicU64,
}

/// 個別の subscriber エントリ
struct TopicSubscription {
    /// 一意な識別子
    id: u64,
    /// マッチング対象のパターン
    pattern: TopicPattern,
    /// メッセージ配信チャネル
    tx: mpsc::Sender<(String, ProcessMessage)>,
}

impl TopicRouter {
    /// 新しいルーターを作成
    pub fn new() -> Self {
        Self {
            retained: Arc::new(RwLock::new(RetainedStore::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
            next_id: AtomicU64::new(0),
        }
    }

    /// Hub からメッセージを受け取り、topic に振り分ける
    ///
    /// 1. ProcessMessage → topic 文字列に変換
    /// 2. Retained 対象なら最新値を保存
    /// 3. パターンマッチする全 subscriber に配信
    pub async fn route(&self, msg: ProcessMessage) {
        let topic = Self::message_to_topic(&msg);

        // Retained 対象（state/command カテゴリ）なら保存
        if TopicPath::parse(&topic).is_retained() {
            self.retained.write().await.set(&topic, msg.clone());
        }

        // マッチする subscriber に配信（送信失敗は無視）
        let subs = self.subscribers.read().await;
        for sub in subs.iter() {
            if TopicPath::parse(&topic).matches(&sub.pattern) {
                let _ = sub.tx.try_send((topic.clone(), msg.clone()));
            }
        }
    }

    /// ProcessMessage → Topic 文字列のマッピング
    ///
    /// 命名規則: `{scope}/{capability}/{category}/{detail}`
    /// - scope: "process"
    /// - capability: paisley-park, heavens-door, terminal, debug, star-platinum
    /// - category: command, event, state, data, log, trace
    fn message_to_topic(msg: &ProcessMessage) -> String {
        match msg {
            // === Paisley Park（Canvas 表示能力）===
            ProcessMessage::Show { pane_id, .. } => {
                format!("process/paisley-park/command/show/{}", pane_id)
            }
            ProcessMessage::Clear { pane_id, .. } => {
                format!("process/paisley-park/command/clear/{}", pane_id)
            }
            ProcessMessage::Split { pane_id, .. } => {
                format!("process/paisley-park/command/split/{}", pane_id)
            }
            ProcessMessage::Close { pane_id, .. } => {
                format!("process/paisley-park/command/close/{}", pane_id)
            }
            ProcessMessage::TogglePane { pane_id, .. } => {
                format!("process/paisley-park/command/toggle/{}", pane_id)
            }
            ProcessMessage::ScreenshotRequest { .. } => {
                "process/paisley-park/command/screenshot".to_string()
            }

            // === Heaven's Door（AI Agent 能力）===
            ProcessMessage::ChatChunk { .. } => "process/heavens-door/event/text-chunk".to_string(),
            ProcessMessage::ChatMessage { .. } => {
                "process/heavens-door/event/chat-message".to_string()
            }
            ProcessMessage::ChatComponent { .. } => {
                "process/heavens-door/event/component".to_string()
            }
            ProcessMessage::ComponentDismissed { .. } => {
                "process/heavens-door/event/component-dismissed".to_string()
            }
            ProcessMessage::AgUi { .. } => "process/heavens-door/event/ag-ui".to_string(),
            ProcessMessage::SessionList { .. } => {
                "process/heavens-door/state/session-list".to_string()
            }
            ProcessMessage::SessionSwitched { .. } => {
                "process/heavens-door/state/session".to_string()
            }
            ProcessMessage::SessionCreated { .. } => {
                "process/heavens-door/event/session-created".to_string()
            }
            ProcessMessage::SessionClosed { .. } => {
                "process/heavens-door/event/session-closed".to_string()
            }
            ProcessMessage::SessionHistory { .. } => {
                "process/heavens-door/event/session-history".to_string()
            }

            // === Terminal（PTY 出力）===
            ProcessMessage::TerminalOutput { .. } => "process/terminal/data/output".to_string(),
            ProcessMessage::TerminalReady => "process/terminal/state/ready".to_string(),
            ProcessMessage::TerminalExited => "process/terminal/state/exited".to_string(),

            // === Debug（デバッグ情報）===
            ProcessMessage::DebugInfo { .. } => "process/debug/log".to_string(),
            ProcessMessage::DebugModeChanged { .. } => "process/debug/state/mode".to_string(),
            ProcessMessage::TraceLog { .. } => "process/debug/trace".to_string(),

            // === Star Platinum（Process 管理）===
            ProcessMessage::Ping => "process/star-platinum/event/ping".to_string(),
        }
    }

    /// パターンに一致するメッセージを受信する subscriber を登録
    ///
    /// 登録時に retained ストアから初期値を配信する。
    /// 返り値の Receiver でメッセージを受信し、u64 で unsubscribe に使う。
    pub async fn subscribe(
        &self,
        pattern: &str,
    ) -> (u64, mpsc::Receiver<(String, ProcessMessage)>) {
        let pattern = TopicPattern::parse(pattern);
        let (tx, rx) = mpsc::channel(256);

        // Retained メッセージの初期配信
        {
            let retained = self.retained.read().await;
            for (topic, msg) in retained.get_matching(&pattern) {
                let _ = tx.try_send((topic.to_string(), msg.clone()));
            }
        }

        // subscriber 登録（アトミックに ID を採番）
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut subs = self.subscribers.write().await;
            subs.push(TopicSubscription { id, pattern, tx });
        }

        (id, rx)
    }

    /// subscriber を削除
    pub async fn unsubscribe(&self, id: u64) {
        let mut subs = self.subscribers.write().await;
        subs.retain(|s| s.id != id);
    }

    /// Retained store への直接アクセス
    pub fn retained(&self) -> Arc<RwLock<RetainedStore>> {
        self.retained.clone()
    }
}

impl Default for TopicRouter {
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

    // =========================================================================
    // message_to_topic マッピング
    // =========================================================================

    #[test]
    fn test_message_to_topic_show() {
        let msg = make_show("main", "# Hello");
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/paisley-park/command/show/main");
    }

    #[test]
    fn test_message_to_topic_clear() {
        let msg = ProcessMessage::Clear {
            pane_id: "side".to_string(),
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/paisley-park/command/clear/side");
    }

    #[test]
    fn test_message_to_topic_chat_chunk() {
        let msg = ProcessMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/heavens-door/event/text-chunk");
    }

    #[test]
    fn test_message_to_topic_session_list() {
        let msg = ProcessMessage::SessionList {
            sessions: vec![],
            active_id: None,
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/heavens-door/state/session-list");
    }

    #[test]
    fn test_message_to_topic_terminal_ready() {
        let topic = TopicRouter::message_to_topic(&ProcessMessage::TerminalReady);
        assert_eq!(topic, "process/terminal/state/ready");
    }

    #[test]
    fn test_message_to_topic_ping() {
        let topic = TopicRouter::message_to_topic(&ProcessMessage::Ping);
        assert_eq!(topic, "process/star-platinum/event/ping");
    }

    #[test]
    fn test_message_to_topic_debug_info() {
        let msg = ProcessMessage::DebugInfo {
            level: crate::protocol::DebugMode::Simple,
            category: "test".to_string(),
            message: "hello".to_string(),
            data: None,
            tags: vec![],
        };
        let topic = TopicRouter::message_to_topic(&msg);
        assert_eq!(topic, "process/debug/log");
    }

    // =========================================================================
    // route → retained に保存
    // =========================================================================

    #[tokio::test]
    async fn test_route_stores_retained_for_state() {
        let router = TopicRouter::new();

        // state カテゴリは retained
        router.route(ProcessMessage::TerminalReady).await;

        let retained = router.retained.read().await;
        let msg = retained.get("process/terminal/state/ready");
        assert!(msg.is_some());
        assert!(matches!(msg.unwrap(), ProcessMessage::TerminalReady));
    }

    #[tokio::test]
    async fn test_route_stores_retained_for_command() {
        let router = TopicRouter::new();

        // command カテゴリも retained
        let show = make_show("main", "# Hello");
        router.route(show).await;

        let retained = router.retained.read().await;
        let msg = retained.get("process/paisley-park/command/show/main");
        assert!(msg.is_some());
    }

    #[tokio::test]
    async fn test_route_does_not_store_event() {
        let router = TopicRouter::new();

        // event カテゴリは retained 対象外
        let msg = ProcessMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        router.route(msg).await;

        let retained = router.retained.read().await;
        assert!(retained.is_empty());
    }

    // =========================================================================
    // subscribe → retained の初期配信
    // =========================================================================

    #[tokio::test]
    async fn test_subscribe_receives_retained_initial() {
        let router = TopicRouter::new();

        // 先に retained に保存
        router.route(ProcessMessage::TerminalReady).await;
        router.route(make_show("main", "# Test")).await;

        // state を subscribe → retained から初期配信される
        let (_id, mut rx) = router.subscribe("process/terminal/state/#").await;

        let (topic, msg) = rx.try_recv().expect("初期配信があるはず");
        assert_eq!(topic, "process/terminal/state/ready");
        assert!(matches!(msg, ProcessMessage::TerminalReady));

        // command は別 topic なので配信されない
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // subscribe → route で新規メッセージ受信
    // =========================================================================

    #[tokio::test]
    async fn test_subscribe_receives_new_messages() {
        let router = TopicRouter::new();

        // 先に subscribe
        let (_id, mut rx) = router.subscribe("process/heavens-door/event/#").await;

        // route でメッセージ配信
        let msg = ProcessMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        router.route(msg).await;

        let (topic, received) = rx.try_recv().expect("メッセージを受信できるはず");
        assert_eq!(topic, "process/heavens-door/event/text-chunk");
        assert!(matches!(received, ProcessMessage::ChatChunk { .. }));
    }

    #[tokio::test]
    async fn test_subscribe_does_not_receive_unmatched() {
        let router = TopicRouter::new();

        // terminal だけ subscribe
        let (_id, mut rx) = router.subscribe("process/terminal/#").await;

        // 別 capability のメッセージを route
        let msg = ProcessMessage::ChatChunk {
            content: "hello".to_string(),
            done: false,
        };
        router.route(msg).await;

        // 受信しないはず
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // ワイルドカード subscribe
    // =========================================================================

    #[tokio::test]
    async fn test_wildcard_subscribe_all() {
        let router = TopicRouter::new();

        // 全メッセージを subscribe
        let (_id, mut rx) = router.subscribe("#").await;

        router.route(ProcessMessage::Ping).await;
        router.route(ProcessMessage::TerminalReady).await;

        let (topic1, _) = rx.try_recv().expect("Ping を受信");
        assert_eq!(topic1, "process/star-platinum/event/ping");

        let (topic2, _) = rx.try_recv().expect("TerminalReady を受信");
        assert_eq!(topic2, "process/terminal/state/ready");
    }

    #[tokio::test]
    async fn test_single_wildcard_subscribe() {
        let router = TopicRouter::new();

        // 全 capability の state を subscribe
        let (_id, mut rx) = router.subscribe("process/+/state/#").await;

        router.route(ProcessMessage::TerminalReady).await;
        router
            .route(ProcessMessage::SessionList {
                sessions: vec![],
                active_id: None,
            })
            .await;
        // event はマッチしない
        router
            .route(ProcessMessage::ChatChunk {
                content: "x".to_string(),
                done: true,
            })
            .await;

        let (t1, _) = rx.try_recv().expect("TerminalReady を受信");
        assert_eq!(t1, "process/terminal/state/ready");

        let (t2, _) = rx.try_recv().expect("SessionList を受信");
        assert_eq!(t2, "process/heavens-door/state/session-list");

        // 3つ目はないはず
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // unsubscribe
    // =========================================================================

    #[tokio::test]
    async fn test_unsubscribe_stops_delivery() {
        let router = TopicRouter::new();

        let (id, mut rx) = router.subscribe("process/terminal/#").await;

        // unsubscribe 前は受信できる
        router.route(ProcessMessage::TerminalReady).await;
        assert!(rx.try_recv().is_ok());

        // unsubscribe
        router.unsubscribe(id).await;

        // unsubscribe 後は配信されない
        router.route(ProcessMessage::TerminalExited).await;
        assert!(rx.try_recv().is_err());
    }

    // =========================================================================
    // 複数 subscriber
    // =========================================================================

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let router = TopicRouter::new();

        let (_id1, mut rx1) = router.subscribe("process/terminal/#").await;
        let (_id2, mut rx2) = router.subscribe("process/+/state/#").await;

        // TerminalReady は両方にマッチ
        router.route(ProcessMessage::TerminalReady).await;

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    // =========================================================================
    // Default trait
    // =========================================================================

    #[test]
    fn test_default() {
        let _router = TopicRouter::default();
    }
}
