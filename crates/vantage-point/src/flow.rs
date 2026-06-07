//! dev-flow primitives 共有 module
//!
//! ## 5-state FSM (= control surrender model、 2026-05-28 conductor 説示)
//!
//! Conductor × Performer の interaction 状態を **performer 単体の current FSM state** として derive する。
//! data sources:
//! - 最新 wire activity (= `latest_msg_for_agent(performer_addr)`) の direction (conductor↔performer) と
//!   `body.kind` (= task / question / ack / decision / approve / modify / clarify / complete / request)
//! - performer_status (= dirty_count / last_commit)
//!
//! state はあくまで **observation**。 wire / performer_status を mutate しない。 metadata table
//! も持たない (derive できるものは store しない原則)。
//!
//! ## wire kind taxonomy (= 2026-05-28 conductor 説示)
//!
//! | kind | direction | 意味 |
//! |---|---|---|
//! | `task` | conductor → performer | 初手 handoff spec |
//! | `question` | performer → conductor | 質問 / decision 依頼 |
//! | `ack` | performer → conductor | 受領 / progress |
//! | `decision` | performer → conductor | 自己判断表明 |
//! | `approve` / `modify` / `clarify` | conductor → performer | reply |
//! | `complete` | performer → conductor | 完了報告 |
//! | `request` | performer → conductor | action 依頼 (= dogfood 等) |

use serde::{Deserialize, Serialize};

/// dev-flow の 5 state FSM。 performer 単体の current state を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowState {
    /// wire activity 一切なし (= 新規作成 performer、 まだ handoff されていない)
    Idle,
    /// conductor が task を送って performer が作業中、 もしくは performer が ack 等で進めている (= control surrender 中)
    Working,
    /// performer から question 等 が出て conductor reply 待ち (= HITL 介入要求中、 control surrender false)
    HitlPending,
    /// performer から complete が出た (= 完了、 control は performer に渡したまま but 作業は無い)
    Completed,
    /// 行き詰まり (= conductor 指示後 dirty 残り commit 無し、 reply もなし)
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
    /// `true` = conductor が control 手放して performer 自走中 (= Working or Completed かつ 最終 actor が performer/none)
    /// `false` = conductor reply 待ち or interaction 進行中
    pub control_surrender: bool,
    /// なぜその state か (= human readable)。 e.g. `"conductor task wmsg, performer not yet replied"`
    pub state_reason: String,
    /// 最終 state 遷移時刻 (epoch ms)。 wire activity が無い場合 `None`。
    /// 厳密な「transition 時刻」ではなく、 latest wire の created_at を proxy として返す
    /// (= 真の transition tracking は metadata 必要、 MVP は proxy)。
    pub last_state_transition_at: Option<i64>,
}

/// 最新 wire message が performer からか conductor からかを判定するための入力
///
/// `latest_msg.from` が `performer_addr` と等しければ performer から、 そうでなければ conductor から (= simplification:
/// wire kind taxonomy 上、 third-party from は dev-flow に出てこない前提)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgDirection {
    /// performer → conductor
    FromPerformer,
    /// conductor → performer (= performer が `to_addrs` 含む or sender が performer でない)
    FromConductor,
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

    fn direction(&self, performer_addr: &str) -> MsgDirection {
        if self.from_addr == performer_addr {
            MsgDirection::FromPerformer
        } else {
            MsgDirection::FromConductor
        }
    }
}

/// performer_status の derive に使う最低限 view
#[derive(Debug, Clone, Copy, Default)]
pub struct PerformerStatusView {
    pub dirty: bool,
    pub has_commit: bool,
}

impl PerformerStatusView {
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
/// conductor spec 準拠の 5 state derivation:
/// ```text
/// match (latest_msg, dirty, has_commit) {
///     (None, _, _) => Idle,
///     (Some(m), _, _) if m.from == conductor && m.kind == "task" => Working,
///     (Some(m), _, _) if m.from == performer && m.kind == "question" => HitlPending,
///     (Some(m), _, _) if m.from == performer && m.kind == "complete" => Completed,
///     (Some(m), true, false) if m.from == conductor => Stuck,
///     _ => Working,
/// }
/// ```
///
/// `performer_addr` = この performer の wire address (例: `agent@vantage-point/flow-tools`)。
pub fn derive_flow_state(
    latest: Option<&LatestMsgView>,
    performer_status: PerformerStatusView,
    performer_addr: &str,
) -> FlowStateDerivation {
    let Some(m) = latest else {
        return FlowStateDerivation {
            state: FlowState::Idle,
            control_surrender: true, // wire activity 無し = conductor は手放したまま
            state_reason: "no wire activity yet".to_string(),
            last_state_transition_at: None,
        };
    };

    let dir = m.direction(performer_addr);
    let kind = m.body_kind.as_deref().unwrap_or("");

    // conductor spec の cascade match
    let (state, reason) = match (
        dir,
        kind,
        performer_status.dirty,
        performer_status.has_commit,
    ) {
        // performer → conductor の question = HITL 待ち (= control performer → conductor に戻る)
        (MsgDirection::FromPerformer, "question", _, _) => (
            FlowState::HitlPending,
            "performer posted question, awaiting conductor reply".to_string(),
        ),
        // performer → conductor の complete = 完了
        (MsgDirection::FromPerformer, "complete", _, _) => (
            FlowState::Completed,
            "performer reported complete".to_string(),
        ),
        // conductor → performer の task = 作業中 (= 初手 handoff、 まだ performer 着手)
        (MsgDirection::FromConductor, "task", _, _) => (
            FlowState::Working,
            "conductor sent task, performer not yet replied".to_string(),
        ),
        // conductor → performer 指示後 dirty 残り commit 無し = 行き詰まり
        (MsgDirection::FromConductor, _, true, false) => (
            FlowState::Stuck,
            format!(
                "conductor {} but performer has dirty changes and no commit",
                if kind.is_empty() { "msg" } else { kind }
            ),
        ),
        // performer → conductor の ack / decision / request = まだ作業中 (= control surrender 中)
        (MsgDirection::FromPerformer, "ack" | "decision" | "request", _, _) => (
            FlowState::Working,
            format!("performer posted {kind}, working autonomously"),
        ),
        // conductor → performer の approve / modify / clarify = 作業継続指示
        (MsgDirection::FromConductor, "approve" | "modify" | "clarify", _, _) => (
            FlowState::Working,
            format!("conductor replied {kind}, performer resumes"),
        ),
        // 上記いずれにも当たらない = Working を default (= conductor spec の `_ => Working`)
        _ => (
            FlowState::Working,
            format!(
                "fallback (dir={:?}, kind={kind}, dirty={}, has_commit={})",
                dir, performer_status.dirty, performer_status.has_commit
            ),
        ),
    };

    // control_surrender derive: conductor spec
    //   = state ∈ {Working, Completed} && (last_msg.from == performer || last_msg is None)
    // latest が None の path は上の早期 return で処理済なので、 ここでは latest is Some。
    let control_surrender = matches!(state, FlowState::Working | FlowState::Completed)
        && dir == MsgDirection::FromPerformer;

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

    fn conductor_msg(kind: &str) -> LatestMsgView {
        LatestMsgView {
            from_addr: "agent@vp".to_string(),
            body_kind: Some(kind.to_string()),
            created_at_ms: 1_000,
        }
    }
    fn performer_msg(kind: &str) -> LatestMsgView {
        LatestMsgView {
            from_addr: "agent@vp/feat".to_string(),
            body_kind: Some(kind.to_string()),
            created_at_ms: 2_000,
        }
    }
    const PERFORMER_ADDR: &str = "agent@vp/feat";

    #[test]
    fn idle_when_no_wire_activity() {
        let d = derive_flow_state(None, PerformerStatusView::default(), PERFORMER_ADDR);
        assert_eq!(d.state, FlowState::Idle);
        assert!(d.control_surrender, "Idle は control 手放し済");
        assert!(d.last_state_transition_at.is_none());
    }

    #[test]
    fn working_when_conductor_task_only() {
        let m = conductor_msg("task");
        let d = derive_flow_state(Some(&m), PerformerStatusView::default(), PERFORMER_ADDR);
        assert_eq!(d.state, FlowState::Working);
        // conductor → performer 指示直後 = control はまだ performer に渡っていない (= reply 必要)
        assert!(
            !d.control_surrender,
            "conductor task 直後は performer 側 ack 待ち、 control 未 surrender"
        );
    }

    #[test]
    fn hitl_pending_when_performer_question() {
        let m = performer_msg("question");
        let d = derive_flow_state(Some(&m), PerformerStatusView::default(), PERFORMER_ADDR);
        assert_eq!(d.state, FlowState::HitlPending);
        assert!(!d.control_surrender, "HitlPending は conductor 介入待ち");
    }

    #[test]
    fn completed_when_performer_complete() {
        let m = performer_msg("complete");
        let d = derive_flow_state(Some(&m), PerformerStatusView::default(), PERFORMER_ADDR);
        assert_eq!(d.state, FlowState::Completed);
        assert!(
            d.control_surrender,
            "Completed は performer が control 保持したまま"
        );
    }

    #[test]
    fn stuck_when_conductor_msg_then_dirty_no_commit() {
        let m = conductor_msg("modify");
        let s = PerformerStatusView {
            dirty: true,
            has_commit: false,
        };
        let d = derive_flow_state(Some(&m), s, PERFORMER_ADDR);
        assert_eq!(d.state, FlowState::Stuck);
    }

    #[test]
    fn working_when_performer_ack_or_decision_or_request() {
        for k in ["ack", "decision", "request"] {
            let m = performer_msg(k);
            let d = derive_flow_state(Some(&m), PerformerStatusView::default(), PERFORMER_ADDR);
            assert_eq!(d.state, FlowState::Working, "kind={k}");
            assert!(
                d.control_surrender,
                "performer 側 from = control surrender 中: {k}"
            );
        }
    }

    #[test]
    fn working_when_conductor_approve_or_modify_or_clarify() {
        for k in ["approve", "modify", "clarify"] {
            let m = conductor_msg(k);
            // dirty=false なので Stuck path を踏まない
            let d = derive_flow_state(Some(&m), PerformerStatusView::default(), PERFORMER_ADDR);
            assert_eq!(d.state, FlowState::Working, "kind={k}");
            // conductor → performer reply 直後 = performer 着手前 → control 未 surrender
            assert!(!d.control_surrender, "conductor reply 直後: {k}");
        }
    }

    #[test]
    fn last_state_transition_at_returns_msg_created_at() {
        let m = performer_msg("ack");
        let d = derive_flow_state(Some(&m), PerformerStatusView::default(), PERFORMER_ADDR);
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
