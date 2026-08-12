//! dev-flow primitives 共有 module
//!
//! ## 6-state FSM (= control surrender model、 2026-05-28 main 説示 + 2026-07-11 awaiting_user)
//!
//! Main × Sub の interaction 状態を **sub 単体の current FSM state** として derive する。
//! data sources:
//! - 最新 wire activity (= `latest_msg_for_agent(sub_addr)`) の direction (main↔sub) と
//!   `body.kind` (= task / question / ack / decision / approve / modify / clarify / complete / request)
//! - sub_status (= dirty_count / last_commit)
//! - 未 ack の `needs_user` wire (= ack 台帳ベースの述語、 FSM 投影 task 2026-07-11)
//!
//! state はあくまで **observation**。 wire / sub_status を mutate しない。 metadata table
//! も持たない (derive できるものは store しない原則)。
//!
//! ## wire kind taxonomy (= 2026-05-28 main 説示、 needs_user は 2026-07-11 追加)
//!
//! | kind | direction | 意味 |
//! |---|---|---|
//! | `task` | main → sub | 初手 handoff spec |
//! | `question` | sub → main | 質問 / decision 依頼 (= main が捌ける相談) |
//! | `needs_user` | sub → main | **ユーザ本人**の意見が要る相談 (ack まで AwaitingUser) |
//! | `ack` | sub → main | 受領 / progress |
//! | `decision` | sub → main | 自己判断表明 |
//! | `approve` / `modify` / `clarify` | main → sub | reply |
//! | `complete` | sub → main | 完了報告 |
//! | `request` | sub → main | action 依頼 (= dogfood 等) |
//!
//! ## needs_user 規約 (= AwaitingUser の入力)
//!
//! - sub が「main では捌けない、 mako 本人の意見が要る」相談を投げる時は
//!   `body.kind = "needs_user"` + `body.category = "command"` で main 宛に送る
//!   (command なので ack されるまで delivery loop が re-nudge する)。
//! - 受信側 (main) は **ユーザの回答を sub に relay してから** `wire_ack` する。
//!   ack した瞬間に AwaitingUser が解消される (= ack 台帳が SSOT、 会話の続きでは消えない)。
//! - `derive_flow_state` はこの「未 ack needs_user の存在」を latest message の cascade より
//!   優先する — needs_user 送信後に sub が別 wire (ack / decision) を送っても、
//!   ユーザ待ちである事実は ack されるまで変わらないため。

use serde::{Deserialize, Serialize};

/// dev-flow の 6 state FSM。 sub 単体の current state を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowState {
    /// wire activity 一切なし (= 新規作成 sub、 まだ handoff されていない)
    Idle,
    /// main が task を送って sub が作業中、 もしくは sub が ack 等で進めている (= control surrender 中)
    Working,
    /// sub から question 等 が出て main reply 待ち (= HITL 介入要求中、 control surrender false)
    HitlPending,
    /// sub から needs_user が出て **ユーザ本人**の回答待ち (= 未 ack needs_user が存在、
    /// HitlPending = main 待ちとは別軸。 sidebar の needs-you (magenta diamond) の接続先)
    AwaitingUser,
    /// sub から complete が出た (= 完了、 control は sub に渡したまま but 作業は無い)
    Completed,
    /// 行き詰まり (= main 指示後 dirty 残り commit 無し、 reply もなし)
    Stuck,
}

impl FlowState {
    /// emoji + 短文 label (= CLI table の MODE column 用)
    pub fn label(self) -> &'static str {
        match self {
            FlowState::Idle => "⏸ idle",
            FlowState::Working => "🤖 auto-running",
            FlowState::HitlPending => "🤝 hitl-pending",
            FlowState::AwaitingUser => "🙋 needs-you",
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
    /// `true` = main が control 手放して sub 自走中 (= Working or Completed かつ 最終 actor が sub/none)
    /// `false` = main reply 待ち or interaction 進行中
    pub control_surrender: bool,
    /// なぜその state か (= human readable)。 e.g. `"root task wmsg, sub not yet replied"`
    pub state_reason: String,
    /// 最終 state 遷移時刻 (epoch ms)。 wire activity が無い場合 `None`。
    /// 厳密な「transition 時刻」ではなく、 latest wire の created_at を proxy として返す
    /// (= 真の transition tracking は metadata 必要、 MVP は proxy)。
    pub last_state_transition_at: Option<i64>,
}

/// 最新 wire message が sub からか main からかを判定するための入力
///
/// `latest_msg.from` が `sub_addr` と等しければ sub から、 そうでなければ main から (= simplification:
/// wire kind taxonomy 上、 third-party from は dev-flow に出てこない前提)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgDirection {
    /// sub → main
    FromSub,
    /// main → sub (= sub が `to_addrs` 含む or sender が sub でない)
    FromMain,
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

    fn direction(&self, sub_addr: &str) -> MsgDirection {
        if self.from_addr == sub_addr {
            MsgDirection::FromSub
        } else {
            MsgDirection::FromMain
        }
    }
}

/// sub_status の derive に使う最低限 view
#[derive(Debug, Clone, Copy, Default)]
pub struct SubStatusView {
    pub dirty: bool,
    pub has_commit: bool,
}

impl SubStatusView {
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
/// main spec 準拠の 6 state derivation:
/// ```text
/// if pending_needs_user => AwaitingUser   // 未 ack needs_user は cascade より優先 (ack 台帳が SSOT)
/// match (latest_msg, dirty, has_commit) {
///     (None, _, _) => Idle,
///     (Some(m), _, _) if m.from == main && m.kind == "task" => Working,
///     (Some(m), _, _) if m.from == sub && m.kind == "question" => HitlPending,
///     (Some(m), _, _) if m.from == sub && m.kind == "complete" => Completed,
///     (Some(m), true, false) if m.from == main => Stuck,
///     _ => Working,
/// }
/// ```
///
/// `sub_addr` = この sub の wire address (例: `agent@vantage-point/flow-tools`)。
/// `pending_needs_user` = この sub 発の **未 ack** `needs_user` wire (最新 1 件) の view。
/// caller が ack 台帳 (`WiremsgStore::pending_needs_user`) から引いて渡す。 取得不能 (store
/// 未接続等) は `None` で degrade — AwaitingUser が出ないだけで他 state は通常 derive される。
pub fn derive_flow_state(
    latest: Option<&LatestMsgView>,
    sub_status: SubStatusView,
    sub_addr: &str,
    pending_needs_user: Option<&LatestMsgView>,
) -> FlowStateDerivation {
    // 未 ack needs_user の存在は latest cascade より優先する: needs_user 送信後に sub が
    // 追加の wire (ack / decision 等) を送って latest が変わっても、 ユーザ回答待ちの事実は
    // ack されるまで消えない (= ack 台帳が SSOT)。
    if let Some(nu) = pending_needs_user {
        return FlowStateDerivation {
            state: FlowState::AwaitingUser,
            control_surrender: false, // ユーザ介入待ち = 自走していない
            state_reason: "sub posted needs_user, awaiting the user's answer (unacked)".to_string(),
            last_state_transition_at: Some(nu.created_at_ms),
        };
    }

    let Some(m) = latest else {
        return FlowStateDerivation {
            state: FlowState::Idle,
            control_surrender: true, // wire activity 無し = main は手放したまま
            state_reason: "no wire activity yet".to_string(),
            last_state_transition_at: None,
        };
    };

    let dir = m.direction(sub_addr);
    let kind = m.body_kind.as_deref().unwrap_or("");

    // main spec の cascade match
    let (state, reason) = match (dir, kind, sub_status.dirty, sub_status.has_commit) {
        // sub → main の question = HITL 待ち (= control sub → main に戻る)
        (MsgDirection::FromSub, "question", _, _) => (
            FlowState::HitlPending,
            "sub posted question, awaiting root reply".to_string(),
        ),
        // sub → main の complete = 完了
        (MsgDirection::FromSub, "complete", _, _) => {
            (FlowState::Completed, "sub reported complete".to_string())
        }
        // main → sub の task = 作業中 (= 初手 handoff、 まだ sub 着手)
        (MsgDirection::FromMain, "task", _, _) => (
            FlowState::Working,
            "root sent task, sub not yet replied".to_string(),
        ),
        // main → sub 指示後 dirty 残り commit 無し = 行き詰まり
        (MsgDirection::FromMain, _, true, false) => (
            FlowState::Stuck,
            format!(
                "root {} but sub has dirty changes and no commit",
                if kind.is_empty() { "msg" } else { kind }
            ),
        ),
        // sub → main の ack / decision / request = まだ作業中 (= control surrender 中)
        (MsgDirection::FromSub, "ack" | "decision" | "request", _, _) => (
            FlowState::Working,
            format!("sub posted {kind}, working autonomously"),
        ),
        // main → sub の approve / modify / clarify = 作業継続指示
        (MsgDirection::FromMain, "approve" | "modify" | "clarify", _, _) => (
            FlowState::Working,
            format!("root replied {kind}, sub resumes"),
        ),
        // 上記いずれにも当たらない = Working を default (= main spec の `_ => Working`)
        _ => (
            FlowState::Working,
            format!(
                "fallback (dir={:?}, kind={kind}, dirty={}, has_commit={})",
                dir, sub_status.dirty, sub_status.has_commit
            ),
        ),
    };

    // control_surrender derive: main spec
    //   = state ∈ {Working, Completed} && (last_msg.from == sub || last_msg is None)
    // latest が None の path は上の早期 return で処理済なので、 ここでは latest is Some。
    let control_surrender =
        matches!(state, FlowState::Working | FlowState::Completed) && dir == MsgDirection::FromSub;

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

    fn main_msg(kind: &str) -> LatestMsgView {
        LatestMsgView {
            from_addr: "agent@vp".to_string(),
            body_kind: Some(kind.to_string()),
            created_at_ms: 1_000,
        }
    }
    fn sub_msg(kind: &str) -> LatestMsgView {
        LatestMsgView {
            from_addr: "agent@vp/feat".to_string(),
            body_kind: Some(kind.to_string()),
            created_at_ms: 2_000,
        }
    }
    const SUB_ADDR: &str = "agent@vp/feat";

    #[test]
    fn idle_when_no_wire_activity() {
        let d = derive_flow_state(None, SubStatusView::default(), SUB_ADDR, None);
        assert_eq!(d.state, FlowState::Idle);
        assert!(d.control_surrender, "Idle は control 手放し済");
        assert!(d.last_state_transition_at.is_none());
    }

    #[test]
    fn working_when_main_task_only() {
        let m = main_msg("task");
        let d = derive_flow_state(Some(&m), SubStatusView::default(), SUB_ADDR, None);
        assert_eq!(d.state, FlowState::Working);
        // main → sub 指示直後 = control はまだ sub に渡っていない (= reply 必要)
        assert!(
            !d.control_surrender,
            "root task 直後は sub 側 ack 待ち、 control 未 surrender"
        );
    }

    #[test]
    fn hitl_pending_when_sub_question() {
        let m = sub_msg("question");
        let d = derive_flow_state(Some(&m), SubStatusView::default(), SUB_ADDR, None);
        assert_eq!(d.state, FlowState::HitlPending);
        assert!(!d.control_surrender, "HitlPending は root 介入待ち");
    }

    #[test]
    fn completed_when_sub_complete() {
        let m = sub_msg("complete");
        let d = derive_flow_state(Some(&m), SubStatusView::default(), SUB_ADDR, None);
        assert_eq!(d.state, FlowState::Completed);
        assert!(
            d.control_surrender,
            "Completed は sub が control 保持したまま"
        );
    }

    #[test]
    fn stuck_when_main_msg_then_dirty_no_commit() {
        let m = main_msg("modify");
        let s = SubStatusView {
            dirty: true,
            has_commit: false,
        };
        let d = derive_flow_state(Some(&m), s, SUB_ADDR, None);
        assert_eq!(d.state, FlowState::Stuck);
    }

    #[test]
    fn working_when_sub_ack_or_decision_or_request() {
        for k in ["ack", "decision", "request"] {
            let m = sub_msg(k);
            let d = derive_flow_state(Some(&m), SubStatusView::default(), SUB_ADDR, None);
            assert_eq!(d.state, FlowState::Working, "kind={k}");
            assert!(
                d.control_surrender,
                "sub 側 from = control surrender 中: {k}"
            );
        }
    }

    #[test]
    fn working_when_main_approve_or_modify_or_clarify() {
        for k in ["approve", "modify", "clarify"] {
            let m = main_msg(k);
            // dirty=false なので Stuck path を踏まない
            let d = derive_flow_state(Some(&m), SubStatusView::default(), SUB_ADDR, None);
            assert_eq!(d.state, FlowState::Working, "kind={k}");
            // main → sub reply 直後 = sub 着手前 → control 未 surrender
            assert!(!d.control_surrender, "root reply 直後: {k}");
        }
    }

    #[test]
    fn last_state_transition_at_returns_msg_created_at() {
        let m = sub_msg("ack");
        let d = derive_flow_state(Some(&m), SubStatusView::default(), SUB_ADDR, None);
        assert_eq!(d.last_state_transition_at, Some(2_000));
    }

    #[test]
    fn awaiting_user_when_pending_needs_user() {
        // latest 不在でも needs_user pending だけで AwaitingUser になる
        let nu = sub_msg("needs_user");
        let d = derive_flow_state(None, SubStatusView::default(), SUB_ADDR, Some(&nu));
        assert_eq!(d.state, FlowState::AwaitingUser);
        assert!(!d.control_surrender, "AwaitingUser はユーザ介入待ち");
        assert_eq!(
            d.last_state_transition_at,
            Some(2_000),
            "遷移時刻は needs_user wire の created_at"
        );
    }

    #[test]
    fn awaiting_user_takes_priority_over_latest_cascade() {
        // needs_user 送信後に sub が別 wire (ack / complete) を送って latest が変わっても、
        // 未 ack の needs_user が残る限り AwaitingUser (= ack 台帳が SSOT)
        let nu = LatestMsgView {
            from_addr: SUB_ADDR.to_string(),
            body_kind: Some("needs_user".to_string()),
            created_at_ms: 1_500,
        };
        for k in ["ack", "complete", "decision"] {
            let latest = sub_msg(k);
            let d = derive_flow_state(Some(&latest), SubStatusView::default(), SUB_ADDR, Some(&nu));
            assert_eq!(d.state, FlowState::AwaitingUser, "latest kind={k} でも優先");
            assert_eq!(d.last_state_transition_at, Some(1_500));
        }
    }

    #[test]
    fn needs_user_ack_clears_awaiting_user() {
        // ack 後は pending_needs_user が None になる (caller 側で消える) → 通常 cascade に戻る
        let latest = sub_msg("needs_user");
        let d = derive_flow_state(
            Some(&latest),
            SubStatusView::default(),
            SUB_ADDR,
            None, // ack 済 = pending 無し
        );
        // needs_user は cascade の明示 arm を持たない → fallback Working (= 会話は sub 側)
        assert_eq!(d.state, FlowState::Working);
    }

    #[test]
    fn label_returns_emoji() {
        assert!(FlowState::Idle.label().contains("idle"));
        assert!(FlowState::Working.label().contains("auto-running"));
        assert!(FlowState::HitlPending.label().contains("hitl-pending"));
        assert!(FlowState::AwaitingUser.label().contains("needs-you"));
        assert!(FlowState::Completed.label().contains("completed"));
        assert!(FlowState::Stuck.label().contains("stuck"));
    }

    #[test]
    fn flow_state_serde_is_snake_case() {
        // LaneInfo.flow_state 投影 (wire) と client (TS) の契約: snake_case 文字列
        assert_eq!(
            serde_json::to_value(FlowState::AwaitingUser).unwrap(),
            serde_json::json!("awaiting_user")
        );
        assert_eq!(
            serde_json::to_value(FlowState::HitlPending).unwrap(),
            serde_json::json!("hitl_pending")
        );
    }
}
