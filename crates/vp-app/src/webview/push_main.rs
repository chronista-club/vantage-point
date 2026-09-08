//! Rust → main webview の投影（`window.vpDispatch` への typed push、`schema/vp-push.kdl` の envelope）。
//!
//! 旧 `app.rs` 内の `mod lane_js`（棚卸し 項目 6 / 6-1 で移設、2026-09-08。本文は順序付き diff で一致、
//! 差分は dedent / rustfmt の折り返し / `pub` → `pub(crate)` / private fn への doc link 1 つを平文に）。ここは**投影だけ**を持つ: server の真実（SidebarState /
//! snapshot）を JS の受け口へ押し込む関数群で、state 遷移や購読の寿命はここに置かない（doc 60 §2）。
//! 受け口が居ない boot 窓の救済は `AppEvent::WebviewReady` の replay（`app` 側）。

use wry::WebView;

use crate::generated::push::{
    BoardMessage, CodeEntries, CodeFile, ConsoleAgents, ConsoleEvent, ConsoleModeApplied,
    ConsoleSessionList, DebuglogLines, DevicesRender, InkSnapshot, InkSnapshotError,
    PushEventEnvelope, ShellLayout, TermEnsureLane, TermPaste, TermRemoveLane, TermRemoveSession,
    TermShowLane,
};

/// 生成 envelope を webview の単一受け口 `window.vpDispatch` へ押し込む。
///
/// ## なぜ名前で関数を呼ばず envelope 1 本にするか（`schema/vp-push.kdl`）
///
/// 旧来 Rust → JS は `window.ensureLane(...)` のように**名前で関数を呼ぶ**形で、負債が 2 つ:
///
/// 1. **型が無い** — 引数の数や順序が食い違っても Rust も TS も黙る
/// 2. **押し込みが黙って落ちる** — `window.X && window.X.y(...)` は bundle 準備前なら
///    **no-op で「成功」する**。VP はこの穴を feature ごとの pull で埋めてきた
///    （旧 `lanes:ensure-all` / `bastet:devices_fetch` / `board:demand` — 3 本とも退役済）
///
/// envelope なら ① は codegen が、② は受け側 1 箇所の buffer が塞ぐ。窓口が 1 つになって
/// 初めて buffer を 1 個置けば済む（~24 個の窓口それぞれには置けなかった）。
///
/// ⚠️ `window.vpDispatch &&` の guard は**残す**。bundle 評価前に Rust が撃つ窓は依然あり、
/// そこは JS が存在しないので queue にも積めない。その窓の救済は
/// [`AppEvent::WebviewReady`](crate::events::AppEvent::WebviewReady) の replay
/// （受け口が揃った合図を受けて現在の状態を丸ごと撃ち直す）。
fn push(main_view: &WebView, msg: &PushEventEnvelope) {
    let json = match serde_json::to_string(msg) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!("push envelope の serialize に失敗: {e}");
            return;
        }
    };
    let script = format!("window.vpDispatch && window.vpDispatch({json})");
    if let Err(e) = main_view.evaluate_script(&script) {
        tracing::warn!("vpDispatch script failed: {e}");
    }
}

/// (lane, session) の xterm instance を用意する — 既存ならば no-op (idempotent)。
///
/// terminal S4: repo port は不要になった (xterm の transport は Daemon "canvas" channel +
/// per-lane terminal session、 旧 `/ws/terminal?port=` 直結を撤去)。 JS は xterm instance を
/// 作るだけで socket は持たない。 出力/入力は Rust の terminal session が IPC で橋渡しする。
///
/// doc 50 §4.6 A6: xterm は **(lane, session) ごと**。`is_root` は host の選び方を決める
/// （root = 静的 `#lane-host` / 非 root = 動的 `#term-session-<n>`）。
pub(crate) fn ensure_lane(main_view: &WebView, address: &str, session: u32, is_root: bool) {
    push(
        main_view,
        &PushEventEnvelope::TermEnsureLane(TermEnsureLane {
            lane: address.to_string(),
            session: i64::from(session),
            is_root,
        }),
    );
}

/// 1 session の term instance だけ畳む（mode 切替 tui→chat の後始末。lane 全体は [`remove_lane`]）。
pub(crate) fn remove_lane_session(main_view: &WebView, address: &str, session: u32) {
    push(
        main_view,
        &PushEventEnvelope::TermRemoveSession(TermRemoveSession {
            lane: address.to_string(),
            session: i64::from(session),
        }),
    );
}

/// active な 1 Lane を表示。`None` なら empty placeholder。
///
/// `is_chat` = gui (root mode="gui"、sessions 由来)。 chat lane は xterm を持たない
/// (ChatView が内容) ため、これを渡さないと JS 側が「xterm 無し = 内容無し」と誤判定して
/// placeholder を被せる。
///
/// 「lane 未選択」は schema の `optional` field で表現される（旧: JS の `null` 直書き）。
pub(crate) fn show_lane(main_view: &WebView, address: Option<&str>, is_chat: bool) {
    push(
        main_view,
        &PushEventEnvelope::TermShowLane(TermShowLane {
            lane: address.map(str::to_string),
            // lane 未選択なら chat 判定も意味を持たない（旧実装の `showLane(null, false)`）。
            is_chat: address.is_some() && is_chat,
        }),
    );
}

/// Lane が消えた時に、その lane の **全 session** の xterm を dispose。
pub(crate) fn remove_lane(main_view: &WebView, address: &str) {
    push(
        main_view,
        &PushEventEnvelope::TermRemoveLane(TermRemoveLane {
            lane: address.to_string(),
        }),
    );
}

/// OS clipboard の中身を focus 中の xterm へ流し込む（`paste:request` の戻り）。
///
/// 宛先は JS 側が決める（focus 中の 1 枚。A6 で lane に active pane が複数並ぶように
/// なったので「最初の active」では意図しない pane に貼られる）。
pub(crate) fn deliver_paste(main_view: &WebView, text: &str) {
    push(
        main_view,
        &PushEventEnvelope::TermPaste(TermPaste {
            text: text.to_string(),
        }),
    );
}

/// 計器盤 pane に MIDI device 一覧を render する（daemon-device bridge の出口）。
///
/// 差分ではなく**全量の置き換え**（level 駆動）なので、途中の 1 通を落としても次の 1 通で
/// 正しい状態に戻る。`AppEvent::DeviceEvent` と webview 誕生時の replay の両方から呼ぶ。
pub(crate) fn render_devices(main_view: &WebView, devices: &[crate::pane::DeviceSnapshot]) {
    // 1 件でも黙って消えると「device が 1 つ足りない」だけが残って原因が辿れない。
    // 実路では起きない（`DeviceSnapshot` は平たい 3 field）が、**黙って落とさない**のが
    // この経路の主題なので、省いたことは必ず言う。
    let devices = devices
        .iter()
        .filter_map(|d| match serde_json::to_value(d) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("device の serialize に失敗（この 1 件を省く）: {e}");
                None
            }
        })
        .collect();
    push(
        main_view,
        &PushEventEnvelope::DevicesRender(DevicesRender { devices }),
    );
}

/// shell (L sidebar | main | R sidebar) の形を復元する。
///
/// ⚠️ **保存が無ければ呼ばない**（caller 側の `if let Some`）。撃たなければ webview の
/// 既定値がそのまま残る = 既定を Rust と webview の 2 箇所に書かずに済む。
pub(crate) fn shell_layout(main_view: &WebView, l: &crate::session_state::ShellLayout) {
    use crate::session_state::SidebarForm;
    push(
        main_view,
        &PushEventEnvelope::ShellLayout(ShellLayout {
            sidebar_width: l.sidebar_width as i64,
            right_sidebar_width: l.right_sidebar_width as i64,
            sidebar_form: match l.sidebar_form {
                SidebarForm::Slim => "slim".to_string(),
                SidebarForm::Full => "full".to_string(),
            },
            right_sidebar_open: l.right_sidebar_open,
        }),
    );
}

/// Console 面へ lane の session 一覧（roster）を渡す。
///
/// 供給はこの 1 本（doc 53 §11）。呼び手は `app::lane_view::push_session_list` 経由の 1 箇所だけ。
pub(crate) fn console_session_list(main_view: &WebView, lane: &str, payload: serde_json::Value) {
    push(
        main_view,
        &PushEventEnvelope::ConsoleSessionList(ConsoleSessionList {
            lane: lane.to_string(),
            payload,
        }),
    );
}

/// Console 面へ gui の構造化イベントを渡す。
///
/// ⚠️ これは制御面ではなく **stream**。取りこぼしは受け側の replay 要求
/// （`conversation:demand_start`）が埋める設計で、押し込みの保留箱には頼らない。
pub(crate) fn console_event(
    main_view: &WebView,
    lane: &str,
    event: serde_json::Value,
    session: u32,
) {
    push(
        main_view,
        &PushEventEnvelope::ConsoleEvent(ConsoleEvent {
            lane: lane.to_string(),
            event,
            session: i64::from(session),
        }),
    );
}

/// mode 切替が実体に適用されたことを Console 面へ報せる（`SessionModeApplied` の戻り）。
pub(crate) fn console_mode_applied(main_view: &WebView, lane: &str, session: u32, mode: &str) {
    push(
        main_view,
        &PushEventEnvelope::ConsoleModeApplied(ConsoleModeApplied {
            lane: lane.to_string(),
            session: i64::from(session),
            mode: mode.to_string(),
        }),
    );
}

/// 「+」menu へ agent 一覧を返す。`req` は要求元の相関 id（doc 47 §6、省略 = 誰も拾わない）。
pub(crate) fn console_stands(
    main_view: &WebView,
    lane: &str,
    payload: serde_json::Value,
    req: Option<String>,
) {
    push(
        main_view,
        &PushEventEnvelope::ConsoleAgents(ConsoleAgents {
            lane: lane.to_string(),
            payload,
            req,
        }),
    );
}

/// 対話面（ink）へ snapshot の成功を返す（`path` = PNG の絶対 path）。
pub(crate) fn ink_snapshot(main_view: &WebView, path: String) {
    push(
        main_view,
        &PushEventEnvelope::InkSnapshot(InkSnapshot { path }),
    );
}

/// 対話面（ink）へ snapshot の失敗を返す（注釈は残して再送可能にする）。
pub(crate) fn ink_snapshot_error(main_view: &WebView, message: String) {
    push(
        main_view,
        &PushEventEnvelope::InkSnapshotError(InkSnapshotError { message }),
    );
}

/// 掲示板（board）へ repo の canvas message をそのまま渡す。
///
/// 中身の形は repo が持つ（VP は転送するだけ）。型が要るのは「どの窓口へ届けるか」の方で、
/// それは envelope の tag が担う。
pub(crate) fn board_message(main_view: &WebView, message: serde_json::Value) {
    push(
        main_view,
        &PushEventEnvelope::BoardMessage(BoardMessage { message }),
    );
}

// ===== code pane（コードブラウザ P1）=====

/// `code:list` の応答: lane workdir の file 一覧。要素の形の持ち主は
/// [`crate::webview::file_explorer::Entry`]（serialize 失敗はその 1 件だけ省く —
/// `files_list_result` から引き継いだ方針）。
pub(crate) fn code_entries(
    main_view: &WebView,
    lane: &str,
    entries: &[crate::webview::file_explorer::Entry],
    truncated: bool,
) {
    let entries = entries
        .iter()
        .filter_map(|e| match serde_json::to_value(e) {
            Ok(v) => Some(v),
            Err(err) => {
                tracing::warn!("code entry の serialize に失敗（この 1 件を省く）: {err}");
                None
            }
        })
        .collect();
    push(
        main_view,
        &PushEventEnvelope::CodeEntries(CodeEntries {
            lane: lane.to_string(),
            entries,
            truncated,
        }),
    );
}

/// `code:read` の応答: file 内容。payload は `{"text"} | {"error"}` の 2 択
/// （形の持ち主は `file_explorer::read_file`）。
pub(crate) fn code_file(
    main_view: &WebView,
    lane: &str,
    rel_path: &str,
    payload: &serde_json::Value,
) {
    push(
        main_view,
        &PushEventEnvelope::CodeFile(CodeFile {
            lane: lane.to_string(),
            rel_path: rel_path.to_string(),
            payload: payload.clone(),
        }),
    );
}

/// File menu「Code Browser」→ code pane の toggle（menu 起点の一方向 push、
/// 旧 `file_picker_open` の後継）。active lane 判定は webview 側に委譲。
pub(crate) fn code_toggle(main_view: &WebView) {
    // fieldless event は codegen で unit variant になる（payload struct を包まない）。
    push(main_view, &PushEventEnvelope::CodeToggle);
}

/// R sidebar の debug log viewer へ tail の行群を渡す（sidebar view modes、2026-08-01）。
///
/// ⚠️ stream（`console_event` と同類）— 取りこぼしは次の `debuglog:watch` が
/// backlog 込みで埋めるので、保留箱には頼らない。
pub(crate) fn debuglog_lines(main_view: &WebView, source: &str, reset: bool, lines: Vec<String>) {
    push(
        main_view,
        &PushEventEnvelope::DebuglogLines(DebuglogLines {
            source: source.to_string(),
            reset,
            lines,
        }),
    );
}
