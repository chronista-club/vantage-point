//! Mailbox — Actor 間 1:1 メッセージキュー (VP-24)
//!
//! ECS 的に各 Capability / Agent に付与する通信 Component。
//! EventBus（pub/sub ブロードキャスト）と並列するインフラ層。
//!
//! ## 設計思想
//!
//! - **Actor モデル**: 各エンティティが独立したメールボックスを持つ
//! - **1:1 ポイントツーポイント**: 宛先指定のダイレクトメッセージ
//! - **トランスポート非依存**: 同一プロセス内は tokio::mpsc、将来的にプロセス跨ぎ対応
//! - **ECS Component**: Capability トレイトの下、EventBus と同じレイヤー

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

// =============================================================================
// MailboxMessage
// =============================================================================

/// Mailbox で送受信されるメッセージ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxMessage {
    /// 送信元のアドレス
    pub from: String,
    /// 宛先のアドレス
    pub to: String,
    /// メッセージ種別
    pub kind: MessageKind,
    /// ペイロード（JSON値）
    pub payload: serde_json::Value,
    /// タイムスタンプ（Unix epoch ミリ秒）
    pub timestamp: u64,
    /// 返信先メッセージID（スレッド用）
    pub reply_to: Option<String>,
    /// メッセージID
    pub id: String,
}

/// メッセージ種別
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// ダイレクトメッセージ（通常の通信）
    Direct,
    /// 通知（完了、承認要求など）
    Notification,
    /// リクエスト（応答を期待）
    Request,
    /// レスポンス（リクエストへの応答）
    Response,
}

impl MailboxMessage {
    /// 新しいメッセージを作成
    pub fn new(from: impl Into<String>, to: impl Into<String>, kind: MessageKind) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
            payload: serde_json::Value::Null,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            reply_to: None,
            id: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// ペイロードを設定
    pub fn with_payload<T: Serialize>(mut self, payload: &T) -> Self {
        self.payload = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        self
    }

    /// 返信先を設定
    pub fn with_reply_to(mut self, reply_to: impl Into<String>) -> Self {
        self.reply_to = Some(reply_to.into());
        self
    }

    /// ペイロードを型付きで取得
    pub fn payload_as<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        serde_json::from_value(self.payload.clone()).ok()
    }
}

// =============================================================================
// MailboxHandle — 各 Capability が持つ送受信ハンドル
// =============================================================================

/// 個別の Mailbox ハンドル
///
/// 各 Capability / Agent が保持し、メッセージの送受信に使う。
/// `CapabilityContext` 経由で渡される。
///
/// Selective Receive: `recv_matching()` でフィルタ不一致メッセージを
/// 内部 stash に退避し、次回の recv で再確認する（Erlang 方式）。
#[derive(Debug, Clone)]
pub struct MailboxHandle {
    /// 自身のアドレス
    address: String,
    /// MailboxRouter への送信チャンネル（他者宛メッセージを Router に渡す）
    router_tx: mpsc::Sender<MailboxMessage>,
    /// 自分宛メッセージの受信チャンネル
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<MailboxMessage>>>,
    /// Selective Receive 用のメッセージ退避バッファ
    stash: Arc<tokio::sync::Mutex<std::collections::VecDeque<MailboxMessage>>>,
}

impl MailboxHandle {
    /// 自身のアドレスを取得
    pub fn address(&self) -> &str {
        &self.address
    }

    /// メッセージを送信（Router 経由で宛先に配信）
    pub async fn send(&self, msg: MailboxMessage) -> Result<(), MailboxError> {
        self.router_tx
            .send(msg)
            .await
            .map_err(|_| MailboxError::RouterClosed)
    }

    /// ダイレクトメッセージを簡易送信
    pub async fn send_to(
        &self,
        to: impl Into<String>,
        payload: &impl Serialize,
    ) -> Result<(), MailboxError> {
        let msg = MailboxMessage::new(&self.address, to, MessageKind::Direct).with_payload(payload);
        self.send(msg).await
    }

    /// 通知を送信
    pub async fn notify(
        &self,
        to: impl Into<String>,
        payload: &impl Serialize,
    ) -> Result<(), MailboxError> {
        let msg =
            MailboxMessage::new(&self.address, to, MessageKind::Notification).with_payload(payload);
        self.send(msg).await
    }

    /// メッセージを受信（ブロッキング）
    ///
    /// stash にメッセージがあればそちらを先に返す。
    pub async fn recv(&self) -> Option<MailboxMessage> {
        // stash を先に確認
        {
            let mut stash = self.stash.lock().await;
            if let Some(msg) = stash.pop_front() {
                return Some(msg);
            }
        }
        self.rx.lock().await.recv().await
    }

    /// Selective Receive: 条件に合うメッセージのみ受信（Erlang 方式）
    ///
    /// 条件に合わないメッセージは stash に退避し、次回の recv/recv_matching で再確認。
    /// メッセージロスが起きない安全な設計。
    pub async fn recv_matching<F>(&self, predicate: F) -> Option<MailboxMessage>
    where
        F: Fn(&MailboxMessage) -> bool,
    {
        // まず stash から条件に合うものを探す
        {
            let mut stash = self.stash.lock().await;
            if let Some(pos) = stash.iter().position(|m| predicate(m)) {
                return stash.remove(pos);
            }
        }

        // チャンネルから読み出し、条件に合わないものはローカルに集めてから stash に退避
        // rx と stash のロック順序を分離してデッドロックを防止
        let mut rx = self.rx.lock().await;
        let mut deferred = Vec::new();
        let found = loop {
            match rx.recv().await {
                Some(msg) => {
                    if predicate(&msg) {
                        break Some(msg);
                    }
                    deferred.push(msg);
                }
                None => break None,
            }
        };
        drop(rx); // rx ロック解放してから stash に書き込む
        if !deferred.is_empty() {
            self.stash.lock().await.extend(deferred);
        }
        found
    }
}

// =============================================================================
// MailboxRouter — メッセージルーティング
// =============================================================================

/// メッセージルーター
///
/// 全 Mailbox を管理し、メッセージを宛先に配信する。
/// Process（SP）または TheWorld が保持する。
pub struct MailboxRouter {
    /// アドレス → 送信チャンネルのマッピング
    boxes: Arc<RwLock<HashMap<String, mpsc::Sender<MailboxMessage>>>>,
    /// Router への送信チャンネル
    router_tx: mpsc::Sender<MailboxMessage>,
    /// ルーティングループの停止トークン
    shutdown: tokio_util::sync::CancellationToken,
}

impl MailboxRouter {
    /// 新しい MailboxRouter を作成し、ルーティングループを開始
    pub fn new() -> Self {
        let (router_tx, router_rx) = mpsc::channel::<MailboxMessage>(1024);
        let boxes: Arc<RwLock<HashMap<String, mpsc::Sender<MailboxMessage>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let shutdown = tokio_util::sync::CancellationToken::new();

        // ルーティングループ
        let boxes_clone = boxes.clone();
        let shutdown_clone = shutdown.clone();
        tokio::spawn(Self::routing_loop(router_rx, boxes_clone, shutdown_clone));

        Self {
            boxes,
            router_tx,
            shutdown,
        }
    }

    /// ルーティングループ — Router に届いたメッセージを宛先に配信
    async fn routing_loop(
        mut router_rx: mpsc::Receiver<MailboxMessage>,
        boxes: Arc<RwLock<HashMap<String, mpsc::Sender<MailboxMessage>>>>,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("MailboxRouter: ルーティングループ終了");
                    break;
                }
                msg = router_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            let boxes = boxes.read().await;
                            if let Some(tx) = boxes.get(&msg.to) {
                                if let Err(e) = tx.try_send(msg.clone()) {
                                    tracing::warn!(
                                        "MailboxRouter: {} 宛の配信失敗: {}",
                                        msg.to,
                                        e
                                    );
                                }
                            } else {
                                tracing::debug!(
                                    "MailboxRouter: 宛先 '{}' が見つからない（from: {}）",
                                    msg.to,
                                    msg.from
                                );
                            }
                        }
                        None => {
                            tracing::info!("MailboxRouter: router_tx がドロップされたため終了");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 新しい Mailbox を登録し、ハンドルを返す
    pub async fn register(&self, address: impl Into<String>) -> MailboxHandle {
        let address = address.into();
        let (tx, rx) = mpsc::channel::<MailboxMessage>(256);

        self.boxes.write().await.insert(address.clone(), tx);

        tracing::debug!("MailboxRouter: '{}' を登録", address);

        MailboxHandle {
            address,
            router_tx: self.router_tx.clone(),
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            stash: Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::new())),
        }
    }

    /// Mailbox を登録解除
    pub async fn unregister(&self, address: &str) {
        if self.boxes.write().await.remove(address).is_some() {
            tracing::debug!("MailboxRouter: '{}' を登録解除", address);
        }
    }

    /// 登録済みアドレス一覧を取得
    pub async fn addresses(&self) -> Vec<String> {
        self.boxes.read().await.keys().cloned().collect()
    }

    /// 登録済みアドレス数を取得
    pub async fn count(&self) -> usize {
        self.boxes.read().await.len()
    }

    /// Router をシャットダウン
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

impl Default for MailboxRouter {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// MailboxError
// =============================================================================

/// Mailbox 操作のエラー
#[derive(Debug, thiserror::Error)]
pub enum MailboxError {
    /// Router が閉じている
    #[error("mailbox router is closed")]
    RouterClosed,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_send() {
        let router = MailboxRouter::new();

        let handle_a = router.register("agent-a").await;
        let handle_b = router.register("agent-b").await;

        // A → B にメッセージ送信
        handle_a.send_to("agent-b", &"hello from A").await.unwrap();

        // B で受信
        let msg = handle_b.recv().await.unwrap();
        assert_eq!(msg.from, "agent-a");
        assert_eq!(msg.to, "agent-b");
        assert_eq!(msg.kind, MessageKind::Direct);
        let payload: String = msg.payload_as().unwrap();
        assert_eq!(payload, "hello from A");

        router.shutdown();
    }

    #[tokio::test]
    async fn test_notification() {
        let router = MailboxRouter::new();

        let handle_hd = router.register("heavens-door").await;
        let handle_pp = router.register("paisley-park").await;

        // HD → PP に通知
        handle_hd
            .notify(
                "paisley-park",
                &serde_json::json!({"type": "completion", "message": "タスク完了"}),
            )
            .await
            .unwrap();

        let msg = handle_pp.recv().await.unwrap();
        assert_eq!(msg.from, "heavens-door");
        assert_eq!(msg.kind, MessageKind::Notification);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_bidirectional() {
        let router = MailboxRouter::new();

        let handle_a = router.register("lead").await;
        let handle_b = router.register("worker-1").await;

        // worker → lead: 質問
        let question = MailboxMessage::new("worker-1", "lead", MessageKind::Request)
            .with_payload(&serde_json::json!({"question": "DB スキーマどうする？"}));
        let question_id = question.id.clone();
        handle_b.send(question).await.unwrap();

        // lead で受信
        let received = handle_a.recv().await.unwrap();
        assert_eq!(received.kind, MessageKind::Request);

        // lead → worker: 回答
        let answer = MailboxMessage::new("lead", "worker-1", MessageKind::Response)
            .with_payload(&serde_json::json!({"answer": "PostgreSQL で"}))
            .with_reply_to(question_id.clone());
        handle_a.send(answer).await.unwrap();

        // worker で受信
        let reply = handle_b.recv().await.unwrap();
        assert_eq!(reply.kind, MessageKind::Response);
        assert_eq!(reply.reply_to, Some(question_id));

        router.shutdown();
    }

    #[tokio::test]
    async fn test_unregister() {
        let router = MailboxRouter::new();

        let _handle = router.register("temp-agent").await;
        assert_eq!(router.count().await, 1);

        router.unregister("temp-agent").await;
        assert_eq!(router.count().await, 0);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_addresses() {
        let router = MailboxRouter::new();

        let _a = router.register("hd").await;
        let _b = router.register("pp").await;
        let _c = router.register("ge").await;

        let mut addrs = router.addresses().await;
        addrs.sort();
        assert_eq!(addrs, vec!["ge", "hd", "pp"]);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_send_to_unknown_address() {
        let router = MailboxRouter::new();

        let handle = router.register("sender").await;

        // 存在しない宛先に送信 — エラーにはならず、ログに記録
        let result = handle.send_to("nonexistent", &"hello").await;
        assert!(result.is_ok()); // Router には届く、配信先がないだけ

        router.shutdown();
    }

    #[tokio::test]
    async fn test_message_id_unique() {
        let msg1 = MailboxMessage::new("a", "b", MessageKind::Direct);
        let msg2 = MailboxMessage::new("a", "b", MessageKind::Direct);
        assert_ne!(msg1.id, msg2.id);
    }

    #[tokio::test]
    async fn test_selective_receive_no_message_loss() {
        let router = MailboxRouter::new();

        let handle_a = router.register("a").await;
        let handle_b = router.register("b").await;

        // 3つのメッセージを送信（異なる送信元）
        handle_a.send_to("b", &"from-a-1").await.unwrap();
        let msg_other =
            MailboxMessage::new("other", "b", MessageKind::Direct).with_payload(&"from-other");
        handle_a.send(msg_other).await.unwrap();
        handle_a.send_to("b", &"from-a-2").await.unwrap();

        // recv_matching は内部でチャンネルを読むため sleep 不要
        // "a" からのメッセージのみ受信 — "other" は stash に退避
        let msg = handle_b.recv_matching(|m| m.from == "a").await.unwrap();
        assert_eq!(msg.payload_as::<String>().unwrap(), "from-a-1");

        let msg = handle_b.recv_matching(|m| m.from == "a").await.unwrap();
        assert_eq!(msg.payload_as::<String>().unwrap(), "from-a-2");

        // stash に退避された "other" からのメッセージも recv で回収可能
        let msg = handle_b.recv().await.unwrap();
        assert_eq!(msg.from, "other");
        assert_eq!(msg.payload_as::<String>().unwrap(), "from-other");

        router.shutdown();
    }

    #[tokio::test]
    async fn test_recv_checks_stash_first() {
        let router = MailboxRouter::new();

        let handle = router.register("target").await;
        let sender = router.register("sender").await;

        // メッセージを2つ送信
        sender.send_to("target", &"msg-1").await.unwrap();
        sender.send_to("target", &"msg-2").await.unwrap();

        // recv_matching で msg-1 をスキップ（stash に退避）、msg-2 を取得
        let msg = handle
            .recv_matching(|m| m.payload_as::<String>().as_deref() == Some("msg-2"))
            .await
            .unwrap();
        assert_eq!(msg.payload_as::<String>().unwrap(), "msg-2");

        // 通常の recv で stash から msg-1 が返る
        let msg = handle.recv().await.unwrap();
        assert_eq!(msg.payload_as::<String>().unwrap(), "msg-1");

        router.shutdown();
    }

    #[tokio::test]
    async fn test_unregister_then_send_to_removed_address() {
        let router = MailboxRouter::new();

        let handle_a = router.register("a").await;
        let _handle_b = router.register("b").await;

        // b を登録解除
        router.unregister("b").await;
        assert_eq!(router.count().await, 1);

        // 解除済みアドレスへの送信はエラーにならないが届かない
        let result = handle_a.send_to("b", &"orphan").await;
        assert!(result.is_ok());

        router.shutdown();
    }

    #[tokio::test]
    async fn test_unregister_nonexistent_no_panic() {
        let router = MailboxRouter::new();
        // 存在しないアドレスの解除でパニックしない
        router.unregister("ghost").await;
        assert_eq!(router.count().await, 0);
        router.shutdown();
    }

    #[tokio::test]
    async fn test_duplicate_register_overwrites() {
        let router = MailboxRouter::new();

        let _handle_old = router.register("dup").await;
        let handle_new = router.register("dup").await;
        let sender = router.register("sender").await;

        // 同一アドレスの再登録 → 新しいハンドルが有効
        // Router の HashMap が上書きされるため、新しい tx にメッセージが届く
        sender.send_to("dup", &"hello").await.unwrap();

        // 新しいハンドルで受信できる
        let msg = handle_new.recv().await.unwrap();
        assert_eq!(msg.payload_as::<String>().unwrap(), "hello");

        // アドレス数は1（上書きなので増えない）
        assert_eq!(router.count().await, 2); // "dup" + "sender"

        router.shutdown();
    }

    #[tokio::test]
    async fn test_shutdown_then_send_returns_error() {
        let router = MailboxRouter::new();
        let handle = router.register("agent").await;

        router.shutdown();
        // Router ループ終了を待つ
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Router が閉じているため送信はエラー
        let result = handle.send_to("anyone", &"msg").await;
        assert!(result.is_err());

        // 2回目の shutdown でパニックしない
        router.shutdown();
    }

    #[tokio::test]
    async fn test_handle_address() {
        let router = MailboxRouter::new();
        let handle = router.register("my-agent").await;
        assert_eq!(handle.address(), "my-agent");
        router.shutdown();
    }

    #[tokio::test]
    async fn test_payload_as_type_mismatch_returns_none() {
        let msg = MailboxMessage::new("a", "b", MessageKind::Direct).with_payload(&"hello");
        // 文字列ペイロードを数値として取得 → None
        let result: Option<i32> = msg.payload_as();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_payload_as_null_returns_none() {
        let msg = MailboxMessage::new("a", "b", MessageKind::Direct);
        // ペイロード未設定（Null）→ None
        let result: Option<String> = msg.payload_as();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_fan_in_multiple_senders() {
        let router = MailboxRouter::new();

        let receiver = router.register("receiver").await;
        let mut senders = Vec::new();
        for i in 0..5 {
            senders.push(router.register(format!("sender-{}", i)).await);
        }

        // 5つの送信者から同時送信
        for (i, sender) in senders.iter().enumerate() {
            sender
                .send_to("receiver", &format!("msg-{}", i))
                .await
                .unwrap();
        }

        // 全件受信を確認
        let mut received = Vec::new();
        for _ in 0..5 {
            let msg = receiver.recv().await.unwrap();
            received.push(msg.payload_as::<String>().unwrap());
        }
        received.sort();
        assert_eq!(received, vec!["msg-0", "msg-1", "msg-2", "msg-3", "msg-4"]);

        router.shutdown();
    }
}
