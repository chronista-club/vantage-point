//! ROTO の surface — daemon が持つ ROTO の「見え方」の正本（design 84 §LED）。
//!
//! mako（2026-11-05）「daemon に ROTO の Surface 的なエンティティがあって、そこと ROTO が繋がる
//! イメージ」。実機はこの entity に繋がる。entity は望む姿（LED）と実機に写した姿を持ち、食い違う
//! 分だけ MIDI を送る（reconcile）。送れる条件（自分の lease / VP の setup が選択中 / 書き込み中で
//! ない）が崩れたら写した姿を忘れ、戻った最初の tick で 8 本全部を送り直す — setup の切替や
//! 書き込みで実機の LED は初期化されうるため。
//!
//! 今載っているのは switch の LED だけ: pin の lane が needs-you なら `led_on`（赤）、それ以外は
//! `led_off`（pin 色を暗く）。名前と色は setup（`roto_setup`）が焼く。LED を CC で動かせることは
//! 実測で確かめた（`mem_1CgjSvYj7p3jAE7zKLhMgR`）。webview の FleetFeedback（LPD8 の pad）は
//! 使わない — ROTO は daemon presence の first-class client で、window が無くても光る。
//!
//! ## 層の分離
//! - data: [`RotoSurface`]（実機に写した姿）
//! - calculations: [`needs_you`] / [`leds_for_pins`] / [`ready`] / [`RotoSurface::reconcile`]
//! - actions: 送信は `midi_use` の lease loop（`DeviceRegistry::send_output`）
use crate::{
    conversation::progress::ProgressPhase, flow::FlowState, repo::lane::LaneInfo,
    roto_midi::switch_led, roto_setup::SETUP, roto_setup_sync::Status,
};
use std::collections::HashSet;

/// pin の数 = ROTO の switch 1〜8。
pub(crate) const PINS: usize = 8;

#[derive(Debug, Default)]
pub(crate) struct RotoSurface {
    /// 実機に写した最後の LED 姿。None = 実機が何を示しているか知らない（lease 外 / setup 切替 /
    /// 書き込み中 / 起動直後）
    projected: Option<[bool; PINS]>,
}

/// lane が「ユーザ本人の手」を待っているか。sidebar の needs-you（magenta diamond）と同じ述語の
/// daemon 側の形: sub の `flow_state == AwaitingUser`（未 ack の needs_user、wire 台帳）か、どの
/// session かが chat で承認 / 回答を求めている（`progress.phase == Requesting`）。
/// TUI の OSC 通知（vp-app の `awaiting_input`）は daemon に届かないので、ここでは拾えない。
pub(crate) fn needs_you(lane: &LaneInfo) -> bool {
    matches!(lane.flow_state, Some(FlowState::AwaitingUser))
        || lane.sessions.as_ref().is_some_and(|view| {
            view.sessions.iter().any(|session| {
                session
                    .progress
                    .as_ref()
                    .is_some_and(|p| p.phase == ProgressPhase::Requesting)
            })
        })
}

/// pin ごとの LED の望む姿。pin の address（`<repo>/lane/<name>`）と一致する lane が needs_you なら
/// 点灯。未登録の pin / 不在の lane は消灯。
pub(crate) fn leds_for_pins<'a>(
    addresses: &[Option<String>; PINS],
    lanes: impl IntoIterator<Item = &'a LaneInfo>,
) -> [bool; PINS] {
    let lit: HashSet<String> = lanes
        .into_iter()
        .filter(|lane| needs_you(lane))
        .map(|lane| lane.address.canonical())
        .collect();
    std::array::from_fn(|i| addresses[i].as_deref().is_some_and(|a| lit.contains(a)))
}

/// 実機へ送れる条件（design 84）: 自分の lease が active、VP の setup（内部 index 8）が選択中、
/// 書き込み中でない。
pub(crate) fn ready(status: &Status) -> bool {
    status.lease.is_some() && status.current_setup == Some(SETUP) && !status.configuring
}

impl RotoSurface {
    /// 望む LED 姿を実機に合わせるための MIDI messages。`ready` でなければ写した姿を忘れて空を返す。
    /// `ready` に戻った最初の呼び出しは 8 本全部、以後は変わった switch だけ。
    pub(crate) fn reconcile(&mut self, ready: bool, desired: [bool; PINS]) -> Vec<[u8; 3]> {
        if !ready {
            self.projected = None;
            return Vec::new();
        }
        let messages = match self.projected {
            None => (0..PINS).map(|i| switch_led(i, desired[i])).collect(),
            Some(shown) => (0..PINS)
                .filter(|&i| shown[i] != desired[i])
                .map(|i| switch_led(i, desired[i]))
                .collect(),
        };
        self.projected = Some(desired);
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::progress::ConversationProgressView;
    use crate::lane::session_registry::SessionMode;
    use crate::repo::lane::info::{LaneSessionView, LaneSessionsView};
    use crate::repo::lane::{LaneAddress, LaneState};

    fn lane(
        repo: &str,
        name: &str,
        flow: Option<FlowState>,
        phase: Option<ProgressPhase>,
    ) -> LaneInfo {
        LaneInfo {
            id: Default::default(),
            address: LaneAddress::new(repo, name),
            state: LaneState::Running,
            agent: "claude".into(),
            created_at: "2026-11-05T00:00:00Z".into(),
            pid: Some(1),
            cwd: "/tmp".into(),
            sub_status: None,
            branch: None,
            cc_session_id: None,
            engine_session_id: None,
            agent_name: None,
            sessions: phase.map(|phase| LaneSessionsView {
                root: 1,
                focused: 1,
                sessions: vec![LaneSessionView {
                    key: 1,
                    agent: "claude".into(),
                    mode: SessionMode::Gui,
                    conversation: None,
                    chat_capable: true,
                    image_capable: false,
                    model_choices: vec![],
                    effort_choices: vec![],
                    settings: None,
                    permission_choices: vec![],
                    last_activity_at: None,
                    last_response_at: None,
                    progress: Some(ConversationProgressView {
                        phase,
                        now: None,
                        error: false,
                    }),
                }],
            }),
            flow_state: flow,
        }
    }

    fn status(lease: Option<&str>, current_setup: Option<u8>, configuring: bool) -> Status {
        Status {
            device: "roto".into(),
            lease: lease.map(str::to_owned),
            sequence: 1,
            configuring,
            current_setup,
            error: None,
        }
    }

    #[test]
    fn needs_you_is_awaiting_user_or_a_requesting_session() {
        assert!(needs_you(&lane(
            "vp",
            "sub",
            Some(FlowState::AwaitingUser),
            None
        )));
        assert!(needs_you(&lane(
            "vp",
            "lead",
            None,
            Some(ProgressPhase::Requesting)
        )));
        assert!(!needs_you(&lane(
            "vp",
            "sub",
            Some(FlowState::HitlPending),
            Some(ProgressPhase::Working)
        )));
        assert!(!needs_you(&lane("vp", "lead", None, None)));
    }

    #[test]
    fn leds_follow_pin_addresses_and_ignore_other_lanes() {
        let addresses: [Option<String>; PINS] = [
            Some("creo-memories/lane/lead".into()),
            Some("vantage-point/lane/lead".into()),
            None,
            Some("vantage-point/lane/editor-hub".into()),
            None,
            None,
            None,
            Some("ghost/lane/lead".into()),
        ];
        let lanes = vec![
            lane(
                "creo-memories",
                "lead",
                None,
                Some(ProgressPhase::Requesting),
            ),
            lane("vantage-point", "lead", None, Some(ProgressPhase::Working)),
            lane(
                "vantage-point",
                "editor-hub",
                Some(FlowState::AwaitingUser),
                None,
            ),
            // pin に居ない lane は光らせる席が無い
            lane("other", "lead", Some(FlowState::AwaitingUser), None),
        ];
        assert_eq!(
            leds_for_pins(&addresses, &lanes),
            [true, false, false, true, false, false, false, false]
        );
    }

    #[test]
    fn ready_needs_our_lease_our_setup_and_no_write_in_flight() {
        assert!(ready(&status(Some("t"), Some(SETUP), false)));
        assert!(!ready(&status(None, Some(SETUP), false)));
        assert!(!ready(&status(Some("t"), Some(3), false)));
        assert!(!ready(&status(Some("t"), None, false)));
        assert!(!ready(&status(Some("t"), Some(SETUP), true)));
    }

    #[test]
    fn reconcile_sends_everything_first_then_only_changes() {
        let mut surface = RotoSurface::default();
        let mut desired = [false; PINS];
        desired[1] = true;
        let first = surface.reconcile(true, desired);
        assert_eq!(first.len(), 8, "最初は実機の姿を知らないので全部送る");
        assert_eq!(first[0], switch_led(0, false));
        assert_eq!(first[1], switch_led(1, true));
        assert!(
            surface.reconcile(true, desired).is_empty(),
            "同じ姿なら送らない"
        );
        desired[1] = false;
        desired[7] = true;
        assert_eq!(
            surface.reconcile(true, desired),
            vec![switch_led(1, false), switch_led(7, true)]
        );
    }

    #[test]
    fn losing_readiness_forgets_the_projection_and_resends_all_when_it_returns() {
        let mut surface = RotoSurface::default();
        let desired = [true; PINS];
        assert_eq!(surface.reconcile(true, desired).len(), 8);
        // setup 切替 / 書き込み中 / lease 喪失 — 送らず、写した姿も忘れる
        assert!(surface.reconcile(false, desired).is_empty());
        assert_eq!(
            surface.reconcile(true, desired).len(),
            8,
            "実機の LED は初期化されうるので全部送り直す"
        );
    }
}
