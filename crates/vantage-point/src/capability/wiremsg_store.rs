//! wiremsg threaded inbox store (Phase A ①、 設計 memory `mem_1CbD9H1KGQykBaFG8XXVsn`)
//!
//! agent 間メッセージング「wiremsg」の inbox 実体。 既存 [`WhitesnakeStore`] (= msgs table,
//! claim-based Mailbox) と **並存** する threading 対応 store。 撤去は後続 Phase。
//!
//! ## 設計判断
//!
//! - **TopicRouter を使わない**: inbox = SurrealDB の message store。 `wire_recv` がその
//!   store を直接 long-poll する (= 既存 `msg_recv` / `WhitesnakeStore.claim` と同型)。
//! - **record link を query で辿らない**: `thread_id` / `prev` は plain string (= message
//!   の local id) で保持。 既存 msgs table の `id` / `reply_to` も plain string で同型、
//!   record-link traversal は migration 部分適用で壊れやすい (creo-memories の教訓)。
//! - **`created_at` / `read_cursor` は epoch ms (number)**: 既存 msgs.ts と同じ表現に揃え、
//!   cursor 比較を素直な数値比較にする (datetime serialize の罠回避)。
//! - **id は uuidv7**: 時刻順 sortable id (= ULID 相当)。 `uuid` crate の `now_v7()`。
//!
//! ## table
//!
//! - `wire_messages`: thread に属する message 本体 (`thread_id` / `prev` / `from_addr` /
//!   `to_addrs` / `body` / `created_at`)
//! - `thread_participant`: (thread, agent) ごとの参加情報 + 既読 cursor (plain table)
//!
//! schema は `db/mod.rs` の `SCHEMA_SQL` で define 済。

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use tokio::sync::{Mutex, Notify};

/// 現在時刻 (Unix epoch ms)
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 時刻順 sortable id を生成 (uuidv7 = ULID 相当)
fn new_message_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

// =============================================================================
// WireMessage — wiremsg の message data type
// =============================================================================

/// wiremsg thread に属する 1 件の message
///
/// `wire_messages` table の row に 1:1 対応する。 `id` は時刻順 (uuidv7)。
/// `thread_id` は所属 thread の root id (root は自分自身)、 `prev` は親 message id
/// (`None` = thread の root)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WireMessage {
    /// message id (uuidv7、 時刻順 sortable)
    pub id: String,
    /// 所属 thread の root message id。 root message は自分自身の id
    pub thread_id: String,
    /// 親 message id。 `None` = thread の root
    #[serde(default)]
    pub prev: Option<String>,
    /// 送信 agent address
    pub from: String,
    /// 宛先 agent address 群
    pub to: Vec<String>,
    /// message 本文 (任意 JSON object)
    pub body: serde_json::Value,
    /// 作成時刻 (Unix epoch ms)
    pub created_at: u64,
}

impl WireMessage {
    /// 新規 thread の root message を構築 (`prev = None`、 `thread_id = 自分自身`)
    pub fn new_root(from: impl Into<String>, to: Vec<String>, body: serde_json::Value) -> Self {
        let id = new_message_id();
        Self {
            thread_id: id.clone(),
            id,
            prev: None,
            from: from.into(),
            to,
            body,
            created_at: now_ms(),
        }
    }

    /// 既存 thread への reply message を構築
    ///
    /// `prev` = 返信先 message id、 `thread_id` = 返信先から継承した root id。
    pub fn new_reply(
        from: impl Into<String>,
        to: Vec<String>,
        body: serde_json::Value,
        prev_id: impl Into<String>,
        thread_id: impl Into<String>,
    ) -> Self {
        Self {
            id: new_message_id(),
            thread_id: thread_id.into(),
            prev: Some(prev_id.into()),
            from: from.into(),
            to,
            body,
            created_at: now_ms(),
        }
    }
}

// =============================================================================
// ParticipantStatus
// =============================================================================

/// thread 参加者の状態 (`thread_participant.status`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantStatus {
    /// 参加中 (notify / recv 対象)
    Active,
    /// ミュート中 (recv 対象だが notify は鳴らさない想定、 操作 tool は後続 Phase)
    Muted,
    /// 離脱済 (notify / recv 対象外)
    Left,
}

impl ParticipantStatus {
    /// DB の `status` 文字列から復元する (未知値は `Active` 扱い)
    ///
    /// 逆向き (= enum → 文字列) は現状 DB 書き込みが literal `'active'` で足りるため
    /// 持たない。 mute / leave の操作 tool を足す後続 Phase で必要になれば追加する。
    fn parse(s: &str) -> Self {
        match s {
            "muted" => ParticipantStatus::Muted,
            "left" => ParticipantStatus::Left,
            _ => ParticipantStatus::Active,
        }
    }
}

// =============================================================================
// WiremsgStore — SurrealDB embedded impl
// =============================================================================

/// wiremsg threaded inbox の store (Phase A ①)
///
/// 既存 [`WhitesnakeStore`](super::WhitesnakeStore) と同じく `Surreal<Any>` を共有して
/// 持つ。 `wire_messages` / `thread_participant` の 2 table を扱う。
#[derive(Clone)]
pub struct WiremsgStore {
    db: Arc<Surreal<Any>>,
}

impl WiremsgStore {
    /// 既存 `Surreal<Any>` connection から store を構築
    pub fn new(db: Arc<Surreal<Any>>) -> Self {
        Self { db }
    }

    /// underlying connection を参照
    pub fn db(&self) -> &Surreal<Any> {
        &self.db
    }

    // -------------------------------------------------------------------------
    // wire_send 系
    // -------------------------------------------------------------------------

    /// 新規 thread (root message) を送信
    ///
    /// Operations (Phase A ① 仕様):
    /// 1. `wire_messages` に root message (`prev=None`、 `thread_id`=自分) を INSERT
    /// 2. participant 作成:
    ///    - **送信者**: `read_cursor` = その message の `created_at` (= 起点既読)
    ///    - **受信者**: `read_cursor = None` (= 起点を未読として届ける、 仕様の重要点)
    ///
    /// 戻り値: INSERT した root [`WireMessage`] (caller が notify / id 通知に使う)。
    pub async fn send_root(
        &self,
        from: &str,
        to: &[String],
        body: serde_json::Value,
    ) -> Result<WireMessage> {
        let msg = WireMessage::new_root(from, to.to_vec(), body);
        self.insert_message(&msg).await?;

        // 送信者は起点を既読扱い (read_cursor = root の created_at)
        self.upsert_participant(&msg.thread_id, from, Some(msg.created_at))
            .await?;
        // 受信者は read_cursor=None — None でないと起点 message が既読扱いになるバグ
        for recipient in to {
            if recipient == from {
                continue; // 自己宛は送信者の participant で既に処理済
            }
            self.upsert_participant(&msg.thread_id, recipient, None)
                .await?;
        }
        Ok(msg)
    }

    /// 既存 thread への reply を送信
    ///
    /// Operations (Phase A ① 仕様):
    /// 1. `prev` = 返信先 message を `wire_messages` から取得し `thread_id` を継承
    /// 2. reply message を INSERT
    /// 3. 新規 agent (= `from` + `to` のうち未参加) の participant を upsert (`read_cursor=None`)
    ///    既存 participant の cursor / status は触らない
    ///
    /// 戻り値: INSERT した reply [`WireMessage`]。
    /// `prev_id` の message が存在しなければ `Err`。
    pub async fn send_reply(
        &self,
        from: &str,
        to: &[String],
        body: serde_json::Value,
        prev_id: &str,
    ) -> Result<WireMessage> {
        // 返信先 message を取得して thread_id を継承
        let prev_msg = self.get_message(prev_id).await?.ok_or_else(|| {
            anyhow::anyhow!("wiremsg send_reply: prev message '{prev_id}' not found")
        })?;

        let msg = WireMessage::new_reply(from, to.to_vec(), body, prev_id, &prev_msg.thread_id);
        self.insert_message(&msg).await?;

        // 新規 agent の participant のみ upsert (read_cursor=None)。
        // upsert_participant_if_absent は既存 row を触らないので cursor / status が保たれる。
        self.upsert_participant_if_absent(&msg.thread_id, from)
            .await?;
        for recipient in to {
            self.upsert_participant_if_absent(&msg.thread_id, recipient)
                .await?;
        }
        Ok(msg)
    }

    // -------------------------------------------------------------------------
    // wire_recv 系
    // -------------------------------------------------------------------------

    /// 指定 agent の未読 message を 1 回分取得 (long-poll はしない、 caller がループ制御)
    ///
    /// agent の参加 thread (`status != left`) から `created_at > read_cursor`
    /// (`read_cursor = None` なら全件) の message を `created_at` 昇順で取得する。
    ///
    /// 取得後、 caller は [`advance_cursor`](Self::advance_cursor) で cursor を
    /// **取得した最新 message の `created_at`** に前進させること (`now` ではなく — fetch 中
    /// 着信の取りこぼし race を避けるため)。 本メソッドは cursor を変更しない。
    pub async fn fetch_unread(&self, agent: &str) -> Result<Vec<WireMessage>> {
        // 1. agent の参加 thread と read_cursor を取得 (left は除外)
        let participants = self.list_active_participants(agent).await?;
        if participants.is_empty() {
            return Ok(Vec::new());
        }

        let mut out: Vec<WireMessage> = Vec::new();
        for (thread_id, read_cursor) in participants {
            // 2. thread 内の cursor 超過 message を取得
            let msgs = self.messages_after(&thread_id, read_cursor).await?;
            out.extend(msgs);
        }
        // 3. 全 thread 横断で created_at 昇順に整列
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// `wire_recv` 1 回分の store 操作: 未読取得 + read_cursor 前進をまとめて行う
    ///
    /// [`fetch_unread`](Self::fetch_unread) で未読を取得し、 thread ごとに
    /// 「取得した最新 message の `created_at`」 まで cursor を前進させる。
    /// 未読が空なら cursor は触らない。
    ///
    /// cursor を `now` ではなく **取得済 message の `created_at`** に合わせるのが要点
    /// (= fetch と advance の隙間に着信した message を取りこぼさないため)。
    pub async fn recv(&self, agent: &str) -> Result<Vec<WireMessage>> {
        let unread = self.fetch_unread(agent).await?;
        if unread.is_empty() {
            return Ok(unread);
        }
        // thread ごとに取得済 message の created_at 最大値を求める
        let mut thread_max: HashMap<String, u64> = HashMap::new();
        for m in &unread {
            let e = thread_max.entry(m.thread_id.clone()).or_insert(0);
            if m.created_at > *e {
                *e = m.created_at;
            }
        }
        for (thread_id, cursor) in thread_max {
            self.advance_cursor(&thread_id, agent, cursor).await?;
        }
        Ok(unread)
    }

    /// 指定 agent の指定 thread の read_cursor を前進させる
    ///
    /// `cursor` は [`fetch_unread`](Self::fetch_unread) で取得した最新 message の
    /// `created_at` を渡す。 既存 cursor より小さい値は無視 (= 後退させない)。
    pub async fn advance_cursor(&self, thread_id: &str, agent: &str, cursor: u64) -> Result<()> {
        let now = now_ms();
        // read_cursor IS NONE または read_cursor < cursor のときだけ前進
        self.db
            .query(
                "UPDATE thread_participant
                     SET read_cursor = $cursor, updated_at = $now
                     WHERE thread = $thread AND agent = $agent
                       AND (read_cursor IS NONE OR read_cursor < $cursor);",
            )
            .bind(("thread", thread_id.to_string()))
            .bind(("agent", agent.to_string()))
            .bind(("cursor", cursor))
            .bind(("now", now))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg advance_cursor failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("wiremsg advance_cursor check failed: {e}"))?;
        Ok(())
    }

    /// 指定 thread の参加者 address 群を返す (`wire_send` reply の notify 対象決定用)
    ///
    /// `exclude_left = true` なら `status = left` の participant を除外する。
    pub async fn thread_participants(
        &self,
        thread_id: &str,
        exclude_left: bool,
    ) -> Result<Vec<String>> {
        let mut res = self
            .db
            .query("SELECT agent, status FROM thread_participant WHERE thread = $thread;")
            .bind(("thread", thread_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg thread_participants failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg thread_participants take failed: {e}"))?;

        let mut agents = Vec::new();
        for row in rows {
            let agent = row["agent"].as_str().unwrap_or_default().to_string();
            if agent.is_empty() {
                continue;
            }
            let status = ParticipantStatus::parse(row["status"].as_str().unwrap_or("active"));
            if exclude_left && status == ParticipantStatus::Left {
                continue;
            }
            agents.push(agent);
        }
        Ok(agents)
    }

    // -------------------------------------------------------------------------
    // 内部 helper
    // -------------------------------------------------------------------------

    /// `wire_messages` table に message を INSERT
    async fn insert_message(&self, msg: &WireMessage) -> Result<()> {
        self.db
            .query(
                r#"
                CREATE type::record('wire_messages', $id) CONTENT {
                    id: $id, thread_id: $thread_id, prev: $prev,
                    from_addr: $from_addr, to_addrs: $to_addrs,
                    body: $body, created_at: $created_at
                }
                "#,
            )
            .bind(("id", msg.id.clone()))
            .bind(("thread_id", msg.thread_id.clone()))
            .bind(("prev", msg.prev.clone()))
            .bind(("from_addr", msg.from.clone()))
            .bind(("to_addrs", msg.to.clone()))
            .bind(("body", msg.body.clone()))
            .bind(("created_at", msg.created_at))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg insert_message failed: {e}"))?
            .check()
            .map_err(|e| anyhow::anyhow!("wiremsg insert_message check failed: {e}"))?;
        Ok(())
    }

    /// id 指定で 1 件の message を取得
    ///
    /// `CONTENT { id: $id }` で record id を message の uuidv7 に固定しているため、
    /// `type::record('wire_messages', $id)` で record id 直引きできる
    /// (= `WHERE id = $id` だと返却 `id` field が `wire_messages:<uuid>` 形式で
    /// bare uuid と一致しないため不可)。
    async fn get_message(&self, id: &str) -> Result<Option<WireMessage>> {
        let mut res = self
            .db
            .query("SELECT * FROM type::record('wire_messages', $id);")
            .bind(("id", id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg get_message failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg get_message take failed: {e}"))?;
        match rows.first() {
            Some(row) => Ok(Some(Self::row_to_message(row)?)),
            None => Ok(None),
        }
    }

    /// thread 内の `created_at > cursor` の message を昇順で取得 (`cursor = None` なら全件)
    async fn messages_after(
        &self,
        thread_id: &str,
        cursor: Option<u64>,
    ) -> Result<Vec<WireMessage>> {
        // cursor IS NONE と cursor 指定で query を分岐 (= bind の None を WHERE で扱うと
        // SurrealDB の比較が意図せぬ挙動になりうるため、 明示的に 2 query に分ける)。
        let mut res = match cursor {
            Some(c) => {
                self.db
                    .query(
                        "SELECT * FROM wire_messages
                             WHERE thread_id = $thread AND created_at > $cursor
                             ORDER BY created_at ASC, id ASC;",
                    )
                    .bind(("thread", thread_id.to_string()))
                    .bind(("cursor", c))
                    .await
            }
            None => {
                self.db
                    .query(
                        "SELECT * FROM wire_messages
                             WHERE thread_id = $thread
                             ORDER BY created_at ASC, id ASC;",
                    )
                    .bind(("thread", thread_id.to_string()))
                    .await
            }
        }
        .map_err(|e| anyhow::anyhow!("wiremsg messages_after failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg messages_after take failed: {e}"))?;
        rows.iter().map(Self::row_to_message).collect()
    }

    /// participant を upsert する (cursor を明示指定する版)
    ///
    /// `(thread, agent)` で一意なので、 既存があれば `read_cursor` / `status` を更新、
    /// 無ければ新規作成する。 `wire_send` (root) で使う。
    /// `read_cursor` は引数で渡された値で**上書き**する (送信者 = Some、 受信者 = None)。
    async fn upsert_participant(
        &self,
        thread_id: &str,
        agent: &str,
        read_cursor: Option<u64>,
    ) -> Result<()> {
        let now = now_ms();
        // 既存有無を確認 (UNIQUE index 前提)。 SurrealDB の UPSERT は record id 指定が
        // 要るが thread_participant は複合 key なので、 SELECT → UPDATE / CREATE で実装。
        let existing = self.find_participant_record_id(thread_id, agent).await?;
        match existing {
            Some(rid) => {
                self.db
                    .query(
                        "UPDATE type::thing('thread_participant', $rid)
                             SET read_cursor = $cursor, status = 'active', updated_at = $now;",
                    )
                    .bind(("rid", rid))
                    .bind(("cursor", read_cursor))
                    .bind(("now", now))
                    .await
                    .map_err(|e| anyhow::anyhow!("wiremsg upsert_participant update failed: {e}"))?
                    .check()
                    .map_err(|e| {
                        anyhow::anyhow!("wiremsg upsert_participant update check failed: {e}")
                    })?;
            }
            None => {
                self.db
                    .query(
                        "CREATE thread_participant CONTENT {
                             thread: $thread, agent: $agent, read_cursor: $cursor,
                             status: 'active', updated_at: $now
                         };",
                    )
                    .bind(("thread", thread_id.to_string()))
                    .bind(("agent", agent.to_string()))
                    .bind(("cursor", read_cursor))
                    .bind(("now", now))
                    .await
                    .map_err(|e| anyhow::anyhow!("wiremsg upsert_participant create failed: {e}"))?
                    .check()
                    .map_err(|e| {
                        anyhow::anyhow!("wiremsg upsert_participant create check failed: {e}")
                    })?;
            }
        }
        Ok(())
    }

    /// participant が無ければ `read_cursor=None` で作成する (既存は一切触らない)
    ///
    /// `wire_send` (reply) で使う。 既存参加者の cursor / status を保つのが目的。
    async fn upsert_participant_if_absent(&self, thread_id: &str, agent: &str) -> Result<()> {
        let now = now_ms();
        if self
            .find_participant_record_id(thread_id, agent)
            .await?
            .is_some()
        {
            return Ok(()); // 既存 — 触らない
        }
        self.db
            .query(
                "CREATE thread_participant CONTENT {
                     thread: $thread, agent: $agent, read_cursor: NONE,
                     status: 'active', updated_at: $now
                 };",
            )
            .bind(("thread", thread_id.to_string()))
            .bind(("agent", agent.to_string()))
            .bind(("now", now))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg upsert_participant_if_absent failed: {e}"))?
            .check()
            .map_err(|e| {
                anyhow::anyhow!("wiremsg upsert_participant_if_absent check failed: {e}")
            })?;
        Ok(())
    }

    /// `(thread, agent)` の participant の record id (local 部分) を返す
    async fn find_participant_record_id(
        &self,
        thread_id: &str,
        agent: &str,
    ) -> Result<Option<String>> {
        let mut res = self
            .db
            .query(
                "SELECT * FROM thread_participant
                     WHERE thread = $thread AND agent = $agent LIMIT 1;",
            )
            .bind(("thread", thread_id.to_string()))
            .bind(("agent", agent.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg find_participant failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg find_participant take failed: {e}"))?;
        match rows.first() {
            Some(row) => Ok(Some(Self::extract_record_local_id(
                &row["id"],
                "thread_participant",
            ))),
            None => Ok(None),
        }
    }

    /// agent が参加中 (`status != left`) の (thread_id, read_cursor) 一覧を返す
    async fn list_active_participants(&self, agent: &str) -> Result<Vec<(String, Option<u64>)>> {
        let mut res = self
            .db
            .query(
                "SELECT thread, read_cursor, status FROM thread_participant
                     WHERE agent = $agent AND status != 'left';",
            )
            .bind(("agent", agent.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg list_active_participants failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg list_active_participants take failed: {e}"))?;

        let mut out = Vec::new();
        for row in rows {
            let thread = row["thread"].as_str().unwrap_or_default().to_string();
            if thread.is_empty() {
                continue;
            }
            // read_cursor は number または null
            let cursor = row["read_cursor"].as_u64();
            out.push((thread, cursor));
        }
        Ok(out)
    }

    /// SurrealDB record id (`tb:<local>` 形式 or object) から local 部分を抽出
    fn extract_record_local_id(id_value: &serde_json::Value, table: &str) -> String {
        if let Some(s) = id_value.as_str() {
            let prefix = format!("{table}:");
            return s
                .strip_prefix(&prefix)
                .unwrap_or(s)
                .trim_matches('`')
                .to_string();
        }
        // raw record id object ({ "tb": ..., "id": ... })
        id_value["id"]
            .as_str()
            .or_else(|| id_value["String"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// `wire_messages` row JSON を [`WireMessage`] に hydrate
    fn row_to_message(row: &serde_json::Value) -> Result<WireMessage> {
        let id = Self::extract_record_local_id(&row["id"], "wire_messages");
        let to = row["to_addrs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(WireMessage {
            id,
            thread_id: row["thread_id"].as_str().unwrap_or_default().to_string(),
            prev: row["prev"].as_str().map(|s| s.to_string()),
            from: row["from_addr"].as_str().unwrap_or_default().to_string(),
            to,
            body: row["body"].clone(),
            created_at: row["created_at"].as_u64().unwrap_or_default(),
        })
    }
}

// =============================================================================
// WireNotifier — wire_recv long-poll の SP 内 in-process 起床機構
// =============================================================================

/// `wire_recv` の long-poll 待機を `wire_send` から起こすための notifier
///
/// agent address ごとに [`tokio::sync::Notify`] を持つ。 `wire_recv` が未読 0 のとき
/// その agent の `Notify` で待機し、 `wire_send` が宛先 agent の `Notify` を
/// `notify_waiters` する。 TopicRouter は介さず、 SP プロセス内の純粋な in-process 機構。
///
/// ## 取りこぼし防止プロトコル (重要)
///
/// `Notify::notify_waiters` は **その時点の待機者のみ** を起こし、 permit を貯めない。
/// したがって「notify → 後から wait」 の順だと取りこぼす。 これを避けるため
/// `wire_recv` handler は必ず次の順序を守ること:
///
/// 1. [`handle`](Self::handle) で agent の `Arc<Notify>` を取得
/// 2. `notify.notified()` で **待機 future を先に生成** (この瞬間以降の notify を捕捉)
/// 3. store を poll → 未読あれば即 return
/// 4. 未読なければ step 2 の future を await
///
/// store poll の前に future を生成しておけば、 poll と await の隙間に来た `wire_send`
/// も確実に拾える。
#[derive(Clone, Default)]
pub struct WireNotifier {
    /// agent address → Notify
    waiters: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

impl WireNotifier {
    /// 空の notifier を構築
    pub fn new() -> Self {
        Self::default()
    }

    /// 指定 agent の `Notify` ハンドルを取得 (無ければ作成)
    ///
    /// `wire_recv` handler は戻り値の `notified()` を **store poll の前に** 生成すること
    /// (struct doc の取りこぼし防止プロトコル参照)。
    pub async fn handle(&self, agent: &str) -> Arc<Notify> {
        let mut guard = self.waiters.lock().await;
        guard
            .entry(agent.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    /// 指定 agent の待機中 `wire_recv` を全て起こす
    ///
    /// `wire_send` が宛先 agent ごとに呼ぶ。 待機者がいなければ no-op。
    pub async fn notify(&self, agent: &str) {
        let notify = self.handle(agent).await;
        notify.notify_waiters();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// kv-mem で wiremsg schema を define 済の Surreal を返す
    async fn make_test_store() -> WiremsgStore {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("kv-mem connect");
        db.use_ns("vp").use_db("vp").await.expect("use_ns/db");
        // db/mod.rs SCHEMA_SQL の wiremsg 部分を再現 (= テスト独立性)
        db.query(
            r#"
            DEFINE TABLE wire_messages SCHEMAFULL;
            DEFINE FIELD id ON wire_messages TYPE string;
            DEFINE FIELD thread_id ON wire_messages TYPE string;
            DEFINE FIELD prev ON wire_messages TYPE option<string>;
            DEFINE FIELD from_addr ON wire_messages TYPE string;
            DEFINE FIELD to_addrs ON wire_messages TYPE array<string>;
            DEFINE FIELD body ON wire_messages TYPE object FLEXIBLE;
            DEFINE FIELD created_at ON wire_messages TYPE number;
            DEFINE INDEX wire_thread_idx ON wire_messages FIELDS thread_id, created_at;

            DEFINE TABLE thread_participant SCHEMAFULL;
            DEFINE FIELD thread ON thread_participant TYPE string;
            DEFINE FIELD agent ON thread_participant TYPE string;
            DEFINE FIELD read_cursor ON thread_participant TYPE option<number>;
            DEFINE FIELD status ON thread_participant TYPE string DEFAULT 'active';
            DEFINE FIELD updated_at ON thread_participant TYPE number;
            DEFINE INDEX thread_participant_uniq ON thread_participant FIELDS thread, agent UNIQUE;
            DEFINE INDEX thread_participant_agent_idx ON thread_participant FIELDS agent, status;
            "#,
        )
        .await
        .expect("schema query")
        .check()
        .expect("schema check");

        WiremsgStore::new(Arc::new(db))
    }

    /// body helper
    fn body(text: &str) -> serde_json::Value {
        serde_json::json!({ "text": text })
    }

    /// WireMessage::new_root は thread_id = 自分自身、 prev = None
    #[test]
    fn wire_message_root_self_reference() {
        let msg = WireMessage::new_root("a@vp", vec!["b@vp".into()], body("hi"));
        assert_eq!(msg.thread_id, msg.id, "root の thread_id は自 id");
        assert!(msg.prev.is_none(), "root の prev は None");
    }

    /// WireMessage::new_reply は prev=返信先、 thread_id を継承
    #[test]
    fn wire_message_reply_inherits_thread() {
        let reply = WireMessage::new_reply(
            "a@vp",
            vec!["b@vp".into()],
            body("re"),
            "parent-id",
            "root-id",
        );
        assert_eq!(reply.prev.as_deref(), Some("parent-id"));
        assert_eq!(reply.thread_id, "root-id", "reply は thread_id を継承");
        assert_ne!(reply.id, reply.thread_id, "reply の id は thread_id と別物");
    }

    /// send_root: 受信者は read_cursor=None (= 起点 message が未読として届く)
    #[tokio::test]
    async fn send_root_recipient_sees_root_message() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("hello bob"))
            .await
            .expect("send_root");

        // 受信者 bob は起点 message を未読として受け取れる
        let unread = store.fetch_unread("bob@vp").await.expect("fetch bob");
        assert_eq!(unread.len(), 1, "起点 message が未読で 1 件届く");
        assert_eq!(unread[0].id, root.id);
        assert_eq!(unread[0].body, body("hello bob"));
    }

    /// send_root: 送信者は read_cursor=root.created_at (= 自分のメッセージは既読)
    #[tokio::test]
    async fn send_root_sender_does_not_see_own_message() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("hello"))
            .await
            .expect("send_root");

        // 送信者 alice には未読なし (起点を既読扱い)
        let unread = store.fetch_unread("alice@vp").await.expect("fetch alice");
        assert!(unread.is_empty(), "送信者は自分の root message を読まない");
    }

    /// fetch → advance_cursor → 再 fetch で同じ message は二度読まれない
    #[tokio::test]
    async fn cursor_advances_and_message_not_redelivered() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("once"))
            .await
            .expect("send_root");

        let unread = store.fetch_unread("bob@vp").await.expect("fetch 1");
        assert_eq!(unread.len(), 1);

        // cursor を取得した最新 message の created_at に前進
        let last = unread.last().unwrap();
        store
            .advance_cursor(&last.thread_id, "bob@vp", last.created_at)
            .await
            .expect("advance");

        // 再 fetch で空 (= 二度読みしない)
        let again = store.fetch_unread("bob@vp").await.expect("fetch 2");
        assert!(
            again.is_empty(),
            "cursor 前進後は同じ message を再配信しない"
        );
        // unused 変数の linter 回避
        assert_eq!(root.thread_id, last.thread_id);
    }

    /// recv() は未読取得 + cursor 前進をまとめて行う (= wire_recv の store 操作)
    #[tokio::test]
    async fn recv_fetches_then_advances_cursor() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("once"))
            .await
            .expect("send_root");

        // 1 回目の recv: 1 件取得
        let first = store.recv("bob@vp").await.expect("recv 1");
        assert_eq!(first.len(), 1, "未読 1 件");

        // 2 回目の recv: cursor 前進済なので空 (= 二度読みしない)
        let second = store.recv("bob@vp").await.expect("recv 2");
        assert!(second.is_empty(), "recv 後は cursor が前進し再配信されない");
    }

    /// recv() は複数 thread の cursor を thread ごとに正しく前進させる
    #[tokio::test]
    async fn recv_advances_each_thread_independently() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("t1"))
            .await
            .expect("t1");
        store
            .send_root("carol@vp", &["bob@vp".to_string()], body("t2"))
            .await
            .expect("t2");

        // recv で 2 thread 分まとめて取得
        let first = store.recv("bob@vp").await.expect("recv 1");
        assert_eq!(first.len(), 2, "2 thread 分の未読");

        // 再 recv で両 thread とも空 (= 各 thread の cursor が前進している)
        let second = store.recv("bob@vp").await.expect("recv 2");
        assert!(second.is_empty(), "両 thread の cursor が前進し再配信なし");

        // reply が来たら recv で拾える (= cursor が reply 以前で止まっている)
        let t1_msgs = store.recv("alice@vp").await;
        assert!(t1_msgs.is_ok());
    }

    /// reply: thread 参加者が reply を未読として受け取る
    #[tokio::test]
    async fn reply_delivered_to_thread_participants() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("q"))
            .await
            .expect("send_root");

        // bob が root を読んで cursor 前進
        let bob_unread = store.fetch_unread("bob@vp").await.expect("bob fetch 1");
        let last = bob_unread.last().unwrap();
        store
            .advance_cursor(&last.thread_id, "bob@vp", last.created_at)
            .await
            .expect("bob advance");

        // alice が reply
        let reply = store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("a"), &root.id)
            .await
            .expect("send_reply");
        assert_eq!(reply.thread_id, root.thread_id, "reply は同 thread");
        assert_eq!(reply.prev.as_deref(), Some(root.id.as_str()));

        // bob は reply を未読として受け取る
        let bob_unread2 = store.fetch_unread("bob@vp").await.expect("bob fetch 2");
        assert_eq!(bob_unread2.len(), 1, "reply 1 件が未読で届く");
        assert_eq!(bob_unread2[0].id, reply.id);
    }

    /// reply: 送信者 (alice) は自分の reply を読まない (cursor が root で止まっていても
    /// 自分の reply は既読 cursor より前 — ではなく、 alice の cursor は root.created_at の
    /// まま。 reply.created_at > root.created_at なので alice にも reply が見える)。
    ///
    /// → 仕様: reply の cursor は送信者分も前進させない (root のみ前進)。 そのため
    ///   alice には自分の reply が「未読」として見える。 これは仕様通り (= reply の
    ///   participant upsert は新規 agent のみ、 cursor は触らない)。
    ///   notify 対象から「送信者を除く」ことで二重通知は防ぐ。 recv では見える。
    #[tokio::test]
    async fn reply_sender_cursor_not_advanced() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("q"))
            .await
            .expect("send_root");
        let reply = store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("a"), &root.id)
            .await
            .expect("send_reply");

        // alice の cursor は root.created_at のまま → reply は未読として見える
        let alice_unread = store.fetch_unread("alice@vp").await.expect("alice fetch");
        assert_eq!(
            alice_unread.len(),
            1,
            "reply の cursor は前進しないので送信者にも reply が見える"
        );
        assert_eq!(alice_unread[0].id, reply.id);
    }

    /// reply: thread に新規参加した agent は read_cursor=None で全 message を受け取る
    #[tokio::test]
    async fn reply_new_participant_sees_full_thread() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("root"))
            .await
            .expect("send_root");

        // alice が carol を新規に巻き込んで reply
        let reply = store
            .send_reply(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("reply"),
                &root.id,
            )
            .await
            .expect("send_reply");

        // carol は read_cursor=None なので root + reply の 2 件を受け取る
        let carol_unread = store.fetch_unread("carol@vp").await.expect("carol fetch");
        assert_eq!(
            carol_unread.len(),
            2,
            "新規参加者は thread 全 message を受け取る"
        );
        assert_eq!(carol_unread[0].id, root.id, "1 件目は root");
        assert_eq!(carol_unread[1].id, reply.id, "2 件目は reply");
    }

    /// send_reply: 存在しない prev_id を指定したら Err
    #[tokio::test]
    async fn reply_to_missing_message_errors() {
        let store = make_test_store().await;
        let result = store
            .send_reply("a@vp", &["b@vp".to_string()], body("x"), "nonexistent-id")
            .await;
        assert!(result.is_err(), "存在しない prev は Err");
    }

    /// thread_participants: exclude_left で left 参加者を除外
    #[tokio::test]
    async fn thread_participants_excludes_left() {
        let store = make_test_store().await;
        let root = store
            .send_root(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("x"),
            )
            .await
            .expect("send_root");

        // carol を left に
        store
            .db()
            .query("UPDATE thread_participant SET status = 'left' WHERE agent = 'carol@vp';")
            .await
            .expect("set left")
            .check()
            .expect("set left check");

        let all = store
            .thread_participants(&root.thread_id, false)
            .await
            .expect("all");
        assert_eq!(all.len(), 3, "exclude_left=false なら全 3 名");

        let active = store
            .thread_participants(&root.thread_id, true)
            .await
            .expect("active");
        assert_eq!(active.len(), 2, "exclude_left=true なら carol を除く 2 名");
        assert!(!active.contains(&"carol@vp".to_string()));
    }

    /// left した agent は fetch_unread の対象から外れる
    #[tokio::test]
    async fn left_agent_does_not_fetch() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // bob を left に
        store
            .db()
            .query("UPDATE thread_participant SET status = 'left' WHERE agent = 'bob@vp';")
            .await
            .expect("set left")
            .check()
            .expect("set left check");

        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert!(
            unread.is_empty(),
            "left した agent は message を受け取らない"
        );
    }

    /// 複数 thread にまたがる未読が created_at 昇順で返る
    #[tokio::test]
    async fn fetch_unread_spans_multiple_threads_sorted() {
        let store = make_test_store().await;
        // bob 宛に 2 つの別 thread
        let t1 = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("t1"))
            .await
            .expect("t1");
        let t2 = store
            .send_root("carol@vp", &["bob@vp".to_string()], body("t2"))
            .await
            .expect("t2");
        assert_ne!(t1.thread_id, t2.thread_id, "別 thread");

        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert_eq!(unread.len(), 2, "2 thread 分の未読");
        // created_at 昇順
        assert!(
            unread[0].created_at <= unread[1].created_at,
            "created_at 昇順で整列"
        );
    }

    /// fetch_unread で参加 thread が無い agent は空 vec
    #[tokio::test]
    async fn fetch_unread_no_participation_is_empty() {
        let store = make_test_store().await;
        let unread = store.fetch_unread("stranger@vp").await.expect("fetch");
        assert!(unread.is_empty());
    }

    /// WireNotifier: notified() future を先に生成しておけば後続 notify を拾える
    /// (= 取りこぼし防止プロトコル: handle → notified() → poll → await の順)
    #[tokio::test]
    async fn wire_notifier_future_before_notify_is_caught() {
        let notifier = WireNotifier::new();
        // step 1-2: handle 取得 → 待機 future 先生成
        let handle = notifier.handle("bob@vp").await;
        let fut = handle.notified();
        tokio::pin!(fut);
        // future 生成後に notify (= poll と await の隙間に来た送信を模擬)
        notifier.notify("bob@vp").await;
        // future が完了する (取りこぼしなし)
        tokio::time::timeout(std::time::Duration::from_millis(500), fut)
            .await
            .expect("future 生成後の notify は捕捉される");
    }

    /// WireNotifier: 別 agent への notify では起きない
    #[tokio::test]
    async fn wire_notifier_isolated_per_agent() {
        let notifier = WireNotifier::new();
        let handle = notifier.handle("bob@vp").await;
        let fut = handle.notified();
        tokio::pin!(fut);
        // carol 宛 notify では bob は起きない
        notifier.notify("carol@vp").await;
        let result = tokio::time::timeout(std::time::Duration::from_millis(150), fut).await;
        assert!(result.is_err(), "別 agent への notify では起床しない");
    }

    /// advance_cursor は cursor を後退させない
    #[tokio::test]
    async fn advance_cursor_does_not_regress() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // 大きい値 (= 遠未来の epoch ms) に前進。 root.created_at より十分大きく取る。
        let far_future = root.created_at + 1_000_000;
        store
            .advance_cursor(&root.thread_id, "bob@vp", far_future)
            .await
            .expect("advance big");
        // 小さい値で再前進を試みる
        store
            .advance_cursor(&root.thread_id, "bob@vp", 1)
            .await
            .expect("advance small");

        // cursor が 1 に後退していないこと → root は依然既読 (未読 0)
        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert!(unread.is_empty(), "cursor は後退しない");
    }
}
