//! Terminal pane の **IPC handler** + AppEvent 定義のみ
//!
//! ## Phase 2.x-d (Architecture v4 cleanup)
//!
//! Phase 2.5 で **per-Lane 化 + browser-native WebSocket** に移行したため、
//! Rust 側で PTY を持つ必要が無くなった。 旧 `PtyHandle` / `spawn_shell` /
//! `TerminalHandle::Local` / `TerminalHandle::Daemon` / `build_output_script` /
//! `dirs_home` / `writer_loop` / `reader_loop` / `AppEvent::Output` / `AppEvent::XtermReady` を
//! 一括撤去 (合計 -250 行)。 関連: Purple Haze 調査 (2026-04-27) の A6-a/e。
//!
//! 残った責務はとても薄い:
//! - `AppEvent` enum: tao の EventLoop に流す app-wide event
//! - `handle_ipc_message`: main_area webview からの IPC で `ready` / `copy` / `debug` /
//!   `slot:rect` の **non-PTY** event だけを処理 (Lane の input/output は browser native WS で完結)
//!
//! 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane = Session Process)

use tao::event_loop::EventLoopProxy;

/// EventLoop に送る app 全体のイベント
///
/// Phase 2.x-d: PTY-related variant (Output/XtermReady) は撤去。
/// Lane terminals は per-Lane の browser-native WebSocket で input/output を扱う。
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// daemon から Repo list 取得成功 (= `fetch_repos_with_ports` 経由で runtime port 込み)。
    ReposLoaded(Vec<crate::client::RepoInfo>),
    /// daemon への接続失敗 (= daemon 未起動 / network エラー)。
    ReposError(String),
    /// VP-95: Activity widget の定期更新 payload
    ActivityUpdate(crate::pane::ActivitySnapshot),
    /// VP-95: sidebar webview からの IPC メッセージ (JSON 文字列、main loop でパース)
    SidebarIpc(String),
    /// doc 48 Phase 2 (editor bridge): daemon からの `EditorCommand` を webview で評価する。
    ///
    /// `js` を main webview で `evaluate_script_with_callback` し、結果 (wry が JSON
    /// 文字列化した評価値) を `resp` に 1 回送る。sender が mpsc なのは AppEvent の
    /// Clone derive と両立させるため (oneshot は Clone 不可)。受け手は
    /// `run_canvas_session` の editor_command intercept (timeout 側が受信を打ち切る)。
    EditorEval {
        js: String,
        resp: tokio::sync::mpsc::UnboundedSender<String>,
    },
    /// VP-100 γ-light: main area の active pane slot 矩形通知。
    ///
    /// Phase 2 時点では受け取って store するだけ。Phase 4+ で native pane が
    /// 追加された時に native widget の `set_position` 同期に使う想定。
    /// 詳細は memory:vp_app_native_overlay_resize_ghost.md。
    SlotRect {
        pane_id: Option<String>,
        kind: String,
        rect: crate::main_area::SlotRect,
    },
    /// VP-100 follow-up: muda メニュー項目クリック (developer mode toggle / open devtools 等)
    MenuClicked(muda::MenuId),
    /// Phase A4-3b: repo (= Runtime Process) の `/api/lanes` を fetch して Lane list を main thread に通知
    /// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Process recursive)
    LanesLoaded {
        repo_path: String,
        lanes: Vec<crate::client::LaneInfo>,
        /// doc 44 D4: この repo の開発起点 lane 名（Host の帳簿が解決した値）。
        ///
        /// `None` = snapshot に載っていなかった（旧 server / 解決不能）。受け手は
        /// **前回値を保つ** — 既定値に落とすと、起点を指定済の repo で ⭐ が明滅する。
        origin: Option<String>,
    },
    /// Phase A4-3b: Lane fetch 失敗 (repo 未起動 / 接続失敗)
    LanesError { repo_path: String, message: String },
    /// オンデマンド respawn (maybe_respawn_dead_lane) の restart_lane が失敗した通知。
    /// event loop で lane_respawn_triggered から address を除去し、 次の Dead 検出で
    /// 再 respawn できるようにする (失敗が永続 suppression にならないための解除通知)。
    LaneRespawnFailed { address: String },
    /// in-app update: 適用フローの進行状態（true = 適用中）。sidebar の「更新する」ボタンを
    /// 「更新中…」表示に切り替える。false = キャンセル / 失敗で通常表示へ戻す
    /// （成功時はプロセスごと終了するので戻し event は来ない）。
    UpdateFlowPhase(bool),
    /// Wire inbox (doc 34 §4 V1): Daemon "wire" channel への read-only fetch 結果。
    /// event loop が `window.vpWire.handleResult(payload)` で sidebar に push back する。
    /// payload = `{address, agent, history, unread}` (エラーは `{address, error}`)。
    WireHistoryResult {
        address: String,
        payload: serde_json::Value,
    },
    /// Clone 先フォルダ picker で選択された path を sidebar JS に push (キャンセル時は None)
    ClonePathPicked(Option<String>),
    /// Phase 4-paste-fix: clipboard paste request の応答。 OS clipboard の内容を JS に届ける。
    /// 空文字なら paste skip。 `term:paste` の push で focus 中の xterm に inject。
    PasteText(String),
    /// Phase 5-D Sprint C P2.1: Lane HD notification 通知 (OSC 99 final-chunk + a=focus)。
    /// main_area xterm.js が capture → Rust が SidebarState の per-Lane unread count を加算 →
    /// sidebar に push back → badge UI 表示。 active lane への switch で 0 reset。
    OscNotification { lane: String, code: u32 },
    /// R5 Sub create flow: Add Sub form が送信した `lane:add_sub` の結果を sidebar に
    /// push back する。 `error` Some の時 form 下に inline error 表示、 None の時 form を閉じる。
    /// 例: 名前重複 (CONFLICT)、 lane clone 失敗、 repo 未起動 等。
    SubCreateResult {
        repo_path: String,
        name: String,
        error: Option<String>,
    },
    /// doc 11 PR-C / F6④: 利用可能 Agent 一覧を sidebar に push back する。
    /// `+ Add Sub` form 開閉時に JS から `agents:fetch` が来て、 Rust 側で Daemon
    /// repo-proxy ask (`agents_list`) を叩いた結果がここに乗る。 JS は `window.handleAgentsResult`
    /// で受領し、 dropdown を populate する。 `error` Some なら fetch 失敗、 dropdown は
    /// disabled + error message 表示。
    AgentsResult {
        repo_path: String,
        agents: Vec<crate::client::AgentInfo>,
        error: Option<String>,
    },
    /// webview が受け口を全部生やした（`entry.tsx` の `t:"ready"`）。
    ///
    /// bundle 評価**前**に Rust が撃った押し込みは受け口が居ないので届かない。この合図を受けて
    /// Rust は**現在の状態を丸ごと撃ち直す**（lane の xterm / roster / terminal replay demand /
    /// active view / device 一覧 / board snapshot）。全部 idempotent か全量置き換えなので、
    /// 二重に撃っても壊れない。
    ///
    /// ⚠️ 以前は同じことを feature ごとの pull 3 本（`lanes:ensure-all` / `bastet:devices_fetch`
    /// / `board:demand`）でやっており、面を足すたびに tag が 1 本増えていた。「webview が
    /// 生まれた」は 1 つの事実なので signal も 1 本。**新しい面の replay はここに足す**。
    WebviewReady,
    /// ink（対話面、doc 52 §3）: webview から board pane（#ink-stage）の snapshot 要求。
    /// event loop が WKWebView.takeSnapshot で `rect` を撮って PNG を state_dir に書き、完了を
    /// `InkSnapshotReady` で受けて push envelope `ink:snapshot` を webview に返す。
    /// 送信文面・宛先（chat/tui）の決定は webview 側（ink.ts）が既存 IPC で行う（server 0 行）。
    InkSnapshot { rect: crate::ink_snapshot::InkRect },
    /// ink: takeSnapshot の completion handler（main thread）から event loop へ返す結果。
    /// `path` Some = 成功（PNG の絶対パス）、`error` Some = 失敗（理由）。
    InkSnapshotReady {
        path: Option<String>,
        error: Option<String>,
    },
    /// VP-143: 全 lane の cc session display name (custom-title) を再 resolve する周期 tick。
    /// `tokio::spawn` で 5s 間隔の background task が proxy 経由で send。 main thread は
    /// `sidebar_state.lanes_by_repo` を walk して `session_title::resolve_title_for_cwd` を
    /// 呼び、 結果を `sidebar_state.session_titles` に diff/update + sidebar に push back する。
    ResolveSessionTitles,
    /// VP-147 PR-P2-3: 全 lane の mailbox inbox 状況を再 resolve する周期 tick。
    /// `spawn_lane_inbox_poller` (5s 間隔) が proxy 経由で send。 main thread は
    /// `sidebar_state.lanes_by_repo` を walk して各 lane の MessageState を build し、
    /// `sidebar_state.lane_inboxes` に diff/update + sidebar に push back する。
    /// Phase 2 (icon visibility のみ) では active Lane に対して placeholder MessageState
    /// (= 0 件 default) を populate し、 sidebar UI で `.vp-message-icon` を表示するための
    /// signal として機能。 unread_count / has_persistent / last_msg_ts の actual 値は
    /// 後続 PR で backend peek API + 永続 store query を実装して populate。
    ResolveLaneInboxes,
    // ===== code pane（コードブラウザ P1）— demand は main webview 発（CodePane.tsx） =====
    /// `code:list` 要求。lane address から cwd を解決して blocking walk へ。
    CodeList { lane: String },
    /// `code:read` 要求。pane 内表示用の raw text 読み（`file_explorer::read_file`）。
    CodeRead { lane: String, rel_path: String },
    /// `code:list` の walk 結果 → `lane_js::code_entries` で main webview へ push。
    CodeEntriesResult {
        lane: String,
        entries: Vec<crate::file_explorer::Entry>,
        truncated: bool,
    },
    /// `code:read` の読み結果 → `lane_js::code_file` で main webview へ push。
    /// `payload` は `{"text": string} | {"error": string}` の 2 択（read_file の返り値）。
    CodeFileResult {
        lane: String,
        rel_path: String,
        payload: serde_json::Value,
    },
    /// wiremsg Stage 2: repo の "canvas" Unison channel から受信した Canvas (Board)
    /// RepoMessage 1 件。`message` は RepoMessage の生 JSON (`{"type":"show",...}` 等)。
    /// handler は active repo の分のみ main_view WebView に転送する。
    CanvasMessage {
        repo_path: String,
        message: serde_json::Value,
    },
    /// DeviceRegistry 🧲 device event (DeviceConnected / DeviceDisconnected / ControlEvent)。
    /// daemon "daemon-device" Unison channel から受信した `DeviceEvent` の生 JSON。
    /// Phase 1 handler は tracing で log。 Phase 2 で DeviceRegistry pane / sidebar に反映予定。
    DeviceEvent { payload: serde_json::Value },
    /// board モデル (2026-07-15): WebView からの board mutate（thumbnail ✕ / Clear ボタン）。
    /// `method` = "board_delete_item" | "board_clear"、 `body` は IPC payload の生 JSON
    /// (scope / lane / item_id 等)。 active repo の repo に daemon repo-proxy ask で forward し、
    /// repo が DB 更新 → BoardUpdated(retained) broadcast → canvas channel で webview に反映する。
    /// board は repo が truth を持つため、 webview 側の save/load 経路（旧 PpState*）は撤去した。
    BoardMutate {
        method: String,
        body: serde_json::Value,
    },
    /// terminal S4 (doc 27 §4.1): per-lane terminal session が daemon canvas channel から受信した
    /// PTY 出力 1 chunk。 `data` は base64 (LaneTerminalOutput.data)。 event loop が
    /// `window.vpTerminal.handleOutput(lane, session, data)` で当該 (lane, session) の xterm に
    /// inject する。
    ///
    /// doc 50 §4.6 A6: `session` = 発生元 session の VP 採番 key。topic は lane 単位で共有し、
    /// session は `LaneTerminalOutput.session`（serde default=1）で運ぶ（`ConversationEvent` と対称、
    /// doc 38 落とし穴① =「session を lane 名に埋めない」）。
    TerminalOutput {
        lane: String,
        session: u32,
        data: String,
    },
    /// terminal S4: WebView (xterm onData) からの入力。 `data` は base64。 event loop が
    /// 当該 lane の terminal session に渡し、 canvas channel 上り request `terminal_write` で repo へ。
    /// doc 50 §4.6 A6: `session` = 宛先 slot（どの xterm から打たれたか。宛先は引数で運ぶ）。
    TerminalWrite {
        lane: String,
        session: u32,
        data: String,
    },
    /// terminal S4: WebView からの resize。 event loop が当該 lane の terminal session に渡し、
    /// canvas channel 上り request `terminal_resize` で repo へ。
    /// doc 50 §4.6 A6: `session` = 宛先 slot（pane ごとに大きさが違う）。
    TerminalResize {
        lane: String,
        session: u32,
        cols: u16,
        rows: u16,
    },
    /// Conversation gui (doc 32): 当該 lane の conversation session が daemon canvas channel から受信した
    /// 構造化イベント 1 件。 `event` は ConversationEvent の生 JSON (`{"kind":"message_chunk",...}`)。
    /// event loop が push envelope `console:event` で当該 lane の Console pane に渡す。
    /// doc 38 Phase 2: `session` = 発生元 session の VP 採番 key（1 Lane = N session）。topic の
    /// `RepoMessage::ConversationEvent::session`（serde default=1）由来。session は lane 名に埋めず
    /// 常に別 field で運ぶ（doc 38 落とし穴①）。
    ConversationEvent {
        lane: String,
        event: serde_json::Value,
        session: u32,
    },
    /// Conversation gui: WebView (ChatPane) からのプロンプト投入。 event loop が当該 lane の
    /// conversation session を lazy spawn し、 canvas channel 上り request `conversation_submit` で repo へ。
    ConversationSubmit {
        lane: String,
        prompt: String,
        /// 宛先 session（doc 50 P2）。None = lane の focused（旧 SP / 旧 UI 互換）。
        session: Option<u32>,
        /// 添付画像（chat 入力欄への貼り付け）。空 = text だけ。
        images: Vec<serde_json::Value>,
    },
    /// Conversation gui HITL (doc 35 PR1): PromptCard の回答。 event loop が当該 lane の conversation
    /// session へ渡し、 canvas channel 上り request `conversation_respond` で repo へ。 `request_id` は
    /// Question event 由来の control_response マッチング用。 allow は `answers`、 deny は
    /// `behavior="deny"`+`message` を運ぶ（どちらか）。
    ConversationRespond {
        lane: String,
        request_id: String,
        /// 宛先 session（doc 50 P2）。None = focused。
        session: Option<u32>,
        answers: Option<serde_json::Value>,
        behavior: Option<String>,
        message: Option<String>,
    },
    /// Conversation gui HITL (doc 35 §5 / PR2): 実行中 turn の中断（stop ボタン / Esc）。
    /// event loop が当該 lane の conversation session へ渡し、`conversation_interrupt` で repo へ。
    ConversationInterrupt {
        lane: String,
        /// 宛先 session（doc 50 P2）。None = focused。
        session: Option<u32>,
    },
    /// Conversation gui HITL (doc 35 §2.5 / PR3): permission mode 動的切替。event loop が当該 lane の
    /// conversation session へ渡し、`conversation_set_permission_mode` で repo へ。`mode` = "default"|"bypassPermissions" 等。
    ConversationSetPermissionMode {
        lane: String,
        mode: String,
        /// 宛先 session（doc 50 P2）。None = focused。
        session: Option<u32>,
    },
    /// doc 50 §4.6 A6: session = Pane の Mode（見え方）切替要求。名札の kind badge が撃つ。
    /// event loop が daemon repo-proxy ask `session_set_mode` で repo に forward し、成功したら
    /// `SessionModeApplied` で WebView の roster を更新する。`mode` は "tui" | "gui"。
    ///
    /// ⚠️ 宛先は **引数で運ぶ**（session を明示）。「focus してから送る」型の分割はレース
    /// （doc 50 §4.3 の警告）。
    SessionSetMode {
        lane: String,
        session: u32,
        mode: String,
    },
    /// `session_set_mode` 成功後、WebView へ mode を反映する内部 event
    /// （`ConsoleModeApplied` と同じ async → main thread 橋渡し）。
    SessionModeApplied {
        lane: String,
        session: u32,
        mode: String,
    },
    /// 新セッション開始要求（console の New Session ボタン）。 event loop が
    /// `lane_restart` (fresh=true) で repo に forward — cc_session 破棄 = `/exit` → 手打ち
    /// `claude` の置き換え。 tui/gui 両対応（restart_lane が mode で分岐）。
    ConsoleNewSession {
        lane: String,
        /// doc 46 P2 要件 4: どの engine で作るか（agent 名。`None` = 現 focused を継承）。
        engine: Option<String>,
        /// doc 46 P2 要件 4: どの Mode で作るか（`"tui"` / `"gui"`。`None` = lane の現 Mode）。
        ///
        /// doc 46 §1.4 の途中経過: Mode は最終的に Pane の kind になるが、P2 時点では
        /// まだ lane の mode が残っている。**明示指定を受け取れるようにする**のが
        /// この field の役割で、指定が無ければ従来どおり lane の Mode を継ぐ。
        mode: Option<String>,
    },
    /// doc 39 P3: Root 切替 picker（ヘッダ chip dropdown）からの root 向け替え要求。
    /// event loop が `conversation_session_switch_root` で repo に forward（slot は対象 session の
    /// store で Resume respawn）→ session list 再取得 + demand_start で表示を追従させる。
    ConsoleSwitchRoot { lane: String, session: u64 },
    /// gui モデル切替要求（ChatView の model picker）。 event loop が
    /// `conversation_set_model` で repo に forward（**session 単位** — doc 50 session=Pane、
    /// 2026-07-27 に旧 root/lane 単位 `console_set_model` から移行）。
    /// `model` None = engine 既定に戻す。
    ConversationSetModel {
        lane: String,
        session: u64,
        model: Option<String>,
    },
    // doc 53 §11: 旧 `ConversationSessionsFetch`（session 一覧の ask 要求）は退役。roster の供給は
    // lanes snapshot 1 本になった（fetch は GUI 自身の動詞でしか撃たれず、CLI / MCP 由来の
    // session 変化が pane grid に出なかった）。
    /// doc 38 Phase 2: chat header「+」からの新 session 作成（`agent` 省略 = lane の agent）。
    /// ask `conversation_session_create`（focus は送らない = backend 既定 true）。roster の更新は
    /// server の `emit_lane_update` → lanes snapshot が運ぶ（doc 53 §11）。
    ConversationSessionCreate { lane: String, agent: Option<String> },
    /// replay demand（2026-07-24）: webview の renderer 準備完了後に撃つ消費者主導 demand。
    /// ask `conversation_demand_start` → repo が engine ensure + transcript replay を配送する。
    ConversationDemandStart { lane: String },
    /// doc 38 Phase 2: session tab click による focused 切替。ask `conversation_session_focus` →
    /// 一覧再取得 → `conversation_demand_start`（新 focused の transcript replay を発火）。
    ConversationSessionFocus { lane: String, session: u32 },
    /// doc 38 Phase 3: session tab の × による close。ask `conversation_session_remove` →
    /// 一覧再取得 → `conversation_demand_start`（除去後の新 focused の会話を replay）。最後の 1 本は
    /// backend が Err で拒否（GUI も × は 2 本以上でしか出さない）。session は lane 名に埋めず
    /// 常に別 field で運ぶ（doc 38 落とし穴①）。
    ConversationSessionRemove { lane: String, session: u32 },
    /// doc 38 Phase 2: 「+」menu の engine 選択肢を埋める agents 一覧取得。
    /// ask `agents_list` → `Agents` で push back。
    /// doc 47 §6: `req` = webview が採番した相関 id。`vp:conversation-agents` は複数の「+」menu が
    /// 購読する共有 bus なので、要求元をそのまま往復させて応答側で振り分けさせる
    /// （Rust は中身を解釈しない不透明な札）。
    AgentsFetch { lane: String, req: Option<String> },
    // doc 53 §11: 旧 `ConversationSessionList`（ask 結果の push back）は退役。roster は LanesLoaded で
    // snapshot から直接 webview へ渡す（`push_session_list`）。
    /// doc 38 Phase 2: `agents_list` の結果を「+」menu へ push back する内部 event。
    /// doc 47 §6: `req` は `AgentsFetch` から持ち回った相関 id（そのまま JS へ返す）。
    Agents {
        lane: String,
        payload: serde_json::Value,
        req: Option<String>,
    },
    /// R sidebar の debug log（sidebar view modes、2026-08-01）: webview からの tail 購読要求。
    /// `source` = "app" | "daemon"（file への解決は `debug_log::log_path`）。
    /// 最後の watch が勝つ = 単一 tail（source 切替も watch の送り直し）。
    DebugLogWatch { source: String },
    /// shell (L sidebar | main | R sidebar) の形が確定した（drag 終了 / form 切替 / R 開閉）。
    ///
    /// ⚠️ **確定時のみ**送られる。pointermove ごとに撃つと window resize と同じ頻度で
    /// session.json を書くことになる（webview 側で pointerup まで抑えている）。
    ShellLayout {
        sidebar_width: f64,
        right_sidebar_width: f64,
        /// `"full"` | `"slim"`。未知値は Rust 側で `full` に倒す。
        sidebar_form: String,
        right_sidebar_open: bool,
    },
    /// tail 購読の停止（R sidebar を閉じた）。見ていない log は読み続けない。
    DebugLogUnwatch,
    /// tail thread からの 1 chunk。event loop が push envelope `debuglog:lines` で webview へ流す。
    /// `reset` = 表示を捨てて置き換え（watch 開始の backlog / rotate 検出）。
    /// `generation` = 発生元 tail の世代。event loop が現世代と照合し、退場直前の旧 thread が
    /// 送った残 chunk を棄てる（旧行が新表示へ 1 回混ざる race の封じ）。
    DebugLogChunk {
        source: String,
        reset: bool,
        lines: Vec<String>,
        generation: u64,
    },
}

/// xterm.js から IPC で送られてきた JSON メッセージを処理
///
/// Phase 2.x-d (per-Lane instance + browser native WS): `in` / `resize` は Lane WebSocket が
/// browser native で repo に直接送信するので、 Rust 経路は使わない (silent no-op)。
/// `ready` も per-Lane instance ごとに発火するが、 Rust 側で flush するものは無い (no-op)。
/// 残り `copy` / `debug` / `slot:rect` を処理する thin wrapper。
/// chat 動詞の宛先 session を IPC payload から読む（doc 50 P2、additive）。
/// 省略 / 型不正は None = lane の focused（repo 側 payload_session_key と同じ後方互換）。
fn parse_session(parsed: &serde_json::Value) -> Option<u32> {
    parsed
        .get("session")
        .and_then(|v| v.as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n >= 1)
}

pub fn handle_ipc_message(msg: &str, proxy: &EventLoopProxy<AppEvent>) {
    let parsed: serde_json::Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("terminal IPC JSON パース失敗: {}", e);
            return;
        }
    };

    match parsed.get("t").and_then(|v| v.as_str()) {
        Some("ready") => {
            // webview の全 install が済んだ合図（`entry.tsx` が `openDispatch` →
            // `installTerm` → `installSlotRect` の直後に 1 度だけ撃つ）。Rust は現在の状態を
            // 丸ごと撃ち直す（`AppEvent::WebviewReady` の handler が SSOT）。
            //
            // ⚠️ 外から「GUI が使える状態になった」を待つ信号は **webview 側の
            // `console.info("[vp-bundle] ready")`**（console bridge が `target="webview"` で
            // 必ずログに出す）。ここで `tracing::info!` を足しても出ない — default filter が
            // `vp_app::terminal=warn` で、PTY hot path の洪水を防ぐため意図的に絞ってある。
            tracing::debug!("webview ready");
            let _ = proxy.send_event(AppEvent::WebviewReady);
        }
        // terminal S4 (doc 27 §4.1): xterm onData / resize → per-lane terminal session →
        // canvas channel 上り request で repo へ。 lane 必須、 data は base64 (write のみ)。
        Some("term:write") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let data = parsed.get("data").and_then(|v| v.as_str());
            if let (Some(lane), Some(data)) = (lane, data) {
                let _ = proxy.send_event(AppEvent::TerminalWrite {
                    lane: lane.to_string(),
                    // doc 50 §4.6 A6: 打った xterm の session（省略 = root。slot 系の None は
                    // root 解決 = `payload_session_key` の規律。0 を「未指定」の印に使う）。
                    session: parse_session(&parsed).unwrap_or(0),
                    data: data.to_string(),
                });
            }
        }
        Some("term:resize") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let cols = parsed.get("cols").and_then(|v| v.as_u64());
            let rows = parsed.get("rows").and_then(|v| v.as_u64());
            if let (Some(lane), Some(cols), Some(rows)) = (lane, cols, rows) {
                let _ = proxy.send_event(AppEvent::TerminalResize {
                    lane: lane.to_string(),
                    session: parse_session(&parsed).unwrap_or(0),
                    cols: cols as u16,
                    rows: rows as u16,
                });
            }
        }
        // Conversation gui (doc 32): ChatPane からのプロンプト投入。 lane + prompt 必須。
        Some("conversation:submit") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let prompt = parsed.get("prompt").and_then(|v| v.as_str());
            if let (Some(lane), Some(prompt)) = (lane, prompt) {
                let _ = proxy.send_event(AppEvent::ConversationSubmit {
                    lane: lane.to_string(),
                    prompt: prompt.to_string(),
                    session: parse_session(&parsed),
                    // 添付画像（2026-08-30）。webview が clipboard から base64 化して運ぶ。
                    // 中身の検査は repo 側 `parse_image_inputs` に任せる（判定を 2 箇所に置かない）。
                    images: parsed
                        .get("images")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default(),
                });
            }
        }
        // Conversation gui HITL (doc 35 PR1): PromptCard の回答。 lane + request_id 必須。
        // answers（allow）or behavior+message（deny）を運ぶ。
        Some("conversation:respond") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let request_id = parsed.get("request_id").and_then(|v| v.as_str());
            if let (Some(lane), Some(request_id)) = (lane, request_id) {
                let _ = proxy.send_event(AppEvent::ConversationRespond {
                    lane: lane.to_string(),
                    request_id: request_id.to_string(),
                    session: parse_session(&parsed),
                    answers: parsed.get("answers").cloned(),
                    behavior: parsed
                        .get("behavior")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    message: parsed
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                });
            }
        }
        // Conversation gui HITL (doc 35 §5 / PR2): 実行中 turn の中断。 lane 必須。
        Some("conversation:interrupt") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                let _ = proxy.send_event(AppEvent::ConversationInterrupt {
                    lane: lane.to_string(),
                    session: parse_session(&parsed),
                });
            }
        }
        // Conversation gui HITL (doc 35 §2.5 / PR3): permission mode 切替。 lane + mode 必須。
        Some("conversation:set_permission_mode") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let mode = parsed.get("mode").and_then(|v| v.as_str());
            if let (Some(lane), Some(mode)) = (lane, mode) {
                let _ = proxy.send_event(AppEvent::ConversationSetPermissionMode {
                    lane: lane.to_string(),
                    mode: mode.to_string(),
                    session: parse_session(&parsed),
                });
            }
        }
        // doc 50 §4.6 A6: 名札 kind badge からの Mode 切替。lane / session / mode 必須
        // （session は明示のみ — root 決め打ちにしない = 誤配送を黙って起こさない）。
        Some("session:set_mode") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let session = parsed.get("session").and_then(serde_json::Value::as_u64);
            let mode = parsed.get("mode").and_then(|v| v.as_str());
            if let (Some(lane), Some(session), Some(mode)) = (lane, session, mode) {
                let _ = proxy.send_event(AppEvent::SessionSetMode {
                    lane: lane.to_string(),
                    session: session as u32,
                    mode: mode.to_string(),
                });
            } else {
                tracing::warn!("session:set_mode skip — lane / session / mode が揃っていない");
            }
        }
        // 新セッション開始（console の New Session ボタン）。 lane 必須。
        Some("console:new_session") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                // doc 46 P2 要件 4: engine / mode は **任意**。省略時は従来の継承挙動
                // （現 focused の engine / lane の Mode）。空文字は未指定に畳む —
                // menu の「既定」項目が空文字を送っても継承にしたい。
                let opt = |k: &str| {
                    parsed
                        .get(k)
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                let _ = proxy.send_event(AppEvent::ConsoleNewSession {
                    lane: lane.to_string(),
                    engine: opt("engine"),
                    mode: opt("mode"),
                });
            }
        }
        // doc 39 P3: Root 切替（ヘッダ chip dropdown）。 lane / session 必須。
        Some("console:switch_root") => {
            if let (Some(lane), Some(session)) = (
                parsed.get("lane").and_then(|v| v.as_str()),
                parsed.get("session").and_then(|v| v.as_u64()),
            ) {
                let _ = proxy.send_event(AppEvent::ConsoleSwitchRoot {
                    lane: lane.to_string(),
                    session,
                });
            }
        }
        // gui モデル切替（ChatView の model picker）。 lane / session 必須、 model 省略/null =
        // engine 既定。session を運ばない要求は捨てる（root 決め打ちに丸めない — server 側
        // `conversation_set_model` と同じ規律）。
        Some("conversation:set_model") => {
            if let (Some(lane), Some(session)) = (
                parsed.get("lane").and_then(|v| v.as_str()),
                parsed.get("session").and_then(|v| v.as_u64()),
            ) {
                let model = parsed
                    .get("model")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let _ = proxy.send_event(AppEvent::ConversationSetModel {
                    lane: lane.to_string(),
                    session,
                    model,
                });
            }
        }
        // doc 38 Phase 2: session tab strip。lane は常に別 field で運び、session を lane 名に
        // 埋めない（doc 38 落とし穴①）。作成 / focused 切替 / agents 取得。
        // 一覧取得（`echoes:sessions_fetch`）は doc 53 §11 で退役 — roster は snapshot が運ぶ。
        Some("conversation:session_create") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                // agent 省略 = lane の agent（backend 既定）。
                let agent = parsed
                    .get("agent")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let _ = proxy.send_event(AppEvent::ConversationSessionCreate {
                    lane: lane.to_string(),
                    agent,
                });
            }
        }
        Some("conversation:session_focus") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let session = parsed.get("session").and_then(|v| v.as_u64());
            if let (Some(lane), Some(session)) = (lane, session) {
                let _ = proxy.send_event(AppEvent::ConversationSessionFocus {
                    lane: lane.to_string(),
                    session: session as u32,
                });
            }
        }
        // replay demand（2026-07-24）: webview が renderer を張った直後に撃つ「消費者主導」の
        // demand。Rust attach 時 demand の boot 窓取りこぼし（bundle 読込前配送 = silent drop）
        // を埋める。⚠️ app.rs の is_main_ipc_tag allowlist と両方更新（片側だと sidebar IPC へ
        // 流れて silent drop — 2026-07-16 の「+」無反応 regression と同じ罠）。
        Some("conversation:demand_start") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                let _ = proxy.send_event(AppEvent::ConversationDemandStart {
                    lane: lane.to_string(),
                });
            }
        }
        // doc 38 Phase 3: session tab の × による close。lane + session 必須。
        Some("conversation:session_remove") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let session = parsed.get("session").and_then(|v| v.as_u64());
            if let (Some(lane), Some(session)) = (lane, session) {
                let _ = proxy.send_event(AppEvent::ConversationSessionRemove {
                    lane: lane.to_string(),
                    session: session as u32,
                });
            }
        }
        Some("conversation:agents_fetch") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                // doc 47 §6: 要求元の相関 id（省略可 = 応答を誰も拾わない）。
                let req = parsed
                    .get("req")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let _ = proxy.send_event(AppEvent::AgentsFetch {
                    lane: lane.to_string(),
                    req,
                });
            }
        }
        Some("ink:snapshot") => {
            // ink（対話面、doc 52 §3）: board pane（#ink-stage）の rect を WKWebView.takeSnapshot で
            // 撮り PNG 化する要求。rect は webview 論理座標（getBoundingClientRect）= WKWebView の
            // 座標系そのままなので Retina 換算不要。結果は app.rs が `ink:snapshot` で返す。
            if let Some(r) = parsed.get("rect") {
                let get = |k: &str| r.get(k).and_then(|v| v.as_f64());
                if let (Some(x), Some(y), Some(w), Some(h)) =
                    (get("x"), get("y"), get("w"), get("h"))
                {
                    let _ = proxy.send_event(AppEvent::InkSnapshot {
                        rect: crate::ink_snapshot::InkRect { x, y, w, h },
                    });
                }
            }
        }
        // R sidebar の debug log（sidebar view modes、2026-08-01）: tail の購読開始 / 停止。
        // watch は source 必須（"app" | "daemon"）。file への解決と thread 管理は app.rs 側。
        Some("debuglog:watch") => {
            if let Some(source) = parsed.get("source").and_then(|v| v.as_str()) {
                let _ = proxy.send_event(AppEvent::DebugLogWatch {
                    source: source.to_string(),
                });
            }
        }
        // shell layout（L sidebar | main | R sidebar の形）の確定通知。
        // 値の検証（範囲外の clamp）は session_state 側の setter が持つ — ここは運ぶだけ。
        Some("shell:layout") => {
            let num = |k: &str| parsed.get(k).and_then(|v| v.as_f64());
            if let (Some(sidebar_width), Some(right_sidebar_width)) =
                (num("sidebar_width"), num("right_sidebar_width"))
            {
                let _ = proxy.send_event(AppEvent::ShellLayout {
                    sidebar_width,
                    right_sidebar_width,
                    sidebar_form: parsed
                        .get("sidebar_form")
                        .and_then(|v| v.as_str())
                        .unwrap_or("full")
                        .to_string(),
                    right_sidebar_open: parsed
                        .get("right_sidebar_open")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                });
            }
        }
        Some("debuglog:unwatch") => {
            let _ = proxy.send_event(AppEvent::DebugLogUnwatch);
        }
        Some("copy") => {
            // navigator.clipboard が使えなかった時の fallback: arboard で OS clipboard 直書き
            if let Some(data) = parsed.get("d").and_then(|v| v.as_str()) {
                match arboard::Clipboard::new() {
                    Ok(mut cb) => match cb.set_text(data) {
                        Ok(_) => {
                            tracing::info!("[clipboard] copy via arboard: {} chars", data.len())
                        }
                        Err(e) => tracing::warn!("[clipboard] arboard set_text failed: {}", e),
                    },
                    Err(e) => tracing::warn!("[clipboard] arboard init failed: {}", e),
                }
            }
        }
        Some("open-url") => {
            // console (xterm) の link を cmd/ctrl+click した時の OS default browser 起動。
            // webview 内遷移 (window.open) を避け、 Rust から native open する。
            // 安全: webview 由来の URL を無検証で OS に渡すと file:// 等の scheme を
            // 開かせる隙になるため http(s) のみ許可 (多層防御、 linkify 側も http(s) 限定)。
            if let Some(url) = parsed.get("url").and_then(|v| v.as_str()) {
                if url.starts_with("http://") || url.starts_with("https://") {
                    match webbrowser::open(url) {
                        Ok(_) => tracing::info!("[link] open in browser: {}", url),
                        Err(e) => {
                            tracing::warn!("[link] webbrowser::open failed: {} ({})", url, e)
                        }
                    }
                } else {
                    tracing::warn!("[link] 非 http(s) scheme は open しない: {}", url);
                }
            }
        }
        Some("paste:request") => {
            // Phase 4-paste-fix: navigator.clipboard.readText() が webview で permission denied する
            // ケースの fallback。 arboard で OS clipboard を読んで AppEvent::PasteText で main thread
            // に届ける → event loop が `lane_js::deliver_paste` で `term:paste` を push。
            let text = match arboard::Clipboard::new() {
                Ok(mut cb) => match cb.get_text() {
                    Ok(t) => {
                        tracing::info!("[clipboard] paste via arboard: {} chars", t.len());
                        t
                    }
                    Err(e) => {
                        tracing::warn!("[clipboard] arboard get_text failed: {}", e);
                        String::new()
                    }
                },
                Err(e) => {
                    tracing::warn!("[clipboard] arboard init (paste) failed: {}", e);
                    String::new()
                }
            };
            let _ = proxy.send_event(AppEvent::PasteText(text));
        }
        Some("debug") => {
            if let Some(msg) = parsed.get("msg").and_then(|v| v.as_str()) {
                tracing::info!("[xterm debug] {}", msg);
            }
        }
        // Phase 5-D Sprint C P2.1: per-Lane HD notification (OSC 99 final-chunk + a=focus 起源)。
        // 「user attention 要求」 を sidebar の unread count として蓄積する経路。
        Some("osc:notification") => {
            let lane = parsed
                .get("lane")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let code = parsed.get("code").and_then(|v| v.as_u64()).unwrap_or(99) as u32;
            if let Some(lane) = lane {
                let _ = proxy.send_event(AppEvent::OscNotification { lane, code });
            }
        }
        // VP-100 γ-light: main area の active slot 矩形通知 (ResizeObserver から)
        Some("slot:rect") => {
            let pane_id = parsed
                .get("pane_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let kind = parsed
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("empty")
                .to_string();
            if let Some(rect_v) = parsed.get("rect") {
                let rect = crate::main_area::SlotRect {
                    x: rect_v.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    y: rect_v.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    w: rect_v.get("w").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    h: rect_v.get("h").and_then(|v| v.as_f64()).unwrap_or(0.0),
                };
                let _ = proxy.send_event(AppEvent::SlotRect {
                    pane_id,
                    kind,
                    rect,
                });
            }
        }
        Some("board:delete") => {
            // board モデル: thumbnail ✕。 repo の board_delete_item に forward（app.rs で Daemon ask）。
            let _ = proxy.send_event(AppEvent::BoardMutate {
                method: "board_delete_item".to_string(),
                body: parsed.clone(),
            });
        }
        Some("board:clear") => {
            // board モデル: Clear ボタン。 repo の board_clear に forward。
            let _ = proxy.send_event(AppEvent::BoardMutate {
                method: "board_clear".to_string(),
                body: parsed.clone(),
            });
        }
        Some("board:cursor") => {
            // cursor の server 昇格（doc 52 §5 計器盤）: thumbnail click / scrollback の注視を
            // repo の board_set_cursor へ forward（app.rs で Daemon ask → BoardUpdated 再配信）。
            let _ = proxy.send_event(AppEvent::BoardMutate {
                method: "board_set_cursor".to_string(),
                body: parsed.clone(),
            });
        }
        // ===== code pane（コードブラウザ P1）=====
        // ⚠️ app.rs `is_main_ipc_tag` の allowlist と対（片側更新は sidebar IPC へ silent drop）。
        Some("code:list") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                let _ = proxy.send_event(AppEvent::CodeList {
                    lane: lane.to_string(),
                });
            }
        }
        Some("code:read") => {
            if let (Some(lane), Some(rel_path)) = (
                parsed.get("lane").and_then(|v| v.as_str()),
                parsed.get("rel_path").and_then(|v| v.as_str()),
            ) {
                let _ = proxy.send_event(AppEvent::CodeRead {
                    lane: lane.to_string(),
                    rel_path: rel_path.to_string(),
                });
            }
        }
        // console bridge: webview の console.* を vp-app log (app.kdl.log) に転送する。
        // agent が DevTools を開かず log Read で webview console を観測する経路。
        Some("console") => {
            let level = parsed
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("log");
            let text = parsed.get("text").and_then(|v| v.as_str()).unwrap_or("");
            match level {
                "error" => tracing::error!(target: "webview", "{}", text),
                "warn" => tracing::warn!(target: "webview", "{}", text),
                // console.debug は DEBUG に落とす (RUST_LOG=info 運用で log を汚さない)
                "debug" => tracing::debug!(target: "webview", "{}", text),
                _ => tracing::info!(target: "webview", "{}", text),
            }
        }
        other => {
            tracing::debug!("terminal IPC: unknown type {:?}", other);
        }
    }
}
