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
/// R3-c: 同一 (message, agent) への bg dispatch 再試行間隔
const BG_REDISPATCH_AFTER: Duration = Duration::from_secs(600);
/// R3-c: 同一 (message, agent) への bg dispatch 上限 (誤起動の暴発防止、 nudge より控えめ)
const MAX_BG_DISPATCHES: u32 = 2;

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

/// lane 一覧から nudge 先 (tmux session 名, lane cwd, cc_session_id) を引く (純関数)
///
/// Running かつ Tmux mode の session を持つ lane のみ nudge 可能。
/// 該当なし = offline 扱い (pending 保持)。 cwd は R3-a の CC activity 照合
/// (`agents --json` の cwd と突き合わせ) に使う。 cc_session_id は R3-c の
/// `claude -p --resume <id>` headless 再開に使う (None なら fresh headless)。
fn pick_nudge_target(
    lanes: &[LaneInfo],
    lane_display: &str,
) -> Option<(String, String, Option<String>)> {
    lanes
        .iter()
        .find(|l| l.address.to_string() == lane_display && matches!(l.state, LaneState::Running))
        .and_then(|l| {
            l.tmux
                .iter()
                .find(|t| matches!(t.mode, TmuxMode::Tmux))
                .map(|t| (t.session.clone(), l.cwd.clone(), l.cc_session_id.clone()))
        })
}

/// 受信者の配信準備状態 (R3-a、 設計 policy table の lane 状態軸)
#[derive(Debug, PartialEq)]
enum Readiness {
    /// idle / waiting (or degraded で Running) — 即 nudge してよい
    Ready,
    /// busy — 待つ (idle 遷移を次 pulse で拾う。 台帳は進めない)
    Busy,
    /// lane 不在 / session 不在 — pending 保持 (Phase A 後にチャネル D で配信)
    Offline,
}

/// 受信者の readiness を判定する (純関数、 R3-a)
///
/// - `lane_nudgeable`: lane registry で Running かつ tmux session あり (= send-keys 可能)
/// - `activity`: 外側 None = poll 不能 (degraded fallback = R2-b の user 確定挙動)、
///   内側 None = poll は成功したが当該 cwd に interactive session 不在 (= offline)
fn recipient_readiness(
    lane_nudgeable: bool,
    activity: Option<Option<crate::process::cc_activity::CcState>>,
) -> Readiness {
    use crate::process::cc_activity::CcState;
    if !lane_nudgeable {
        return Readiness::Offline;
    }
    match activity {
        // degraded (R2-b 挙動): Running なら nudge
        None => Readiness::Ready,
        Some(None) => Readiness::Offline,
        Some(Some(CcState::Idle)) | Some(Some(CcState::Waiting)) => Readiness::Ready,
        Some(Some(CcState::Busy)) => Readiness::Busy,
    }
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

/// wire agent address → headless dispatch 用の (VP_PROJECT, VP_LANE) (純関数、 R3-c)
///
/// spawn する claude に lane と同じ identity env を渡すための導出。
/// - `agent@<project>` → (project, "conductor")
/// - `agent@<project>/<name>` → (project, name)  ※ VP_LANE は performer 名
/// - それ以外 → None
fn lane_identity_from_agent(addr: &str) -> Option<(String, String)> {
    let rest = addr.strip_prefix("agent@")?;
    if rest.is_empty() {
        return None;
    }
    match rest.split_once('/') {
        None => Some((rest.to_string(), "conductor".to_string())),
        Some((project, name)) if !project.is_empty() && !name.is_empty() => {
            Some((project.to_string(), name.to_string()))
        }
        Some(_) => None,
    }
}

/// headless 配信セッションへ渡す prompt (受信→処理→ack の導線、 R3-c)
fn bg_dispatch_prompt(message_id: &str) -> String {
    format!(
        "📨 未処理の wire command があります。 mcp__vantage-point__wire_recv で受信し、 \
         内容に従って処理した後 mcp__vantage-point__wire_ack (message_id={}) してください。 \
         このセッションは VP が headless 配信 (R3-c channel D) で起動しています。",
        message_id
    )
}

/// `claude` headless dispatch の引数列を組む (純関数、 R3-c)
///
/// `-p` (print = 非対話) で起動するため CC 2.1 の Agent View dashboard は開かない。
/// cc_session_id があれば `--resume <id>` で当該 lane の会話文脈を継ぐ。
fn build_bg_args(cc_session_id: Option<&str>, prompt: &str) -> Vec<String> {
    let mut args = vec!["-p".to_string()];
    if let Some(id) = cc_session_id {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    args.push(prompt.to_string());
    args
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
            // R3-c: bg dispatch 台帳。 nudge と別管理 (offline 配信は別チャネル)
            let mut bg_ledger: HashMap<(String, String), NudgeRecord> = HashMap::new();
            tracing::info!(
                "wire delivery loop 起動 (tick={:?}, renudge_after={:?}, max={}, bg_max={})",
                TICK,
                RENUDGE_AFTER,
                MAX_NUDGES,
                MAX_BG_DISPATCHES
            );
            loop {
                // wake (command 着信) / tick / shutdown のいずれかで pulse
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(TICK) => {}
                }
                if let Err(e) = pulse(&store, &lane_registry, &mut ledger, &mut bg_ledger).await {
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
    bg_ledger: &mut HashMap<(String, String), NudgeRecord>,
) -> anyhow::Result<()> {
    let pending = store.unacked_commands().await?;

    // 台帳 GC: ack 済 (= pending から消えた) entry を落とす (nudge / bg 両方)
    let live: std::collections::HashSet<(String, String)> = pending
        .iter()
        .flat_map(|(m, agents)| agents.iter().map(move |a| (m.id.clone(), a.clone())))
        .collect();
    ledger.retain(|k, _| live.contains(k));
    bg_ledger.retain(|k, _| live.contains(k));

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
    if lanes.is_empty() {
        // lane なし = nudge 先がない (TheWorld 起動直後等)。 poll の claude fork を省く
        return Ok(());
    }
    // R3-a: CC activity を pulse ごとに 1 回 poll。 None = poll 不能 (claude 不在 /
    // timeout / schema 変動) で degraded fallback (R2-b 挙動) に落ちる。
    let activity = crate::process::cc_activity::poll_cc_activity().await;
    let now = Instant::now();

    for (msg, agents) in &pending {
        for agent in agents {
            // nudge 可能な address (conductor / performer) のみ。 他は対象外
            let Some(lane_display) = wire_agent_to_lane_display(agent) else {
                continue;
            };
            let target = pick_nudge_target(&lanes, &lane_display);
            // R3-a policy: lane の cwd で CC 状態を照合 (設計 policy table)
            let act_view = activity.as_ref().map(|s| match &target {
                Some((_, cwd, _)) => crate::process::cc_activity::state_for_cwd(s, cwd),
                None => None,
            });
            let Some((session, cwd, cc_session_id)) = target else {
                continue; // lane 不在 / Dead / PtySlotFallback = offline (pending 保持)
            };
            // 不変条件: target=Some が確定した後のみここに到達 = lane Running + Tmux
            // = lane_nudgeable は常に真 (target=None は直前の continue で排除済み)
            let key = (msg.id.clone(), agent.clone());
            match recipient_readiness(true, act_view) {
                Readiness::Ready => {}
                // busy: 待つ (台帳は進めない — idle 遷移を次 pulse で拾う)。
                Readiness::Busy => continue,
                // R3-c (channel D, scope a): lane は Running+Tmux だが interactive CC
                // session が落ちている (Some(None))。 send-keys は届かないので headless
                // `claude -p --resume` を detached 起動して wire を処理させる。
                Readiness::Offline => {
                    match decide_nudge(
                        bg_ledger.get(&key),
                        now,
                        BG_REDISPATCH_AFTER,
                        MAX_BG_DISPATCHES,
                    ) {
                        NudgeDecision::Send => {
                            if let Some((project, lane_label)) = lane_identity_from_agent(agent) {
                                let count = bg_ledger.get(&key).map(|r| r.count).unwrap_or(0);
                                let prompt = bg_dispatch_prompt(&msg.id);
                                let args = build_bg_args(cc_session_id.as_deref(), &prompt);
                                spawn_bg_dispatch(
                                    cwd.clone(),
                                    project,
                                    lane_label,
                                    session.clone(),
                                    args,
                                );
                                tracing::info!(
                                    "wire delivery: bg dispatch 起動 (msg={}, agent={}, cwd={}, resume={}, count={})",
                                    msg.id,
                                    agent,
                                    cwd,
                                    cc_session_id.is_some(),
                                    count + 1
                                );
                                bg_ledger.insert(
                                    key,
                                    NudgeRecord {
                                        count: count + 1,
                                        last_at: now,
                                    },
                                );
                            } else {
                                tracing::warn!(
                                    "wire delivery: bg dispatch 不能 (agent address 解析失敗: {})",
                                    agent
                                );
                            }
                        }
                        NudgeDecision::Wait | NudgeDecision::Exhausted => {}
                    }
                    continue;
                }
            }
            match decide_nudge(ledger.get(&key), now, RENUDGE_AFTER, MAX_NUDGES) {
                NudgeDecision::Send => {
                    let count = ledger.get(&key).map(|r| r.count).unwrap_or(0);
                    let text = nudge_text(&msg.id, count);
                    match send_keys_to_session(&session, &text).await {
                        Ok(()) => {
                            tracing::info!(
                                "wire delivery: nudge 送出 (msg={}, agent={}, session={}, count={}, activity={})",
                                msg.id,
                                agent,
                                session,
                                count + 1,
                                // degraded か精密 (poll 成功) かを後から判別できるように
                                if activity.is_some() {
                                    "precise"
                                } else {
                                    "degraded"
                                }
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

/// R3-c: headless `claude -p [--resume <id>] "<prompt>"` を detached 起動する。
///
/// lane と同じ identity env (`VP_CWD/VP_PROJECT/VP_LANE/VP_SESSION`) を渡し、
/// 当該 lane の wire address で wire_recv/wire_ack できるようにする (MCP server は
/// project/user settings から解決され、 interactive lane と同経路)。
///
/// 配信を block しないよう、 子プロセスは別 tokio task が await して reap する
/// (zombie 防止 + 終了 status を結果ログとして観測 = 簡易な結果収集)。
fn spawn_bg_dispatch(
    cwd: String,
    project: String,
    lane: String,
    session: String,
    args: Vec<String>,
) {
    tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args(&args)
            .current_dir(&cwd)
            .env("VP_CWD", &cwd)
            .env("VP_PROJECT", &project)
            .env("VP_LANE", &lane)
            .env("VP_SESSION", &session)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.status().await {
            Ok(st) => tracing::info!(
                "wire delivery: bg dispatch 完了 (project={}, lane={}, status={})",
                project,
                lane,
                st
            ),
            Err(e) => tracing::warn!(
                "wire delivery: bg dispatch 子プロセス失敗 (project={}, lane={}): {}",
                project,
                lane,
                e
            ),
        }
    });
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

    /// pick_tmux_session 用の test lane (vp/conductor)
    fn test_lane(state: LaneState, mode: TmuxMode) -> LaneInfo {
        use crate::process::lanes_state::{LaneAddress, LaneKind, TmuxLaneAddress};
        LaneInfo {
            address: LaneAddress::conductor("vp"),
            kind: LaneKind::Conductor,
            name: None,
            state,
            stand: "echoes".to_string(),
            created_at: "2026-06-11T00:00:00Z".to_string(),
            pid: None,
            cwd: String::new(),
            performer_status: None,
            cc_session_id: None,
            tmux: vec![TmuxLaneAddress {
                stand: "echoes".to_string(),
                session: "vp-vp-conductor-echoes".to_string(),
                mode,
            }],
        }
    }

    /// Running + Tmux mode の lane は (session 名, cwd, cc_session_id) が返る (nudge 可能)
    #[test]
    fn pick_running_tmux_lane_returns_session() {
        let lanes = vec![test_lane(LaneState::Running, TmuxMode::Tmux)];
        let (session, cwd, cc_session_id) =
            pick_nudge_target(&lanes, "vp/conductor").expect("nudge 可能");
        assert_eq!(session, "vp-vp-conductor-echoes");
        assert_eq!(cwd, "", "test_lane の cwd (CC activity 照合に使う)");
        assert_eq!(cc_session_id, None, "test_lane は cc_session_id 未設定");
        // 別 lane 宛は None (offline 扱い = pending 保持)
        assert_eq!(pick_nudge_target(&lanes, "other/conductor"), None);
    }

    /// PtySlotFallback (send-keys 不可) と Dead lane は nudge 対象外 = pending 保持
    #[test]
    fn pick_skips_fallback_mode_and_dead_lane() {
        let fallback = vec![test_lane(LaneState::Running, TmuxMode::PtySlotFallback)];
        assert_eq!(pick_nudge_target(&fallback, "vp/conductor"), None);

        let dead = vec![test_lane(LaneState::Dead, TmuxMode::Tmux)];
        assert_eq!(pick_nudge_target(&dead, "vp/conductor"), None);
    }

    /// R3-a: activity 供給ありの policy table — idle/waiting → Ready、busy → Busy、不在 → Offline
    #[test]
    fn readiness_with_activity_follows_policy_table() {
        use crate::process::cc_activity::CcState;
        assert_eq!(
            recipient_readiness(true, Some(Some(CcState::Idle))),
            Readiness::Ready
        );
        assert_eq!(
            recipient_readiness(true, Some(Some(CcState::Waiting))),
            Readiness::Ready
        );
        assert_eq!(
            recipient_readiness(true, Some(Some(CcState::Busy))),
            Readiness::Busy
        );
        // poll は成功したが当該 cwd に interactive session 不在 = offline (pending 保持)
        assert_eq!(recipient_readiness(true, Some(None)), Readiness::Offline);
    }

    /// R3-a: poll 不能 (None) は R2-b の degraded 挙動 (lane Running → nudge) に fallback
    #[test]
    fn readiness_degraded_falls_back_to_lane_running() {
        use crate::process::cc_activity::CcState;
        assert_eq!(recipient_readiness(true, None), Readiness::Ready);
        assert_eq!(recipient_readiness(false, None), Readiness::Offline);
        // activity があっても lane (tmux session) が無ければ nudge 不能
        assert_eq!(
            recipient_readiness(false, Some(Some(CcState::Idle))),
            Readiness::Offline
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

    /// R3-c: agent address → headless dispatch 用の (VP_PROJECT, VP_LANE)
    #[test]
    fn lane_identity_maps_project_and_lane() {
        assert_eq!(
            lane_identity_from_agent("agent@vp"),
            Some(("vp".to_string(), "conductor".to_string()))
        );
        assert_eq!(
            lane_identity_from_agent("agent@vp/w1"),
            Some(("vp".to_string(), "w1".to_string()))
        );
        // nudge 対象外 address は identity も None
        assert_eq!(lane_identity_from_agent("notify@vp"), None);
        assert_eq!(lane_identity_from_agent("agent@"), None);
        assert_eq!(lane_identity_from_agent("agent@vp/"), None);
    }

    /// R3-c: cc_session_id ありは --resume、 なしは fresh headless
    #[test]
    fn bg_args_include_resume_when_session_known() {
        let with_id = build_bg_args(Some("sess-123"), "do it");
        assert_eq!(with_id, vec!["-p", "--resume", "sess-123", "do it"]);
        // 必ず先頭は -p (print = 非対話、 Agent View dashboard 回避)
        assert_eq!(with_id[0], "-p");

        let without_id = build_bg_args(None, "do it");
        assert_eq!(without_id, vec!["-p", "do it"]);
    }

    /// R3-c: dispatch prompt は受信→ack の導線と message_id を含む
    #[test]
    fn bg_prompt_carries_message_id_and_flow() {
        let p = bg_dispatch_prompt("msg-42");
        assert!(p.contains("msg-42"));
        assert!(p.contains("wire_recv"));
        assert!(p.contains("wire_ack"));
    }
}
