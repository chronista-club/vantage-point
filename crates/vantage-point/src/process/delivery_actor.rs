//! wire delivery loop (R2-b、 設計 mem_1CbvcJj4ppU3QKH9d7xMpT) — TheWorld 常駐 actor
//!
//! 未 ack の command category message を受信者の tmux session に nudge (チャネル C) する。
//! TheWorld 上で store と同居 (in-process query、 決定 D1-c)。
//!
//! ## policy (R3-a 精密化 + R3-c channel D + doc 34 channel E)
//!
//! CC activity poll (`agents --json`) を供給に、 受信者 lane の状態で配信経路を分ける:
//! - **lane Running + console_mode=Chat → engine 直接注入 (チャネル E、 doc 34 §3)**。
//!   Chat lane は PtySlot を持たず (nudge 不能)、 engine は lazy spawn + turn 中 submit を
//!   自前 queue するため、 R3-a readiness とチャネル D は適用しない (常時 deliverable)。
//!   台帳・再掲示パラメータはチャネル C と共有。
//! - lane Running + CC idle/waiting (or poll 不能の degraded) → 即 nudge (チャネル C)
//! - lane Running + CC busy → 待つ (idle 遷移を次 pulse で拾う、 台帳は進めない)
//! - **lane Running + CC session 不在 (Some(None)) → headless bg dispatch (チャネル D、 R3-c)**
//!   前回 dispatch から BG_REDISPATCH_AFTER 経過 && 未 ack → 再 dispatch (MAX_BG_DISPATCHES まで)
//! - lane 不在 / Dead → pending 保持 (チャネル D の対象外 = cwd/session_id の足場が無い)
//! - 再掲示 (nudge): 前回 nudge から RENUDGE_AFTER 経過 && 未 ack → 再 nudge (MAX_NUDGES まで)
//!
//! ## 台帳は in-memory (nudge / bg dispatch を別管理)
//!
//! どちらも (message_id, agent) → (回数, 最終時刻)。 ack 済 (pending から消えた) entry は
//! 共通 GC で落とす。 TheWorld 再起動でリセットされ上限回が再付与されるが、 ack されれば
//! 止まるため許容 (table 化は必要になってから)。
//!
//! ## チャネル C の送出は SP control channel 経由 (tmux decoupling PR1)
//!
//! TheWorld は PtySlot を持たない（PtySlot は SP scope）ので、 lane を所有する SP の control
//! channel に `lane_nudge` を forward し、 SP 側で `LanePool::write_nudge` が PtySlot に直書きする。
//! 旧実装は lane registry の tmux session 名に daemon から直接 `send-keys` していた（cross-process
//! IPC namespace = tmux session への依存）が、 PR1 で SP-proxy に寄せて tmux 非依存化した。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capability::WiremsgStore;
use crate::capability::stand_service::{LayerScope, Service, SpawnableService};
// tmux decoupling PR1: nudge の forward 先解決に使う SP control channel registry（SSOT は daemon）。
use crate::daemon::server::ControlChannels;
// channel E (doc 34): console_mode が forward method (lane_nudge / echoes_nudge) を分ける。
use crate::lane::session_registry::SessionAct;
use crate::process::lanes_state::{LaneAddress, LaneInfo, LaneState};

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
/// nudge 可能なのは agent (lane 宛) のみ:
/// - `agent@<project>` → `<project>/conductor`（lane 名省略 = 開発起点）
/// - `agent@<project>/<name>` → `<project>/<name>`
/// - それ以外 (notify@ / lane-spawn@ / vp-cli 等) → None (nudge 対象外)
///
/// ⚠️ 戻り値は [`pick_nudge_target`] が `LaneAddress::to_string()` と**生の完全一致**で
/// 照合する（間に `parse_address` を挟まない唯一の経路）。したがって
/// [`LaneAddress`](crate::process::lanes_state::LaneAddress) の Display 形と
/// **byte-for-byte 同じ形を組み立てる責任がここにある**。
/// doc 44 P2 のフラット化ではこの直書きが取り残され、performer 宛 nudge が恒久的に
/// 不一致になる回帰を生んだ（conductor は形が変わらないため無症状で気付きにくい）。
pub(crate) fn wire_agent_to_lane_display(addr: &str) -> Option<String> {
    let rest = addr.strip_prefix("agent@")?;
    if rest.is_empty() {
        return None;
    }
    match rest.split_once('/') {
        None => Some(LaneAddress::conductor(rest).to_string()),
        Some((project, name)) if !project.is_empty() && !name.is_empty() => {
            Some(LaneAddress::new(project, name).to_string())
        }
        Some(_) => None,
    }
}

/// nudge 先の解決結果 (tmux decoupling PR1)
///
/// 旧 `(tmux session 名, cwd, cc_session_id)` タプルの置換。 tmux session の代わりに、
/// 所有 SP への forward に必要な `path_key`（= `control_channels` の key）と、 SP 側 `lane_nudge`
/// の宛先になる `lane_display`（`LaneAddress` の Display 形）を持つ。
#[derive(Debug, PartialEq)]
pub(crate) struct NudgeTarget {
    /// lane を所有する SP の path_key（`lane_registry` / `control_channels` の HashMap key）
    pub path_key: String,
    /// `LaneAddress` の Display 形（`lane_nudge` payload の `lane`）
    pub lane_display: String,
    /// R3-a の CC activity 照合（`agents --json` の cwd と突き合わせ）に使う lane cwd
    pub cwd: String,
    /// R3-c の `claude -p --resume <id>` headless 再開に使う（None なら fresh headless）
    pub cc_session_id: Option<String>,
    /// lane の console engine 種別（doc 33 の排他スロット）。forward method の分水嶺（doc 34 §3）。
    pub console_mode: SessionAct,
}

impl NudgeTarget {
    /// forward する SP control method（channel 判別の純関数、doc 34 §3）。
    ///
    /// Chat lane は PtySlot を持たないため `lane_nudge` は構造的に失敗する（`write_to_lane` が
    /// `Err("Lane has no PtySlot")`）— engine 直接注入の `echoes_nudge` へ route する。
    /// payload は両 method とも `{lane, text}` で共通。
    pub(crate) fn nudge_method(&self) -> &'static str {
        match self.console_mode {
            SessionAct::Chat => "echoes_nudge",
            SessionAct::Tui => "lane_nudge",
        }
    }
}

/// `(path_key, lane)` 一覧から nudge 先を引く (純関数、 tmux decoupling PR1)
///
/// Running な lane なら nudge 可能（PtySlot 直書きは tmux 起動可否に依存しないため、 旧
/// `TmuxMode::Tmux` gate は撤去）。 該当なし = offline 扱い (pending 保持)。 入力を
/// `(path_key, LaneInfo)` ペアにするのは、 forward 先 SP を `path_key` で特定するため
/// （`lane_registry` の HashMap key が path_key）。
pub(crate) fn pick_nudge_target(
    lanes: &[(String, LaneInfo)],
    lane_display: &str,
) -> Option<NudgeTarget> {
    lanes
        .iter()
        .find(|(_, l)| {
            l.address.to_string() == lane_display && matches!(l.state, LaneState::Running)
        })
        .map(|(path_key, l)| NudgeTarget {
            path_key: path_key.clone(),
            lane_display: l.address.to_string(),
            cwd: l.cwd.clone(),
            cc_session_id: l.cc_session_id.clone(),
            console_mode: l.console_mode,
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
    /// tmux decoupling PR1: SP control channel registry（nudge の forward 先解決に使う）
    control_channels: ControlChannels,
    /// wire_send (command) 時の即時 wake (AppState.delivery_notify と共有)
    wake: Arc<tokio::sync::Notify>,
}

impl DeliveryActor {
    pub fn new(
        store: WiremsgStore,
        lane_registry: Arc<RwLock<HashMap<String, Vec<LaneInfo>>>>,
        control_channels: ControlChannels,
        wake: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            store,
            lane_registry,
            control_channels,
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
                control_channels,
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
                if let Err(e) = pulse(
                    &store,
                    &lane_registry,
                    &control_channels,
                    &mut ledger,
                    &mut bg_ledger,
                )
                .await
                {
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
    control_channels: &ControlChannels,
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
    // tmux decoupling PR1: (path_key, lane) ペアで収集する。 path_key は所有 SP の control
    // channel key（forward 先の特定に要る）= lane_registry の HashMap key。
    let lanes: Vec<(String, LaneInfo)> = lane_registry
        .read()
        .await
        .iter()
        .flat_map(|(k, v)| v.iter().map(move |l| (k.clone(), l.clone())))
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
                Some(t) => crate::process::cc_activity::state_for_cwd(s, &t.cwd),
                None => None,
            });
            let Some(t) = target else {
                continue; // lane 不在 / Dead = offline (pending 保持)
            };
            // 不変条件: target=Some が確定した後のみここに到達 = lane Running
            // = lane_nudgeable は常に真 (target=None は直前の continue で排除済み)
            let key = (msg.id.clone(), agent.clone());
            // channel E (doc 34 §3): chat lane は R3-a readiness と channel D を通らない。
            // engine は lazy spawn（Offline が無い）かつ turn 実行中の submit を自前 queue する
            // （Busy が無い、doc 34 Step 0 spike ①実測）ため常時 deliverable — PTY / CC-activity
            // の意味論は Tui 専用。channel D を踏ませないこと自体が、同一 session 二重 --resume
            // （conductor）/ fresh headless 文脈喪失（performer）の構造的排除でもある（doc 34 §2-3）。
            if t.console_mode == SessionAct::Tui {
                match recipient_readiness(true, act_view) {
                    Readiness::Ready => {}
                    // busy: 待つ (台帳は進めない — idle 遷移を次 pulse で拾う)。
                    Readiness::Busy => continue,
                    // R3-c (channel D, scope a): lane は Running だが interactive CC
                    // session が落ちている (Some(None))。 nudge は届かないので headless
                    // `claude -p --resume` を detached 起動して wire を処理させる。
                    Readiness::Offline => {
                        match decide_nudge(
                            bg_ledger.get(&key),
                            now,
                            BG_REDISPATCH_AFTER,
                            MAX_BG_DISPATCHES,
                        ) {
                            NudgeDecision::Send => {
                                if let Some((project, lane_label)) = lane_identity_from_agent(agent)
                                {
                                    let count = bg_ledger.get(&key).map(|r| r.count).unwrap_or(0);
                                    let prompt = bg_dispatch_prompt(&msg.id);
                                    // transcript_exists pre-flight（doc 40 §5 / moody 指摘）:
                                    // headless `claude -p --resume` は `|| claude` fallback を
                                    // 持たないため、stale/phantom id を渡すと配信が黙って失敗
                                    // し続ける。他の resume 経路（slot spawn / chat spawn）と同じ
                                    // filter で fresh headless に倒す。配信時のみ実行 = 常時
                                    // tick 経路に fs walk を持ち込まない。
                                    let resume = t.cc_session_id.as_deref().filter(|id| {
                                        crate::lane::cc_session::transcript_exists(id)
                                    });
                                    let args = build_bg_args(resume, &prompt);
                                    spawn_bg_dispatch(t.cwd.clone(), project, lane_label, args);
                                    tracing::info!(
                                        "wire delivery: bg dispatch 起動 (msg={}, agent={}, cwd={}, resume={}, count={})",
                                        msg.id,
                                        agent,
                                        t.cwd,
                                        resume.is_some(),
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
            }
            match decide_nudge(ledger.get(&key), now, RENUDGE_AFTER, MAX_NUDGES) {
                NudgeDecision::Send => {
                    let count = ledger.get(&key).map(|r| r.count).unwrap_or(0);
                    let text = nudge_text(&msg.id, count);
                    // 所有 SP の control channel へ forward。method は console_mode で分岐
                    // （Tui = lane_nudge → PtySlot 直書き / Chat = echoes_nudge → engine 注入、
                    // doc 34 §3。payload は共通 {lane, text}）。
                    let resp = crate::daemon::server::forward_to_sp_control(
                        control_channels,
                        &t.path_key,
                        t.nudge_method(),
                        &serde_json::json!({ "lane": t.lane_display, "text": text }),
                    )
                    .await;
                    if resp.get("error").is_none() {
                        tracing::info!(
                            "wire delivery: nudge 送出 (msg={}, agent={}, lane={}, method={}, count={}, activity={})",
                            msg.id,
                            agent,
                            t.lane_display,
                            t.nudge_method(),
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
                    } else {
                        // best-effort: 送出失敗は台帳を進めない (次 pulse で再試行)
                        tracing::warn!(
                            "wire delivery: nudge 失敗 (msg={}, agent={}, lane={}): {}",
                            msg.id,
                            agent,
                            t.lane_display,
                            resp
                        );
                    }
                }
                NudgeDecision::Wait | NudgeDecision::Exhausted => {}
            }
        }
    }
    Ok(())
}

/// R3-c: headless `claude -p [--resume <id>] "<prompt>"` を detached 起動する。
///
/// lane と同じ identity env (`VP_PROJECT/VP_LANE` — doc 40 PR-3 で VP_CWD/VP_SESSION は退役) を渡し、
/// 当該 lane の wire address で wire_recv/wire_ack できるようにする (MCP server は
/// project/user settings から解決され、 interactive lane と同経路)。
///
/// 配信を block しないよう、 子プロセスは別 tokio task が await して reap する
/// (zombie 防止 + 終了 status を結果ログとして観測 = 簡易な結果収集)。
///
/// 注意 (TheWorld shutdown): in-flight の claude 子プロセスは runtime abort で孤立し得るが、
/// その場合 wire_ack が届かず pending が残るため、 次回起動の pulse で再 dispatch される
/// (idempotent に収束)。 重複処理は wire_ack の冪等性で吸収される。
fn spawn_bg_dispatch(cwd: String, project: String, lane: String, args: Vec<String>) {
    tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args(&args)
            .current_dir(&cwd)
            .env("VP_PROJECT", &project)
            .env("VP_LANE", &lane)
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

    /// pick_nudge_target 用の test lane (vp/conductor)。 tmux decoupling PR1 で nudge は
    /// tmux 状態を読まなくなったため tmux entry は空でよい。
    fn test_lane(state: LaneState) -> LaneInfo {
        use crate::process::lanes_state::LaneAddress;
        LaneInfo {
            console_mode: Default::default(),
            id: Default::default(),
            address: LaneAddress::conductor("vp"),
            state,
            stand: "echoes".to_string(),
            created_at: "2026-06-11T00:00:00Z".to_string(),
            pid: None,
            cwd: String::new(),
            performer_status: None,
            cc_session_id: None,
            engine_session_id: None,
            engine_stand: None,
            sessions: None,
            flow_state: None,
        }
    }

    /// registry の `(path_key, lane)` ペアに包む test helper（path_key = "/repo/vp"）
    fn registry(l: LaneInfo) -> Vec<(String, LaneInfo)> {
        vec![("/repo/vp".to_string(), l)]
    }

    /// Running な lane は NudgeTarget (path_key, lane_display, cwd, cc_session_id) が返る (nudge 可能)
    #[test]
    fn pick_running_lane_returns_target() {
        let lanes = registry(test_lane(LaneState::Running));
        let t = pick_nudge_target(&lanes, "vp/conductor").expect("nudge 可能");
        assert_eq!(
            t.path_key, "/repo/vp",
            "forward 先 SP の control channel key"
        );
        assert_eq!(t.lane_display, "vp/conductor", "lane_nudge の宛先");
        assert_eq!(t.cwd, "", "test_lane の cwd (CC activity 照合に使う)");
        assert_eq!(t.cc_session_id, None, "test_lane は cc_session_id 未設定");
        assert_eq!(t.console_mode, SessionAct::Tui, "default は Tui");
        // 別 lane 宛は None (offline 扱い = pending 保持)
        assert_eq!(pick_nudge_target(&lanes, "other/conductor"), None);
    }

    /// channel E (doc 34 §3): console_mode が forward method を分ける。
    /// Chat lane は PtySlot を持たないため lane_nudge は構造的に失敗する — echoes_nudge へ。
    #[test]
    fn nudge_method_follows_console_mode() {
        let mut lane = test_lane(LaneState::Running);
        lane.console_mode = SessionAct::Chat;
        let t = pick_nudge_target(&registry(lane), "vp/conductor")
            .expect("chat lane も Running なら target");
        assert_eq!(
            t.console_mode,
            SessionAct::Chat,
            "LaneInfo の console_mode が伝播する"
        );
        assert_eq!(t.nudge_method(), "echoes_nudge", "Chat は engine 直接注入");

        let t = pick_nudge_target(&registry(test_lane(LaneState::Running)), "vp/conductor")
            .expect("tui lane");
        assert_eq!(
            t.nudge_method(),
            "lane_nudge",
            "Tui は従来の PtySlot 直書き"
        );
    }

    /// tmux decoupling PR1: Running なら tmux 状態に関係なく nudge 可能 (旧 TmuxMode gate 撤去)、
    /// Dead lane のみ nudge 対象外 = pending 保持。
    #[test]
    fn pick_nudges_running_regardless_of_tmux_and_skips_dead() {
        // tmux entry 無し (旧 PtySlotFallback 相当) でも Running なら nudge 可能
        let running = registry(test_lane(LaneState::Running));
        assert!(pick_nudge_target(&running, "vp/conductor").is_some());

        let dead = registry(test_lane(LaneState::Dead));
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
        // doc 44 P2: フラット化で `<project>/<name>` になった
        assert_eq!(
            wire_agent_to_lane_display("agent@vp/w1").as_deref(),
            Some("vp/w1")
        );
        assert_eq!(wire_agent_to_lane_display("notify@vp"), None);
        assert_eq!(wire_agent_to_lane_display("vp-cli"), None);
        assert_eq!(wire_agent_to_lane_display("agent@"), None);
        assert_eq!(wire_agent_to_lane_display("agent@vp/"), None);
    }

    /// 回帰固定: **wire agent address → nudge 先 lane** の解決が performer でも成立すること。
    ///
    /// `wire_agent_to_lane_display` は lane address を**文字列で組み立てる**唯一の経路で、
    /// その結果を `pick_nudge_target` が `LaneAddress::to_string()` と**生の完全一致**で
    /// 照合する（間に `parse_address` を挟まない）。doc 44 P2 のフラット化で前者が旧形
    /// （`<project>/performer/<name>`）のまま取り残され、performer 宛 wire nudge が恒久的に
    /// 「lane 不在」となって永久リトライに落ちる回帰を出した。
    ///
    /// このテストが無かったのは、2 関数を**個別には**テストしていた一方で、
    /// `pick_nudge_target` 側の fixture が conductor lane 固定だったため
    /// （conductor は形が変わらないので回帰が無症状になる）。両者を繋いで検証する。
    #[test]
    fn agent_address_resolves_to_performer_nudge_target() {
        let performer = LaneInfo {
            address: LaneAddress::performer("vp", "w1"),
            pid: Some(4242),
            ..test_lane(LaneState::Running)
        };
        let lanes = vec![("/repos/vp".to_string(), performer)];

        let display =
            wire_agent_to_lane_display("agent@vp/w1").expect("agent アドレスは解決される");
        let target = pick_nudge_target(&lanes, &display)
            .expect("performer lane が nudge 先として見つかること（回帰: 旧形との不一致で None）");
        assert_eq!(target.path_key, "/repos/vp");
        assert_eq!(target.lane_display, "vp/w1");
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
