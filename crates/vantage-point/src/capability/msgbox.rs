//! Msgbox — Actor 間 1:1 メッセージキュー (VP-24)
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

use crate::capability::msgbox_registry::{ResolvedAddress, parse_address};
use crate::capability::msgbox_remote::RemoteRoutingClient;
use crate::capability::whitesnake::Whitesnake;

/// 永続化メッセージを保存する Whitesnake namespace
const MAILBOX_NAMESPACE: &str = "msgbox";
/// 永続化メッセージの key prefix（`msg/{id}`）
const MAILBOX_KEY_PREFIX: &str = "msg/";
/// デフォルト TTL（48 時間、ミリ秒）
const DEFAULT_TTL_MS: u64 = 48 * 3600 * 1000;
/// GC スイープ間隔（ミリ秒）
const GC_INTERVAL_MS: u64 = 5 * 60 * 1000;
/// Remote forward queue 容量（メモリ無防備防止、Phase 3 Step 2）
const REMOTE_FORWARD_QUEUE_CAP: usize = 10_000;

/// 現在時刻（Unix epoch ミリ秒）
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// Message
// =============================================================================

/// Msgbox で送受信されるメッセージ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
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
    /// 永続化フラグ（true の場合、Process 再起動後も生存）
    #[serde(default)]
    pub persistent: bool,
    /// 失効時刻（Unix epoch ミリ秒）
    ///
    /// persistent メッセージのみ有効。`now_ms()` 超過で GC 対象。
    /// None の場合、送信時に `DEFAULT_TTL_MS` (48h) が自動適用される。
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// 明示 ack モード（true の場合、recv での自動 ack を無効化）
    ///
    /// 受信側が `Handle::ack(id)` を明示呼び出しするまで DISC を保持。
    /// 受信後の処理中クラッシュで再配信したいパターン向け。
    #[serde(default)]
    pub manual_ack: bool,
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

impl Message {
    /// 新しいメッセージを作成
    pub fn new(from: impl Into<String>, to: impl Into<String>, kind: MessageKind) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            kind,
            payload: serde_json::Value::Null,
            timestamp: now_ms(),
            reply_to: None,
            id: uuid::Uuid::new_v4().to_string(),
            persistent: false,
            expires_at: None,
            manual_ack: false,
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

    /// 永続化フラグを設定（Process 再起動後も生存）
    ///
    /// Router が Whitesnake で構築されている場合のみ実効。
    /// `with_persistence()` で作られた Router 以外では no-op（in-memory 配信）。
    pub fn persistent(mut self) -> Self {
        self.persistent = true;
        self
    }

    /// TTL を秒単位で設定（persistent メッセージの失効時刻を now + secs に）
    pub fn with_ttl_secs(mut self, secs: u64) -> Self {
        self.expires_at = Some(now_ms().saturating_add(secs.saturating_mul(1000)));
        self
    }

    /// TTL をミリ秒単位で設定
    pub fn with_ttl_ms(mut self, ms: u64) -> Self {
        self.expires_at = Some(now_ms().saturating_add(ms));
        self
    }

    /// 明示 ack モードを有効化（recv での自動 ack を無効化）
    ///
    /// 受信側が `Handle::ack(id)` を呼ぶまで DISC を保持。
    /// 受信後の処理中にクラッシュしても、Process 再起動時に再配信される。
    pub fn manual_ack(mut self) -> Self {
        self.manual_ack = true;
        self
    }

    /// メッセージが失効しているか判定
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => now_ms() >= exp,
            None => false,
        }
    }

    /// ペイロードを型付きで取得
    pub fn payload_as<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        serde_json::from_value(self.payload.clone()).ok()
    }
}

// =============================================================================
// MessageEnvelope — 診断用の msg 履歴 entry (VP-83 Stand 自己診断)
// =============================================================================

/// Mailbox msg lifecycle の observable state
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeState {
    /// send された (Router 受領)
    Queued,
    /// recv で取り出された (宛先 Handle から外された = "開封")
    Received,
    /// 明示 ack された (manual_ack 時のみ、persistent store 削除)
    Acked,
}

/// 診断用 envelope (history 表示用の軽量メタ、payload は truncate)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEnvelope {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: String, // MessageKind の文字列表現
    pub sent_at_ms: u64,
    pub received_at_ms: Option<u64>,
    pub acked_at_ms: Option<u64>,
    pub state: EnvelopeState,
    /// payload の要約 (先頭 80 文字)
    pub payload_preview: String,
}

impl MessageEnvelope {
    fn from_msg(msg: &Message) -> Self {
        let kind = match msg.kind {
            MessageKind::Direct => "direct",
            MessageKind::Notification => "notification",
            MessageKind::Request => "request",
            MessageKind::Response => "response",
        };
        let preview = {
            let s = msg.payload.to_string();
            if s.len() > 80 {
                format!("{}…", &s[..80])
            } else {
                s
            }
        };
        Self {
            id: msg.id.clone(),
            from: msg.from.clone(),
            to: msg.to.clone(),
            kind: kind.to_string(),
            sent_at_ms: msg.timestamp,
            received_at_ms: None,
            acked_at_ms: None,
            state: EnvelopeState::Queued,
            payload_preview: preview,
        }
    }
}

/// bounded history buffer (最新 N 件を保持、push で古い entry を pop)
const HISTORY_CAP: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct MessageHistory {
    /// envelope の index (id → buffer idx)
    /// 注: ring buffer で popfront されると id は無効化、その時は skip
    pub(crate) entries: Arc<RwLock<std::collections::VecDeque<MessageEnvelope>>>,
}

impl MessageHistory {
    fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(std::collections::VecDeque::with_capacity(
                HISTORY_CAP,
            ))),
        }
    }

    async fn record_sent(&self, msg: &Message) {
        let env = MessageEnvelope::from_msg(msg);
        let mut h = self.entries.write().await;
        if h.len() >= HISTORY_CAP {
            h.pop_front();
        }
        h.push_back(env);
    }

    async fn mark_received(&self, msg_id: &str) {
        let mut h = self.entries.write().await;
        if let Some(env) = h.iter_mut().find(|e| e.id == msg_id) {
            env.received_at_ms = Some(now_ms());
            env.state = EnvelopeState::Received;
        }
    }

    async fn mark_acked(&self, msg_id: &str) {
        let mut h = self.entries.write().await;
        if let Some(env) = h.iter_mut().find(|e| e.id == msg_id) {
            env.acked_at_ms = Some(now_ms());
            env.state = EnvelopeState::Acked;
        }
    }

    /// 新しい順に最大 limit 件
    pub async fn recent(&self, limit: usize) -> Vec<MessageEnvelope> {
        let h = self.entries.read().await;
        h.iter().rev().take(limit).cloned().collect()
    }
}

// =============================================================================
// Handle — 各 Capability が持つ送受信ハンドル
// =============================================================================

/// 個別の Msgbox ハンドル
///
/// 各 Capability / Agent が保持し、メッセージの送受信に使う。
/// `CapabilityContext` 経由で渡される。
///
/// Selective Receive: `recv_matching()` でフィルタ不一致メッセージを
/// 内部 stash に退避し、次回の recv で再確認する（Erlang 方式）。
#[derive(Debug, Clone)]
pub struct Handle {
    /// 自身のアドレス
    address: String,
    /// Router への送信チャンネル（他者宛メッセージを Router に渡す）
    router_tx: mpsc::Sender<Message>,
    /// 自分宛メッセージの受信チャンネル
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Message>>>,
    /// Selective Receive 用のメッセージ退避バッファ
    stash: Arc<tokio::sync::Mutex<std::collections::VecDeque<Message>>>,
    /// 永続化バックエンド（persistent メッセージ受信時に ack = DISC 削除）
    whitesnake: Option<Whitesnake>,
    /// msg 履歴 tracker (VP-83 Stand 自己診断、Router と共有)
    history: MessageHistory,
}

impl Handle {
    /// 自身のアドレスを取得
    pub fn address(&self) -> &str {
        &self.address
    }

    /// メッセージを送信（Router 経由で宛先に配信）
    pub async fn send(&self, msg: Message) -> Result<(), Error> {
        // VP-83: 診断用に履歴を記録 (send hook)
        self.history.record_sent(&msg).await;
        self.router_tx
            .send(msg)
            .await
            .map_err(|_| Error::RouterClosed)
    }

    /// ダイレクトメッセージを簡易送信
    pub async fn send_to(
        &self,
        to: impl Into<String>,
        payload: &impl Serialize,
    ) -> Result<(), Error> {
        let msg = Message::new(&self.address, to, MessageKind::Direct).with_payload(payload);
        self.send(msg).await
    }

    /// 通知を送信
    pub async fn notify(
        &self,
        to: impl Into<String>,
        payload: &impl Serialize,
    ) -> Result<(), Error> {
        let msg = Message::new(&self.address, to, MessageKind::Notification).with_payload(payload);
        self.send(msg).await
    }

    /// メッセージを受信（ブロッキング）
    ///
    /// stash にメッセージがあればそちらを先に返す。
    /// persistent メッセージの場合、受信完了時に永続ストアから DISC を削除（ack）。
    pub async fn recv(&self) -> Option<Message> {
        // stash を先に確認
        let msg = {
            let mut stash = self.stash.lock().await;
            stash.pop_front()
        };
        let msg = match msg {
            Some(msg) => Some(msg),
            None => self.rx.lock().await.recv().await,
        };
        // VP-83: 診断用に history を更新 (recv hook、開封状況に相当)
        if let Some(ref m) = msg {
            self.history.mark_received(&m.id).await;
        }
        self.ack_if_persistent(msg.as_ref()).await;
        msg
    }

    /// Selective Receive: 条件に合うメッセージのみ受信（Erlang 方式）
    ///
    /// 条件に合わないメッセージは stash に退避し、次回の recv/recv_matching で再確認。
    /// メッセージロスが起きない cancel-safe 設計。
    /// persistent メッセージの場合、受信完了時に永続ストアから DISC を削除（ack）。
    ///
    /// ## Cancel Safety
    ///
    /// `tokio::time::timeout` 等で外側からこの Future をキャンセルしても
    /// メッセージは失われない。実装方針:
    ///
    /// 1. `rx.lock()` を保持したままの長時間 await を避ける
    /// 2. `rx.recv()` 呼び出し前後でロックを取り直す（try_recv + 待機のループ）
    /// 3. deferred メッセージは stash に随時書き込む（rx ロックを手放した直後）
    ///    → キャンセル時点で stash に保存済みのため消失しない
    pub async fn recv_matching<F>(&self, predicate: F) -> Option<Message>
    where
        F: Fn(&Message) -> bool,
    {
        // まず stash から条件に合うものを探す
        {
            let mut stash = self.stash.lock().await;
            if let Some(pos) = stash.iter().position(&predicate) {
                let msg = stash.remove(pos);
                drop(stash);
                self.ack_if_persistent(msg.as_ref()).await;
                return msg;
            }
        }

        // チャンネルからメッセージを 1 件ずつ取り出す。
        //
        // cancel-safe のために rx ロックの保持を 1 メッセージ分ずつに限定する:
        //   1. try_recv で即時取得を試みる
        //   2. メッセージがなければ recv().await で待機（1 件取れたら即ロック解放）
        //   3. deferred は stash に即書き込み → キャンセルされても消えない
        loop {
            // --- step 1: stash を再確認（別タスクが追加した可能性） ---
            {
                let mut stash = self.stash.lock().await;
                if let Some(pos) = stash.iter().position(&predicate) {
                    let msg = stash.remove(pos);
                    drop(stash);
                    self.ack_if_persistent(msg.as_ref()).await;
                    return msg;
                }
            }

            // --- step 2: チャンネルから 1 件受信（rx ロックは最小限） ---
            let msg = {
                let mut rx = self.rx.lock().await;
                // try_recv で即時取得を試みる
                match rx.try_recv() {
                    Ok(msg) => Some(msg),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        // 待機が必要 — ロックを保持したまま await するが、
                        // 1 件取れたら即 break するため最短で解放される。
                        // キャンセルされた場合 MutexGuard は Drop され、
                        // 取得前のメッセージ（= チャンネル内）は消えない。
                        rx.recv().await
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => None,
                }
            }; // ← rx ロックをここで解放

            match msg {
                None => {
                    // チャンネルが閉じた
                    return None;
                }
                Some(msg) => {
                    if predicate(&msg) {
                        self.ack_if_persistent(Some(&msg)).await;
                        return Some(msg);
                    }
                    // 条件不一致 → stash に即書き込み（rx ロック解放後なので安全）
                    self.stash.lock().await.push_back(msg);
                    // 次のイテレーションへ（stash + チャンネルを再スキャン）
                }
            }
        }
    }

    /// persistent メッセージの受信完了を永続ストアに反映（DISC 削除 = ack）
    ///
    /// `manual_ack: true` のメッセージは自動 ack せず、受信側が `ack()` を
    /// 明示呼び出しするまで DISC を保持する。
    async fn ack_if_persistent(&self, msg: Option<&Message>) {
        let Some(msg) = msg else { return };
        if !msg.persistent || msg.manual_ack {
            return;
        }
        self.ack_by_id(&msg.id).await;
    }

    /// メッセージID で明示的に ack（DISC 削除）
    ///
    /// `manual_ack: true` で受信したメッセージに対して呼ぶ。
    /// 受信後の処理が完了してから呼ぶことで、途中クラッシュ時の再配信を保証。
    pub async fn ack(&self, msg_id: &str) {
        self.ack_by_id(msg_id).await;
    }

    async fn ack_by_id(&self, msg_id: &str) {
        let Some(ws) = &self.whitesnake else {
            // Whitesnake 未注入でも history は更新（観測用）
            self.history.mark_acked(msg_id).await;
            return;
        };
        let key = format!("{}{}", MAILBOX_KEY_PREFIX, msg_id);
        if let Err(e) = ws.remove(MAILBOX_NAMESPACE, &key).await {
            tracing::warn!("Msgbox: persistent ack failed id={} err={}", msg_id, e);
            return;
        }
        // 永続削除成功時のみ acked 記録
        self.history.mark_acked(msg_id).await;
    }
}

// =============================================================================
// Router — メッセージルーティング
// =============================================================================

/// メッセージルーター
///
/// 全 Msgbox を管理し、メッセージを宛先に配信する。
/// Process（SP）または TheWorld が保持する。
///
/// Whitesnake を注入した場合、`persistent: true` のメッセージを SurrealDB/ファイルに保存し、
/// Process 再起動後に `restore_pending()` で未配信メッセージを再投入可能。
pub struct Router {
    /// アドレス → 送信チャンネルのマッピング
    boxes: Arc<RwLock<HashMap<String, mpsc::Sender<Message>>>>,
    /// Router への送信チャンネル
    router_tx: mpsc::Sender<Message>,
    /// ルーティングループの停止トークン
    shutdown: tokio_util::sync::CancellationToken,
    /// 永続化バックエンド（persistent メッセージ対応）
    whitesnake: Option<Whitesnake>,
    /// Remote routing client（cross-Process 配信、Phase 3 Step 2）
    /// None の場合は Process-local のみ
    remote: Option<RemoteRoutingClient>,
    /// msg 履歴 tracker (VP-83 Stand 自己診断、全 Handle と共有)
    history: MessageHistory,
}

impl Router {
    /// 新しい Router を作成し、ルーティングループを開始（永続化・remote なし）
    pub fn new() -> Self {
        Self::new_inner(None, None)
    }

    /// 永続化バックエンド付きで Router を作成
    ///
    /// `persistent: true` のメッセージは Whitesnake に保存され、
    /// Process 再起動後に `restore_pending()` で再投入できる。
    pub fn with_persistence(whitesnake: Whitesnake) -> Self {
        Self::new_inner(Some(whitesnake), None)
    }

    /// Remote routing 付き（永続化なし）— 主にテスト・World モード以外用
    pub fn with_remote(remote: RemoteRoutingClient) -> Self {
        Self::new_inner(None, Some(remote))
    }

    /// 永続化 + Remote routing 両方付き — 通常の VP Process 構成
    pub fn with_persistence_and_remote(
        whitesnake: Whitesnake,
        remote: RemoteRoutingClient,
    ) -> Self {
        Self::new_inner(Some(whitesnake), Some(remote))
    }

    /// Remote forward 専用 worker task
    ///
    /// routing_loop から `(ResolvedAddress, Message)` を受け取り、
    /// `RemoteRoutingClient::forward` を呼ぶ。
    /// unbounded で受けて routing_loop 側を絶対ブロックさせない。
    /// エラー時は warn ログのみ（persistent message は送信側で永続化済みなので
    /// 再起動で復元される設計）。
    async fn remote_forward_loop(
        mut rx: mpsc::Receiver<(ResolvedAddress, Message)>,
        client: RemoteRoutingClient,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::debug!("Router: remote forward loop 終了");
                    break;
                }
                item = rx.recv() => {
                    let Some((resolved, msg)) = item else { break };
                    if let Err(e) = client.forward(&resolved, msg).await {
                        tracing::warn!(
                            "Router: remote forward 失敗 to='{}' err={}",
                            resolved.actor_or_unknown(),
                            e
                        );
                    }
                }
            }
        }
    }

    fn new_inner(whitesnake: Option<Whitesnake>, remote: Option<RemoteRoutingClient>) -> Self {
        let (router_tx, router_rx) = mpsc::channel::<Message>(1024);
        let boxes: Arc<RwLock<HashMap<String, mpsc::Sender<Message>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let shutdown = tokio_util::sync::CancellationToken::new();

        // Remote forward 用 bounded channel（cap=10000、メモリ無防備防止）+ worker task
        let remote_tx = remote.as_ref().map(|client| {
            let (tx, rx) = mpsc::channel::<(ResolvedAddress, Message)>(REMOTE_FORWARD_QUEUE_CAP);
            let shutdown_remote = shutdown.clone();
            tokio::spawn(Self::remote_forward_loop(
                rx,
                client.clone(),
                shutdown_remote,
            ));
            tx
        });

        // ルーティングループ
        let boxes_clone = boxes.clone();
        let shutdown_clone = shutdown.clone();
        let ws_clone = whitesnake.clone();
        let remote_clone = remote.clone();
        let remote_tx_clone = remote_tx.clone();
        tokio::spawn(Self::routing_loop(
            router_rx,
            boxes_clone,
            shutdown_clone,
            ws_clone,
            remote_clone,
            remote_tx_clone,
        ));

        // GC タスク（Whitesnake 注入時のみ）
        if let Some(ws) = whitesnake.clone() {
            let shutdown_gc = shutdown.clone();
            tokio::spawn(Self::gc_loop(ws, shutdown_gc));
        }

        Self {
            boxes,
            router_tx,
            shutdown,
            whitesnake,
            remote,
            history: MessageHistory::new(),
        }
    }

    /// 診断用の msg 履歴 (新しい順、最大 limit 件)
    pub async fn recent_history(&self, limit: usize) -> Vec<MessageEnvelope> {
        self.history.recent(limit).await
    }

    /// Remote forward 経由で受け取ったメッセージを **そのまま** ローカル box に配信
    ///
    /// HTTP `/api/msgbox/remote_deliver` ハンドラから呼ぶ。
    /// `routing_loop` を介さないため remote forward の二重ループを起こさない。
    /// 受信側で permanent 化済み（送信側 Process が永続化済み）なのでここでは ack 不要。
    pub async fn deliver_local(&self, msg: Message) -> Result<(), Error> {
        let boxes = self.boxes.read().await;
        let Some(tx) = boxes.get(&msg.to) else {
            tracing::debug!(
                "Router::deliver_local: 宛先 '{}' が見つからない（from: {}）",
                msg.to,
                msg.from
            );
            return Err(Error::BoxNotFound {
                address: msg.to.clone(),
            });
        };
        tx.try_send(msg).map_err(|_| Error::RouterClosed)
    }

    /// GC ループ — 期限切れ persistent メッセージを定期的にクリーンアップ
    async fn gc_loop(whitesnake: Whitesnake, shutdown: tokio_util::sync::CancellationToken) {
        let interval = std::time::Duration::from_millis(GC_INTERVAL_MS);
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 初回の即時 tick はスキップして間隔空ける
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::debug!("Router: GC ループ終了");
                    break;
                }
                _ = ticker.tick() => {
                    match Self::sweep_expired(&whitesnake).await {
                        Ok(0) => {}
                        Ok(n) => tracing::info!("Msgbox GC: {} 件の期限切れを削除", n),
                        Err(e) => tracing::warn!("Msgbox GC: スイープ失敗 {}", e),
                    }
                }
            }
        }
    }

    /// 期限切れメッセージを一括削除
    async fn sweep_expired(whitesnake: &Whitesnake) -> anyhow::Result<usize> {
        let discs = whitesnake
            .list_by_prefix(MAILBOX_NAMESPACE, MAILBOX_KEY_PREFIX)
            .await?;
        let mut removed = 0;
        for disc in discs {
            if let Ok(msg) = disc.extract::<Message>()
                && msg.is_expired()
            {
                let key = format!("{}{}", MAILBOX_KEY_PREFIX, msg.id);
                if whitesnake.remove(MAILBOX_NAMESPACE, &key).await.is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// ルーティングループ — Router に届いたメッセージを宛先に配信
    ///
    /// persistent メッセージは配信前に Whitesnake に保存。
    /// 受信側の recv() で ack（DISC 削除）される。
    async fn routing_loop(
        mut router_rx: mpsc::Receiver<Message>,
        boxes: Arc<RwLock<HashMap<String, mpsc::Sender<Message>>>>,
        shutdown: tokio_util::sync::CancellationToken,
        whitesnake: Option<Whitesnake>,
        remote: Option<RemoteRoutingClient>,
        remote_tx: Option<mpsc::Sender<(ResolvedAddress, Message)>>,
    ) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("Router: ルーティングループ終了");
                    break;
                }
                msg = router_rx.recv() => {
                    match msg {
                        Some(mut msg) => {
                            // persistent なら配信前に永続化
                            if msg.persistent && let Some(ws) = &whitesnake {
                                // TTL 未設定ならデフォルト（48h）を適用
                                if msg.expires_at.is_none() {
                                    msg.expires_at = Some(now_ms().saturating_add(DEFAULT_TTL_MS));
                                }
                                let key = format!("{}{}", MAILBOX_KEY_PREFIX, msg.id);
                                if let Err(e) = ws.extract(MAILBOX_NAMESPACE, &key, &msg).await {
                                    tracing::warn!(
                                        "Router: persistent 保存失敗 id={} err={}",
                                        msg.id,
                                        e
                                    );
                                }
                            }

                            // Remote routing 判定（Phase 3 Step 2）
                            // 1. parse_address で local / port / project を判定
                            // 2. remote が None → 全部 local 扱い（後方互換）
                            // 3. remote.is_local(addr) → ローカル配信、それ以外 → forward
                            let resolved = match parse_address(&msg.to) {
                                Ok(r) => r,
                                Err(e) => {
                                    tracing::warn!(
                                        "Router: address parse error to='{}' err={}",
                                        msg.to,
                                        e
                                    );
                                    continue;
                                }
                            };

                            let is_local = match (&remote, &resolved) {
                                (None, _) => true,
                                (Some(client), addr) => client.is_local(addr),
                            };

                            // remote 配信: bounded channel に投函（worker task が forward）
                            // 満杯時は warn ログ + drop（persistent message は Whitesnake に
                            // 既に保存済みなので restore で復元される）
                            if !is_local {
                                if let Some(tx) = &remote_tx
                                    && let Err(e) = tx.try_send((resolved.clone(), msg.clone()))
                                {
                                    tracing::warn!(
                                        "Router: remote forward queue 満杯 / 閉鎖: {} (msg_id={})",
                                        e,
                                        msg.id
                                    );
                                }
                                continue;
                            }

                            // ローカル配信: actor 名のみで box を引く（@... は剥がす）
                            let local_actor = match &resolved {
                                ResolvedAddress::Local { actor }
                                | ResolvedAddress::Port { actor, .. }
                                | ResolvedAddress::Project { actor, .. } => actor.clone(),
                            };

                            let boxes = boxes.read().await;
                            if let Some(tx) = boxes.get(&local_actor) {
                                if let Err(e) = tx.try_send(msg.clone()) {
                                    tracing::warn!(
                                        "Router: {} 宛の配信失敗: {}",
                                        local_actor,
                                        e
                                    );
                                }
                            } else {
                                tracing::debug!(
                                    "Router: 宛先 '{}' が見つからない（from: {}）",
                                    msg.to,
                                    msg.from
                                );
                            }
                        }
                        None => {
                            tracing::info!("Router: router_tx がドロップされたため終了");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// 新しい Msgbox を登録し、ハンドルを返す
    pub async fn register(&self, address: impl Into<String>) -> Handle {
        let address = address.into();
        let (tx, rx) = mpsc::channel::<Message>(256);

        self.boxes.write().await.insert(address.clone(), tx);

        tracing::debug!("Router: '{}' を登録", address);

        Handle {
            address,
            router_tx: self.router_tx.clone(),
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            stash: Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::new())),
            whitesnake: self.whitesnake.clone(),
            history: self.history.clone(),
        }
    }

    /// 永続化ストアから未配信メッセージを復元し、Router に再投入
    ///
    /// Process 起動時に全 Stand の registration が完了した後で呼ぶ。
    /// 戻り値は再投入したメッセージ数。
    pub async fn restore_pending(&self) -> anyhow::Result<usize> {
        let Some(ws) = &self.whitesnake else {
            return Ok(0);
        };

        let discs = ws
            .list_by_prefix(MAILBOX_NAMESPACE, MAILBOX_KEY_PREFIX)
            .await?;
        let mut restored = 0;
        let mut expired = 0;
        for disc in discs {
            match disc.extract::<Message>() {
                Ok(msg) => {
                    // 期限切れは再投入せず即削除（GC を起動時に兼ねる）
                    if msg.is_expired() {
                        let key = format!("{}{}", MAILBOX_KEY_PREFIX, msg.id);
                        let _ = ws.remove(MAILBOX_NAMESPACE, &key).await;
                        expired += 1;
                        continue;
                    }
                    if let Err(e) = self.router_tx.send(msg).await {
                        tracing::warn!("Router: 復元したメッセージの再投入失敗: {}", e);
                        break;
                    }
                    restored += 1;
                }
                Err(e) => {
                    tracing::warn!("Router: DISC デコード失敗 path={} err={}", disc.path(), e);
                }
            }
        }

        if restored > 0 || expired > 0 {
            tracing::info!("Router: 復元={} 件 / 期限切れ削除={} 件", restored, expired);
        }
        Ok(restored)
    }

    /// Msgbox を登録解除
    pub async fn unregister(&self, address: &str) {
        if self.boxes.write().await.remove(address).is_some() {
            tracing::debug!("Router: '{}' を登録解除", address);
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

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Error
// =============================================================================

/// Msgbox 操作のエラー
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Router が閉じている（router_tx が drop された）
    #[error("msgbox router is closed")]
    RouterClosed,
    /// 宛先 box が見つからない
    #[error("msgbox address not found: {address}")]
    BoxNotFound { address: String },
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_send() {
        let router = Router::new();

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
        let router = Router::new();

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
        let router = Router::new();

        let handle_a = router.register("lead").await;
        let handle_b = router.register("worker-1").await;

        // worker → lead: 質問
        let question = Message::new("worker-1", "lead", MessageKind::Request)
            .with_payload(&serde_json::json!({"question": "DB スキーマどうする？"}));
        let question_id = question.id.clone();
        handle_b.send(question).await.unwrap();

        // lead で受信
        let received = handle_a.recv().await.unwrap();
        assert_eq!(received.kind, MessageKind::Request);

        // lead → worker: 回答
        let answer = Message::new("lead", "worker-1", MessageKind::Response)
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
        let router = Router::new();

        let _handle = router.register("temp-agent").await;
        assert_eq!(router.count().await, 1);

        router.unregister("temp-agent").await;
        assert_eq!(router.count().await, 0);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_addresses() {
        let router = Router::new();

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
        let router = Router::new();

        let handle = router.register("sender").await;

        // 存在しない宛先に送信 — エラーにはならず、ログに記録
        let result = handle.send_to("nonexistent", &"hello").await;
        assert!(result.is_ok()); // Router には届く、配信先がないだけ

        router.shutdown();
    }

    #[tokio::test]
    async fn test_message_id_unique() {
        let msg1 = Message::new("a", "b", MessageKind::Direct);
        let msg2 = Message::new("a", "b", MessageKind::Direct);
        assert_ne!(msg1.id, msg2.id);
    }

    #[tokio::test]
    async fn test_selective_receive_no_message_loss() {
        let router = Router::new();

        let handle_a = router.register("a").await;
        let handle_b = router.register("b").await;

        // 3つのメッセージを送信（異なる送信元）
        handle_a.send_to("b", &"from-a-1").await.unwrap();
        let msg_other = Message::new("other", "b", MessageKind::Direct).with_payload(&"from-other");
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
        let router = Router::new();

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
        let router = Router::new();

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
        let router = Router::new();
        // 存在しないアドレスの解除でパニックしない
        router.unregister("ghost").await;
        assert_eq!(router.count().await, 0);
        router.shutdown();
    }

    #[tokio::test]
    async fn test_duplicate_register_overwrites() {
        let router = Router::new();

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
        let router = Router::new();
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
        let router = Router::new();
        let handle = router.register("my-agent").await;
        assert_eq!(handle.address(), "my-agent");
        router.shutdown();
    }

    #[tokio::test]
    async fn test_payload_as_type_mismatch_returns_none() {
        let msg = Message::new("a", "b", MessageKind::Direct).with_payload(&"hello");
        // 文字列ペイロードを数値として取得 → None
        let result: Option<i32> = msg.payload_as();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_payload_as_null_returns_none() {
        let msg = Message::new("a", "b", MessageKind::Direct);
        // ペイロード未設定（Null）→ None
        let result: Option<String> = msg.payload_as();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_fan_in_multiple_senders() {
        let router = Router::new();

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

    // =========================================================================
    // 永続化テスト (opt-in persistent + Whitesnake)
    // =========================================================================

    #[tokio::test]
    async fn test_persistent_message_survives_router_restart() {
        let ws = Whitesnake::in_memory();

        // --- 第1ラウンド: persistent 送信 → recv 前に Router 消失 ---
        {
            let router = Router::with_persistence(ws.clone());
            let sender = router.register("sender").await;
            let _target = router.register("target").await;

            let msg = Message::new("sender", "target", MessageKind::Request)
                .with_payload(&"persistent-payload")
                .persistent();
            sender.send(msg).await.unwrap();

            // routing_loop による永続化完了を待機
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            router.shutdown();
        }

        // Whitesnake に 1 件残っているはず（未 recv なので未 ack）
        let pending = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "persistent メッセージが Whitesnake に残る"
        );

        // --- 第2ラウンド: 新しい Router で restore_pending → recv ---
        let router2 = Router::with_persistence(ws.clone());
        let target2 = router2.register("target").await;

        let restored = router2.restore_pending().await.unwrap();
        assert_eq!(restored, 1);

        let msg = target2.recv().await.unwrap();
        assert_eq!(msg.payload_as::<String>().unwrap(), "persistent-payload");
        assert!(msg.persistent);

        // recv 完了で ack → Whitesnake から削除
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
        let remaining = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(remaining.len(), 0, "recv 後に ack で DISC が消える");

        router2.shutdown();
    }

    #[tokio::test]
    async fn test_ephemeral_message_not_persisted() {
        let ws = Whitesnake::in_memory();
        let router = Router::with_persistence(ws.clone());
        let sender = router.register("sender").await;
        let target = router.register("target").await;

        // persistent フラグなしで送信（デフォルト ephemeral）
        sender.send_to("target", &"ephemeral").await.unwrap();

        let msg = target.recv().await.unwrap();
        assert!(!msg.persistent);
        assert_eq!(msg.payload_as::<String>().unwrap(), "ephemeral");

        // Whitesnake には何も保存されない
        let stored = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(stored.len(), 0);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_persistent_without_whitesnake_is_noop() {
        // Whitesnake なしの Router では persistent フラグは無視される
        let router = Router::new();
        let sender = router.register("sender").await;
        let target = router.register("target").await;

        let msg = Message::new("sender", "target", MessageKind::Direct)
            .with_payload(&"would-be-persistent")
            .persistent();
        sender.send(msg).await.unwrap();

        // 通常通り配信される（in-memory のみ）
        let received = target.recv().await.unwrap();
        assert!(received.persistent);
        assert_eq!(
            received.payload_as::<String>().unwrap(),
            "would-be-persistent"
        );

        router.shutdown();
    }

    #[tokio::test]
    async fn test_restore_pending_without_whitesnake_returns_zero() {
        let router = Router::new();
        let restored = router.restore_pending().await.unwrap();
        assert_eq!(restored, 0);
        router.shutdown();
    }

    #[tokio::test]
    async fn test_persistent_builder_sets_flag() {
        let msg = Message::new("a", "b", MessageKind::Direct).persistent();
        assert!(msg.persistent);

        let msg2 = Message::new("a", "b", MessageKind::Direct);
        assert!(!msg2.persistent);
    }

    #[tokio::test]
    async fn test_persistent_field_serde_default() {
        // 旧バージョンの JSON（persistent フィールドなし）をデコードできる
        let json = r#"{
            "from": "a",
            "to": "b",
            "kind": "direct",
            "payload": null,
            "timestamp": 0,
            "reply_to": null,
            "id": "test-id"
        }"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert!(!msg.persistent, "persistent が無い JSON はデフォルト false");
        assert!(msg.expires_at.is_none(), "expires_at デフォルト None");
        assert!(!msg.manual_ack, "manual_ack デフォルト false");
    }

    // =========================================================================
    // Phase 2: TTL + 明示 ack テスト
    // =========================================================================

    #[tokio::test]
    async fn test_ttl_builder_sets_expires_at() {
        let before = now_ms();
        let msg = Message::new("a", "b", MessageKind::Direct).with_ttl_secs(10);
        let after = now_ms();

        let exp = msg.expires_at.expect("expires_at should be set");
        assert!(exp >= before + 10_000);
        assert!(exp <= after + 10_000);
        assert!(!msg.is_expired());
    }

    #[tokio::test]
    async fn test_is_expired_true_when_past() {
        let mut msg = Message::new("a", "b", MessageKind::Direct);
        msg.expires_at = Some(now_ms().saturating_sub(1000)); // 1 秒前に失効
        assert!(msg.is_expired());
    }

    #[tokio::test]
    async fn test_is_expired_false_when_no_expiry() {
        let msg = Message::new("a", "b", MessageKind::Direct);
        assert!(!msg.is_expired(), "expires_at=None は is_expired=false");
    }

    #[tokio::test]
    async fn test_default_ttl_applied_on_persist() {
        let ws = Whitesnake::in_memory();
        let router = Router::with_persistence(ws.clone());
        let sender = router.register("sender").await;
        let _target = router.register("target").await;

        // TTL を明示せず persistent 送信
        let msg = Message::new("sender", "target", MessageKind::Direct)
            .with_payload(&"no-ttl")
            .persistent();
        sender.send(msg).await.unwrap();

        // routing_loop による永続化完了を待機
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let discs = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(discs.len(), 1);
        let stored: Message = discs[0].extract().unwrap();
        assert!(
            stored.expires_at.is_some(),
            "デフォルト TTL が自動適用される"
        );
        let exp = stored.expires_at.unwrap();
        // デフォルト 48h を大きく超えないことを確認（ほぼ now + 48h）
        assert!(exp > now_ms() + DEFAULT_TTL_MS - 60_000);
        assert!(exp < now_ms() + DEFAULT_TTL_MS + 60_000);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_restore_pending_skips_expired() {
        let ws = Whitesnake::in_memory();

        // 事前に期限切れメッセージを DISC に直接書き込み
        let mut expired_msg =
            Message::new("a", "target", MessageKind::Direct).with_payload(&"expired");
        expired_msg.persistent = true;
        expired_msg.expires_at = Some(now_ms().saturating_sub(1000)); // 1 秒前に失効
        let key = format!("msg/{}", expired_msg.id);
        ws.extract("msgbox", &key, &expired_msg).await.unwrap();

        // 有効なメッセージも 1 件
        let mut valid_msg = Message::new("a", "target", MessageKind::Direct).with_payload(&"valid");
        valid_msg.persistent = true;
        valid_msg.expires_at = Some(now_ms() + 60_000); // 1 分後失効
        let valid_id = valid_msg.id.clone();
        let key2 = format!("msg/{}", valid_msg.id);
        ws.extract("msgbox", &key2, &valid_msg).await.unwrap();

        // 復元: 期限切れは捨て、有効な 1 件だけ再投入
        let router = Router::with_persistence(ws.clone());
        let target = router.register("target").await;

        let restored = router.restore_pending().await.unwrap();
        assert_eq!(restored, 1);

        let received = target.recv().await.unwrap();
        assert_eq!(received.id, valid_id);
        assert_eq!(received.payload_as::<String>().unwrap(), "valid");

        // 期限切れは Whitesnake からも削除済み
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
        let remaining = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(remaining.len(), 0);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_manual_ack_keeps_disc_until_explicit_ack() {
        let ws = Whitesnake::in_memory();
        let router = Router::with_persistence(ws.clone());
        let sender = router.register("sender").await;
        let target = router.register("target").await;

        let msg = Message::new("sender", "target", MessageKind::Request)
            .with_payload(&"needs-ack")
            .persistent()
            .manual_ack();
        sender.send(msg).await.unwrap();

        // recv しても auto-ack されない
        let received = target.recv().await.unwrap();
        assert!(received.manual_ack);
        let msg_id = received.id.clone();

        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
        let still_stored = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(still_stored.len(), 1, "manual_ack では recv 後も DISC 保持");

        // 明示 ack で削除
        target.ack(&msg_id).await;
        let after_ack = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(after_ack.len(), 0);

        router.shutdown();
    }

    #[tokio::test]
    async fn test_auto_ack_still_works_by_default() {
        // 既存の persistent（manual_ack なし）は従来通り auto-ack
        let ws = Whitesnake::in_memory();
        let router = Router::with_persistence(ws.clone());
        let sender = router.register("sender").await;
        let target = router.register("target").await;

        let msg = Message::new("sender", "target", MessageKind::Direct)
            .with_payload(&"auto")
            .persistent();
        sender.send(msg).await.unwrap();

        let _received = target.recv().await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let remaining = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(remaining.len(), 0, "manual_ack なしでは auto-ack");

        router.shutdown();
    }

    #[tokio::test]
    async fn test_sweep_expired_removes_only_expired() {
        let ws = Whitesnake::in_memory();

        // 期限切れ × 2
        for i in 0..2 {
            let mut m =
                Message::new("a", "b", MessageKind::Direct).with_payload(&format!("expired-{}", i));
            m.persistent = true;
            m.expires_at = Some(now_ms().saturating_sub(1000));
            ws.extract("msgbox", &format!("msg/{}", m.id), &m)
                .await
                .unwrap();
        }
        // 有効 × 1
        let mut valid = Message::new("a", "b", MessageKind::Direct).with_payload(&"valid");
        valid.persistent = true;
        valid.expires_at = Some(now_ms() + 60_000);
        ws.extract("msgbox", &format!("msg/{}", valid.id), &valid)
            .await
            .unwrap();

        let removed = Router::sweep_expired(&ws).await.unwrap();
        assert_eq!(removed, 2);

        let remaining = ws.list_by_prefix("msgbox", "msg/").await.unwrap();
        assert_eq!(remaining.len(), 1);
        let stored: Message = remaining[0].extract().unwrap();
        assert_eq!(stored.payload_as::<String>().unwrap(), "valid");
    }
}
