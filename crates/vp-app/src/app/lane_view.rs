//! lane の見え方の**調整役**（doc 60 §6 6-2 / Codex ①「lane_view は app/ に」）。
//!
//! `activate_lane` は state 更新・永続化・sidebar / main の描画・dead lane の再起動を束ねる。
//! `webview/` の投影を呼ぶので `lane/` ではなく UI thread の世界（`app/`）に住む。
//! helper は **field を明示引数で受ける**（`&mut UiState` を取らない — 呼び手の field borrow と衝突するため）。
//!
//! 旧 `app/mod.rs` の lane 系 helper と test module（6-2 PR-2、2026-09-08。本文は順序保持で一致、
//! 差分は可視性 `pub(super)` のみ）。

use tao::event_loop::EventLoopProxy;
use wry::WebView;

use super::persist::Persist;
use crate::daemon::conn::{SharedDaemonConn, daemon_repo_request};
use crate::events::AppEvent;
use crate::lane::conversation::{LaneConversation, spawn_conversation_session};
use crate::pane::SidebarState;
use crate::webview::main_area::{self, ActivePaneInfo};
use crate::webview::push_main;
use crate::webview::push_sidebar::push_sidebar_state;

/// doc 50 §4.6 A6: 「どの session に xterm / 購読が要るか」の導出（実機で踏んだ穴の固定）。
///
/// 旧実装は lane 単位の `console_mode` / `pid` で gate していた。あれは「term になれるのは
/// root だけ」という制約下では正しかったが、A6 で非 root も term になれるので **root の mode で
/// lane 全体を切ると、非 root の住人が丸ごと落ちる**（2026-07-25 実機 dogfood で観測 —
/// pane は並ぶのに中身が来ない）。導出は registry の mode から行う、をここで固定する。
#[cfg(test)]
mod session_derivation_tests {
    use super::{
        forget_roster_push, remember_roster_push, roster_push_needed, session_list_payload,
        term_sessions_of,
    };
    use crate::daemon_wire::LaneInfo;

    /// doc 53 §11: snapshot の roster が **webview 契約の形**に写ること。
    ///
    /// 旧実装ではこの payload は `conversation_session_list` の ask 結果そのものだった。供給を
    /// snapshot に 1 本化した今、変換はこの純関数 1 箇所 — 形がずれると tab strip / pane grid /
    /// 名札が同時に壊れるので、**client が読む field を名指しで**固定する。
    ///
    /// （旧テスト `dropped_fetch_is_replayed_once_repo_resolves` が守っていた性質
    /// 「boot 窓で roster を取りこぼさない」は、供給が retained snapshot になったことで
    /// 構造的に消滅した — 取りこぼす対象の要求が存在しない。§8.6 の規律に従い、
    /// 代わりに新しい供給の契約をここで固定する。）
    #[test]
    fn session_list_payload_matches_webview_contract() {
        let lane = lane_with(
            16,
            serde_json::json!([
                {"key": 16, "agent": "claude", "mode": "gui",
                 "conversation": "conv-abc", "chat_capable": true, "image_capable": true},
                {"key": 24, "agent": "shell", "mode": "tui", "chat_capable": false},
            ]),
        );
        let sessions = lane.sessions.as_ref().expect("roster");
        let payload = session_list_payload("vp/root", sessions);

        assert_eq!(payload["lane"], "vp/root");
        assert_eq!(payload["focused"], 16, "focused は top-level にも出す");
        let entries = payload["sessions"].as_array().expect("sessions array");
        assert_eq!(entries.len(), 2);

        // root / focused は **entry ごとの bool** に展開する（webview はこの形で読む）。
        assert_eq!(entries[0]["key"], 16);
        assert_eq!(entries[0]["root"], true);
        assert_eq!(entries[0]["focused"], true);
        assert_eq!(entries[0]["mode"], "gui");
        assert_eq!(
            entries[0]["engine_session_id"], "conv-abc",
            "会話 id は engine_session_id という名で運ぶ（webview 契約）"
        );
        assert_eq!(
            entries[0]["chat_capable"], true,
            "能力表は server が SSOT — client に engine 名の分岐を作らない"
        );
        // ⚠️ 回帰固定（2026-08-30）: entry は手書きの写像なので、wire に足しただけでは
        // ここに現れない。`image_capable` を落として「server は true なのに client は false」
        // で貼り付け UI が出なかった実害があった。**能力 field は 1 つずつ assert する**。
        assert_eq!(
            entries[0]["image_capable"], true,
            "画像投入の能力表明も webview へ運ぶ（落とすと貼り付け UI が出ない）"
        );

        assert_eq!(entries[1]["key"], 24);
        assert_eq!(entries[1]["root"], false);
        assert_eq!(entries[1]["focused"], false);
        assert_eq!(entries[1]["mode"], "tui");
        assert!(
            entries[1]["engine_session_id"].is_null(),
            "会話 id 未発番（Draft / shell）は null"
        );
        assert_eq!(entries[1]["chat_capable"], false);
    }

    /// doc 53 §11: 定期 snapshot で roster を撃ち直さない指紋 gate の意味論。
    ///
    /// LanesLoaded は高頻度 event なので、値が同じなら push しない（毎回撃つと webview が
    /// roster を作り直して pane が無用に再配置される）。**replay 経路（`WebviewReady`）は
    /// この gate を通さない** — boot 窓で落ちた push を取り戻す唯一の機会で、そこで
    /// 「変化なし」と判断すると roster が永久に空のままになる（team-b 指摘 2026-07-25）。
    /// 呼び分けは実装側の構造（gate を呼ぶ / 呼ばない）で表す。
    #[test]
    fn roster_push_gate_fires_on_change_only() {
        let lane = lane_with(
            16,
            serde_json::json!([{"key": 16, "agent": "claude", "mode": "gui"}]),
        );
        let payload = session_list_payload("vp/root", lane.sessions.as_ref().expect("roster"));
        let mut last: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        assert!(
            roster_push_needed(&last, "vp/root", &payload),
            "初回は push（指紋が無い）"
        );
        remember_roster_push(&mut last, "vp/root", &payload);
        assert!(
            !roster_push_needed(&last, "vp/root", &payload),
            "変化が無ければ push しない"
        );

        // session が 1 本増えた = roster の変化 → 撃つ。
        let grown = lane_with(
            16,
            serde_json::json!([
                {"key": 16, "agent": "claude", "mode": "gui"},
                {"key": 24, "agent": "shell", "mode": "tui"},
            ]),
        );
        let grown_payload =
            session_list_payload("vp/root", grown.sessions.as_ref().expect("roster"));
        assert!(
            roster_push_needed(&last, "vp/root", &grown_payload),
            "roster が変われば push する"
        );

        // lane が消えたら指紋も落とす（同名再作成で「変化なし」と誤判定しないため）。
        forget_roster_push(&mut last, "vp/root");
        assert!(
            roster_push_needed(&last, "vp/root", &payload),
            "lane 削除後は初回扱いに戻る"
        );
    }

    /// registry snapshot 付きの最小 LaneInfo（wire と同じ JSON 形で組む）。
    /// doc 53 R1: mode は sessions（registry snapshot）だけが運ぶ — 旧 console_mode field は退役。
    fn lane_with(root: u32, sessions: serde_json::Value) -> LaneInfo {
        serde_json::from_value(serde_json::json!({
            "address": {"kind": "root", "repo": "vp"},
            "sessions": {"root": root, "focused": root, "sessions": sessions},
        }))
        .expect("LaneInfo deserialize")
    }

    fn s(key: u32, mode: &str) -> serde_json::Value {
        serde_json::json!({"key": key, "agent": "claude", "mode": mode})
    }

    #[test]
    fn term_sessions_picks_every_tui_session_with_root_flag() {
        // root=16 が chat、非 root=19 が tui（2026-07-25 実機で踏んだ構成そのもの）。
        let lane = lane_with(16, serde_json::json!([s(16, "gui"), s(19, "tui")]));
        assert_eq!(
            term_sessions_of(&lane),
            vec![(19, false)],
            "root が chat でも非 root の term は拾う（lane ごと skip しない）"
        );

        // root も tui なら root フラグ付きで拾う。
        let lane = lane_with(16, serde_json::json!([s(16, "tui"), s(19, "tui")]));
        assert_eq!(term_sessions_of(&lane), vec![(16, true), (19, false)]);

        // 全部 chat なら term はゼロ = lane に xterm は要らない。
        let lane = lane_with(16, serde_json::json!([s(16, "gui")]));
        assert!(term_sessions_of(&lane).is_empty());
    }

    #[test]
    fn term_sessions_falls_back_to_root_for_legacy_wire() {
        // registry snapshot が無い旧 SP からの wire は root 1 枚に畳む（従来挙動）。
        let lane: LaneInfo = serde_json::from_value(serde_json::json!({
            "address": {"kind": "root", "repo": "vp"},
        }))
        .expect("LaneInfo deserialize");
        assert_eq!(term_sessions_of(&lane), vec![(1, true)]);
    }
}

/// lane の term session（tui = mode "tui"）の (session, is_root) 一覧を返す。
///
/// doc 50 §4.6 A6: xterm は (lane, session) ごとなので、boot / lane 選択の経路は
/// 「この lane にどの term pane が要るか」をここで解決する。registry snapshot（`sessions`）が
/// 無い旧 SP からの wire は root=1 の 1 枚に畳む（従来挙動 = lane に xterm 1 枚）。
pub(super) fn term_sessions_of(lane: &crate::daemon_wire::LaneInfo) -> Vec<(u32, bool)> {
    match &lane.sessions {
        Some(reg) if !reg.sessions.is_empty() => reg
            .sessions
            .iter()
            .filter(|s| s.mode != "gui")
            .map(|s| (s.key, s.key == reg.root))
            .collect(),
        // registry 不在（boot 窓の placeholder / N=1 特殊ケース）: root 1 枚（tui）に畳む。
        _ => vec![(1, true)],
    }
}

/// roster を push すべきか（前回渡した値と違うか）。純関数 = テスト可能。
///
/// doc 53 §11: LanesLoaded は定期 snapshot でも走る高頻度 event なので、**変化した lane だけ**
/// 撃つ（毎回撃つと webview が roster を作り直して pane が無用に再配置される）。
///
/// ⚠️ **replay 経路（`WebviewReady`）はこの gate を通さない** — bundle ロード前の
/// `evaluate_script` は無言 no-op になるのに指紋だけ残るため、gate を共有すると boot 窓で
/// 落ちた 1 回目を永久に取り戻せない（team-b 指摘 2026-07-25）。
pub(super) fn roster_push_needed(
    last: &std::collections::HashMap<String, String>,
    addr: &str,
    payload: &serde_json::Value,
) -> bool {
    last.get(addr) != Some(&payload.to_string())
}

/// [`roster_push_needed`] の対 — 撃った値を覚える。
pub(super) fn remember_roster_push(
    last: &mut std::collections::HashMap<String, String>,
    addr: &str,
    payload: &serde_json::Value,
) {
    last.insert(addr.to_string(), payload.to_string());
}

/// 消えた lane の指紋を落とす（同名再作成で「変化なし」と誤判定しないため）。
pub(super) fn forget_roster_push(last: &mut std::collections::HashMap<String, String>, addr: &str) {
    last.remove(addr);
}

/// roster を webview へ渡す（push envelope `console:session_list` → `vp:conversation-sessions`）。
///
/// doc 53 §11: 呼び手は LanesLoaded の 1 箇所だけ（旧実装は動詞ごとの再取得 7 箇所から
/// 撃っていた — 供給路が 2 本ある構造そのものだった）。
pub(super) fn push_session_list(webview: &wry::WebView, lane: &str, payload: &serde_json::Value) {
    push_main::console_session_list(webview, lane, payload.clone());
}

/// doc 53 §11: lane snapshot の roster を webview の session 一覧 payload に写す（純関数）。
///
/// **roster の供給はこの 1 本**。旧実装は `conversation_session_list` の ask 結果を流していたが、
/// その fetch は「lane を開いた時 / GUI 自身が動詞を撃った後 / boot 窓の再送」でしか走らず、
/// **CLI・MCP 由来の session 変化が pane grid に出なかった**（doc 53 §11.1）。snapshot は
/// server が動詞の末尾で push する（`emit_lane_update`）ので、誰が起こした変化でも届く。
///
/// payload の形は webview 契約（`console.ts` の `ConversationSessionListPayload`）そのまま — 供給路を差し替える
/// だけで消費側（tab strip / pane grid / 名札）は無改造。`root` / `focused` は entry の
/// bool に展開する（webview は entry ごとの flag で読む）。
/// ⚠️ **entry は手書きの写像**。`LaneSessionEntryWire` に field を足しても
/// **ここに書かない限り webview へ届かない**（rustc は何も言わない = 静かに落ちる）。
/// 2026-08-30 に `image_capable` を roster と wire mirror に足したのにこの 1 箇所を忘れ、
/// 「server は true を返しているのに client では false」で貼り付け UI が出なかった。
pub(super) fn session_list_payload(
    lane: &str,
    sessions: &crate::daemon_wire::LaneSessionsWire,
) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = sessions
        .sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "key": s.key,
                "agent": s.agent,
                "engine_session_id": s.conversation,
                "focused": s.key == sessions.focused,
                "root": s.key == sessions.root,
                "mode": s.mode,
                "chat_capable": s.chat_capable,
                "image_capable": s.image_capable,
                "model": s.model,
                "model_choices": s.model_choices,
                "permission_choices": s.permission_choices,
            })
        })
        .collect();
    serde_json::json!({ "lane": lane, "focused": sessions.focused, "sessions": entries })
}

/// lane address から root session key を引く（snapshot 由来。不明は 1 = 従来の既定）。
///
/// lane 名しか手元に無い経路（mode 切替の適用など）が root の xterm を ensure するのに使う。
pub(super) fn root_session_of(sidebar_state: &crate::pane::SidebarState, lane: &str) -> u32 {
    sidebar_state
        .lanes_by_repo
        .values()
        .flatten()
        .find(|l| l.address.key() == lane)
        .and_then(|l| l.sessions.as_ref().map(|r| r.root))
        .unwrap_or(1)
}

/// Architecture v4: sidebar の active selection に応じて main area の表示 kind を切替。
///
/// Phase 5-A 拡張: Lane と component が **mutually exclusive** な active 軸として扱われる。
/// 優先順位:
///   1. `active_component` Some → kind = "board" / "runner" / "devices"
///   2. `active_lane_address` Some → kind = "lane"、 pane_id = Lane address
///   3. 両方 None → kind=None で empty placeholder
///
/// Lane address ごとの terminal 接続は per-Lane xterm.js (Phase 2.5) が JS-side で管理。
/// root session の mode（"tui" | "gui"）を wire の `sessions`（registry snapshot）から導出する。
///
/// doc 53 R1: 旧 `console_mode` field（root mode の投影）は退役 — **client 側の導出はこの
/// 1 関数に閉じる**（読み手 3 系統: chat 判定 / respawn gate / header 差分。R4 で pane 一覧
/// 配信に差し替える時の改修点もここ 1 箇所）。sessions 欠落（boot 窓の placeholder 等）は
/// "tui"（旧 serde default と同値）に倒す。
pub(super) fn root_mode_of(lane: &crate::daemon_wire::LaneInfo) -> &str {
    lane.sessions
        .as_ref()
        .and_then(|reg| reg.sessions.iter().find(|s| s.key == reg.root))
        .map(|s| s.mode.as_str())
        .unwrap_or("tui")
}

/// 指定 lane address が gui (root mode="gui") かを `lanes_by_repo` から引く。
///
/// 未知 address (LanesLoaded 未着 等) は false (= tui 扱い) に倒す。 chat lane は
/// engine-less (pid=None) が正常形なので、 pid では判定できない — sessions（registry
/// snapshot）由来の root mode が真実源（doc 53 R1）。
pub(super) fn lane_is_chat(state: &SidebarState, address: &str) -> bool {
    state
        .lanes_by_repo
        .values()
        .flatten()
        .find(|l| l.address.key() == address)
        .map(|l| root_mode_of(l) == "gui")
        .unwrap_or(false)
}

/// doc 38 §4.2: `conversation_session_list` payload（`{focused, sessions:[{key, agent, focused, ...}]}`）
/// から focused session の agent を引く。New Session の chat 分岐で「現 focused と同じ engine の
/// 新 Draft を作る」ために使う。`focused` フラグ優先 → `focused` key 一致 → 先頭 の順で解決し、
/// 取れなければ None（backend が lane 既定 agent を使うため送らなくてよい）。純粋 = テスト可能。
pub(super) fn focused_session_agent(payload: &serde_json::Value) -> Option<String> {
    let sessions = payload.get("sessions").and_then(|v| v.as_array())?;
    let focused_key = payload.get("focused").and_then(|v| v.as_u64());
    sessions
        .iter()
        .find(|s| s.get("focused").and_then(|v| v.as_bool()) == Some(true))
        .or_else(|| {
            focused_key.and_then(|k| {
                sessions
                    .iter()
                    .find(|s| s.get("key").and_then(|v| v.as_u64()) == Some(k))
            })
        })
        .or_else(|| sessions.first())
        .and_then(|s| s.get("agent").and_then(|v| v.as_str()).map(str::to_string))
}

#[cfg(test)]
mod header_lane_fields_changed_tests {
    use super::header_lane_fields_changed;
    use crate::daemon_wire::LaneInfo;

    /// 最小 LaneInfo（全 field serde default）に engine_session_id だけ与える。
    fn lane(engine_session_id: Option<&str>) -> LaneInfo {
        serde_json::from_value(serde_json::json!({
            "address": {"kind": "root", "repo": "vp"},
            "engine_session_id": engine_session_id,
        }))
        .expect("LaneInfo deserialize")
    }

    /// 供給 push 根治: session chip の供給源（engine_session_id）の変化と消灯を検知する。
    #[test]
    fn detects_engine_session_id_change() {
        assert!(header_lane_fields_changed(
            &lane(Some("old")),
            &lane(Some("new"))
        ));
        assert!(header_lane_fields_changed(&lane(Some("old")), &lane(None)));
    }

    /// 変化なしは false（LanesLoaded は高頻度 loop event — setActivePane を無駄打ちしない）。
    #[test]
    fn unchanged_is_false() {
        assert!(!header_lane_fields_changed(
            &lane(Some("same")),
            &lane(Some("same"))
        ));
        assert!(!header_lane_fields_changed(&lane(None), &lane(None)));
    }
}

#[cfg(test)]
mod focused_session_stand_tests {
    use super::focused_session_agent;

    /// focused フラグ付き session の agent を引く（doc 38 §4.2 New Session の chat 分岐）。
    #[test]
    fn picks_stand_of_focused_flagged_session() {
        let payload = serde_json::json!({
            "focused": 2,
            "sessions": [
                {"key": 1, "agent": "claude", "focused": false},
                {"key": 2, "agent": "codex", "focused": true},
            ]
        });
        assert_eq!(focused_session_agent(&payload).as_deref(), Some("codex"));
    }

    /// focused フラグが無ければ `focused` key と一致する session に落ちる。
    #[test]
    fn falls_back_to_focused_key() {
        let payload = serde_json::json!({
            "focused": 3,
            "sessions": [
                {"key": 1, "agent": "claude"},
                {"key": 3, "agent": "grok"},
            ]
        });
        assert_eq!(focused_session_agent(&payload).as_deref(), Some("grok"));
    }

    /// どちらも決まらなければ先頭 session の agent（安全側 = とにかく作れる）。
    #[test]
    fn falls_back_to_first_session() {
        let payload = serde_json::json!({
            "sessions": [{"key": 1, "agent": "claude"}, {"key": 2, "agent": "codex"}]
        });
        assert_eq!(focused_session_agent(&payload).as_deref(), Some("claude"));
    }

    /// sessions が空 / 欠落なら None（backend の lane 既定 agent に委ねる）。
    #[test]
    fn returns_none_when_no_sessions() {
        assert_eq!(focused_session_agent(&serde_json::json!({})), None);
        assert_eq!(
            focused_session_agent(&serde_json::json!({"sessions": []})),
            None
        );
    }
}

/// lane を conversation topic に attach する（`terminal_sessions` の対）。
///
/// 購読 0→1 が daemon の demand hook を撃ち、chat session を持つ lane では repo が
/// **transcript replay**（過去会話）を返す。これが無いと conversation topic は非 retained
/// なので「submit するまで ChatView が空」になる（app 再起動で会話が消えたように見える）。
/// idempotent — 既に session があれば no-op。
///
/// doc 58 ②-a: 旧実装は「chat session を持つ lane だけ」に張っていたが、名簿の
/// now-line（`vp now` → NowLine event）は **TUI lane からも** conversation topic に
/// 流れてくるため、gate を外して全 lane に張る。chat 専用の副作用（transcript replay /
/// eager engine spawn）は repo 側 `handle_conversation_demand_start` の session-mode gate
/// が守る — TUI session の demand は `not_chat` で graceful no-op（ReplayStart も出ない）。
pub(super) fn ensure_conversation_attach(
    address: &str,
    sidebar_state: &SidebarState,
    conversation_sessions: &mut std::collections::HashMap<String, LaneConversation>,
    rt_handle: &tokio::runtime::Handle,
    proxy: &EventLoopProxy<AppEvent>,
    daemon_conn: &SharedDaemonConn,
) {
    // 購読は lane 単位で全 session の event を運ぶ（doc 50 §4.6 A6 — root=tui + 非 root=chat
    // の構成も 1 本で拾う）。mode による撃ち分けは repo 側 gate に委譲済み（上記 doc）。
    let Some(repo_path) = resolve_repo_path_for_lane(sidebar_state, address) else {
        return; // repo 未解決 (LanesLoaded 未着) — 後続の LanesLoaded で再評価される
    };
    // 「Lane タイトルが見えている Lane だけ生きている」（mako 2026-08-28）: accordion を
    // 畳んだ repo の lane は名簿に出ないので、購読も外す。購読 1→0 が daemon の demand hook
    // を撃ち、repo 側 `conversation_demand_stop` が暇な engine を寝かせる。
    //
    // ⚠️ **detach は now-line も止める**（NowLine は conversation topic を通るため）。
    // 畳んだ repo の「今」は名簿ごと見えないので、モデル上これで正しい（mako 裁定）。
    // 副作用: 開き直した直後は now-line が空欄（NowLine は replay 対象外 = 揮発の自己申告、
    // conversation_pump の doc）。次の `vp now` で埋まる。
    if !repo_is_expanded(sidebar_state, &repo_path) {
        if conversation_sessions.remove(address).is_some() {
            tracing::info!("conversation detach (repo collapsed): {}", address);
        }
        return;
    }
    if conversation_sessions.contains_key(address) {
        return;
    }
    tracing::info!("conversation attach (chat lane): {}", address);
    let session = spawn_conversation_session(
        rt_handle,
        proxy.clone(),
        daemon_conn.clone(),
        repo_path,
        address.to_string(),
    );
    conversation_sessions.insert(address.to_string(), session);
}

pub(super) fn push_active_view(main_view: &WebView, state: &SidebarState) {
    let info = if let Some(agent) = state.active_component.as_ref() {
        ActivePaneInfo {
            kind: Some(agent.kind.as_str()),
            pane_id: None,
            preview_url: None,
            chat: false,
            // 非 lane pane (Agent) は Conversation ヘッダの lane 情報を持たない。
            cwd: None,
            branch: None,
            lane_name: None,
            session_id: None,
            agent: None,
        }
    } else if let Some(addr) = state.active_lane_address.as_deref() {
        // Conversation 共通ヘッダ用: active lane の LaneInfo から cwd / branch を引く。cwd は
        // address (pane_id) から導出できない唯一の lane 情報なので、setActivePane に相乗り
        // させて運ぶ (新しい配信チャネルは増やさない)。branch は sub のみ (安価に取れる時)。
        let lane = state
            .lanes_by_repo
            .values()
            .flatten()
            .find(|l| l.address.key() == addr);
        ActivePaneInfo {
            kind: Some("lane"),
            pane_id: Some(addr),
            preview_url: None,
            // doc 33: chat lane は xterm を持たない (ChatView が内容)。 これを JS に伝えないと
            // showLane が「xterm 無し = 内容無し」と誤判定し placeholder が ChatView を覆う。
            chat: lane_is_chat(state, addr),
            cwd: lane.map(|l| l.cwd.as_str()).filter(|c| !c.is_empty()),
            branch: lane
                .and_then(|l| l.sub_status.as_ref())
                .and_then(|p| p.branch.as_deref()),
            // doc 44 P2: 旧 `LaneInfo.name` は複製 field で **常に None** だった（JS は addr
            // 短縮名に fallback していた）。フラット化で `address.name` が唯一の在処になり、
            // 常に実体を持つのでヘッダにそのまま供給できる。
            lane_name: lane.map(|l| l.address.name.as_str()),
            // tui の session chip はこの相乗りが唯一の供給路（gui は event が上書き）。
            session_id: lane.and_then(|l| l.engine_session_id.as_deref()),
            // doc 39 P4-C: chip prefix は root session の engine（agent_name）を優先する
            // （cross-engine root で slot の engine を正しく映す）。無ければ lane 固定の agent に fallback。
            agent: lane
                .map(|l| l.agent_name.as_deref().unwrap_or(l.agent.as_str()))
                .filter(|st| !st.is_empty()),
        }
    } else {
        ActivePaneInfo {
            kind: None,
            pane_id: None,
            preview_url: None,
            chat: false,
            cwd: None,
            branch: None,
            lane_name: None,
            session_id: None,
            agent: None,
        }
    };
    let script = main_area::build_set_active_pane_script(&info);
    if let Err(e) = main_view.evaluate_script(&script) {
        tracing::warn!("main setActivePane 失敗: {}", e);
    }
}

/// LanesLoaded の snapshot 差し替えで、active lane の Conversation ヘッダに載る field が変わったか。
///
/// `push_active_view` 再発行の gate（供給 push 根治）。LanesLoaded は loop event で頻発する
/// ため毎回撃つと setActivePane が noise になる — header が実際に読む field（session chip /
/// cwd / branch / lane 名 / agent / Mode 初期値）に変化がある時だけ true を返す。
pub(super) fn header_lane_fields_changed(
    prev: &crate::daemon_wire::LaneInfo,
    next: &crate::daemon_wire::LaneInfo,
) -> bool {
    prev.engine_session_id != next.engine_session_id
        || prev.cwd != next.cwd
        || prev.agent != next.agent
        // doc 39 P4-C: chip prefix は agent_name（root session の engine）で決まるため、
        // その変化（cross-engine root 切替）でも header を再 push する。
        || prev.agent_name != next.agent_name
        || prev.address.name != next.address.name
        // doc 53 R1: root mode の変化（mode 切替 / root 付け替え）は sessions から導出して比較。
        || root_mode_of(prev) != root_mode_of(next)
        || prev
            .sub_status
            .as_ref()
            .and_then(|p| p.branch.as_deref())
            != next
                .sub_status
                .as_ref()
                .and_then(|p| p.branch.as_deref())
}

/// Lane address (Display 形 `"<repo>/root"` 等) から所属 repo path を逆引きする。
///
/// `lanes_by_repo` (= repo_path → LaneInfo list) を走査し、 `address.key()` が一致する
/// lane を持つ repo の path を返す。 `lane:select` 経路 (= JS から path を受け取る) の鏡像で、
/// focus 経路は address しか持たないためここで path を解決する。 一致なしは None。
pub(super) fn resolve_repo_path_for_lane(state: &SidebarState, address: &str) -> Option<String> {
    state
        .lanes_by_repo
        .iter()
        .find(|(_path, lanes)| lanes.iter().any(|l| l.address.key() == address))
        .map(|(path, _)| path.clone())
}

/// repo の accordion が開いているか（= その repo の lane タイトルが名簿に出ているか）。
///
/// conversation 購読の gate（[`ensure_conversation_attach`]）が使う。**未知の repo は
/// 開いている扱い**に倒す — 判断材料が無い時に「畳んでいる」と決めつけると、
/// snapshot 未着の窓で購読を落として engine を寝かせてしまう（fail-open）。
pub(super) fn repo_is_expanded(state: &SidebarState, repo_path: &str) -> bool {
    state
        .processes
        .iter()
        .find(|p| p.path == repo_path)
        .is_none_or(|p| p.expanded)
}

/// Active Lane を切替える — 全副作用を 1 箇所に集約（Simplicity 原則）。
///
/// sidebar click / switch_lane (QUIC) / auto-select の 3 入口すべてがこの関数を呼ぶ。
/// 副作用:
///   1. `sidebar_state.active_lane_address` + `active_component` (排他 clear)
///   2. session 永続化（`Persist::activate`）
///   3. notification / awaiting_input reset
///   4. sidebar UI push (`sidebar:state`)
///   5. main area push (`setActivePane` → `showLane`)
///   6. dead lane respawn
#[allow(clippy::too_many_arguments)]
pub(super) fn activate_lane(
    address: &str,
    sidebar_state: &mut SidebarState,
    persist: &mut Persist,
    webview: &wry::WebView,
    lane_respawn_triggered: &mut std::collections::HashSet<String>,
    rt_handle: &tokio::runtime::Handle,
    respawn_proxy: &EventLoopProxy<AppEvent>,
    conn: &SharedDaemonConn,
) {
    // 1. State
    sidebar_state.active_lane_address = Some(address.to_string());
    if sidebar_state.active_component.is_some() {
        sidebar_state.active_component = None;
    }

    // 2. Session persistence（保存は Persist が 1 箇所で担う）
    persist.activate(address);

    // 3. Notification reset (同 lane click 連打でも badge を消す)
    sidebar_state.unread_notifications.remove(address);
    sidebar_state.awaiting_input.remove(address);
    // canvas 着信 badge (D) も active 化で消す (unread_notifications と同 lifecycle)。
    sidebar_state.canvas_unread.remove(address);

    // 4-6. UI push + dead lane respawn
    // BUG#3: 旧実装は push_active_view / respawn を `view_changed` (active_lane_address が
    // 変わった時だけ) に gate していたが、 address 一致だが main area 未表示 / pump 未成立
    // (restart 直後の楽観反映 × canonical desync) の状態で同一 lane を再 click すると no-op に
    // なり「切り替えられない」。 setActivePane → showLane は冪等 (setWantedLane 撤去済で WS 付替
    // churn 無し)、 respawn は triggered set で dedup 済なので、 view_changed に依らず毎回実行して
    // desync を確定的に解消する。 activate_lane の 3 caller (初回 auto-select は
    // active_lane_address.is_none() gate で 1 回 / switch_lane / sidebar click) はいずれも
    // genuine activation で高頻度発火しないため、 毎回 push しても focus 奪取 flood は起きない。
    push_sidebar_state(webview, sidebar_state);
    push_active_view(webview, sidebar_state);
    // doc 50 §4.6 A6: lane 単位 console_mode の同期は退役。表示（roster + focus）は World B の
    // `applyLaneView` が lane 切替を契機に開き、顔ぶれは session 一覧 × 各 session の mode から
    // 導出される（見え方は session の属性なので、lane 単位の mode を送る意味が無くなった）。
    maybe_respawn_dead_lane(
        address,
        sidebar_state,
        lane_respawn_triggered,
        rt_handle,
        respawn_proxy,
        conn,
    );
}

/// オンデマンド respawn: active にしようとする lane が Dead (pid:null) なら repo に restart_lane を
/// 発火して蘇らせる。 lane (main / sub) の Conversation プロセスが死ぬと repo の lifecycle monitor は
/// Dead を検知するだけで auto-respawn しない (server.rs の設計判断) ため、 user が lane を
/// 開いた時点でオンデマンドに復活させる。 これが無いと「一度死んだ lane は手動 restart するまで
/// Conversation が出ない」状態になる (= 全 repo で console 非表示の真因)。
///
/// dedup: `triggered` set で同一 lane の連打を防ぐ (LanesLoaded は loop event で頻発するため必須)。
/// 解除タイミングは 2 つ: (a) lane が Running に戻った時 caller が `triggered.remove` する、
/// (b) restart_lane が失敗した時 `AppEvent::LaneRespawnFailed` 経由で caller が `triggered.remove`
/// する (= 失敗が永続 suppression にならないようにする、 Moody Blues Issue #1)。
pub(super) fn maybe_respawn_dead_lane(
    addr: &str,
    state: &SidebarState,
    triggered: &mut std::collections::HashSet<String>,
    rt_handle: &tokio::runtime::Handle,
    proxy: &EventLoopProxy<AppEvent>,
    conn: &SharedDaemonConn,
) {
    // addr の lane を lanes_by_repo から探し、 所属 repo path と pid を取得。
    let entry = state.lanes_by_repo.iter().find_map(|(path, lanes)| {
        lanes
            .iter()
            .find(|l| l.address.key() == addr)
            .map(|l| (path.clone(), l.pid, root_mode_of(l).to_string()))
    });
    let Some((repo_path, pid, root_mode)) = entry else {
        return; // lane 未知 (まだ LanesLoaded 来てない等) — 後続の LanesLoaded で再評価される
    };
    if pid.is_some() {
        return; // Running、 respawn 不要
    }
    // doc 33 §3: chat lane は engine-less (pid=None) が正常形。
    // respawn 対象は「root mode=tui かつ pid=None」のみ（chat lane を殺しに行かない — #683
    // 再演防止。mode は sessions 由来 — doc 53 R1）。
    if root_mode == "gui" {
        return;
    }
    // dedup: 既に respawn 進行中なら skip
    if !triggered.insert(addr.to_string()) {
        return;
    }
    // F6③: 旧 DaemonRpcClient.restart_lane (repo 直結 reqwest) を daemon repo-proxy ask
    // (lane_restart) に移管。 repo port 解決は不要 (Daemon :32000 固定 + repo_path handshake)、
    // 旧「port 未解決 skip」分岐も消滅。 失敗時の trigger 解除は LaneRespawnFailed 経路に一本化。
    let addr_owned = addr.to_string();
    let proxy = proxy.clone();
    tracing::info!("auto-respawn dead lane (on-demand): addr={}", addr_owned);
    let conn = conn.clone();
    rt_handle.spawn(async move {
        // auto-respawn は Dead lane の復活なので会話を継ぐ (fresh=false)。
        let payload = serde_json::json!({ "address": &addr_owned, "fresh": false });
        match daemon_repo_request(&conn, &repo_path, "lane_restart", payload).await {
            Ok(_) => {
                // 成功時は LanesLoaded で Running 検出時に triggered から解除される。
                tracing::info!("auto-respawn lane_restart ok: {}", addr_owned);
            }
            Err(e) => {
                tracing::warn!("auto-respawn lane_restart failed: {}: {}", addr_owned, e);
                // 失敗を event loop に通知して triggered を解除する (永続 suppression 回避)。
                // これが無いと repo クラッシュ等で全 retry 失敗した lane は vp-app 再起動まで
                // auto-respawn 対象外になってしまう (Moody Blues Issue #1)。
                let _ = proxy.send_event(AppEvent::LaneRespawnFailed {
                    address: addr_owned,
                });
            }
        }
    });
}

/// lane を「入力待ち（要注意）」として記録し、sidebar の unread count / 黄 dot を更新する。
///
/// active lane（今まさに見ている lane）は即読扱いで skip する（見ている lane に dot を出さない）。
/// これは通知の**単一 sink** で、2 つのソースがここに合流する。tui は OSC 99/9/777
/// notification（`AppEvent::OscNotification`、xterm が parse）。gui は
/// `ConversationEvent::turn_completed`（headless stream-json は Notification hook を発火しないため、
/// stream `result` 由来の turn_completed が「Claude が返し終えた＝入力待ち」の唯一のシグナル。
/// memory echoes-act2-notification-signal 参照）。
/// `source` はログ用ラベル（`"osc:notification"` / `"gui:turn_completed"` 等）。
pub(super) fn mark_lane_awaiting_input(
    lane: &str,
    source: &str,
    sidebar_state: &mut SidebarState,
    webview: &WebView,
) {
    if sidebar_state.active_lane_address.as_deref() == Some(lane) {
        tracing::debug!("{source} skip (active lane): lane={lane}");
        return;
    }
    let count = sidebar_state
        .unread_notifications
        .entry(lane.to_string())
        .or_insert(0);
    *count += 1;
    // 「入力待ち」 = 行右端に黄 dot。 active 切替で reset される。
    sidebar_state.awaiting_input.insert(lane.to_string(), true);
    tracing::info!("{source} lane={lane} unread={}", *count);
    push_sidebar_state(webview, sidebar_state);
}

/// lane に Canvas (board) show が着信したことを sidebar の canvas_unread に計上する。
///
/// `mark_lane_awaiting_input` (HITL/OSC = 黄 dot) とは**別 sink**。Canvas 着信は sidebar 行に
/// Canvas 専用 icon (Phosphor easel) として出し、「用事(黄 dot)」と「絵が届いた(easel)」の
/// 語彙を分離する (bug: canvas 可観測性 D、show 偽 success の viewer 文脈対策)。
/// active lane（今見ている lane）宛の show は panel 側 (pp-overlay auto-open) で解決するので、
/// ここでは badge を出さない（呼び出し側で active 判定済だが二重防御で skip）。
pub(super) fn mark_lane_canvas_unread(
    lane: &str,
    sidebar_state: &mut SidebarState,
    webview: &WebView,
) {
    if sidebar_state.active_lane_address.as_deref() == Some(lane) {
        return;
    }
    let count = sidebar_state
        .canvas_unread
        .entry(lane.to_string())
        .or_insert(0);
    *count += 1;
    tracing::info!("canvas:show lane={lane} canvas_unread={}", *count);
    push_sidebar_state(webview, sidebar_state);
}

/// address だけから lane の workdir を引く（code pane 用）。
///
/// main bundle（CodePane.tsx）は sidebar と違い `lanes_by_repo` の repo_path を
/// 持たないため、address（`LaneAddressWire::key()` = daemon 発行の canonical）を
/// 全 repo に対して探す。key は repo 名を含む合成キーなので全 repo 走査でも
/// 衝突しない（`<repo>/lane/<name>` — 同名 lane が別 repo に居ても key が違う）。
pub(super) fn lookup_lane_cwd_by_address(
    state: &SidebarState,
    address: &str,
) -> Option<std::path::PathBuf> {
    state
        .lanes_by_repo
        .values()
        .flatten()
        .find(|l| l.address.key() == address)
        .map(|l| std::path::PathBuf::from(&l.cwd))
}
