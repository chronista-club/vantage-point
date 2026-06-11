# wiremsg R2-a: store 中央化 + 取得 primitives 完備 — 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** wire store を TheWorld (port 32000) に中央化して SP を proxy 化し、取得 primitives (recv / inbox / thread / ack) を MCP + CLI 両系統で完備する。

**Architecture:** TheWorld の AppState は既に `db/world/` 上の `WiremsgStore` を構築済み (server.rs:853-860)。これを正式な中央 store に昇格し、run_world の Router に `/api/wire/*` を新設する。SP の `handle_wire_*` (QUIC + HTTP の共通層) は「アドレス正規化 → TheWorld へ HTTP relay」の薄い proxy に書き換える。これで MCP / CLI / flow 系の全クライアントが無改修で中央 store に追従する。cross-process forward (`wire_remote.rs`) と legacy bare alias は概念ごと撤去。

**Tech Stack:** Rust (Tokio, Axum, reqwest), SurrealDB embedded (surrealkv), rmcp (MCP), Clap (CLI)

**設計 SSOT:** creo memory `mem_1CbvcJj4ppU3QKH9d7xMpT` (R2 設計確定)。決定 D1-a/b/c, D3, 改訂実装順序 (取得 primitives 最優先) に基づく。

**設計メモからの実装上の解釈 2 点 (deviation ではなく具体化):**
1. wire store の置き場は `db/world/wire` 新設ではなく **既存の TheWorld VpDb (`db/world/`) を共用**する。SCHEMA_SQL は共通で wire tables が既に定義済み・TheWorld は単一プロセスなので分離の根拠 (VP-182 の OS 排他ロック) が無く、Surreal インスタンス +1 を避ける。
2. SP→TheWorld の中継は **HTTP** (`http://127.0.0.1:32000/api/wire/*`)。QUIC registry チャネルへの相乗りは R2-b 以降の最適化余地として残す (設計図の QUIC 矢印はトポロジ表記と解釈)。

**前提 (実施済み):** D1-b の旧 data 退避 — thread `019eafe6` の nexus 未読 2 通は drain 済み、内容は creo memory に記録済み。

---

## 進め方の規約

- ブランチ: `git fetch origin nightly && git checkout -b mako/wiremsg-r2a-central-store origin/nightly`
- 各 Task 末尾で `cargo test -p vantage-point` (または該当 crate) green を確認してから commit
- GitNexus 規約: シンボル編集前に `impact({target, direction: "upstream"})`、commit 前に `detect_changes()`
- コメントは日本語

---

### Task 0: lane 開始 + impact 分析

**Files:** なし (git + GitNexus 操作のみ)

- [ ] **Step 0-1: lane 開始**

```bash
git fetch origin nightly && git checkout -b mako/wiremsg-r2a-central-store origin/nightly
```

- [ ] **Step 0-2: 主要シンボルの impact 分析 (GitNexus MCP)**

対象: `handle_wire_send`, `handle_wire_recv`, `WiremsgStore`, `forward_to_remote`, `NotificationActor`, `LaneSpawnActor`。`direction: "upstream"` で blast radius を確認し、HIGH/CRITICAL があれば user に報告してから進む。

---

### Task 1: WiremsgStore に ack primitive (wire_acks 台帳)

決定 D3: ack は cursor 非破壊の台帳方式。R2-a では primitive (table + store メソッド) のみ。再掲示 loop は R2-b。

**Files:**
- Modify: `crates/vantage-point/src/db/mod.rs` (SCHEMA_SQL、wire_messages 定義の直後 ~717 行付近)
- Modify: `crates/vantage-point/src/capability/wiremsg_store.rs` (メソッド追加 + テスト)

- [ ] **Step 1-1: SCHEMA_SQL に wire_acks table を追加**

`db/mod.rs` の thread_participant 定義ブロックの後に:

```sql
-- wiremsg R2-a (設計 mem_1CbvcJj4ppU3QKH9d7xMpT 決定 D3): per-message ack 台帳。
-- command category の「読まれた」確認用。cursor (agent_cursor) とは独立で、
-- recv で cursor が進んでも wire_ack されるまで delivery loop (R2-b) の再掲示対象。
DEFINE TABLE IF NOT EXISTS wire_acks SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS message_id ON wire_acks TYPE string;
DEFINE FIELD IF NOT EXISTS agent ON wire_acks TYPE string;
DEFINE FIELD IF NOT EXISTS acked_at ON wire_acks TYPE number;
DEFINE INDEX IF NOT EXISTS wire_acks_uniq ON wire_acks FIELDS message_id, agent UNIQUE;
```

- [ ] **Step 1-2: 失敗するテストを書く (wiremsg_store.rs の `#[cfg(test)]` mod 末尾)**

```rust
#[tokio::test]
async fn ack_records_and_is_idempotent() {
    let store = test_store().await;
    let msg = store
        .send_root("agent@vp", &["agent@nexus".to_string()], json!({"kind": "command"}))
        .await
        .unwrap();

    // 初回 ack は true (新規)、同一 (message, agent) の再 ack は false (冪等)
    assert!(store.ack(&msg.id, "agent@nexus").await.unwrap());
    assert!(!store.ack(&msg.id, "agent@nexus").await.unwrap());

    // acks_for で ack 済 agent 一覧が引ける
    let acked = store.acks_for(&msg.id).await.unwrap();
    assert_eq!(acked, vec!["agent@nexus".to_string()]);
}

#[tokio::test]
async fn ack_unknown_message_errors() {
    let store = test_store().await;
    assert!(store.ack("no-such-id", "agent@nexus").await.is_err());
}
```

注: 既存テスト群が使う store 構築 helper (`mem://` 接続、wiremsg_store.rs:808 付近) の実名に合わせること。`test_store()` が存在しない場合は既存テストと同じ inline 構築を流用する。

- [ ] **Step 1-3: テスト失敗を確認**

Run: `cargo test -p vantage-point ack_ -- --nocapture`
Expected: FAIL (`ack` メソッド未定義のコンパイルエラー)

- [ ] **Step 1-4: WiremsgStore に ack / acks_for を実装**

`wiremsg_store.rs` の read-only 系メソッド (unread_count_by_thread 等) の近くに:

```rust
/// message を ack する (R2-a、決定 D3: cursor 非破壊の ack 台帳)
///
/// 戻り値: 新規 ack なら true、既に ack 済 (冪等) なら false。
/// 存在しない message_id は Err (typo を握り潰さない)。
pub async fn ack(&self, message_id: &str, agent: &str) -> Result<bool> {
    // message 実在確認 (誤 id の ack を黙って成功させない)
    let mut res = self
        .db
        .query("SELECT id FROM wire_messages WHERE id = $id LIMIT 1")
        .bind(("id", message_id.to_string()))
        .await?;
    let found: Vec<serde_json::Value> = res.take(0)?;
    if found.is_empty() {
        anyhow::bail!("wire_ack: message not found: {message_id}");
    }

    // 既 ack なら冪等 false
    let mut res = self
        .db
        .query("SELECT agent FROM wire_acks WHERE message_id = $id AND agent = $agent LIMIT 1")
        .bind(("id", message_id.to_string()))
        .bind(("agent", agent.to_string()))
        .await?;
    let existing: Vec<serde_json::Value> = res.take(0)?;
    if !existing.is_empty() {
        return Ok(false);
    }

    self.db
        .query("CREATE wire_acks SET message_id = $id, agent = $agent, acked_at = $now")
        .bind(("id", message_id.to_string()))
        .bind(("agent", agent.to_string()))
        .bind(("now", chrono::Utc::now().timestamp_millis()))
        .await?;
    Ok(true)
}

/// message を ack 済の agent 一覧を返す (read-only)
pub async fn acks_for(&self, message_id: &str) -> Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Row {
        agent: String,
    }
    let mut res = self
        .db
        .query("SELECT agent FROM wire_acks WHERE message_id = $id ORDER BY agent")
        .bind(("id", message_id.to_string()))
        .await?;
    let rows: Vec<Row> = res.take(0)?;
    Ok(rows.into_iter().map(|r| r.agent).collect())
}
```

注: 既存メソッドの query / bind スタイル (`.bind(("key", value))` の所有権形) に合わせて調整。

- [ ] **Step 1-5: テスト green を確認**

Run: `cargo test -p vantage-point ack_`
Expected: PASS (2 tests)

- [ ] **Step 1-6: Commit**

```bash
git add crates/vantage-point/src/db/mod.rs crates/vantage-point/src/capability/wiremsg_store.rs
git commit -m "feat(wire): wire_acks 台帳 + WiremsgStore::ack/acks_for (R2-a, 決定 D3)"
```

---

### Task 2: TheWorld 側 wire handlers (routes/wire.rs 新設 + run_world 登録)

store 直結のハンドラ群を新 module に置き、run_world の Router に `/api/wire/*` を登録する。ロジックは unison_server の現 handle_wire_* から **legacy alias と remote forward を除いて**移植。store/notifier を引数に取る純粋寄りの形にして単体テスト可能にする。

この Task の時点では SP 側は旧実装のまま並存する (壊さない)。

**Files:**
- Create: `crates/vantage-point/src/process/routes/wire.rs`
- Modify: `crates/vantage-point/src/process/routes/mod.rs` (`pub mod wire;` 追加)
- Modify: `crates/vantage-point/src/process/server.rs` (run_world Router、~982 行 `/api/world/refresh` の後に wire routes 追加)

- [ ] **Step 2-1: routes/wire.rs を作成 — store 直結ハンドラ + 検証**

```rust
//! TheWorld 中央 wire store のハンドラ群 (R2-a、設計 mem_1CbvcJj4ppU3QKH9d7xMpT)
//!
//! wiremsg R2-a で wire store は TheWorld (`db/world/`) に中央化された。
//! 本 module は store 直結のロジック層 (`*_store` 関数) と、run_world Router に
//! 登録する axum wrapper を提供する。SP 側 (`unison_server::handle_wire_*`) は
//! アドレス正規化のみ行い、ここへ HTTP relay する薄い proxy。
//!
//! ## アドレス規約 (N1: canonical = qualified 一本)
//!
//! 正規化は project 文脈を持つ SP の入口で行う。TheWorld は文脈を持たないため
//! 正規化せず、曖昧な bare `"agent"` を **reject** する (validate_addr)。

use crate::capability::{WireNotifier, WiremsgStore};

/// bare `"agent"` を reject する (TheWorld は project 文脈が無く正規化できない)
fn validate_addr(addr: &str) -> Result<(), String> {
    if addr == "agent" {
        return Err(
            "wire: bare \"agent\" は中央 store では曖昧です。SP (port 33000 台) 経由で送るか、\
             qualified 形 (agent@<project>) を指定してください"
                .to_string(),
        );
    }
    Ok(())
}

/// wire_send の body を JSON object に正規化する (unison_server から移設)
/// MCP client が typeless な body schema を string と解釈し JSON 文字列で
/// 送ってくることがあるため、string なら parse を試みる。
pub(crate) fn coerce_wire_body(
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    // (現 unison_server.rs:885-914 の実装をそのまま移設)
}

/// 送信 (root / reply)。payload: `{from, to: [..], body, reply_to?}`
pub(crate) async fn wire_send_store(
    store: &WiremsgStore,
    notifier: &WireNotifier,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_send: 'from' required".to_string())?
        .to_string();
    validate_addr(&from)?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    for addr in &to {
        validate_addr(addr)?;
    }
    let body = coerce_wire_body(payload.get("body").cloned())?;
    let reply_to = payload
        .get("reply_to")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // (以降は現 handle_wire_send の root / reply 分岐をそのまま移植。
    //  forward_remote_recipients 呼び出しは入れない — 中央化で forward は消滅)
}

/// 受信 long-poll。payload: `{agent, timeout?}` (timeout default 5s / max 30s)
pub(crate) async fn wire_recv_store(
    store: &WiremsgStore,
    notifier: &WireNotifier,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // 現 handle_wire_recv から legacy_bare_alias 系 (legacy 変数、legacy_notify、
    // dedup) を全て除いた形で移植。agent は validate_addr で検証。
}

/// 系譜取得 (read-only)。payload: `{message_id}`
pub(crate) async fn wire_thread_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // 現 handle_wire_thread をそのまま移植 (store 引数化のみ)
}

/// 未読 count (read-only)。payload: `{agent}`
pub(crate) async fn wire_unread_count_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // 現 handle_wire_unread_count から legacy alias 合算を除いて移植
}

/// 関与最新 message (read-only)。payload: `{agent}`
pub(crate) async fn wire_latest_msg_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // 現 handle_wire_latest_msg から legacy alias 比較を除いて移植
}

/// ack (R2-a 新設)。payload: `{message_id, agent}`
pub(crate) async fn wire_ack_store(
    store: &WiremsgStore,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    validate_addr(agent)?;
    let newly = store
        .ack(message_id, agent)
        .await
        .map_err(|e| format!("wire_ack failed: {e}"))?;
    Ok(serde_json::json!({ "status": "ok", "acked": newly }))
}
```

- [ ] **Step 2-2: axum wrapper を同ファイルに追加**

run_world の AppState から store/notifier を取り出す。store 未構築 (DB 接続失敗) は error JSON。6 endpoint 全て同型:

```rust
use axum::{Json, extract::State};
use std::sync::Arc;

use super::super::state::AppState;

/// store/notifier を取り出す共通前処理。World mode で DB 接続失敗時は None。
macro_rules! world_store {
    ($state:expr) => {
        match $state.wiremsg_store.as_ref() {
            Some(s) => s,
            None => {
                return Json(serde_json::json!({
                    "status": "error",
                    "error": "wire store not initialized (TheWorld DB 接続失敗)"
                }));
            }
        }
    };
}

/// POST /api/wire/send (TheWorld 中央 store 直結)
pub async fn world_wire_send_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let store = world_store!(state);
    match wire_send_store(store, &state.wire_notifier, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

// world_wire_recv_handler / world_wire_thread_handler / world_wire_unread_count_handler /
// world_wire_latest_msg_handler / world_wire_ack_handler も同型で定義
// (thread / unread-count / latest-msg / ack は notifier 不要なので store のみ渡す)
```

- [ ] **Step 2-3: routes/mod.rs に `pub mod wire;` を追加、run_world Router に登録**

server.rs run_world の `.route("/api/world/refresh", ...)` (982 行付近) の後に:

```rust
// wiremsg R2-a: 中央 wire store (設計 mem_1CbvcJj4ppU3QKH9d7xMpT 決定 D1-c)。
// TheWorld が唯一の writer。SP の /api/wire/* はここへの proxy。
.route("/api/wire/send", post(wire::world_wire_send_handler))
.route("/api/wire/recv", post(wire::world_wire_recv_handler))
.route("/api/wire/thread", post(wire::world_wire_thread_handler))
.route("/api/wire/unread-count", post(wire::world_wire_unread_count_handler))
.route("/api/wire/latest-msg", post(wire::world_wire_latest_msg_handler))
.route("/api/wire/ack", post(wire::world_wire_ack_handler))
```

use 行 (server.rs:20) の routes import に `wire` を追加。

- [ ] **Step 2-4: *_store 関数の単体テストを書く**

routes/wire.rs の `#[cfg(test)]` mod に。store 構築は wiremsg_store.rs のテストと同じ `mem://` パターン:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn mem_store() -> WiremsgStore {
        let db = surrealdb::engine::any::connect("mem://").await.unwrap();
        db.use_ns("vp").use_db("vp").await.unwrap();
        db.query(crate::db::SCHEMA_SQL).await.unwrap();
        WiremsgStore::new(std::sync::Arc::new(db)).await.unwrap()
    }
    // 注: wiremsg_store.rs 既存テストの構築手順 (808 行付近) と完全に同じ形にする。
    // SCHEMA_SQL が non-pub なら pub(crate) 化するか、既存テストの schema 適用手順を流用。

    #[tokio::test]
    async fn send_store_rejects_bare_agent() {
        let store = mem_store().await;
        let notifier = WireNotifier::new();
        let err = wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent", "to": ["agent@nexus"], "body": {"k": 1}}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("bare"));
    }

    #[tokio::test]
    async fn send_recv_roundtrip_via_store_handlers() {
        let store = mem_store().await;
        let notifier = WireNotifier::new();
        let sent = wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent@vp", "to": ["agent@nexus"], "body": {"kind": "event"}}),
        )
        .await
        .unwrap();
        assert_eq!(sent["status"], "ok");

        let recvd = wire_recv_store(
            &store,
            &notifier,
            json!({"agent": "agent@nexus", "timeout": 0}),
        )
        .await
        .unwrap();
        assert_eq!(recvd["count"], 1);
    }

    #[tokio::test]
    async fn ack_store_roundtrip() {
        let store = mem_store().await;
        let notifier = WireNotifier::new();
        let sent = wire_send_store(
            &store,
            &notifier,
            json!({"from": "agent@vp", "to": ["agent@nexus"], "body": {"kind": "command"}}),
        )
        .await
        .unwrap();
        let id = sent["id"].as_str().unwrap();

        let acked = wire_ack_store(&store, json!({"message_id": id, "agent": "agent@nexus"}))
            .await
            .unwrap();
        assert_eq!(acked["acked"], true);
        let again = wire_ack_store(&store, json!({"message_id": id, "agent": "agent@nexus"}))
            .await
            .unwrap();
        assert_eq!(again["acked"], false);
    }
}
```

- [ ] **Step 2-5: build + テスト green を確認**

Run: `cargo test -p vantage-point wire`
Expected: PASS (新規 3 + 既存 wire 系全部)

- [ ] **Step 2-6: Commit**

```bash
git add crates/vantage-point/src/process/routes/
git add crates/vantage-point/src/process/server.rs
git commit -m "feat(wire): TheWorld に中央 wire store handlers + /api/wire/* routes (R2-a)"
```

---

### Task 3: SP→TheWorld HTTP client (world_wire.rs)

**Files:**
- Create: `crates/vantage-point/src/process/world_wire.rs`
- Modify: `crates/vantage-point/src/process/mod.rs` (`pub mod world_wire;` 追加)

- [ ] **Step 3-1: world_wire.rs を作成**

```rust
//! SP → TheWorld の wire HTTP client (R2-a)
//!
//! wire store は TheWorld (port 32000、config override 可) に中央化された。
//! SP の wire ハンドラ / actor はこの client 経由で中央 store を読み書きする。
//! TheWorld 停止 = wire 停止 (設計 D1-c で許容済)。呼び出し側は Err を受けて
//! 各自の方針 (proxy はエラー返却、actor は retry) で扱う。

use std::sync::OnceLock;
use std::time::Duration;

/// long-poll (max 30s) を内包するため余裕を持った client timeout
const REQUEST_TIMEOUT: Duration = Duration::from_secs(40);

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client build failed")
    })
}

/// TheWorld の port を解決する (config override → default 32000)
pub(crate) fn world_port() -> u16 {
    crate::config::Config::load()
        .map(|c| c.port_layout().world_port)
        .unwrap_or(crate::cli::WORLD_PORT)
}

/// TheWorld の wire API を呼ぶ。`path` は `/api/wire/send` 等。
///
/// エラー規約: transport 失敗 → Err("TheWorld unreachable...")、
/// 応答 JSON に `error` field → その内容を Err として relay。
pub(crate) async fn call(
    path: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let url = format!("http://127.0.0.1:{}{}", world_port(), path);
    let resp = http_client()
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("TheWorld unreachable ({url}): {e} — wire store は TheWorld に中央化されています。`vp daemon start` を確認してください"))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("TheWorld wire 応答の JSON parse 失敗: {e}"))?;
    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    Ok(body)
}
```

注: `Config::load()` の戻り型 (Result) に合わせて `.map/.unwrap_or` を調整。`port_layout()` は config.rs:374。

- [ ] **Step 3-2: build 確認 + Commit**

Run: `cargo build -p vantage-point`
Expected: warning なしで成功 (未使用警告が出る場合は `#[allow(dead_code)]` を付けず、Task 4 と同 commit にせずこのまま commit して良い — 次 task で即解消)

```bash
git add crates/vantage-point/src/process/world_wire.rs crates/vantage-point/src/process/mod.rs
git commit -m "feat(wire): SP→TheWorld wire HTTP client world_wire (R2-a)"
```

---

### Task 4: SP handlers の proxy 化 + legacy bare alias 撤去 + ack 経路追加

`handle_wire_*` を「正規化 → world_wire::call relay」に書き換える。QUIC dispatch (unison_server.rs:555-561) と HTTP wrapper (health.rs) は signature 不変なので無改修で追従。ack の QUIC dispatch / SP HTTP route を追加。

**Files:**
- Modify: `crates/vantage-point/src/process/unison_server.rs` (handle_wire_* 書換、legacy_bare_alias 削除、coerce_wire_body 移設に伴う調整、QUIC dispatch に wire_ack 追加、テスト整理)
- Modify: `crates/vantage-point/src/process/routes/health.rs` (wire_thread_handler / wire_ack_handler 追加)
- Modify: `crates/vantage-point/src/process/server.rs` (SP Router に /api/wire/thread, /api/wire/ack 追加)

- [ ] **Step 4-1: handle_wire_send を proxy 化**

```rust
/// wiremsg を送信する (R2-a: TheWorld 中央 store への proxy)
///
/// payload: `{ from, to: [..], body, reply_to? }`
/// SP の責務はアドレス正規化 (N1: bare "agent" → "agent@<self_project>") のみ。
/// 保存・notify・採番は全て TheWorld 側 (routes/wire.rs)。
pub(crate) async fn handle_wire_send(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let from = payload
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_send: 'from' required".to_string())?;
    let to: Vec<String> = payload
        .get("to")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| normalize_agent_addr(s, &state.project_name))
                .collect()
        })
        .unwrap_or_default();
    let mut forwarded = serde_json::json!({
        "from": from,
        "to": to,
        "body": payload.get("body").cloned().unwrap_or(serde_json::Value::Null),
    });
    if let Some(reply_to) = payload.get("reply_to") {
        forwarded["reply_to"] = reply_to.clone();
    }
    super::world_wire::call("/api/wire/send", forwarded).await
}
```

- [ ] **Step 4-2: handle_wire_recv / thread / unread_count / latest_msg を proxy 化**

全て同型。recv は timeout を素通しする (clamp は TheWorld 側):

```rust
pub(crate) async fn handle_wire_recv(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_recv: 'agent' required".to_string())?;
    let timeout = payload.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);
    super::world_wire::call(
        "/api/wire/recv",
        serde_json::json!({ "agent": agent, "timeout": timeout }),
    )
    .await
}

pub(crate) async fn handle_wire_thread(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _ = state; // thread は agent 文脈不要 (message_id のみ)
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_thread: 'message_id' required".to_string())?;
    super::world_wire::call(
        "/api/wire/thread",
        serde_json::json!({ "message_id": message_id }),
    )
    .await
}

// handle_wire_unread_count / handle_wire_latest_msg: agent を normalize して
// /api/wire/unread-count, /api/wire/latest-msg へ relay (recv と同型、timeout なし)
```

- [ ] **Step 4-3: handle_wire_ack を新設 + QUIC dispatch に登録**

```rust
/// wiremsg を ack する (R2-a 新設、決定 D3)。payload: `{ message_id, agent }`
pub(crate) async fn handle_wire_ack(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let message_id = payload
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "wire_ack: 'message_id' required".to_string())?;
    let agent = payload
        .get("agent")
        .and_then(|v| v.as_str())
        .map(|s| normalize_agent_addr(s, &state.project_name))
        .ok_or_else(|| "wire_ack: 'agent' required".to_string())?;
    super::world_wire::call(
        "/api/wire/ack",
        serde_json::json!({ "message_id": message_id, "agent": agent }),
    )
    .await
}
```

QUIC dispatch (unison_server.rs:561 `"wire_latest_msg"` の行の後):

```rust
"wire_ack" => handle_wire_ack(&state, payload).await,
```

- [ ] **Step 4-4: legacy_bare_alias と coerce_wire_body を削除**

- `legacy_bare_alias` 関数 (937-943) と全呼び出し (proxy 化で自然消滅) を削除
- `coerce_wire_body` は Task 2 で routes/wire.rs に移設済 → unison_server の定義 (885-914) を削除
- テスト mod (1305 行以降): `legacy_bare_alias` / `coerce_wire_body` のテストを削除 (coerce 系テストは Task 2 で routes/wire.rs に移設済であること)。`normalize_agent_addr` のテストは**残す**

- [ ] **Step 4-5: SP HTTP に thread / ack route を追加 (CLI parity の土台)**

health.rs (wire_latest_msg_handler の後) に同型 wrapper:

```rust
/// POST /api/wire/thread - thread 系譜取得 HTTP 入口 (read-only、cursor 不触り)
pub async fn wire_thread_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_thread(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}

/// POST /api/wire/ack - per-message ack HTTP 入口 (R2-a、決定 D3)
pub async fn wire_ack_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match crate::process::unison_server::handle_wire_ack(&state, payload).await {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}
```

server.rs SP Router (428-442 の wire 群) に追加:

```rust
.route("/api/wire/thread", post(health::wire_thread_handler))
.route("/api/wire/ack", post(health::wire_ack_handler))
```

- [ ] **Step 4-6: build + テスト + Commit**

Run: `cargo test -p vantage-point`
Expected: PASS (legacy alias テスト削除済、normalize テスト green)

```bash
git add crates/vantage-point/src/process/unison_server.rs crates/vantage-point/src/process/routes/health.rs crates/vantage-point/src/process/server.rs
git commit -m "refactor(wire): SP handlers を TheWorld proxy 化 + legacy bare alias 撤去 + ack 経路 (R2-a)"
```

---

### Task 5: wire_remote 全撤去

中央化で cross-process forward は概念ごと消滅 (B1/B2 の問題領域が物理的になくなる)。

**Files:**
- Delete: `crates/vantage-point/src/capability/wire_remote.rs`
- Modify: `crates/vantage-point/src/capability/mod.rs` (mod 宣言 + re-export 削除)
- Modify: `crates/vantage-point/src/process/unison_server.rs` (`forward_remote_recipients` 削除 — Task 4 で proxy 化済なら呼び出しは既に消えている。関数本体を削除)
- Modify: `crates/vantage-point/src/process/routes/health.rs` (`wire_remote_deliver_handler` 削除、369-422 行付近)
- Modify: `crates/vantage-point/src/process/server.rs` (SP Router の `/api/wire/remote-deliver` route 削除)
- Modify: `crates/vantage-point/src/capability/wiremsg_store.rs` (`receive_forwarded` 削除 — 呼び出し元は remote_deliver_handler のみ。関連テストも削除)

- [ ] **Step 5-1: 上記を順に削除**

`grep -rn "wire_remote\|receive_forwarded\|remote-deliver\|remote_deliver\|classify_recipients\|forward_to_remote\|lookup_sp_port" crates/` で残骸ゼロを確認。`lookup_sp_port` が wire 以外 (lane 等) から使われていれば、その利用箇所ごと残すか個別判断 (wire_remote 内定義なら移設が必要 — 事前 grep で確認)。

- [ ] **Step 5-2: build + テスト + Commit**

Run: `cargo test -p vantage-point && cargo clippy -p vantage-point --all-targets`
Expected: PASS、unused warning なし

```bash
git add -A
git commit -m "refactor(wire): wire_remote (cross-process forward) 全撤去 — 中央化で概念ごと消滅 (R2-a, D1-b)"
```

---

### Task 6: SP の store 配線撤去 + actor rewire (notify / lane-spawn / bootstrap)

SP は wire store を持たない。`notify@<project>` / `lane-spawn@<project>` の consumer actor は TheWorld への HTTP long-poll に rewire。

**Files:**
- Modify: `crates/vantage-point/src/process/server.rs` (run() の store 構築削除 170-178、actor 生成 183-191 / 297-317、bootstrap 319-397)
- Modify: `crates/vantage-point/src/process/notification_actor.rs`
- Modify: `crates/vantage-point/src/process/lane_spawn_actor.rs`

- [ ] **Step 6-1: NotificationActor を HTTP long-poll に書き換え**

struct から `wiremsg_store` / `wire_notifier` field を削除し、recv loop を:

```rust
// (spawn_loop 内、store 取得部を置換)
let address = format!("notify@{}", project);
tracing::info!(
    "Notification bridge 起動 (= TheWorld 中央 wire store long-poll、address={})",
    address
);

loop {
    if shutdown.is_cancelled() {
        break;
    }
    // TheWorld 側 handle が max 30s の long-poll を行う。25s で投げて余裕を持つ。
    let payload = serde_json::json!({ "agent": address, "timeout": 25 });
    let resp = tokio::select! {
        _ = shutdown.cancelled() => break,
        r = crate::process::world_wire::call("/api/wire/recv", payload) => r,
    };
    match resp {
        Ok(v) => {
            let msgs = v
                .get("messages")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default();
            for msg in &msgs {
                if let Some(body) = msg.get("body") {
                    post_notification(body, &project_dir);
                }
            }
            // 空応答 (timeout) は即再 poll — TheWorld 側で待機しているので busy loop にならない
        }
        Err(e) => {
            // TheWorld 不在は standalone SP (`vp sp start` 単独) で正常系。debug に留め、
            // IDLE_POLL 間隔で再試行する。
            tracing::debug!("notify wire recv (TheWorld) 失敗、retry: {}", e);
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(IDLE_POLL) => {}
            }
        }
    }
}
```

`new()` signature: `new(project: String, project_dir: String)`。caller (server.rs:183-191) を合わせる。

- [ ] **Step 6-2: LaneSpawnActor を同パターンで書き換え**

struct から `wiremsg_store` / `wire_notifier` を削除、recv loop を NotificationActor と同じ HTTP long-poll 型に。取得した `msg["body"]` を `serde_json::from_value::<LaneCmd>` する既存処理・Semaphore gate はそのまま。`new()` から 2 引数を落とし、caller (server.rs:306-317) を合わせる。

- [ ] **Step 6-3: server.rs run() の SP store 配線を撤去 + bootstrap を HTTP send 化**

- 170-175 の `wiremsg_store` 構築を削除し、AppState には `wiremsg_store: None` を渡す (field 自体は world mode が使うので残す)
- 178 の `wire_notifier` は AppState 必須 field なのでそのまま (SP では実質未使用になる旨をコメント)
- bootstrap (321-397): `store.send_root(...)` を world_wire 経由に置換。TheWorld より SP が先に起動するケースのため spawned task 内で retry:

```rust
// wiremsg R2-a: bootstrap producer も中央 store へ HTTP send。
// daemon (TheWorld) 起動前に SP が上がるケースがあるため、spawn した task 内で
// 最大 60s retry する (SP 起動は block しない)。
let bootstrap_cmds: Vec<serde_json::Value> = performers
    .iter()
    .filter_map(|entry| {
        let default_stand = crate::config::Config::load()
            .unwrap_or_default()
            .default_stand_or_echoes()
            .to_string();
        let cmd = super::lane_cmd::LaneCmd::SpawnLane {
            project_id: performers_project_id.clone(),
            name: entry.name.clone(),
            cwd: entry.path.clone(),
            stand: default_stand,
        };
        serde_json::to_value(&cmd).ok()
    })
    .collect();
if !bootstrap_cmds.is_empty() {
    let from = bootstrap_from.clone();
    let to_addr = lane_spawn_addr.clone();
    tokio::spawn(async move {
        for body in bootstrap_cmds {
            let payload = serde_json::json!({
                "from": from,
                "to": [to_addr.clone()],
                "body": body,
            });
            let mut sent = false;
            for _attempt in 0..12u32 {
                match crate::process::world_wire::call("/api/wire/send", payload.clone()).await {
                    Ok(_) => {
                        sent = true;
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_secs(5)).await,
                }
            }
            if !sent {
                tracing::warn!(
                    "SP startup bootstrap: TheWorld 不達で SpawnLane 投入失敗 (60s retry 後)。\
                     `vp daemon start` 後に SP を再起動してください"
                );
            }
        }
    });
}
```

- [ ] **Step 6-4: build + テスト + Commit**

Run: `cargo test -p vantage-point`
Expected: PASS

```bash
git add crates/vantage-point/src/process/
git commit -m "refactor(wire): SP の local store 配線撤去、notify/lane-spawn actor を TheWorld long-poll 化 (R2-a)"
```

---

### Task 7: CLI parity — `vp wire inbox / thread / ack` 追加

現状 send / recv / watch / watch-supervised。inbox / thread / ack を追加して MCP と同等にする。default URL は SP (`http://127.0.0.1:33000`) のまま — SP proxy が正規化してくれる。`-u http://127.0.0.1:32000` で TheWorld 直叩きも可 (qualified 必須)。

**Files:**
- Modify: `crates/vantage-point/src/commands/wire.rs`

- [ ] **Step 7-1: subcommand 3 種を追加**

```rust
/// 未読の在庫確認 (read-only、cursor 不触り)
Inbox {
    /// SP の base URL
    #[arg(short, long, default_value = "http://127.0.0.1:33000")]
    url: String,
    /// 自 agent address (例: agent@vantage-point)
    #[arg(short, long)]
    agent: String,
},
/// thread 系譜の取得 (read-only、root-first)
Thread {
    #[arg(short, long, default_value = "http://127.0.0.1:33000")]
    url: String,
    /// 系譜を辿る起点 message id
    #[arg(short, long)]
    message_id: String,
},
/// message の ack (R2-a、command category の受領確認)
Ack {
    #[arg(short, long, default_value = "http://127.0.0.1:33000")]
    url: String,
    #[arg(short, long)]
    message_id: String,
    /// ack する agent address
    #[arg(short, long)]
    agent: String,
},
```

- [ ] **Step 7-2: 実行関数を追加 (既存 send/recv と同じ reqwest パターン)**

```rust
/// POST /api/wire/unread-count を叩いて未読在庫を表示する
async fn inbox(url: &str, agent: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/wire/unread-count", url.trim_end_matches('/')))
        .json(&serde_json::json!({ "agent": agent }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

/// POST /api/wire/thread で系譜 (root-first) を表示する
async fn thread(url: &str, message_id: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/wire/thread", url.trim_end_matches('/')))
        .json(&serde_json::json!({ "message_id": message_id }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

/// POST /api/wire/ack で message を ack する
async fn ack(url: &str, message_id: &str, agent: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/wire/ack", url.trim_end_matches('/')))
        .json(&serde_json::json!({ "message_id": message_id, "agent": agent }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}
```

match 分岐 (113-130 付近) に 3 arm を追加。既存コードの reqwest 利用形 (client 再利用等) に合わせる。

- [ ] **Step 7-3: 既存の arg parse テスト (383-427 付近) に倣い 3 subcommand のテストを追加**

```rust
#[test]
fn inbox_parses_agent() {
    let cmd = parse(&["wire", "inbox", "-a", "agent@vp"]);
    match cmd {
        WireCommands::Inbox { agent, .. } => assert_eq!(agent, "agent@vp"),
        _ => panic!("expected Inbox"),
    }
}
// thread (message_id) / ack (message_id + agent) も同型で
// 注: 既存テストの parse helper の実名・形に合わせる
```

- [ ] **Step 7-4: build + テスト + Commit**

Run: `cargo test -p vantage-point wire && cargo build -p vp-cli`
Expected: PASS

```bash
git add crates/vantage-point/src/commands/wire.rs
git commit -m "feat(cli): vp wire inbox/thread/ack 追加 — MCP との取得 primitives parity (R2-a)"
```

---

### Task 8: MCP `wire_ack` tool 追加

**Files:**
- Modify: `crates/vantage-point/src/mcp.rs` (wire_inbox の後、2891 行付近)

- [ ] **Step 8-1: params struct + tool を追加 (wire_thread のパターンに倣う)**

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WireAckParams {
    /// ack する message の id (wire_recv / wire_inbox で得た id)
    pub message_id: String,
}

/// wire message を ack する (R2-a、決定 D3: command category の受領確認)
///
/// recv で cursor が進んでも、command は ack されるまで delivery loop (R2-b) の
/// 再掲示対象。command の処理を終えたら必ず ack すること。read-only な
/// wire_inbox / wire_thread と異なり、ack 台帳に書き込む。
#[tool(
    description = "wire message を ack する (command 受領確認)。処理を終えた command message の id を渡す。"
)]
async fn wire_ack(
    &self,
    rmcp::handler::server::wrapper::Parameters(params): rmcp::handler::server::wrapper::Parameters<
        WireAckParams,
    >,
) -> Result<CallToolResult, McpError> {
    let agent = self.self_lane.from_address();
    let payload = serde_json::json!({
        "message_id": params.message_id,
        "agent": agent,
    });
    let resp = self.quic_call("wire_ack", payload).await?;
    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string(&resp).unwrap_or_default(),
    )]))
}
```

注: `#[tool]` attribute の形式・`self_lane.from_address()` の実名・Content 構築は既存 wire_thread (2863-2874) の実装をそのまま踏襲する。

- [ ] **Step 8-2: build + Commit**

Run: `cargo build -p vantage-point && cargo test -p vantage-point mcp`
Expected: PASS

```bash
git add crates/vantage-point/src/mcp.rs
git commit -m "feat(mcp): wire_ack tool 追加 (R2-a、取得 primitives 完備)"
```

---

### Task 9: 全体検証 + ドキュメント追従

**Files:**
- Modify: `CLAUDE.md` (CLI コマンド一覧の wire 行)
- Modify: 必要に応じ `docs/` の wiremsg 関連記述

- [ ] **Step 9-1: workspace 全体の検証**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

Expected: 全て green。clippy warning ゼロ。

- [ ] **Step 9-2: GitNexus detect_changes**

`detect_changes()` を実行し、影響シンボルが wire 系 + actor 系 + CLI/MCP の想定範囲に収まることを確認。想定外の flow が出たら原因を調べて報告。

- [ ] **Step 9-3: CLAUDE.md の CLI 行を更新**

```
vp wire send|recv|inbox|thread|ack|watch   # wire 中央 store messaging (store は TheWorld :32000)
```

- [ ] **Step 9-4: Commit**

```bash
git add CLAUDE.md docs/
git commit -m "docs: wiremsg R2-a (中央化 + primitives) を CLI 一覧に反映"
```

---

### Task 10: E2E dogfood 検証 + 出荷

R2-a の完成条件 (設計メモ): primitives が両系統で動き、round-trip の土台が立つこと。

- [ ] **Step 10-1: ローカル E2E**

```bash
cargo install --path crates/vp-cli
vp restart-all     # TheWorld + SP を新 binary で再起動
# 1. CLI send (SP proxy 経由)
vp wire send -t agent@vantage-point -b '{"kind":"event","note":"r2a-e2e"}' -f tester@e2e
# 2. CLI inbox (read-only、未読 1 を確認)
vp wire inbox -a agent@vantage-point
# 3. CLI recv (drain)
vp wire recv -a agent@vantage-point
# 4. recv 結果の id で thread / ack
vp wire thread -m <id>
vp wire ack -m <id> -a agent@vantage-point
# 5. TheWorld 直叩きで bare reject を確認 (エラーが返れば OK)
curl -s -X POST http://127.0.0.1:32000/api/wire/send \
  -H 'content-type: application/json' \
  -d '{"from":"agent","to":["agent@vantage-point"],"body":{"k":1}}'
```

Expected: 1-4 が成功、5 が bare reject エラー。MCP 側は本セッションの `wire_inbox` / `wire_recv` ツールでも確認。

- [ ] **Step 10-2: 出荷 (ship flow 規約: team-b レビュー → PR → auto-merge ON)**

1. team-bucciarati (moody-blues) でレビュー
2. `gh pr create --base nightly` (本文に設計 memory id `mem_1CbvcJj4ppU3QKH9d7xMpT` を記載)
3. auto-merge ON

- [ ] **Step 10-3: creo memories に work_log 記録 + R2-a todo を done 化**

---

## Self-Review チェック済み事項

- 設計メモの R2-a スコープ全項目に対応 task あり: 中央化 (T2/T3/T4/T6)、primitives MCP+CLI 両系統 (T1/T4/T7/T8)、wire_remote 撤去 (T5)、legacy alias 撤去 (T4)、退避 (前提として実施済み)
- `wire_acks` / `ack()` / `acks_for()` / `handle_wire_ack` / `wire_ack_store` の命名は全 task で一貫
- 「現実装をそのまま移植」と書いた箇所 (Task 2 の *_store 群) は移植元の file:line を明記済み — 実装者は該当箇所を読みながら写すこと
