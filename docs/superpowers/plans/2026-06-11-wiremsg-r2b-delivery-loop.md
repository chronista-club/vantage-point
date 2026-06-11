# wiremsg R2-b: delivery loop + tmux nudge (C) + ack 再掲示 — 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** TheWorld に delivery loop を新設し、未 ack の command category メッセージを tmux nudge (チャネル C) で受信者に届け、ack されるまで再掲示する。

**Architecture:** TheWorld (run_world) 上に `DeliveryActor` (LayerScope::World、初の World 常駐 actor) を spawn。in-process の `WiremsgStore::unacked_commands()` (新設) で「未 ack の command」を引き、lane registry (LaneInfo: state + tmux session 名) で受信者の状態を判定して、**directmsg と同じ方式で TheWorld から tmux に直接 send-keys** する (同一マシン前提のチャネル C は SP hop 不要 — 設計の「TheWorld→SP 経由」より簡約、C は Phase A 後に退役するつなぎ)。即時性は wire_send (command) 時の Notify wake + 30s tick で担保。

**Tech Stack:** Rust (Tokio)、SurrealDB embedded、tmux send-keys

**設計 SSOT:** `mem_1CbvcJj4ppU3QKH9d7xMpT` (R2 設計確定)。user hearing 確定 2 点 (2026-06-11):
1. **degraded mode (activity 未供給) は「lane Running なら nudge、不在/Dead なら pending 保持」**。busy/idle/waiting の精密化は Phase A の activity 供給後
2. **category は body.category 明示** (∈ {command, event, state, data, log}、無指定 = event 扱いで nudge 対象外)。command 産出元 (flow_handoff) に付与、CLI に --category 追加

**policy 定数 (設計初期値):** 再掲示間隔 T=10min、最大 N=3 回、tick=30s。nudge 台帳は in-memory (TheWorld 再起動でリセット = 最大 N 回が再付与される。許容、将来必要なら table 化)。

---

## 進め方の規約

- ブランチ: `git fetch origin nightly && git checkout -b mako/wiremsg-r2b-delivery-loop origin/nightly`
- 各 Task 末尾で `cargo test -p vantage-point` green → commit
- GitNexus: 編集前 impact、commit 前 detect_changes
- コメントは日本語。data / calculations / actions 分離 (decide_nudge 等は純関数に)

---

### Task 0: lane 開始 + impact 分析

- [ ] `git fetch origin nightly && git checkout -b mako/wiremsg-r2b-delivery-loop origin/nightly`
- [ ] impact (upstream): `wire_send_store`, `run_world`, `WiremsgStore` — HIGH/CRITICAL なら報告

---

### Task 1: `WiremsgStore::unacked_commands()` (TDD)

**Files:**
- Modify: `crates/vantage-point/src/capability/wiremsg_store.rs` (acks_for の後にメソッド追加 + テスト)

- [ ] **Step 1-1: 失敗するテスト (テスト mod 末尾の ack 系の後)**

```rust
/// unacked_commands: command category のみ・未 ack 受信者のみが pending に載る
#[tokio::test]
async fn unacked_commands_lists_pending_recipients() {
    let store = make_test_store().await;
    // command (nudge 対象) と event (対象外) を送る
    let cmd = store
        .send_root(
            "agent@vp",
            &["agent@nexus".to_string(), "agent@vp/w1".to_string()],
            serde_json::json!({"category": "command", "text": "やって"}),
        )
        .await
        .expect("command send");
    store
        .send_root(
            "agent@vp",
            &["agent@nexus".to_string()],
            serde_json::json!({"category": "event", "text": "info"}),
        )
        .await
        .expect("event send");

    let pending = store.unacked_commands().await.expect("unacked");
    assert_eq!(pending.len(), 1, "command のみ (event は対象外)");
    assert_eq!(pending[0].0.id, cmd.id);
    assert_eq!(
        pending[0].1,
        vec!["agent@nexus".to_string(), "agent@vp/w1".to_string()],
        "未 ack の受信者全員 (送信者は含まない)"
    );

    // 片方が ack → pending から消える。全員 ack → message ごと消える
    store.ack(&cmd.id, "agent@nexus").await.expect("ack 1");
    let pending = store.unacked_commands().await.expect("unacked 2");
    assert_eq!(pending[0].1, vec!["agent@vp/w1".to_string()]);
    store.ack(&cmd.id, "agent@vp/w1").await.expect("ack 2");
    assert!(store.unacked_commands().await.expect("unacked 3").is_empty());
}

/// unacked_commands: category 無指定の message は event 扱い (対象外)
#[tokio::test]
async fn unacked_commands_ignores_uncategorized() {
    let store = make_test_store().await;
    store
        .send_root("agent@vp", &["agent@nexus".to_string()], body("no category"))
        .await
        .expect("send");
    assert!(store.unacked_commands().await.expect("unacked").is_empty());
}
```

- [ ] **Step 1-2: 失敗確認** — Run: `cargo test -p vantage-point unacked_commands` → コンパイルエラー (メソッド未定義)

- [ ] **Step 1-3: 実装 (acks_for の直後)**

```rust
/// 未 ack の command category message を pending 受信者付きで返す (R2-b delivery loop 用)
///
/// 戻り値: `(message, pending_agents)` の Vec (local_seq 昇順)。pending = `to` から
/// ack 済 agent と送信者自身を除いた残り。全員 ack 済の message は載らない。
/// cursor (agent_cursor) とは独立 — recv 済でも ack されるまで載り続ける (決定 D3)。
///
/// 注: `body.category` には index が無い。command は低頻度・store は R2-a で
/// リセット済のため全走査で十分。件数が増えたら index 追加を検討。
pub async fn unacked_commands(&self) -> Result<Vec<(WireMessage, Vec<String>)>> {
    let mut res = self
        .db
        .query("SELECT * FROM wire_messages WHERE body.category = 'command' ORDER BY local_seq;")
        .await
        .map_err(|e| anyhow::anyhow!("wiremsg unacked_commands failed: {e}"))?;
    let rows: Vec<serde_json::Value> = res
        .take(0)
        .map_err(|e| anyhow::anyhow!("wiremsg unacked_commands take failed: {e}"))?;

    let mut out = Vec::new();
    for row in &rows {
        let msg = Self::row_to_message(row)?;
        let acked = self.acks_for(&msg.id).await?;
        let pending: Vec<String> = msg
            .to
            .iter()
            .filter(|a| **a != msg.from && !acked.contains(a))
            .cloned()
            .collect();
        if !pending.is_empty() {
            out.push((msg, pending));
        }
    }
    Ok(out)
}
```

- [ ] **Step 1-4: green 確認** — Run: `cargo test -p vantage-point unacked_commands` → PASS (2 tests)
- [ ] **Step 1-5: Commit** — `feat(wire): WiremsgStore::unacked_commands — ack 台帳基準の pending query (R2-b)`

---

### Task 2: DeliveryActor (純関数 + actor + tmux nudge action)

**Files:**
- Create: `crates/vantage-point/src/process/delivery_actor.rs`
- Modify: `crates/vantage-point/src/process/mod.rs` (`pub(crate) mod delivery_actor;`)

data / calculations / actions を分離する: 判定 (`decide_nudge` / `wire_agent_to_lane_display` / `pick_tmux_session`) は純関数で単体テスト、I/O (store query / tmux send) は actor loop に隔離。

- [ ] **Step 2-1: ファイル作成 — 純関数 + 型**

```rust
//! wire delivery loop (R2-b、 設計 mem_1CbvcJj4ppU3QKH9d7xMpT) — TheWorld 常駐 actor
//!
//! 未 ack の command category message を受信者の tmux session に nudge (チャネル C) する。
//! TheWorld 上で store と同居 (in-process query、 決定 D1-c)。
//!
//! ## policy (degraded mode、 user 確定 2026-06-11)
//!
//! Phase A の activity 供給 (idle/busy/waiting) 前は lane registry の粗い状態で代用する:
//! - lane が Running → 即 nudge (busy でも send-keys は次の prompt 境界で処理される)
//! - lane 不在 / Dead → pending 保持 (台帳は進めない。 Phase A 後にチャネル D で配信)
//! - 再掲示: 前回 nudge から RENUDGE_AFTER 経過 && 未 ack → 再 nudge (MAX_NUDGES まで)
//!
//! ## nudge 台帳は in-memory
//!
//! (message_id, agent) → (回数, 最終時刻)。 TheWorld 再起動でリセットされ最大
//! MAX_NUDGES 回が再付与されるが、 ack されれば止まるため許容 (table 化は必要になってから)。
//!
//! ## チャネル C の送出は tmux 直 (directmsg と同方式)
//!
//! TheWorld は SP と同一マシンで動く daemon なので、 SP を hop せず lane registry の
//! tmux session 名に直接 send-keys する。 C は Phase A 後に native channels (A) へ
//! 移行予定のつなぎ (設計の配信チャネル表)。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capability::WiremsgStore;
use crate::capability::stand_service::{LayerScope, Service, SpawnableService};
use crate::process::lanes_state::{LaneInfo, LaneState, TmuxMode};

/// 配信 pulse の定期 tick (Notify wake の取りこぼし安全網)
const TICK: Duration = Duration::from_secs(30);
/// 未 ack command の再掲示間隔 (設計初期値 10min)
const RENUDGE_AFTER: Duration = Duration::from_secs(600);
/// 同一 (message, agent) への nudge 上限 (設計初期値 3 回)
const MAX_NUDGES: u32 = 3;

/// nudge 台帳の 1 record
#[derive(Debug, Clone, Copy)]
struct NudgeRecord {
    count: u32,
    last_at: Instant,
}

/// nudge 判定 (純関数で表すための decision)
#[derive(Debug, PartialEq)]
enum NudgeDecision {
    /// 今 nudge する (初回 or 再掲示時刻到来)
    Send,
    /// まだ待つ (前回 nudge から間隔未経過)
    Wait,
    /// 上限到達 — これ以上 nudge しない (ack 待ちのまま)
    Exhausted,
}

/// nudge するかの判定 (純関数)
fn decide_nudge(
    record: Option<&NudgeRecord>,
    now: Instant,
    renudge_after: Duration,
    max: u32,
) -> NudgeDecision {
    match record {
        None => NudgeDecision::Send,
        Some(r) if r.count >= max => NudgeDecision::Exhausted,
        Some(r) if now.duration_since(r.last_at) >= renudge_after => NudgeDecision::Send,
        Some(_) => NudgeDecision::Wait,
    }
}

/// wire agent address → lane address の Display 形 (純関数)
///
/// nudge 可能なのは agent (conductor / performer) のみ:
/// - `agent@<project>` → `<project>/conductor`
/// - `agent@<project>/<name>` → `<project>/performer/<name>`
/// - それ以外 (notify@ / lane-spawn@ / vp-cli 等) → None (nudge 対象外)
fn wire_agent_to_lane_display(addr: &str) -> Option<String> {
    let rest = addr.strip_prefix("agent@")?;
    if rest.is_empty() {
        return None;
    }
    match rest.split_once('/') {
        None => Some(format!("{}/conductor", rest)),
        Some((project, name)) if !project.is_empty() && !name.is_empty() => {
            Some(format!("{}/performer/{}", project, name))
        }
        Some(_) => None,
    }
}

/// lane 一覧から nudge 先の tmux session を引く (純関数)
///
/// Running かつ Tmux mode の session を持つ lane のみ nudge 可能。
/// 該当なし = offline 扱い (pending 保持)。
fn pick_tmux_session(lanes: &[LaneInfo], lane_display: &str) -> Option<String> {
    lanes
        .iter()
        .find(|l| l.address.to_string() == lane_display && matches!(l.state, LaneState::Running))
        .and_then(|l| {
            l.tmux
                .iter()
                .find(|t| matches!(t.mode, TmuxMode::Tmux))
                .map(|t| t.session.clone())
        })
}

/// nudge 文言 (受信→処理→ack の導線を 1 行で)
fn nudge_text(message_id: &str, renudge_count: u32) -> String {
    let prefix = if renudge_count == 0 {
        "📨 wire: command が届いています".to_string()
    } else {
        format!("📨 wire: 未 ack の command があります (再掲示 {}回目)", renudge_count)
    };
    format!(
        "{}。 mcp__vantage-point__wire_recv で受信し、 処理後に mcp__vantage-point__wire_ack (message_id={}) してください。",
        prefix, message_id
    )
}
```

注: `LaneState` / `TmuxMode` の variant 名は lanes_state.rs の実定義 (`Running` / `TmuxMode::Tmux`) に合わせる。import が合わなければ実物を確認して調整。

- [ ] **Step 2-2: 純関数の単体テスト (同ファイル `#[cfg(test)]`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_first_time_is_send() {
        assert_eq!(
            decide_nudge(None, Instant::now(), RENUDGE_AFTER, MAX_NUDGES),
            NudgeDecision::Send
        );
    }

    #[test]
    fn decide_recent_is_wait_and_elapsed_is_send() {
        let now = Instant::now();
        let recent = NudgeRecord { count: 1, last_at: now };
        assert_eq!(
            decide_nudge(Some(&recent), now, RENUDGE_AFTER, MAX_NUDGES),
            NudgeDecision::Wait
        );
        let old = NudgeRecord {
            count: 1,
            last_at: now - RENUDGE_AFTER - Duration::from_secs(1),
        };
        assert_eq!(
            decide_nudge(Some(&old), now, RENUDGE_AFTER, MAX_NUDGES),
            NudgeDecision::Send
        );
    }

    #[test]
    fn decide_at_max_is_exhausted() {
        let r = NudgeRecord {
            count: MAX_NUDGES,
            last_at: Instant::now() - RENUDGE_AFTER * 2,
        };
        assert_eq!(
            decide_nudge(Some(&r), Instant::now(), RENUDGE_AFTER, MAX_NUDGES),
            NudgeDecision::Exhausted
        );
    }

    #[test]
    fn agent_address_maps_to_lane_display() {
        assert_eq!(
            wire_agent_to_lane_display("agent@vp").as_deref(),
            Some("vp/conductor")
        );
        assert_eq!(
            wire_agent_to_lane_display("agent@vp/w1").as_deref(),
            Some("vp/performer/w1")
        );
        assert_eq!(wire_agent_to_lane_display("notify@vp"), None);
        assert_eq!(wire_agent_to_lane_display("vp-cli"), None);
        assert_eq!(wire_agent_to_lane_display("agent@"), None);
        assert_eq!(wire_agent_to_lane_display("agent@vp/"), None);
    }
}
```

- [ ] **Step 2-3: actor 本体 (同ファイル)**

```rust
/// TheWorld 常駐の wire delivery actor (R2-b)
pub struct DeliveryActor {
    /// 中央 wire store (TheWorld の in-process 参照)
    store: WiremsgStore,
    /// TheWorld lane registry (QUIC push でリアルタイム更新される)
    lane_registry: Arc<RwLock<HashMap<String, Vec<LaneInfo>>>>,
    /// wire_send (command) 時の即時 wake (AppState.delivery_notify と共有)
    wake: Arc<tokio::sync::Notify>,
}

impl DeliveryActor {
    pub fn new(
        store: WiremsgStore,
        lane_registry: Arc<RwLock<HashMap<String, Vec<LaneInfo>>>>,
        wake: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            store,
            lane_registry,
            wake,
        }
    }
}

impl Service for DeliveryActor {
    fn actor_name(&self) -> &str {
        "wire-delivery"
    }

    fn layer_scope(&self) -> LayerScope {
        // machine-wide singleton (TheWorld daemon scope) — 初の World 常駐 actor
        LayerScope::World
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl SpawnableService for DeliveryActor {
    fn spawn_loop(self, shutdown: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let Self {
                store,
                lane_registry,
                wake,
            } = self;
            // nudge 台帳: (message_id, agent) → record。 in-memory (doc 参照)
            let mut ledger: HashMap<(String, String), NudgeRecord> = HashMap::new();
            tracing::info!(
                "wire delivery loop 起動 (tick={:?}, renudge_after={:?}, max={})",
                TICK,
                RENUDGE_AFTER,
                MAX_NUDGES
            );
            loop {
                // wake (command 着信) / tick / shutdown のいずれかで pulse
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(TICK) => {}
                }
                if let Err(e) = pulse(&store, &lane_registry, &mut ledger).await {
                    tracing::warn!("wire delivery pulse 失敗 (次 tick で再試行): {}", e);
                }
            }
            tracing::info!("wire delivery loop: shutdown");
        })
    }
}

/// 1 回の配信 pulse: 未 ack command を引き、 policy に従って nudge する (action 層)
async fn pulse(
    store: &WiremsgStore,
    lane_registry: &Arc<RwLock<HashMap<String, Vec<LaneInfo>>>>,
    ledger: &mut HashMap<(String, String), NudgeRecord>,
) -> anyhow::Result<()> {
    let pending = store.unacked_commands().await?;

    // 台帳 GC: ack 済 (= pending から消えた) entry を落とす
    let live: std::collections::HashSet<(String, String)> = pending
        .iter()
        .flat_map(|(m, agents)| agents.iter().map(move |a| (m.id.clone(), a.clone())))
        .collect();
    ledger.retain(|k, _| live.contains(k));

    if pending.is_empty() {
        return Ok(());
    }
    let lanes: Vec<LaneInfo> = lane_registry.read().await.values().flatten().cloned().collect();
    let now = Instant::now();

    for (msg, agents) in &pending {
        for agent in agents {
            // nudge 可能な address (conductor / performer) のみ。 他は対象外
            let Some(lane_display) = wire_agent_to_lane_display(agent) else {
                continue;
            };
            // degraded policy: Running lane の tmux session が無ければ offline 扱い (pending 保持)
            let Some(session) = pick_tmux_session(&lanes, &lane_display) else {
                continue;
            };
            let key = (msg.id.clone(), agent.clone());
            match decide_nudge(ledger.get(&key), now, RENUDGE_AFTER, MAX_NUDGES) {
                NudgeDecision::Send => {
                    let count = ledger.get(&key).map(|r| r.count).unwrap_or(0);
                    let text = nudge_text(&msg.id, count);
                    match send_keys_to_session(&session, &text).await {
                        Ok(()) => {
                            tracing::info!(
                                "wire delivery: nudge 送出 (msg={}, agent={}, session={}, count={})",
                                msg.id,
                                agent,
                                session,
                                count + 1
                            );
                            ledger.insert(
                                key,
                                NudgeRecord {
                                    count: count + 1,
                                    last_at: now,
                                },
                            );
                        }
                        Err(e) => {
                            // best-effort: 送出失敗は台帳を進めない (次 pulse で再試行)
                            tracing::warn!(
                                "wire delivery: nudge 失敗 (msg={}, agent={}, session={}): {}",
                                msg.id,
                                agent,
                                session,
                                e
                            );
                        }
                    }
                }
                NudgeDecision::Wait | NudgeDecision::Exhausted => {}
            }
        }
    }
    Ok(())
}

/// tmux session に literal text + Enter を送る (directmsg と同方式、 blocking を隔離)
async fn send_keys_to_session(session: &str, text: &str) -> anyhow::Result<()> {
    let session = session.to_string();
    let text = text.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let tmux = crate::tmux::tmux_bin().unwrap_or("tmux");
        let status = std::process::Command::new(tmux)
            .args(["send-keys", "-t", &session, "-l", &text])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux send-keys 失敗 (session={session})");
        }
        let status = std::process::Command::new(tmux)
            .args(["send-keys", "-t", &session, "Enter"])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux send-keys Enter 失敗 (session={session})");
        }
        Ok(())
    })
    .await?
}
```

注: `crate::tmux::tmux_bin()` の実在は directmsg.rs では素の "tmux" を使っている。tmux_actor.rs:602 が `crate::tmux::tmux_bin()` を使っているのでそれに合わせる (無ければ素の "tmux")。

- [ ] **Step 2-4: mod 宣言 + build + テスト** — `process/mod.rs` に `pub(crate) mod delivery_actor;`。Run: `cargo test -p vantage-point delivery` → PASS (純関数 4 tests)
- [ ] **Step 2-5: Commit** — `feat(wire): DeliveryActor — 未 ack command の tmux nudge + 再掲示 (R2-b、World 初の常駐 actor)`

---

### Task 3: delivery_notify 配線 + run_world で actor spawn

**Files:**
- Modify: `crates/vantage-point/src/process/state.rs` (AppState に field 追加)
- Modify: `crates/vantage-point/src/process/server.rs` (run / run_world の AppState 構築 2 箇所 + run_world で spawn)
- Modify: `crates/vantage-point/src/process/routes/wire.rs` (send 成功時の wake)

- [ ] **Step 3-1: AppState に `delivery_notify` 追加**

state.rs の `wire_notifier` field の近くに:

```rust
/// R2-b: wire delivery loop の即時 wake (command 着信時に notify)。
/// World mode でのみ実体が待ち受ける。 SP では未使用 (proxy が TheWorld に送るだけ)。
pub delivery_notify: std::sync::Arc<tokio::sync::Notify>,
```

server.rs の AppState 構築 2 箇所 (run / run_world) と state.rs のテスト fixture (`wire_notifier` を初期化している箇所、 state.rs:437 近辺) に `delivery_notify: std::sync::Arc::new(tokio::sync::Notify::new()),` を追加。

- [ ] **Step 3-2: world_wire_send_handler で command 着信 wake**

routes/wire.rs の `world_wire_send_handler` を:

```rust
pub async fn world_wire_send_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let store = world_store!(state);
    // R2-b: command 着信なら delivery loop を即 wake (判定は保存前の素の payload で
    // 行う — body が文字列化 JSON の場合は coerce 後と一致しないが、 その場合も
    // 30s tick が拾うため fail-open)
    let is_command = payload
        .get("body")
        .and_then(|b| b.get("category"))
        .and_then(|c| c.as_str())
        == Some("command");
    match wire_send_store(store, &state.wire_notifier, payload).await {
        Ok(v) => {
            if is_command {
                state.delivery_notify.notify_one();
            }
            Json(v)
        }
        Err(e) => Json(serde_json::json!({"status": "error", "error": e})),
    }
}
```

- [ ] **Step 3-3: run_world で DeliveryActor を spawn**

server.rs run_world の AppState 構築後 (Router 構築の前) に:

```rust
// R2-b: wire delivery loop (未 ack command の tmux nudge + 再掲示) を spawn。
// store 未構築 (DB 接続失敗) なら skip — wire 自体が動かないため delivery も不要。
if let Some(store) = state.wiremsg_store.clone() {
    let lane_registry = world_cap.read().await.lane_registry_ref();
    state.actor_registry.write().await.spawn_service(
        super::delivery_actor::DeliveryActor::new(
            store,
            lane_registry,
            state.delivery_notify.clone(),
        ),
        shutdown_token.clone(),
    );
}
```

注: world mode の `actor_registry` は空構築 (server.rs:882 付近) — `spawn_service` がそのまま使えるはず。`ActorRegistry::spawn_service` の signature (notification_actor の spawn と同形) に合わせる。

- [ ] **Step 3-4: build + 全 lib テスト + Commit** — `feat(wire): delivery_notify 配線 + run_world で DeliveryActor spawn (R2-b)`

---

### Task 4: 送信側の category 付与 (flow_handoff / CLI / MCP doc)

**Files:**
- Modify: `crates/vantage-point/src/mcp.rs` (flow_handoff の wire_body ~1593 行 + wire_send tool description)
- Modify: `crates/vantage-point/src/commands/wire.rs` (Send variant に --category)

- [ ] **Step 4-1: flow_handoff の body に category 付与**

mcp.rs:1593-1597 を:

```rust
let wire_body = serde_json::json!({
    "kind": "task",
    // R2-b: category は delivery policy selector (command = ack されるまで再掲示対象)
    "category": "command",
    "task_spec": params.task_spec,
    "mode": mode,
});
```

- [ ] **Step 4-2: wire_send MCP tool の description に category 規約を追記**

wire_send の `#[tool(description = ...)]` (mcp.rs 2810 行付近) の説明文末尾に追記:

```
 Set body.category to one of {command, event, state, data, log} to control delivery policy: 'command' messages are re-nudged to the recipient until they wire_ack; omitted category defaults to 'event' (no nudge).
```

- [ ] **Step 4-3: CLI `vp wire send --category`**

commands/wire.rs の Send variant に:

```rust
/// delivery policy selector (command|event|state|data|log)。
/// command は受信者が ack するまで delivery loop の再掲示対象 (R2-b)
#[arg(long)]
category: Option<String>,
```

run() の match arm と send() に引数を通し、payload 構築を:

```rust
let mut body_obj = serde_json::json!({ "text": body });
if let Some(cat) = category {
    body_obj["category"] = serde_json::Value::String(cat.to_string());
}
let mut payload = serde_json::json!({
    "from": from,
    "to": [to],
    "body": body_obj,
});
```

arg parse テストを 1 本追加:

```rust
/// R2-b: --category が Send に渡る
#[test]
fn send_parses_category() {
    let cli = TestCli::try_parse_from([
        "vp-wire-test", "send", "-t", "agent@vp", "-b", "x", "--category", "command",
    ])
    .expect("parse should succeed");
    match cli.cmd {
        WireCommands::Send { category, .. } => assert_eq!(category.as_deref(), Some("command")),
        other => panic!("expected Send variant, got {:?}", other),
    }
}
```

- [ ] **Step 4-4: build + テスト + Commit** — `feat(wire): body.category 規約 — flow_handoff=command 付与 + CLI --category + MCP doc (R2-b)`

---

### Task 5: 全体検証 + ドキュメント追従

- [ ] `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets` / `cargo test --workspace` 全 green
- [ ] `node .gitnexus/run.cjs analyze` → `detect_changes({scope:"compare", base_ref:"origin/nightly"})` で影響範囲が wire/delivery/CLI/MCP に収まることを確認
- [ ] docs/spec/02-capability.md の wiremsg 節に R2-b 1 行追記 (delivery loop + 再掲示。チェックリストに `- [x] delivery loop — 未 ack command の tmux nudge + 再掲示 (R2-b、チャネル C)` を追加)
- [ ] Commit — `docs: wiremsg R2-b (delivery loop) を spec に反映`

---

### Task 6: E2E dogfood + 出荷

- [ ] **Step 6-1: ローカル E2E (nexus conductor を受信者に)**

```bash
cargo install --path crates/vp-cli
vp restart-all && sleep 5
# 1. command を nexus conductor 宛に送る (SP proxy 経由)
vp wire send -t agent@nexus -b 'r2b-e2e: これは command 配信テスト。内容確認だけで OK' -f agent@vantage-point --category command
# 2. 数秒以内に nexus conductor の tmux session に nudge が届くことを capture で確認
sleep 5 && tmux capture-pane -t "$(tmux list-sessions -F '#{session_name}' | grep '^vp-nexus-conductor-')" -p | tail -5
# 期待: 「📨 wire: command が届いています…wire_ack (message_id=…)」の行
# 3. ack して pending が消えることを確認 (再掲示が止まる)
vp wire ack -m <上記 message_id> -a agent@nexus
# 4. event (category なし) は nudge されないことを確認
vp wire send -t agent@nexus -b 'r2b-e2e: event は nudge されない' -f agent@vantage-point
sleep 35 && tmux capture-pane -t <同 session> -p | tail -3   # nudge 行が増えていないこと
```

- [ ] **Step 6-2: 出荷** — team-b (moody-blues) レビュー → 指摘対応 → `gh pr create --base nightly` (本文に設計 memory id) → auto-merge ON
- [ ] **Step 6-3: creo work_log 記録 + nightly に戻して pull + index 再生成**

---

## Self-Review チェック済み事項

- 設計の R2-b スコープ (delivery loop / tmux nudge C / ack 再掲示) に全て対応 task あり。hearing 確定 2 点 (degraded=Running なら nudge、category=body.category) を反映
- 命名一貫: `unacked_commands` (T1) を `pulse` (T2) が呼ぶ。`delivery_notify` (T3) を `DeliveryActor::wake` が待つ。`body.category` (T4) を T1 の query が読む
- 「TheWorld→SP 経由で nudge」(設計 D1-c の文言) → tmux 直送に簡約した根拠は Architecture 節に明記 (同一マシン daemon + C は Phase A 後退役のつなぎ)
