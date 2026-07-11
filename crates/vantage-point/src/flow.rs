//! dev-flow primitives 共有 module
//!
//! ## 6-state FSM (= control surrender model、 2026-05-28 conductor 説示 + 2026-07-11 awaiting_user)
//!
//! Conductor × Performer の interaction 状態を **performer 単体の current FSM state** として derive する。
//! data sources:
//! - 最新 wire activity (= `latest_msg_for_agent(performer_addr)`) の direction (conductor↔performer) と
//!   `body.kind` (= task / question / ack / decision / approve / modify / clarify / complete / request)
//! - performer_status (= dirty_count / last_commit)
//! - 未 ack の `needs_user` wire (= ack 台帳ベースの述語、 FSM 投影 task 2026-07-11)
//!
//! state はあくまで **observation**。 wire / performer_status を mutate しない。 metadata table
//! も持たない (derive できるものは store しない原則)。
//!
//! ## wire kind taxonomy (= 2026-05-28 conductor 説示、 needs_user は 2026-07-11 追加)
//!
//! | kind | direction | 意味 |
//! |---|---|---|
//! | `task` | conductor → performer | 初手 handoff spec |
//! | `question` | performer → conductor | 質問 / decision 依頼 (= conductor が捌ける相談) |
//! | `needs_user` | performer → conductor | **ユーザ本人**の意見が要る相談 (ack まで AwaitingUser) |
//! | `ack` | performer → conductor | 受領 / progress |
//! | `decision` | performer → conductor | 自己判断表明 |
//! | `approve` / `modify` / `clarify` | conductor → performer | reply |
//! | `complete` | performer → conductor | 完了報告 |
//! | `request` | performer → conductor | action 依頼 (= dogfood 等) |
//!
//! ## needs_user 規約 (= AwaitingUser の入力)
//!
//! - performer が「conductor では捌けない、 mako 本人の意見が要る」相談を投げる時は
//!   `body.kind = "needs_user"` + `body.category = "command"` で conductor 宛に送る
//!   (command なので ack されるまで delivery loop が re-nudge する)。
//! - 受信側 (conductor) は **ユーザの回答を performer に relay してから** `wire_ack` する。
//!   ack した瞬間に AwaitingUser が解消される (= ack 台帳が SSOT、 会話の続きでは消えない)。
//! - `derive_flow_state` はこの「未 ack needs_user の存在」を latest message の cascade より
//!   優先する — needs_user 送信後に performer が別 wire (ack / decision) を送っても、
//!   ユーザ待ちである事実は ack されるまで変わらないため。

use serde::{Deserialize, Serialize};

/// dev-flow の 6 state FSM。 performer 単体の current state を表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowState {
    /// wire activity 一切なし (= 新規作成 performer、 まだ handoff されていない)
    Idle,
    /// conductor が task を送って performer が作業中、 もしくは performer が ack 等で進めている (= control surrender 中)
    Working,
    /// performer から question 等 が出て conductor reply 待ち (= HITL 介入要求中、 control surrender false)
    HitlPending,
    /// performer から needs_user が出て **ユーザ本人**の回答待ち (= 未 ack needs_user が存在、
    /// HitlPending = conductor 待ちとは別軸。 sidebar の needs-you (magenta diamond) の接続先)
    AwaitingUser,
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
/// conductor spec 準拠の 6 state derivation:
/// ```text
/// if pending_needs_user => AwaitingUser   // 未 ack needs_user は cascade より優先 (ack 台帳が SSOT)
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
/// `pending_needs_user` = この performer 発の **未 ack** `needs_user` wire (最新 1 件) の view。
/// caller が ack 台帳 (`WiremsgStore::pending_needs_user`) から引いて渡す。 取得不能 (store
/// 未接続等) は `None` で degrade — AwaitingUser が出ないだけで他 state は通常 derive される。
pub fn derive_flow_state(
    latest: Option<&LatestMsgView>,
    performer_status: PerformerStatusView,
    performer_addr: &str,
    pending_needs_user: Option<&LatestMsgView>,
) -> FlowStateDerivation {
    // 未 ack needs_user の存在は latest cascade より優先する: needs_user 送信後に performer が
    // 追加の wire (ack / decision 等) を送って latest が変わっても、 ユーザ回答待ちの事実は
    // ack されるまで消えない (= ack 台帳が SSOT)。
    if let Some(nu) = pending_needs_user {
        return FlowStateDerivation {
            state: FlowState::AwaitingUser,
            control_surrender: false, // ユーザ介入待ち = 自走していない
            state_reason: "performer posted needs_user, awaiting the user's answer (unacked)"
                .to_string(),
            last_state_transition_at: Some(nu.created_at_ms),
        };
    }

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
        let d = derive_flow_state(None, PerformerStatusView::default(), PERFORMER_ADDR, None);
        assert_eq!(d.state, FlowState::Idle);
        assert!(d.control_surrender, "Idle は control 手放し済");
        assert!(d.last_state_transition_at.is_none());
    }

    #[test]
    fn working_when_conductor_task_only() {
        let m = conductor_msg("task");
        let d = derive_flow_state(
            Some(&m),
            PerformerStatusView::default(),
            PERFORMER_ADDR,
            None,
        );
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
        let d = derive_flow_state(
            Some(&m),
            PerformerStatusView::default(),
            PERFORMER_ADDR,
            None,
        );
        assert_eq!(d.state, FlowState::HitlPending);
        assert!(!d.control_surrender, "HitlPending は conductor 介入待ち");
    }

    #[test]
    fn completed_when_performer_complete() {
        let m = performer_msg("complete");
        let d = derive_flow_state(
            Some(&m),
            PerformerStatusView::default(),
            PERFORMER_ADDR,
            None,
        );
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
        let d = derive_flow_state(Some(&m), s, PERFORMER_ADDR, None);
        assert_eq!(d.state, FlowState::Stuck);
    }

    #[test]
    fn working_when_performer_ack_or_decision_or_request() {
        for k in ["ack", "decision", "request"] {
            let m = performer_msg(k);
            let d = derive_flow_state(
                Some(&m),
                PerformerStatusView::default(),
                PERFORMER_ADDR,
                None,
            );
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
            let d = derive_flow_state(
                Some(&m),
                PerformerStatusView::default(),
                PERFORMER_ADDR,
                None,
            );
            assert_eq!(d.state, FlowState::Working, "kind={k}");
            // conductor → performer reply 直後 = performer 着手前 → control 未 surrender
            assert!(!d.control_surrender, "conductor reply 直後: {k}");
        }
    }

    #[test]
    fn last_state_transition_at_returns_msg_created_at() {
        let m = performer_msg("ack");
        let d = derive_flow_state(
            Some(&m),
            PerformerStatusView::default(),
            PERFORMER_ADDR,
            None,
        );
        assert_eq!(d.last_state_transition_at, Some(2_000));
    }

    #[test]
    fn awaiting_user_when_pending_needs_user() {
        // latest 不在でも needs_user pending だけで AwaitingUser になる
        let nu = performer_msg("needs_user");
        let d = derive_flow_state(
            None,
            PerformerStatusView::default(),
            PERFORMER_ADDR,
            Some(&nu),
        );
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
        // needs_user 送信後に performer が別 wire (ack / complete) を送って latest が変わっても、
        // 未 ack の needs_user が残る限り AwaitingUser (= ack 台帳が SSOT)
        let nu = LatestMsgView {
            from_addr: PERFORMER_ADDR.to_string(),
            body_kind: Some("needs_user".to_string()),
            created_at_ms: 1_500,
        };
        for k in ["ack", "complete", "decision"] {
            let latest = performer_msg(k);
            let d = derive_flow_state(
                Some(&latest),
                PerformerStatusView::default(),
                PERFORMER_ADDR,
                Some(&nu),
            );
            assert_eq!(d.state, FlowState::AwaitingUser, "latest kind={k} でも優先");
            assert_eq!(d.last_state_transition_at, Some(1_500));
        }
    }

    #[test]
    fn needs_user_ack_clears_awaiting_user() {
        // ack 後は pending_needs_user が None になる (caller 側で消える) → 通常 cascade に戻る
        let latest = performer_msg("needs_user");
        let d = derive_flow_state(
            Some(&latest),
            PerformerStatusView::default(),
            PERFORMER_ADDR,
            None, // ack 済 = pending 無し
        );
        // needs_user は cascade の明示 arm を持たない → fallback Working (= 会話は performer 側)
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
