//! wiremsg threaded inbox store (設計 memory `mem_1CbDLrECNZiNEZqjySLfSB` 決定 III)
//!
//! agent 間メッセージング「wiremsg」の inbox 実体。 既存 [`WhitesnakeStore`] (= msgs table,
//! claim-based Mailbox) と **並存** する threading 対応 store。 撤去は後続 Phase。
//!
//! ## per-reader state = per-agent 単一 cursor (決定 III)
//!
//! per-reader state を irreducible な最小核に絞る (「derive できるものは store しない」):
//!
//! - **cursor は per-agent 単一** — `agent_cursor { agent, last_read }`、 1 agent 1 行
//!   (O(agents))。 旧 `thread_participant.read_cursor` (per thread×agent、 O(agents×threads))
//!   を廃止。
//! - **配送は `to` ベース** — `fetch_unread(agent)` = `wire_messages` で
//!   `agent ∈ to_addrs` AND `created_at > last_read`。 旧 participation ベース
//!   (`thread_participant` を引いて thread ごとに引く) を廃止。
//! - **`thread_participant` は sparse 例外表** — `status` ∈ {muted, left} の行のみ持つ。
//!   default (active) は行を持たない。 active 参加は message の `to_addrs` から創発。
//! - **derive されるもの** — per-thread unread 数 ([`unread_count_by_thread`](WiremsgStore::unread_count_by_thread))、
//!   active 参加者 ([`thread_participants`](WiremsgStore::thread_participants))。
//!
//! ## 設計判断
//!
//! - **TopicRouter を使わない**: inbox = SurrealDB の message store。 `wire_recv` がその
//!   store を直接 long-poll する (= 既存 `msg_recv` / `WhitesnakeStore.claim` と同型)。
//! - **record link を query で辿らない**: `thread_id` / `prev` は plain string (= message
//!   の local id) で保持。 既存 msgs table の `id` / `reply_to` も plain string で同型、
//!   record-link traversal は migration 部分適用で壊れやすい (creo-memories の教訓)。
//! - **`created_at` / `last_read` は epoch ms (number)**: 既存 msgs.ts と同じ表現に揃え、
//!   cursor 比較を素直な数値比較にする (datetime serialize の罠回避)。
//! - **id は uuidv7**: 時刻順 sortable id (= ULID 相当)。 `uuid` crate の `now_v7()`。
//!
//! ## table
//!
//! - `wire_messages`: thread に属する message 本体 (`thread_id` / `prev` / `from_addr` /
//!   `to_addrs` / `body` / `created_at`)
//! - `agent_cursor`: per-agent 単一既読 cursor (`agent` / `last_read`)
//! - `thread_participant`: mute/left の sparse 例外表 (`thread` / `agent` / `status`)
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

/// thread 参加の sparse 例外表 (`thread_participant.status`) が取りうる状態
///
/// 決定 III: `thread_participant` は mute/left の sparse 例外表に縮小された。
/// default (active) は **行を持たない** ため、 行が存在する = `Muted` か `Left`。
/// `Active` は「行が無い」状態の論理的対概念として残す (mute/leave 操作 tool を
/// 足す後続 Phase で enum → 文字列の変換が要れば拡張する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantStatus {
    /// 参加中 (例外表に行を持たない default 状態)
    Active,
    /// ミュート中 (recv 対象だが notify は鳴らさない想定、 操作 tool は後続 Phase)
    Muted,
    /// 離脱済 (notify / recv 対象外)
    Left,
}

// =============================================================================
// WiremsgStore — SurrealDB embedded impl
// =============================================================================

/// wiremsg threaded inbox の store (決定 III — per-agent 単一 cursor)
///
/// 既存 [`WhitesnakeStore`](super::WhitesnakeStore) と同じく `Surreal<Any>` を共有して
/// 持つ。 `wire_messages` / `agent_cursor` / `thread_participant` の 3 table を扱う。
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
    /// Operations (決定 III):
    /// 1. `wire_messages` に root message (`prev=None`、 `thread_id`=自分) を INSERT
    ///
    /// participant 行は作らない。 配送は `to_addrs` から創発し、 既読判定は
    /// per-agent `agent_cursor` が担う。 送信者は `from` であり `to` に居ないので、
    /// `to` フィルタ配送で自動的に自分の root を未読として見ない (= sender cursor 処理不要)。
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
        Ok(msg)
    }

    /// 既存 thread への reply を送信 (reply-all、 REQ-THREAD-005)
    ///
    /// Operations (決定 III):
    /// 1. `prev` = 返信先 message を `wire_messages` から取得し `thread_id` を継承
    /// 2. `to` を **その thread の現参加者集合** に展開する (reply-all):
    ///    - 参加者集合 = thread 内全 message の `from` ∪ `to_addrs`
    ///    - `left` の agent を除外
    ///    - caller 指定の `to` (新規参加者追加用) も union
    ///    - 送信者自身 (`from`) は `to` から除外 (= 自分の reply を未読で見ない)
    /// 3. 展開した `to` で reply message を INSERT
    ///
    /// participation 行を持たなくても、 `to` 展開により thread 参加者が受信を継続できる。
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

        // reply-all 展開: thread の現参加者 ∪ caller 指定 to、 left 除外、 from 除外
        let expanded_to = self
            .expand_reply_recipients(&prev_msg.thread_id, from, to)
            .await?;

        let msg = WireMessage::new_reply(from, expanded_to, body, prev_id, &prev_msg.thread_id);
        self.insert_message(&msg).await?;
        Ok(msg)
    }

    /// reply の `to` を thread の現参加者集合に展開する (reply-all)
    ///
    /// 参加者集合 = thread 内全 message の `from` ∪ `to_addrs`、 ∪ caller 指定 `extra`。
    /// `left` の agent と 送信者自身 (`from`) を除外する。 結果は安定順序 (BTreeSet)。
    async fn expand_reply_recipients(
        &self,
        thread_id: &str,
        from: &str,
        extra: &[String],
    ) -> Result<Vec<String>> {
        use std::collections::BTreeSet;

        // thread 内全 message を引き、 from / to_addrs を集める
        let msgs = self.messages_after(thread_id, None).await?;
        let mut set: BTreeSet<String> = BTreeSet::new();
        for m in &msgs {
            set.insert(m.from.clone());
            for t in &m.to {
                set.insert(t.clone());
            }
        }
        // caller 指定 (新規参加者追加用) を union
        for e in extra {
            set.insert(e.clone());
        }
        // left の agent を除外
        let left = self.left_agents(thread_id).await?;
        for l in &left {
            set.remove(l);
        }
        // 送信者自身は to に入れない (= 自分の reply を未読で見ない)
        set.remove(from);
        Ok(set.into_iter().collect())
    }

    // -------------------------------------------------------------------------
    // wire_recv 系
    // -------------------------------------------------------------------------

    /// 指定 agent の未読 message を 1 回分取得 (long-poll はしない、 caller がループ制御)
    ///
    /// `to` ベース配送 (決定 III): `wire_messages` で `agent ∈ to_addrs` AND
    /// `created_at > last_read` (`last_read = None` なら全件) の message を `created_at`
    /// 昇順で取得する。 agent が `left` した thread の message は除外する。
    ///
    /// 取得後、 caller は [`advance_cursor`](Self::advance_cursor) で agent の単一 cursor を
    /// **取得した最新 message の `created_at`** に前進させること (`now` ではなく — fetch 中
    /// 着信の取りこぼし race を避けるため)。 本メソッドは cursor を変更しない。
    pub async fn fetch_unread(&self, agent: &str) -> Result<Vec<WireMessage>> {
        // 1. agent の per-agent cursor (last_read) を取得
        let last_read = self.get_cursor(agent).await?;
        // 2. agent ∈ to_addrs かつ cursor 超過の message を取得
        let mut out = self.messages_to_after(agent, last_read).await?;
        // 3. agent が left した thread の message を除外
        let left_threads = self.left_threads(agent).await?;
        if !left_threads.is_empty() {
            out.retain(|m| !left_threads.contains(&m.thread_id));
        }
        // 4. created_at 昇順に整列
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// `wire_recv` 1 回分の store 操作: 未読取得 + per-agent cursor 前進をまとめて行う
    ///
    /// [`fetch_unread`](Self::fetch_unread) で未読を取得し、 単一 cursor を
    /// 「取得した最新 message の `created_at`」 まで前進させる。
    /// 未読が空なら cursor は触らない。
    ///
    /// cursor を `now` ではなく **取得済 message の `created_at`** に合わせるのが要点
    /// (= fetch と advance の隙間に着信した message を取りこぼさないため)。
    pub async fn recv(&self, agent: &str) -> Result<Vec<WireMessage>> {
        let unread = self.fetch_unread(agent).await?;
        if unread.is_empty() {
            return Ok(unread);
        }
        // 取得済 message の created_at 最大値まで単一 cursor を前進
        let max = unread.iter().map(|m| m.created_at).max().unwrap_or(0);
        self.advance_cursor(agent, max).await?;
        Ok(unread)
    }

    /// 指定 agent の per-agent cursor (`last_read`) を前進させる
    ///
    /// `cursor` は [`fetch_unread`](Self::fetch_unread) で取得した最新 message の
    /// `created_at` を渡す。 既存 cursor より小さい値は無視 (= 後退させない)。
    /// 行が無ければ作成する。
    pub async fn advance_cursor(&self, agent: &str, cursor: u64) -> Result<()> {
        let now = now_ms();
        // 既存行があれば前進 (last_read IS NONE または last_read < cursor のときだけ)、
        // 無ければ作成する。 SurrealDB の複合 key UPSERT 制約のため SELECT → UPDATE/CREATE。
        let existing = self.find_cursor_record_id(agent).await?;
        match existing {
            Some(rid) => {
                self.db
                    .query(
                        "UPDATE type::record('agent_cursor', $rid)
                             SET last_read = $cursor, updated_at = $now
                             WHERE last_read IS NONE OR last_read < $cursor;",
                    )
                    .bind(("rid", rid))
                    .bind(("cursor", cursor))
                    .bind(("now", now))
                    .await
                    .map_err(|e| anyhow::anyhow!("wiremsg advance_cursor update failed: {e}"))?
                    .check()
                    .map_err(|e| {
                        anyhow::anyhow!("wiremsg advance_cursor update check failed: {e}")
                    })?;
            }
            None => {
                self.db
                    .query(
                        "CREATE agent_cursor CONTENT {
                             agent: $agent, last_read: $cursor, updated_at: $now
                         };",
                    )
                    .bind(("agent", agent.to_string()))
                    .bind(("cursor", cursor))
                    .bind(("now", now))
                    .await
                    .map_err(|e| anyhow::anyhow!("wiremsg advance_cursor create failed: {e}"))?
                    .check()
                    .map_err(|e| {
                        anyhow::anyhow!("wiremsg advance_cursor create check failed: {e}")
                    })?;
            }
        }
        Ok(())
    }

    /// 指定 agent の per-thread 未読数を derive して返す (「derive できるものは store しない」)
    ///
    /// [`fetch_unread`](Self::fetch_unread) と同じ未読集合を `thread_id` で GROUP BY
    /// した count。 未読 0 の thread は HashMap に現れない。
    pub async fn unread_count_by_thread(&self, agent: &str) -> Result<HashMap<String, u64>> {
        let unread = self.fetch_unread(agent).await?;
        let mut counts: HashMap<String, u64> = HashMap::new();
        for m in &unread {
            *counts.entry(m.thread_id.clone()).or_insert(0) += 1;
        }
        Ok(counts)
    }

    /// 指定 thread の参加者 address 群を derive して返す (`wire_send` reply の notify 対象決定用)
    ///
    /// 参加者集合は thread 内全 message の `from` ∪ `to_addrs` から創発する (決定 III、
    /// active 参加は table を持たない)。 `exclude_left = true` なら `thread_participant`
    /// の sparse 例外表で `status = left` の agent を除外する。 結果は安定順序。
    pub async fn thread_participants(
        &self,
        thread_id: &str,
        exclude_left: bool,
    ) -> Result<Vec<String>> {
        use std::collections::BTreeSet;

        // thread 内全 message から from / to_addrs を集める
        let msgs = self.messages_after(thread_id, None).await?;
        let mut set: BTreeSet<String> = BTreeSet::new();
        for m in &msgs {
            if !m.from.is_empty() {
                set.insert(m.from.clone());
            }
            for t in &m.to {
                if !t.is_empty() {
                    set.insert(t.clone());
                }
            }
        }
        // sparse 例外表で left を除外
        if exclude_left {
            for l in self.left_agents(thread_id).await? {
                set.remove(&l);
            }
        }
        Ok(set.into_iter().collect())
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

    /// `agent ∈ to_addrs` かつ `created_at > cursor` の message を昇順で取得
    /// (`cursor = None` なら全件)
    ///
    /// `to` ベース配送の中核 query (決定 III)。 `to_addrs CONTAINS $agent` で
    /// agent 宛 message を引く。
    async fn messages_to_after(
        &self,
        agent: &str,
        cursor: Option<u64>,
    ) -> Result<Vec<WireMessage>> {
        // cursor IS NONE と cursor 指定で query を分岐 (= bind の None を WHERE で扱うと
        // SurrealDB の比較が意図せぬ挙動になりうるため、 明示的に 2 query に分ける)。
        let mut res = match cursor {
            Some(c) => {
                self.db
                    .query(
                        "SELECT * FROM wire_messages
                             WHERE to_addrs CONTAINS $agent AND created_at > $cursor
                             ORDER BY created_at ASC, id ASC;",
                    )
                    .bind(("agent", agent.to_string()))
                    .bind(("cursor", c))
                    .await
            }
            None => {
                self.db
                    .query(
                        "SELECT * FROM wire_messages
                             WHERE to_addrs CONTAINS $agent
                             ORDER BY created_at ASC, id ASC;",
                    )
                    .bind(("agent", agent.to_string()))
                    .await
            }
        }
        .map_err(|e| anyhow::anyhow!("wiremsg messages_to_after failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg messages_to_after take failed: {e}"))?;
        rows.iter().map(Self::row_to_message).collect()
    }

    /// 指定 agent の per-agent cursor (`last_read`) を返す (行が無ければ `None` = 未読)
    async fn get_cursor(&self, agent: &str) -> Result<Option<u64>> {
        let mut res = self
            .db
            .query("SELECT last_read FROM agent_cursor WHERE agent = $agent LIMIT 1;")
            .bind(("agent", agent.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg get_cursor failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg get_cursor take failed: {e}"))?;
        Ok(rows.first().and_then(|row| row["last_read"].as_u64()))
    }

    /// 指定 agent の `agent_cursor` の record id (local 部分) を返す
    async fn find_cursor_record_id(&self, agent: &str) -> Result<Option<String>> {
        let mut res = self
            .db
            .query("SELECT id FROM agent_cursor WHERE agent = $agent LIMIT 1;")
            .bind(("agent", agent.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg find_cursor failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg find_cursor take failed: {e}"))?;
        match rows.first() {
            Some(row) => Ok(Some(Self::extract_record_local_id(
                &row["id"],
                "agent_cursor",
            ))),
            None => Ok(None),
        }
    }

    /// 指定 agent が `left` した thread の id 集合を返す (sparse 例外表 query)
    async fn left_threads(&self, agent: &str) -> Result<std::collections::HashSet<String>> {
        let mut res = self
            .db
            .query(
                "SELECT thread FROM thread_participant
                     WHERE agent = $agent AND status = 'left';",
            )
            .bind(("agent", agent.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg left_threads failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg left_threads take failed: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|row| row["thread"].as_str().map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// 指定 thread で `left` した agent の address 集合を返す (sparse 例外表 query)
    async fn left_agents(&self, thread_id: &str) -> Result<std::collections::HashSet<String>> {
        let mut res = self
            .db
            .query(
                "SELECT agent FROM thread_participant
                     WHERE thread = $thread AND status = 'left';",
            )
            .bind(("thread", thread_id.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("wiremsg left_agents failed: {e}"))?;
        let rows: Vec<serde_json::Value> = res
            .take(0)
            .map_err(|e| anyhow::anyhow!("wiremsg left_agents take failed: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|row| row["agent"].as_str().map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
            .collect())
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

            DEFINE TABLE agent_cursor SCHEMAFULL;
            DEFINE FIELD agent ON agent_cursor TYPE string;
            DEFINE FIELD last_read ON agent_cursor TYPE option<number>;
            DEFINE FIELD updated_at ON agent_cursor TYPE number;
            DEFINE INDEX agent_cursor_uniq ON agent_cursor FIELDS agent UNIQUE;

            DEFINE TABLE thread_participant SCHEMAFULL;
            DEFINE FIELD thread ON thread_participant TYPE string;
            DEFINE FIELD agent ON thread_participant TYPE string;
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

    // -------------------------------------------------------------------------
    // send_root / fetch / cursor — to ベース配送 + per-agent cursor (決定 III)
    // -------------------------------------------------------------------------

    /// send_root: 受信者は起点 message を未読として受け取る (to ベース配送)
    #[tokio::test]
    async fn send_root_recipient_sees_root_message() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("hello bob"))
            .await
            .expect("send_root");

        // 受信者 bob は起点 message を未読として受け取れる (cursor 行なし = 全件未読)
        let unread = store.fetch_unread("bob@vp").await.expect("fetch bob");
        assert_eq!(unread.len(), 1, "起点 message が未読で 1 件届く");
        assert_eq!(unread[0].id, root.id);
        assert_eq!(unread[0].body, body("hello bob"));
    }

    /// send_root: 送信者は to に居ないため自分の root を未読として見ない
    #[tokio::test]
    async fn send_root_sender_does_not_see_own_message() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("hello"))
            .await
            .expect("send_root");

        // 送信者 alice は from であり to に居ない → to フィルタで未読 0
        let unread = store.fetch_unread("alice@vp").await.expect("fetch alice");
        assert!(unread.is_empty(), "送信者は自分の root message を読まない");
    }

    /// fetch → advance_cursor → 再 fetch で同じ message は二度読まれない
    #[tokio::test]
    async fn cursor_advances_and_message_not_redelivered() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("once"))
            .await
            .expect("send_root");

        let unread = store.fetch_unread("bob@vp").await.expect("fetch 1");
        assert_eq!(unread.len(), 1);

        // per-agent cursor を取得した最新 message の created_at に前進
        let last = unread.last().unwrap();
        store
            .advance_cursor("bob@vp", last.created_at)
            .await
            .expect("advance");

        // 再 fetch で空 (= 二度読みしない)
        let again = store.fetch_unread("bob@vp").await.expect("fetch 2");
        assert!(
            again.is_empty(),
            "cursor 前進後は同じ message を再配信しない"
        );
    }

    /// recv() は未読取得 + per-agent cursor 前進をまとめて行う
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

    /// recv() は単一 cursor で複数 thread の未読をまとめて drain する
    #[tokio::test]
    async fn recv_drains_multiple_threads_with_single_cursor() {
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

        // 再 recv で空 (= 単一 cursor が両 thread の最新まで前進している)
        let second = store.recv("bob@vp").await.expect("recv 2");
        assert!(second.is_empty(), "単一 cursor 前進で再配信なし");
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
        let _ = store.recv("bob@vp").await.expect("bob recv root");

        // alice が reply
        let reply = store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("a"), &root.id)
            .await
            .expect("send_reply");
        assert_eq!(reply.thread_id, root.thread_id, "reply は同 thread");
        assert_eq!(reply.prev.as_deref(), Some(root.id.as_str()));

        // bob は reply を未読として受け取る
        let bob_unread = store.fetch_unread("bob@vp").await.expect("bob fetch 2");
        assert_eq!(bob_unread.len(), 1, "reply 1 件が未読で届く");
        assert_eq!(bob_unread[0].id, reply.id);
    }

    /// reply: 送信者は to に展開されないため、自分の reply を未読として見ない
    ///
    /// 決定 III の意図的変更 — 旧モデルでは sender cursor が前進せず自分の reply が
    /// 見えていた (quirk)。 to ベース配送では sender は to に入らないので見えない。
    /// これは quirk の除去で **より正しい**。
    #[tokio::test]
    async fn reply_sender_does_not_see_own_reply() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("q"))
            .await
            .expect("send_root");
        store
            .send_reply("alice@vp", &["bob@vp".to_string()], body("a"), &root.id)
            .await
            .expect("send_reply");

        // alice は from であり、reply-all 展開でも from は to から除外される
        let alice_unread = store.fetch_unread("alice@vp").await.expect("alice fetch");
        assert!(
            alice_unread.is_empty(),
            "送信者は自分の reply を未読として見ない (to に居ないため)"
        );
    }

    /// reply-all: reply の to は thread の現参加者集合に展開される (REQ-THREAD-005)
    #[tokio::test]
    async fn reply_expands_to_thread_participants() {
        let store = make_test_store().await;
        // alice が bob・carol 宛に root
        let root = store
            .send_root(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("root"),
            )
            .await
            .expect("send_root");

        // bob が reply。 to に carol を明示しなくても reply-all で carol に届く
        let reply = store
            .send_reply("bob@vp", &[], body("reply"), &root.id)
            .await
            .expect("send_reply");
        // 展開後の to: thread 参加者 (alice, bob, carol) から from=bob を除く
        assert!(reply.to.contains(&"alice@vp".to_string()), "alice に展開");
        assert!(reply.to.contains(&"carol@vp".to_string()), "carol に展開");
        assert!(
            !reply.to.contains(&"bob@vp".to_string()),
            "from は to に含めない"
        );

        // carol は participation 行を持たなくても reply を受信できる
        let carol_unread = store.fetch_unread("carol@vp").await.expect("carol fetch");
        assert!(
            carol_unread.iter().any(|m| m.id == reply.id),
            "reply-all 展開で carol が reply を受信"
        );
    }

    /// reply: thread に途中参加した agent は参加時点以降の message のみ受信する
    ///
    /// 決定 III の意図的変更 — 旧モデルでは新規参加者が read_cursor=None で thread
    /// 全体 (root 含む) を受け取っていた。 to ベース配送では reply の to に入った
    /// 時点以降の message のみ受信する。 参加前 backlog は wire_thread query の責務。
    #[tokio::test]
    async fn reply_new_participant_sees_messages_from_join() {
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

        // carol は reply の to に入った → reply のみ受信。 root (参加前) は受信しない
        let carol_unread = store.fetch_unread("carol@vp").await.expect("carol fetch");
        assert_eq!(
            carol_unread.len(),
            1,
            "途中参加者は参加時点以降の message のみ受信"
        );
        assert_eq!(carol_unread[0].id, reply.id, "受信するのは reply");
        assert!(
            !carol_unread.iter().any(|m| m.id == root.id),
            "参加前の root は受信しない"
        );
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

    // -------------------------------------------------------------------------
    // thread_participant — sparse 例外表 (left のみ行を持つ)
    // -------------------------------------------------------------------------

    /// thread_participants: 参加者集合は message の from / to_addrs から創発する
    #[tokio::test]
    async fn thread_participants_derived_from_messages() {
        let store = make_test_store().await;
        let root = store
            .send_root(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("x"),
            )
            .await
            .expect("send_root");

        // 例外表に行が一切無くても、参加者は message から導出される
        let all = store
            .thread_participants(&root.thread_id, false)
            .await
            .expect("all");
        assert_eq!(all.len(), 3, "from + to_addrs = alice/bob/carol の 3 名");
        assert!(all.contains(&"alice@vp".to_string()));
        assert!(all.contains(&"bob@vp".to_string()));
        assert!(all.contains(&"carol@vp".to_string()));
    }

    /// thread_participants: exclude_left で sparse 例外表の left 行を除外
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

        // carol の left 行を sparse 例外表に CREATE する
        store
            .db()
            .query(
                "CREATE thread_participant CONTENT {
                     thread: $thread, agent: 'carol@vp', status: 'left', updated_at: 0
                 };",
            )
            .bind(("thread", root.thread_id.clone()))
            .await
            .expect("create left")
            .check()
            .expect("create left check");

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

    /// left した agent は fetch_unread の対象から外れる (sparse 例外表モデル)
    #[tokio::test]
    async fn left_agent_does_not_fetch() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // bob の left 行を sparse 例外表に CREATE する
        store
            .db()
            .query(
                "CREATE thread_participant CONTENT {
                     thread: $thread, agent: 'bob@vp', status: 'left', updated_at: 0
                 };",
            )
            .bind(("thread", root.thread_id.clone()))
            .await
            .expect("create left")
            .check()
            .expect("create left check");

        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert!(
            unread.is_empty(),
            "left した agent は message を受け取らない"
        );
    }

    /// reply-all 展開は left した agent を除外する
    #[tokio::test]
    async fn reply_expansion_excludes_left() {
        let store = make_test_store().await;
        let root = store
            .send_root(
                "alice@vp",
                &["bob@vp".to_string(), "carol@vp".to_string()],
                body("root"),
            )
            .await
            .expect("send_root");

        // carol が left
        store
            .db()
            .query(
                "CREATE thread_participant CONTENT {
                     thread: $thread, agent: 'carol@vp', status: 'left', updated_at: 0
                 };",
            )
            .bind(("thread", root.thread_id.clone()))
            .await
            .expect("create left")
            .check()
            .expect("create left check");

        // bob が reply — reply-all 展開でも carol (left) は to に入らない
        let reply = store
            .send_reply("bob@vp", &[], body("reply"), &root.id)
            .await
            .expect("send_reply");
        assert!(
            !reply.to.contains(&"carol@vp".to_string()),
            "left した agent は reply-all 展開から除外される"
        );
        assert!(reply.to.contains(&"alice@vp".to_string()), "alice は残る");
    }

    // -------------------------------------------------------------------------
    // fetch_unread — 横断 / 空 / unread_count derive
    // -------------------------------------------------------------------------

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
        assert!(
            unread[0].created_at <= unread[1].created_at,
            "created_at 昇順で整列"
        );
    }

    /// fetch_unread で宛先になったことが無い agent は空 vec
    #[tokio::test]
    async fn fetch_unread_no_messages_is_empty() {
        let store = make_test_store().await;
        let unread = store.fetch_unread("stranger@vp").await.expect("fetch");
        assert!(unread.is_empty());
    }

    /// unread_count_by_thread: 未読を thread_id で GROUP BY した count を derive
    #[tokio::test]
    async fn unread_count_by_thread_derives_per_thread() {
        let store = make_test_store().await;
        // thread t1: root + reply の 2 件が bob 宛
        let t1 = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("t1-root"))
            .await
            .expect("t1");
        store
            .send_reply(
                "alice@vp",
                &["bob@vp".to_string()],
                body("t1-reply"),
                &t1.id,
            )
            .await
            .expect("t1 reply");
        // thread t2: root の 1 件が bob 宛
        let t2 = store
            .send_root("carol@vp", &["bob@vp".to_string()], body("t2-root"))
            .await
            .expect("t2");

        let counts = store
            .unread_count_by_thread("bob@vp")
            .await
            .expect("unread count");
        assert_eq!(counts.get(&t1.thread_id).copied(), Some(2), "t1 は 2 件");
        assert_eq!(counts.get(&t2.thread_id).copied(), Some(1), "t2 は 1 件");

        // recv で drain した後は未読 0 → HashMap は空
        let _ = store.recv("bob@vp").await.expect("drain");
        let after = store
            .unread_count_by_thread("bob@vp")
            .await
            .expect("unread count after");
        assert!(after.is_empty(), "drain 後は未読 thread なし");
    }

    // -------------------------------------------------------------------------
    // advance_cursor — 単一 cursor の前進規律
    // -------------------------------------------------------------------------

    /// advance_cursor は per-agent 単一 cursor を後退させない
    #[tokio::test]
    async fn advance_cursor_does_not_regress() {
        let store = make_test_store().await;
        let root = store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // 大きい値 (= 遠未来の epoch ms) に前進
        let far_future = root.created_at + 1_000_000;
        store
            .advance_cursor("bob@vp", far_future)
            .await
            .expect("advance big");
        // 小さい値で再前進を試みる
        store
            .advance_cursor("bob@vp", 1)
            .await
            .expect("advance small");

        // cursor が 1 に後退していないこと → root は依然既読 (未読 0)
        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert!(unread.is_empty(), "cursor は後退しない");
    }

    /// advance_cursor は agent_cursor 行が無ければ作成する
    #[tokio::test]
    async fn advance_cursor_creates_row_if_absent() {
        let store = make_test_store().await;
        store
            .send_root("alice@vp", &["bob@vp".to_string()], body("x"))
            .await
            .expect("send_root");

        // cursor 行が無い状態でいきなり advance
        store
            .advance_cursor("bob@vp", now_ms() + 1_000_000)
            .await
            .expect("advance creates row");

        // 行が作られ cursor が効いている → 未読 0
        let unread = store.fetch_unread("bob@vp").await.expect("fetch");
        assert!(unread.is_empty(), "cursor 行が作成され未読が drain される");
    }

    // -------------------------------------------------------------------------
    // WireNotifier — 取りこぼし防止
    // -------------------------------------------------------------------------

    /// WireNotifier: notified() future を先に生成しておけば後続 notify を拾える
    #[tokio::test]
    async fn wire_notifier_future_before_notify_is_caught() {
        let notifier = WireNotifier::new();
        let handle = notifier.handle("bob@vp").await;
        let fut = handle.notified();
        tokio::pin!(fut);
        notifier.notify("bob@vp").await;
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
        notifier.notify("carol@vp").await;
        let result = tokio::time::timeout(std::time::Duration::from_millis(150), fut).await;
        assert!(result.is_err(), "別 agent への notify では起床しない");
    }
}
