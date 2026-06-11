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

// =============================================================================
// calculations — nudge 判定の純関数 (I/O なし、 単体テスト対象)
// =============================================================================

/// nudge 台帳の 1 record
#[derive(Debug, Clone, Copy)]
struct NudgeRecord {
    count: u32,
    last_at: Instant,
}

/// nudge するかの判定結果
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
        format!(
            "📨 wire: 未 ack の command があります (再掲示 {}回目)",
            renudge_count
        )
    };
    format!(
        "{}。 mcp__vantage-point__wire_recv で受信し、 処理後に mcp__vantage-point__wire_ack (message_id={}) してください。",
        prefix, message_id
    )
}

// =============================================================================
// actions — actor 本体 (store query / tmux send の I/O 層)
// =============================================================================

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
            // nudge 台帳: (message_id, agent) → record。 in-memory (module doc 参照)
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

/// 1 回の配信 pulse: 未 ack command を引き、 policy に従って nudge する
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
    let lanes: Vec<LaneInfo> = lane_registry
        .read()
        .await
        .values()
        .flatten()
        .cloned()
        .collect();
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
        // Enter は別 send-keys で送る (`-l` と混ぜると Enter も literal 文字列扱いになる)
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

// =============================================================================
// Tests — 純関数のみ (I/O 層は E2E で検証)
// =============================================================================

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
        let recent = NudgeRecord {
            count: 1,
            last_at: now,
        };
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
