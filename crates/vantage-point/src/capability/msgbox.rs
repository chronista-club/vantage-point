//! Msgbox — メッセージ data type (VP-179 Phase 5 スリム化後)
//!
//! VP-169 epic (mpsc 廃止 + Whitesnake-primary refactor) の Phase 5 (= VP-179) で
//! 旧 `Router` / `Handle` / `MessageEnvelope` / `MessageHistory` 等の Router 関連
//! 機構は全廃。 残るのは msg payload の data type (`Message` / `MessageKind`) のみ。
//!
//! ## 現在の role
//!
//! `Message` / `MessageKind` は **公共財** として複数 module で使用:
//! - `msgbox_v2::WhitesnakeStore` (msgs table insert/claim payload)
//! - `notification_actor` / `lane_spawn_actor`
//! - `routes/health::msgbox_send_handler`
//! - `msgbox_remote::RemoteRoutingClient` (cross-process forward)
//!
//! 注: `msg_*` MCP tool / QUIC handler は wiremsg 再設計 R5-1 で撤去済 (= 後続は `wire_*`)。
//! msgbox store 本体・HTTP route はまだ残存。
//!
//! 実際の routing は `WhitesnakeStore` (= SurrealDB msgs table) が単一経路。
//!
//! ## 関連
//!
//! - 設計: `docs/design/19-msgbox-whitesnake-primary.md`
//! - spike: `docs/design/20-spike-report.md`
//! - VP-169 epic: PR-1 (claim) → PR-2 (AppState) → PR-3 (dual write) → PR-4
//!   (external switch) → PR-5 (internal switch) → Phase 4 (cleanup) → Phase 5 (本 PR)

use serde::{Deserialize, Serialize};

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
    /// 失効時刻（Unix epoch ミリ秒）
    ///
    /// `now_ms()` 超過で GC 対象。 None の場合、送信時にデフォルト 48h が自動適用される。
    /// VP-158: 全 msg 永続化が default のため `persistent` flag は廃止、 TTL は全 msg に適用。
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// 明示 ack モード（true の場合、recv での自動 ack を無効化）
    ///
    /// VP-176/177 以降は WhitesnakeStore.claim の `mark_consumed` semantics に集約。
    /// fire-and-forget actor (= NotificationActor) は本 flag に関わらず常に mark_consumed。
    #[serde(default)]
    pub manual_ack: bool,
    /// forward 成功時刻（Unix epoch ミリ秒、 VP-164 / doc 18）
    ///
    /// cross-process msg の sender 側 lifecycle 用。
    /// - `Some(_)` = receiver の store に到達済
    /// - `None` = 未配信
    ///
    /// msgs table の同名 field に persisted (= msgbox_v2.rs)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_at: Option<u64>,
    /// recv 完了時刻（Unix epoch ミリ秒、 VP-164 / doc 18）
    ///
    /// receiver が claim → mark_consumed した時に更新。 msgs table の同名 field に persisted。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<u64>,
    /// 親メッセージ ID（wiremsg threading、 Phase A ①）
    ///
    /// `None` = thread の root。 `Some(id)` = その id への返信。
    /// 既存 `reply_to` (= 旧 Mailbox の thread 用) とは別物: `prev` は wiremsg の
    /// thread tree 用 field で、 `messages` table の `prev` record link に対応する。
    /// wiremsg 以外の msg では常に `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
    /// 所属 thread の root メッセージ ID（wiremsg threading、 Phase A ①）
    ///
    /// `messages` table の `thread_id` に対応。 root メッセージは自分自身の id を持つ。
    /// reply は返信先の `thread_id` を継承する。 wiremsg 以外の msg では `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
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
            expires_at: None,
            manual_ack: false,
            forwarded_at: None,
            consumed_at: None,
            prev: None,
            thread_id: None,
        }
    }

    /// このメッセージを wiremsg thread の root に確定する（Phase A ①）
    ///
    /// `prev = None`、 `thread_id = 自分の id` をセットして返す。
    /// `wire_send`（新規 thread）で使う。 `id` は `Message::new` 時点で確定済なので
    /// それを self-reference にする。
    pub fn as_thread_root(mut self) -> Self {
        self.prev = None;
        self.thread_id = Some(self.id.clone());
        self
    }

    /// このメッセージを wiremsg thread の reply に確定する（Phase A ①）
    ///
    /// `prev = 返信先 id`、 `thread_id = 返信先から継承した root id` をセットして返す。
    /// `wire_send`（`reply_to` 指定時）で使う。
    pub fn as_thread_reply(
        mut self,
        prev_id: impl Into<String>,
        thread_id: impl Into<String>,
    ) -> Self {
        self.prev = Some(prev_id.into());
        self.thread_id = Some(thread_id.into());
        self
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

    /// TTL を秒単位で設定（失効時刻を now + secs に。 VP-158: 全 msg に適用）
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Message::new で id / timestamp が auto-populate されること
    #[test]
    fn new_populates_id_and_timestamp() {
        let msg = Message::new("from", "to", MessageKind::Direct);
        assert!(!msg.id.is_empty());
        assert!(msg.timestamp > 0);
        assert_eq!(msg.from, "from");
        assert_eq!(msg.to, "to");
        assert_eq!(msg.kind, MessageKind::Direct);
    }

    /// with_payload + payload_as で round-trip すること
    #[test]
    fn payload_round_trip() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Cmd {
            action: String,
            count: u32,
        }
        let cmd = Cmd {
            action: "spawn".to_string(),
            count: 3,
        };
        let msg = Message::new("a", "b", MessageKind::Direct).with_payload(&cmd);
        let back: Option<Cmd> = msg.payload_as();
        assert_eq!(back, Some(cmd));
    }

    /// with_ttl_secs / is_expired の境界
    #[test]
    fn ttl_and_expired() {
        let msg = Message::new("a", "b", MessageKind::Direct);
        assert!(!msg.is_expired(), "expires_at = None は never expire");

        let m2 = Message::new("a", "b", MessageKind::Direct).with_ttl_ms(0);
        assert!(m2.is_expired(), "TTL=0ms なら直ちに expired");

        let m3 = Message::new("a", "b", MessageKind::Direct).with_ttl_secs(3600);
        assert!(!m3.is_expired(), "TTL=1h なら通常 expired ではない");
    }

    /// manual_ack flag
    #[test]
    fn manual_ack_flag() {
        let msg = Message::new("a", "b", MessageKind::Direct);
        assert!(!msg.manual_ack);

        let m2 = Message::new("a", "b", MessageKind::Direct).manual_ack();
        assert!(m2.manual_ack);
    }

    /// Message::new 時点では prev / thread_id は None
    #[test]
    fn new_has_no_thread_fields() {
        let msg = Message::new("a", "b", MessageKind::Direct);
        assert!(msg.prev.is_none());
        assert!(msg.thread_id.is_none());
    }

    /// as_thread_root は prev=None、 thread_id=自 id（self-reference）
    #[test]
    fn as_thread_root_sets_self_reference() {
        let msg = Message::new("a", "b", MessageKind::Direct).as_thread_root();
        assert!(msg.prev.is_none(), "root の prev は None");
        assert_eq!(
            msg.thread_id.as_deref(),
            Some(msg.id.as_str()),
            "root の thread_id は自分自身の id"
        );
    }

    /// as_thread_reply は prev=返信先、 thread_id=継承した root id
    #[test]
    fn as_thread_reply_inherits_thread_id() {
        let reply = Message::new("a", "b", MessageKind::Direct)
            .as_thread_reply("parent-msg-id", "root-msg-id");
        assert_eq!(reply.prev.as_deref(), Some("parent-msg-id"));
        assert_eq!(
            reply.thread_id.as_deref(),
            Some("root-msg-id"),
            "reply は返信先の thread_id を継承（自 id ではない）"
        );
    }
}
