//! lane の値に「disk と runtime の事実」を写す層（棚卸し 項目 9-1c）。
//!
//! `address.rs` / `info.rs` は値だけを持ち、disk も engine catalog も読まない。その 2 つを繋ぐ
//! 投影がここ: session registry（disk）と `EngineKind`（engine の能力表）を読んで `LaneInfo` を
//! 完成させる。runtime の実体（`LanePool`）は持たない。
//!
//! ⚠️ **[`refresh_engine_session_id`] と [`apply_session_activity`] は対で呼ぶ**。前者は disk の
//! registry、後者は in-memory の実体（`LanePool::session_activity`）と**入力が別**なので、片方
//! だけだと「session 一覧は出るのに活動時刻が永遠に None」が供給点差で起きる（#683 地形）。
//! 現在の供給点は `lifecycle::build_lanes_snapshot` と `lifecycle::emit_lane_update` の 2 本で、
//! どちらも対で呼んでいる。供給点を足す時もこの対を崩さないこと。

use std::collections::HashMap;

use super::info::{LaneInfo, LaneSessionView, LaneSessionsView, quantize_activity_ms};
use crate::conversation::EngineKind;
use crate::lane::session_registry::{self, SessionKey};

/// doc 37: active engine の session id を state file から lazy read して埋める
/// （Conversation 共通ヘッダの session chip 用。表示専用の別契約 — [`LaneInfo::cc_session_id`] は
/// claude resume 用でここでは触らない）。engine 対応表は `EngineKind` が SSOT。
///
/// ⚠️ **lanes が daemon へ流れる供給点すべてで呼ぶこと**: 現在は `lifecycle::build_lanes_snapshot`
/// （ask 経路 = MCP list_lanes / lanes_list、および `publish_lanes` の 5s snapshot）と
/// `lifecycle::emit_lane_update` の 2 本（旧 uplink の agent_card / LaneDiff push は doc 44 P1
/// fold-in で経路ごと消滅、2026-09-09 時点で code 参照ゼロ）。供給が複数経路あるのは #683 と同じ
/// 地形で、1 箇所だけ enrich すると「ask には出るが registry（= vp-app）には出ない」に化ける
/// （2026-07-16 の tui session chip 不点灯の根因）。1 lane 2 file read
/// （session registry + session store、いずれも数百 byte）で軽微。
pub(crate) fn refresh_engine_session_id(info: &mut LaneInfo) {
    let lane_label = crate::repo::agent_spawner::lane_label(&info.address);
    // doc 40 §5: 会話 id の SSOT = session registry を 1 回 load し、
    // - `engine_session_id`（chip）= root session の conversation（doc 39 P1: chip は
    //   lane の人格を映す。gui のタブ表示は per-session 値 = #796 が担う）
    // - `cc_session_id`（channel D の claude 専用契約）= root が claude の時だけ同値
    //   （他 engine の id を混ぜない — 旧 field doc の不変条件を維持。旧実装の
    //   build_lanes_snapshot 個別 enrich は本 method に畳んだ = 供給点の実装差解消）
    // - `sessions` = registry snapshot 丸ごと（LaneInfo descriptor 完成、doc 40 §3）
    // registry file 不在（N=1 特殊ケース）は root=1 で従来と同一の読み先になる。
    // 旧 engine 別 store の 3-way dispatch は load 内の backfill bridge に移った。
    let reg = crate::lane::session_registry::load(&info.address.repo, lane_label, &info.agent);
    let root = reg.sessions.iter().find(|s| s.key == reg.root);
    info.engine_session_id = root.and_then(|s| s.conversation.clone());
    // doc 39 P4-C: chip prefix は root session の agent（= slot の engine）で決める。lane 固定の
    // `info.agent` は cross-engine root で slot と食い違うため、root entry の agent を別 field で運ぶ。
    info.agent_name = root.map(|s| s.agent.clone());
    info.cc_session_id = root
        .filter(|s| {
            matches!(
                crate::conversation::EngineKind::from_agent(&s.agent),
                Some(crate::conversation::EngineKind::Claude)
            )
        })
        .and_then(|s| s.conversation.clone());
    // doc 53 §11: roster は wire view で載せる（disk 型は載せない — 導出値 chat_capable を
    // 混ぜないため）。GUI の pane 一覧はこの 1 本から作られる。
    info.sessions = Some(sessions_view_from_registry(&reg));
}

/// roster に session ごとの最終活動時刻を焼く（`LanePool::session_activity` の適用側）。
///
/// ⚠️ [`refresh_engine_session_id`] と**対で**呼ぶこと（roster が daemon へ流れる全供給点 —
/// registry read の refresh と in-memory 実体の enrich は別入力なので、片方だけだと
/// 「session 一覧は出るのに活動時刻が永遠に None」が supply 点差で起きる（#683 地形）。
///
/// wire 値は [`super::info::ACTIVITY_WIRE_GRANULARITY_MS`] に切り下げて量子化する。`publish_lanes` は
/// snapshot の**指紋が変わった時だけ** vp-app を起こす（doc 44 §11.3）ので、生 ms を
/// 載せると活動中の lane が 5s tick を全 push 化してしまう — 分粒度なら最大 1 push/min。
pub(crate) fn apply_session_activity(info: &mut LaneInfo, activity: &HashMap<SessionKey, u64>) {
    if let Some(view) = info.sessions.as_mut() {
        for s in view.sessions.iter_mut() {
            s.last_activity_at = activity.get(&s.key).copied().map(quantize_activity_ms);
        }
    }
}

/// disk の registry から wire view を作る（導出値はここで 1 回だけ計算する）。
fn sessions_view_from_registry(reg: &session_registry::SessionRegistry) -> LaneSessionsView {
    LaneSessionsView {
        root: reg.root,
        focused: reg.focused,
        sessions: reg
            .sessions
            .iter()
            .map(|s| {
                // 導出値は engine 判定 1 回に畳む（能力表 = EngineKind が SSOT）。
                let kind = EngineKind::from_agent(&s.agent);
                LaneSessionView {
                    key: s.key,
                    agent: s.agent.clone(),
                    mode: s.mode,
                    conversation: s.conversation.clone(),
                    chat_capable: kind.is_some_and(EngineKind::chat_capable),
                    image_capable: kind.is_some_and(EngineKind::image_capable),
                    model: s.model.clone(),
                    model_choices: kind.map(EngineKind::model_choices).unwrap_or_default(),
                    permission_choices: kind
                        .map(EngineKind::permission_choices)
                        .unwrap_or_default(),
                    last_activity_at: None,
                }
            })
            .collect(),
    }
}
