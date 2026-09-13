//! `AppEvent` — tao の EventLoop に流す app 全体の event（UI thread が受け取る「出来事」）。
//!
//! 旧 `terminal.rs` が所有していたが、sidebar / conversation / device 由来の variant が大半で
//! terminal 固有ではないため独立 module に移設（doc 11 §5 Q4、棚卸し 項目 6 / 6-0、2026-09-08）。
//! 送り手は `EventLoopProxy<AppEvent>` を持つ各 sibling（購読 pump / poller / IPC handler）、
//! 受け手は `app::run()` の event loop。`Clone` を derive しているため `EditorCommand` は
//! `oneshot` でなく `mpsc::UnboundedSender` を運ぶ。

/// EventLoop に送る app 全体のイベント
///
/// Phase 2.x-d: PTY-related variant (Output/XtermReady) は撤去。
/// Lane terminals は per-Lane の browser-native WebSocket で input/output を扱う。
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// daemon から Repo list 取得成功 (= `fetch_repos_with_ports` 経由で runtime port 込み)。
    ReposLoaded(Vec<crate::daemon_wire::RepoInfo>),
    /// daemon への接続失敗 (= daemon 未起動 / network エラー)。
    ReposError(String),
    /// VP-95: Activity widget の定期更新 payload
    ActivityUpdate(crate::pane::ActivitySnapshot),
    /// VP-95: sidebar webview からの IPC メッセージ (JSON 文字列、main loop でパース)
    SidebarIpc(String),
    /// doc 48 Phase 2 (editor bridge): daemon からの `editor_command` を webview で評価する。
    ///
    /// 購読側（`daemon/subscriptions`）は **op と引数だけ**を運び、JS の組み立て
    /// （`webview::editor_bridge::editor_bridge_js`）と評価は UI 側（`app/on_board`）が行う
    /// （doc 60 §2 の依存 rule: daemon/ は webview/ を呼ばない。6-2 PR-EX で解消）。
    /// 結果（wry が JSON 文字列化した評価値、未知 op なら `{"error":…}` の JSON）を `resp` に 1 回送る。
    /// sender が mpsc なのは AppEvent の Clone derive と両立させるため (oneshot は Clone 不可)。
    /// 受け手は `run_canvas_session` の editor_command intercept (timeout 側が受信を打ち切る)。
    EditorCommand {
        op: String,
        field_id: Option<String>,
        value: Option<serde_json::Value>,
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
        rect: crate::webview::main_area::SlotRect,
    },
    /// VP-100 follow-up: muda メニュー項目クリック (developer mode toggle / open devtools 等)
    MenuClicked(muda::MenuId),
    /// Phase A4-3b: repo (= Runtime Process) の `/api/lanes` を fetch して Lane list を main thread に通知
    /// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Process recursive)
    LanesLoaded {
        repo_path: String,
        lanes: Vec<crate::daemon_wire::LaneInfo>,
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
    /// 設定ページの「Add Repo 初期フォルダ」picker の結果（doc 59 P1）。
    /// ⚠️ **キャンセルは `None`** = 既存値を保持する（「選ばなかった」を「空にした」と
    /// 取り違えると設定が黙って消える）。
    SettingsRepoRootPicked(Option<String>),
    /// daemon 側（settings.kdl）の設定を引き終えた（doc 59 P3）。
    ///
    /// `None` = **daemon に届かなかった**（オフライン / `settings/get` を知らない旧 binary）。
    /// 未設定（= 空 object）と区別する必要がある — 前者は「編集できない」、後者は
    /// 「既定で動いている」で、UI の出し方が変わる。
    /// 生 JSON で運ぶのは、daemon 応答の形の持ち主を `vantage-point` 側に留めるため。
    SettingsDaemonFetched(Option<serde_json::Value>),
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
    /// repo-proxy ask (`agents_list`) を叩いた結果がここに乗る。 JS は sidebar の `agents:result`
    /// で受領し、 dropdown を populate する。 `error` Some なら fetch 失敗、 dropdown は
    /// disabled + error message 表示。
    AgentsResult {
        repo_path: String,
        agents: Vec<crate::daemon_wire::AgentInfo>,
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
    InkSnapshot {
        rect: crate::webview::ink_snapshot::InkRect,
    },
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
    /// `code:list` の walk 結果 → `push_main::code_entries` で main webview へ push。
    CodeEntriesResult {
        lane: String,
        entries: Vec<crate::webview::file_explorer::Entry>,
        truncated: bool,
    },
    /// `code:read` の読み結果 → `push_main::code_file` で main webview へ push。
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
    ConversationCodexInput {
        lane: String,
        session: u32,
        thread_id: String,
        request_id: String,
        action: serde_json::Value,
    },
    ConversationSubmit {
        request_id: String,
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
        effort: Option<String>,
        request_id: Option<String>,
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
