//! dev-flow primitives 共有 module
//!
//! ## 5-state FSM (= control surrender model、 2026-05-28 lead 説示)
//!
//! Lead × Wing の interaction 状態を **wing 単体の current FSM state** として derive する。
//! data sources:
//! - 最新 wire activity (= `latest_msg_for_agent(wing_addr)`) の direction (lead↔wing) と
//!   `body.kind` (= task / question / ack / decision / approve / modify / clarify / complete / request)
//! - wing_status (= dirty_count / last_commit)
//!
//! state はあくまで **observation**。 wire / wing_status を mutate しない。 metadata table
//! も持たない (derive できるものは store しない原則)。
//!
//! ## wire kind taxonomy (= 2026-05-28 lead 説示)
//!
//! | kind | direction | 意味 |
//! |---|---|---|
//! | `task` | lead → wing | 初手 handoff spec |
//! | `question` | wing → lead | 質問 / decision 依頼 |
//! | `ack` | wing → lead | 受領 / progress |
//! | `decision` | wing → lead | 自己判断表明 |
//! | `approve` / `modify` / `clarify` | lead → wing | reply |
//! | `complete` | wing → lead | 完了報告 |
//! | `request` | wing → lead | action 依頼 (= dogfood 等) |

use serde::{Deserialize, Serialize};

/// dev-flow の 5 state FSM。 wing 単体の current state を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowState {
    /// wire activity 一切なし (= 新規作成 wing、 まだ handoff されていない)
    Idle,
    /// lead が task を送って wing が作業中、 もしくは wing が ack 等で進めている (= control surrender 中)
    Working,
    /// wing から question 等 が出て lead reply 待ち (= HITL 介入要求中、 control surrender false)
    HitlPending,
    /// wing から complete が出た (= 完了、 control は wing に渡したまま but 作業は無い)
    Completed,
    /// 行き詰まり (= lead 指示後 dirty 残り commit 無し、 reply もなし)
    Stuck,
}

impl FlowState {
    /// emoji + 短文 label (= CLI table の MODE column 用)
    pub fn label(self) -> &'static str {
        match self {
            FlowState::Idle => "⏸ idle",
            FlowState::Working => "🤖 auto-running",
            FlowState::HitlPending => "🤝 hitl-pending",
            FlowState::Completed => "✅ completed",
            FlowState::Stuck => "⚠ stuck",
        }
    }
}

/// `derive_flow_state` の戻り値
///
/// `state` / `control_surrender` / `state_reason` / `last_state_transition_at` を一括返却。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowStateDerivation {
    pub state: FlowState,
    /// `true` = lead が control 手放して wing 自走中 (= Working or Completed かつ 最終 actor が wing/none)
    /// `false` = lead reply 待ち or interaction 進行中
    pub control_surrender: bool,
    /// なぜその state か (= human readable)。 e.g. `"lead task wmsg, wing not yet replied"`
    pub state_reason: String,
    /// 最終 state 遷移時刻 (epoch ms)。 wire activity が無い場合 `None`。
    /// 厳密な「transition 時刻」ではなく、 latest wire の created_at を proxy として返す
    /// (= 真の transition tracking は metadata 必要、 MVP は proxy)。
    pub last_state_transition_at: Option<i64>,
}

/// 最新 wire message が wing からか lead からかを判定するための入力
///
/// `latest_msg.from` が `wing_addr` と等しければ wing から、 そうでなければ lead から (= simplification:
/// wire kind taxonomy 上、 third-party from は dev-flow に出てこない前提)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgDirection {
    /// wing → lead
    FromWing,
    /// lead → wing (= wing が `to_addrs` 含む or sender が wing でない)
    FromLead,
}

/// latest_msg の最低限 metadata (= derive に必要な部分のみ)
///
/// `from` / `body.kind` / `created_at` を抜き出した薄い view。 `serde_json::Value` をそのまま
/// 入れない方が unit test しやすい (= pure data に閉じる)。
#[derive(Debug, Clone)]
pub struct LatestMsgView {
    pub from_addr: String,
    pub body_kind: Option<String>,
    pub created_at_ms: i64,
}

impl LatestMsgView {
    /// `WireMessage` JSON value から抽出 (caller が `wire_latest_msg` response から渡す)
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let from_addr = value.get("from")?.as_str()?.to_string();
        let body_kind = value
            .pointer("/body/kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let created_at_ms = value
            .get("created_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        Some(Self {
            from_addr,
            body_kind,
            created_at_ms,
        })
    }

    fn direction(&self, wing_addr: &str) -> MsgDirection {
        if self.from_addr == wing_addr {
            MsgDirection::FromWing
        } else {
            MsgDirection::FromLead
        }
    }
}

/// wing_status の derive に使う最低限 view
#[derive(Debug, Clone, Copy, Default)]
pub struct WingStatusView {
    pub dirty: bool,
    pub has_commit: bool,
}

impl WingStatusView {
    pub fn from_json(value: &serde_json::Value) -> Self {
        let dirty_count = value
            .get("dirty_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let last_commit = value
            .get("last_commit")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && *s != "-");
        Self {
            dirty: dirty_count > 0,
            has_commit: last_commit.is_some(),
        }
    }
}

/// FSM derive 本体 (= pure function、 unit test 可)
///
/// lead spec 準拠の 5 state derivation:
/// ```text
/// match (latest_msg, dirty, has_commit) {
///     (None, _, _) => Idle,
///     (Some(m), _, _) if m.from == lead && m.kind == "task" => Working,
///     (Some(m), _, _) if m.from == wing && m.kind == "question" => HitlPending,
///     (Some(m), _, _) if m.from == wing && m.kind == "complete" => Completed,
///     (Some(m), true, false) if m.from == lead => Stuck,
///     _ => Working,
/// }
/// ```
///
/// `wing_addr` = この wing の wire address (例: `agent@vantage-point/flow-tools`)。
pub fn derive_flow_state(
    latest: Option<&LatestMsgView>,
    wing_status: WingStatusView,
    wing_addr: &str,
) -> FlowStateDerivation {
    let Some(m) = latest else {
        return FlowStateDerivation {
            state: FlowState::Idle,
            control_surrender: true, // wire activity 無し = lead は手放したまま
            state_reason: "no wire activity yet".to_string(),
            last_state_transition_at: None,
        };
    };

    let dir = m.direction(wing_addr);
    let kind = m.body_kind.as_deref().unwrap_or("");

    // lead spec の cascade match
    let (state, reason) = match (dir, kind, wing_status.dirty, wing_status.has_commit) {
        // wing → lead の question = HITL 待ち (= control wing → lead に戻る)
        (MsgDirection::FromWing, "question", _, _) => (
            FlowState::HitlPending,
            "wing posted question, awaiting lead reply".to_string(),
        ),
        // wing → lead の complete = 完了
        (MsgDirection::FromWing, "complete", _, _) => {
            (FlowState::Completed, "wing reported complete".to_string())
        }
        // lead → wing の task = 作業中 (= 初手 handoff、 まだ wing 着手)
        (MsgDirection::FromLead, "task", _, _) => (
            FlowState::Working,
            "lead sent task, wing not yet replied".to_string(),
        ),
        // lead → wing 指示後 dirty 残り commit 無し = 行き詰まり
        (MsgDirection::FromLead, _, true, false) => (
            FlowState::Stuck,
            format!(
                "lead {} but wing has dirty changes and no commit",
                if kind.is_empty() { "msg" } else { kind }
            ),
        ),
        // wing → lead の ack / decision / request = まだ作業中 (= control surrender 中)
        (MsgDirection::FromWing, "ack" | "decision" | "request", _, _) => (
            FlowState::Working,
            format!("wing posted {kind}, working autonomously"),
        ),
        // lead → wing の approve / modify / clarify = 作業継続指示
        (MsgDirection::FromLead, "approve" | "modify" | "clarify", _, _) => (
            FlowState::Working,
            format!("lead replied {kind}, wing resumes"),
        ),
        // 上記いずれにも当たらない = Working を default (= lead spec の `_ => Working`)
        _ => (
            FlowState::Working,
            format!(
                "fallback (dir={:?}, kind={kind}, dirty={}, has_commit={})",
                dir, wing_status.dirty, wing_status.has_commit
            ),
        ),
    };

    // control_surrender derive: lead spec
    //   = state ∈ {Working, Completed} && (last_msg.from == wing || last_msg is None)
    // latest が None の path は上の早期 return で処理済なので、 ここでは latest is Some。
    let control_surrender =
        matches!(state, FlowState::Working | FlowState::Completed) && dir == MsgDirection::FromWing;

    FlowStateDerivation {
        state,
        control_surrender,
        state_reason: reason,
        last_state_transition_at: Some(m.created_at_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead_msg(kind: &str) -> LatestMsgView {
        LatestMsgView {
            from_addr: "agent@vp".to_string(),
            body_kind: Some(kind.to_string()),
            created_at_ms: 1_000,
        }
    }
    fn wing_msg(kind: &str) -> LatestMsgView {
        LatestMsgView {
            from_addr: "agent@vp/feat".to_string(),
            body_kind: Some(kind.to_string()),
            created_at_ms: 2_000,
        }
    }
    const WING_ADDR: &str = "agent@vp/feat";

    #[test]
    fn idle_when_no_wire_activity() {
        let d = derive_flow_state(None, WingStatusView::default(), WING_ADDR);
        assert_eq!(d.state, FlowState::Idle);
        assert!(d.control_surrender, "Idle は control 手放し済");
        assert!(d.last_state_transition_at.is_none());
    }

    #[test]
    fn working_when_lead_task_only() {
        let m = lead_msg("task");
        let d = derive_flow_state(Some(&m), WingStatusView::default(), WING_ADDR);
        assert_eq!(d.state, FlowState::Working);
        // lead → wing 指示直後 = control はまだ wing に渡っていない (= reply 必要)
        assert!(
            !d.control_surrender,
            "lead task 直後は wing 側 ack 待ち、 control 未 surrender"
        );
    }

    #[test]
    fn hitl_pending_when_wing_question() {
        let m = wing_msg("question");
        let d = derive_flow_state(Some(&m), WingStatusView::default(), WING_ADDR);
        assert_eq!(d.state, FlowState::HitlPending);
        assert!(!d.control_surrender, "HitlPending は lead 介入待ち");
    }

    #[test]
    fn completed_when_wing_complete() {
        let m = wing_msg("complete");
        let d = derive_flow_state(Some(&m), WingStatusView::default(), WING_ADDR);
        assert_eq!(d.state, FlowState::Completed);
        assert!(
            d.control_surrender,
            "Completed は wing が control 保持したまま"
        );
    }

    #[test]
    fn stuck_when_lead_msg_then_dirty_no_commit() {
        let m = lead_msg("modify");
        let s = WingStatusView {
            dirty: true,
            has_commit: false,
        };
        let d = derive_flow_state(Some(&m), s, WING_ADDR);
        assert_eq!(d.state, FlowState::Stuck);
    }

    #[test]
    fn working_when_wing_ack_or_decision_or_request() {
        for k in ["ack", "decision", "request"] {
            let m = wing_msg(k);
            let d = derive_flow_state(Some(&m), WingStatusView::default(), WING_ADDR);
            assert_eq!(d.state, FlowState::Working, "kind={k}");
            assert!(
                d.control_surrender,
                "wing 側 from = control surrender 中: {k}"
            );
        }
    }

    #[test]
    fn working_when_lead_approve_or_modify_or_clarify() {
        for k in ["approve", "modify", "clarify"] {
            let m = lead_msg(k);
            // dirty=false なので Stuck path を踏まない
            let d = derive_flow_state(Some(&m), WingStatusView::default(), WING_ADDR);
            assert_eq!(d.state, FlowState::Working, "kind={k}");
            // lead → wing reply 直後 = wing 着手前 → control 未 surrender
            assert!(!d.control_surrender, "lead reply 直後: {k}");
        }
    }

    #[test]
    fn last_state_transition_at_returns_msg_created_at() {
        let m = wing_msg("ack");
        let d = derive_flow_state(Some(&m), WingStatusView::default(), WING_ADDR);
        assert_eq!(d.last_state_transition_at, Some(2_000));
    }

    #[test]
    fn label_returns_emoji() {
        assert!(FlowState::Idle.label().contains("idle"));
        assert!(FlowState::Working.label().contains("auto-running"));
        assert!(FlowState::HitlPending.label().contains("hitl-pending"));
        assert!(FlowState::Completed.label().contains("completed"));
        assert!(FlowState::Stuck.label().contains("stuck"));
    }
}
