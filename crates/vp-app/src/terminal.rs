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
    /// TheWorld から Project list 取得成功 (= `fetch_projects_with_ports` 経由で runtime port 込み)。
    ProjectsLoaded(Vec<crate::client::ProjectInfo>),
    /// TheWorld への接続失敗 (= daemon 未起動 / network エラー)。
    ProjectsError(String),
    /// VP-95: Activity widget の定期更新 payload
    ActivityUpdate(crate::pane::ActivitySnapshot),
    /// VP-95: sidebar webview からの IPC メッセージ (JSON 文字列、main loop でパース)
    SidebarIpc(String),
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
    /// Phase A4-3b: SP (= Runtime Process) の `/api/lanes` を fetch して Lane list を main thread に通知
    /// 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Process recursive)
    LanesLoaded {
        process_path: String,
        lanes: Vec<crate::client::LaneInfo>,
        /// doc 44 D4: この project の開発起点 lane 名（Host の帳簿が解決した値）。
        ///
        /// `None` = snapshot に載っていなかった（旧 server / 解決不能）。受け手は
        /// **前回値を保つ** — 既定値に落とすと、起点を指定済の project で ⭐ が明滅する。
        origin: Option<String>,
    },
    /// Phase A4-3b: Lane fetch 失敗 (SP 未起動 / 接続失敗)
    LanesError {
        process_path: String,
        message: String,
    },
    /// オンデマンド respawn (maybe_respawn_dead_lane) の restart_lane が失敗した通知。
    /// event loop で lane_respawn_triggered から address を除去し、 次の Dead 検出で
    /// 再 respawn できるようにする (失敗が永続 suppression にならないための解除通知)。
    LaneRespawnFailed { address: String },
    /// Wire inbox (doc 34 §4 V1): World "wire" channel への read-only fetch 結果。
    /// event loop が `window.vpWire.handleResult(payload)` で sidebar に push back する。
    /// payload = `{address, agent, history, unread}` (エラーは `{address, error}`)。
    WireHistoryResult {
        address: String,
        payload: serde_json::Value,
    },
    /// Clone 先フォルダ picker で選択された path を sidebar JS に push (キャンセル時は None)
    ClonePathPicked(Option<String>),
    /// Phase 4-paste-fix: clipboard paste request の応答。 OS clipboard の内容を JS に届ける。
    /// 空文字なら paste skip。 main_view の `window.deliverPaste(text)` で active Lane の xterm に inject。
    PasteText(String),
    /// Phase 5-D Sprint C P2.1: Lane HD notification 通知 (OSC 99 final-chunk + a=focus)。
    /// main_area xterm.js が capture → Rust が SidebarState の per-Lane unread count を加算 →
    /// sidebar に push back → badge UI 表示。 active lane への switch で 0 reset。
    OscNotification { lane: String, code: u32 },
    /// R5 Performer create flow: Add Performer form が送信した `lane:add_performer` の結果を sidebar に
    /// push back する。 `error` Some の時 form 下に inline error 表示、 None の時 form を閉じる。
    /// 例: 名前重複 (CONFLICT)、 lane clone 失敗、 SP 未起動 等。
    PerformerCreateResult {
        project_path: String,
        name: String,
        error: Option<String>,
    },
    /// doc 11 PR-C / F6④: 利用可能 Stand 一覧を sidebar に push back する。
    /// `+ Add Performer` form 開閉時に JS から `stands:fetch` が来て、 Rust 側で World
    /// process-proxy ask (`stands_list`) を叩いた結果がここに乗る。 JS は `window.handleStandsResult`
    /// で受領し、 dropdown を populate する。 `error` Some なら fetch 失敗、 dropdown は
    /// disabled + error message 表示。
    StandsResult {
        project_path: String,
        stands: Vec<crate::client::StandInfo>,
        error: Option<String>,
    },
    /// VP-140: main_area JS が DOMContentLoaded 後に送る lane catch-up 要求。
    /// 起動初期の `evaluate_script` race (WebView HTML 未 load 時に Rust 側 ensureLane 発行が
    /// silent drop される) の救済策として、 JS 側が ready になったタイミングで Rust に再発行を
    /// 要求する。 Rust は `sidebar_state.lanes_by_project` 全 lane に対して ensureLane + 現在
    /// active lane に showLane を再発行する (idempotent)。
    LanesEnsureAll,
    /// VP-143: 全 lane の cc session display name (custom-title) を再 resolve する周期 tick。
    /// `tokio::spawn` で 5s 間隔の background task が proxy 経由で send。 main thread は
    /// `sidebar_state.lanes_by_project` を walk して `session_title::resolve_title_for_cwd` を
    /// 呼び、 結果を `sidebar_state.session_titles` に diff/update + sidebar に push back する。
    ResolveSessionTitles,
    /// VP-147 PR-P2-3: 全 lane の mailbox inbox 状況を再 resolve する周期 tick。
    /// `spawn_lane_inbox_poller` (5s 間隔) が proxy 経由で send。 main thread は
    /// `sidebar_state.lanes_by_project` を walk して各 lane の MessageState を build し、
    /// `sidebar_state.lane_inboxes` に diff/update + sidebar に push back する。
    /// Phase 2 (icon visibility のみ) では active Lane に対して placeholder MessageState
    /// (= 0 件 default) を populate し、 sidebar UI で `.vp-message-icon` を表示するための
    /// signal として機能。 unread_count / has_persistent / last_msg_ts の actual 値は
    /// 後続 PR で backend peek API + Whitesnake query を実装して populate。
    ResolveLaneInboxes,
    /// Sidebar File Explorer: `files:list` の blocking walk 結果を sidebar webview に
    /// push back する。 `window.vpFiles.handleListResult({address,entries,truncated})` で
    /// receive される。
    FilesListResult {
        address: String,
        entries: Vec<crate::file_explorer::Entry>,
        truncated: bool,
    },
    /// Sidebar File Explorer: `files:open` の blocking file 読み込み結果。
    /// `content` は `canvas-handler.ts:22-28` の `ShowMessage::content` shape
    /// (`{markdown}` | `{log}` | `{html}` のいずれか)。 main_view に
    /// `window.vpCanvas.handleMessage({type:'show',pane_id:'main',content,append:false})` で注入。
    FilesOpenResult { content: serde_json::Value },
    /// wiremsg Stage 2: SP の "canvas" Unison channel から受信した Canvas (Paisley Park)
    /// ProcessMessage 1 件。`message` は ProcessMessage の生 JSON (`{"type":"show",...}` 等)。
    /// handler は active project の分のみ main_view WebView に転送する。
    CanvasMessage {
        process_path: String,
        message: serde_json::Value,
    },
    /// Bastet 🧲 device event (DeviceConnected / DeviceDisconnected / ControlEvent)。
    /// daemon "world-device" Unison channel から受信した `DeviceEvent` の生 JSON。
    /// Phase 1 handler は tracing で log。 Phase 2 で Bastet pane / sidebar に反映予定。
    DeviceEvent { payload: serde_json::Value },
    /// board モデル (2026-07-15): WebView からの board mutate（thumbnail ✕ / Clear ボタン）。
    /// `method` = "board_delete_item" | "board_clear"、 `body` は IPC payload の生 JSON
    /// (scope / lane / item_id 等)。 active project の SP に World process-proxy ask で forward し、
    /// SP が DB 更新 → BoardUpdated(retained) broadcast → canvas channel で webview に反映する。
    /// board は SP が truth を持つため、 webview 側の save/load 経路（旧 PpState*）は撤去した。
    BoardMutate {
        method: String,
        body: serde_json::Value,
    },
    /// terminal S4 (doc 27 §4.1): per-lane terminal session が World canvas channel から受信した
    /// PTY 出力 1 chunk。 `data` は base64 (LaneTerminalOutput.data)。 event loop が
    /// `window.vpTerminal.handleOutput(lane, data)` で当該 lane の xterm に inject する。
    TerminalOutput { lane: String, data: String },
    /// terminal S4: WebView (xterm onData) からの入力。 `data` は base64。 event loop が
    /// 当該 lane の terminal session に渡し、 canvas channel 上り request `terminal_write` で SP へ。
    TerminalWrite { lane: String, data: String },
    /// terminal S4: WebView からの resize。 event loop が当該 lane の terminal session に渡し、
    /// canvas channel 上り request `terminal_resize` で SP へ。
    TerminalResize { lane: String, cols: u16, rows: u16 },
    /// Echoes Act II (doc 32): 当該 lane の echoes session が World canvas channel から受信した
    /// 構造化イベント 1 件。 `event` は EchoesEvent の生 JSON (`{"kind":"message_chunk",...}`)。
    /// event loop が `window.vpConsole.handleEvent(lane, event, session)` で当該 lane の Console pane に渡す。
    /// doc 38 Phase 2: `session` = 発生元 session の VP 採番 key（1 Lane = N session）。topic の
    /// `ProcessMessage::EchoesEvent::session`（serde default=1）由来。session は lane 名に埋めず
    /// 常に別 field で運ぶ（doc 38 落とし穴①）。
    EchoesEvent {
        lane: String,
        event: serde_json::Value,
        session: u32,
    },
    /// Echoes Act II: WebView (EchoesChatPane) からのプロンプト投入。 event loop が当該 lane の
    /// echoes session を lazy spawn し、 canvas channel 上り request `echoes_submit` で SP へ。
    EchoesSubmit { lane: String, prompt: String },
    /// Echoes Act II HITL (doc 35 PR1): PromptCard の回答。 event loop が当該 lane の echoes
    /// session へ渡し、 canvas channel 上り request `echoes_respond` で SP へ。 `request_id` は
    /// Question event 由来の control_response マッチング用。 allow は `answers`、 deny は
    /// `behavior="deny"`+`message` を運ぶ（どちらか）。
    EchoesRespond {
        lane: String,
        request_id: String,
        answers: Option<serde_json::Value>,
        behavior: Option<String>,
        message: Option<String>,
    },
    /// Echoes Act II HITL (doc 35 §5 / PR2): 実行中 turn の中断（stop ボタン / Esc）。
    /// event loop が当該 lane の echoes session へ渡し、`echoes_interrupt` で SP へ。
    EchoesInterrupt { lane: String },
    /// Echoes Act II HITL (doc 35 §2.5 / PR3): permission mode 動的切替。event loop が当該 lane の
    /// echoes session へ渡し、`echoes_set_permission_mode` で SP へ。`mode` = "default"|"bypassPermissions" 等。
    EchoesSetPermissionMode { lane: String, mode: String },
    /// doc 33 C2: Console のエンジンモード切替要求（Act toggle）。 event loop が World
    /// process-proxy ask `console_set_mode` で SP に forward し、成功したら vpConsole.setMode で
    /// WebView の表示を切替える。 `mode` は "tui" | "chat"。
    ConsoleSetMode { lane: String, mode: String },
    /// doc 33 C2: console_set_mode 成功後、WebView へ mode を反映する内部 event
    /// (async task → main thread の evaluate_script 橋渡し)。
    ConsoleModeApplied { lane: String, mode: String },
    /// 新セッション開始要求（console の New Session ボタン）。 event loop が
    /// `lane_restart` (fresh=true) で SP に forward — cc_session 破棄 = `/exit` → 手打ち
    /// `claude` の置き換え。 Act I/II 両対応（restart_lane が mode で分岐）。
    ConsoleNewSession {
        lane: String,
        /// doc 46 P2 要件 4: どの engine で作るか（stand 名。`None` = 現 focused を継承）。
        engine: Option<String>,
        /// doc 46 P2 要件 4: どの Act で作るか（`"tui"` / `"chat"`。`None` = lane の現 Act）。
        ///
        /// doc 46 §1.4 の途中経過: Act は最終的に Pane の kind になるが、P2 時点では
        /// まだ lane の mode が残っている。**明示指定を受け取れるようにする**のが
        /// この field の役割で、指定が無ければ従来どおり lane の Act を継ぐ。
        act: Option<String>,
    },
    /// lane_restart(fresh=true) 成功後、WebView の会話表示をクリアする内部 event
    /// (ConsoleModeApplied と同じ async → main thread 橋渡し)。
    ConsoleSessionRenewed { lane: String },
    /// doc 39 P3: Root 切替 picker（ヘッダ chip dropdown）からの root 向け替え要求。
    /// event loop が `echoes_session_switch_root` で SP に forward（slot は対象 session の
    /// store で Resume respawn）→ session list 再取得 + demand_start で表示を追従させる。
    ConsoleSwitchRoot { lane: String, session: u64 },
    /// Act II モデル切替要求（ChatView の model picker）。 event loop が
    /// `console_set_model` で SP に forward。 `model` None = claude default に戻す。
    ConsoleSetModel { lane: String, model: Option<String> },
    /// doc 38 Phase 2: session tab strip の一覧取得要求（chat attach / 表示時）。
    /// event loop が World process-proxy ask `echoes_session_list` → `EchoesSessionList` で push back。
    EchoesSessionsFetch { lane: String },
    /// doc 38 Phase 2: chat header「+」からの新 session 作成（`stand` 省略 = lane の stand）。
    /// ask `echoes_session_create`（focus は送らない = backend 既定 true）→ 続けて一覧を再取得して
    /// push back（作成と一覧更新を 1 task で直列に）。
    EchoesSessionCreate { lane: String, stand: Option<String> },
    /// doc 38 Phase 2: session tab click による focused 切替。ask `echoes_session_focus` →
    /// 一覧再取得 → `echoes_demand_start`（新 focused の transcript replay を発火）。
    EchoesSessionFocus { lane: String, session: u32 },
    /// doc 38 Phase 3: session tab の × による close。ask `echoes_session_remove` →
    /// 一覧再取得 → `echoes_demand_start`（除去後の新 focused の会話を replay）。最後の 1 本は
    /// backend が Err で拒否（GUI も × は 2 本以上でしか出さない）。session は lane 名に埋めず
    /// 常に別 field で運ぶ（doc 38 落とし穴①）。
    EchoesSessionRemove { lane: String, session: u32 },
    /// doc 38 Phase 2: 「+」menu の engine 選択肢を埋める stands 一覧取得。
    /// ask `stands_list` → `EchoesStands` で push back。
    /// doc 47 §6: `req` = webview が採番した相関 id。`vp:echoes-stands` は複数の「+」menu が
    /// 購読する共有 bus なので、要求元をそのまま往復させて応答側で振り分けさせる
    /// （Rust は中身を解釈しない不透明な札）。
    EchoesStandsFetch { lane: String, req: Option<String> },
    /// doc 38 Phase 2: `echoes_session_list` の結果を webview の tab strip へ push back する内部 event
    /// （async task → main thread の evaluate_script 橋渡し。`ConsoleModeApplied` と同型）。
    EchoesSessionList {
        lane: String,
        payload: serde_json::Value,
    },
    /// doc 38 Phase 2: `stands_list` の結果を「+」menu へ push back する内部 event。
    /// doc 47 §6: `req` は `EchoesStandsFetch` から持ち回った相関 id（そのまま JS へ返す）。
    EchoesStands {
        lane: String,
        payload: serde_json::Value,
        req: Option<String>,
    },
}

/// xterm.js から IPC で送られてきた JSON メッセージを処理
///
/// Phase 2.x-d (per-Lane instance + browser native WS): `in` / `resize` は Lane WebSocket が
/// browser native で SP に直接送信するので、 Rust 経路は使わない (silent no-op)。
/// `ready` も per-Lane instance ごとに発火するが、 Rust 側で flush するものは無い (no-op)。
/// 残り `copy` / `debug` / `slot:rect` を処理する thin wrapper。
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
            // per-Lane instance ごとに発火するが Rust 側で flush するものは無い (no-op)。
        }
        // terminal S4 (doc 27 §4.1): xterm onData / resize → per-lane terminal session →
        // canvas channel 上り request で SP へ。 lane 必須、 data は base64 (write のみ)。
        Some("term:write") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let data = parsed.get("data").and_then(|v| v.as_str());
            if let (Some(lane), Some(data)) = (lane, data) {
                let _ = proxy.send_event(AppEvent::TerminalWrite {
                    lane: lane.to_string(),
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
                    cols: cols as u16,
                    rows: rows as u16,
                });
            }
        }
        // Echoes Act II (doc 32): EchoesChatPane からのプロンプト投入。 lane + prompt 必須。
        Some("echoes:submit") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let prompt = parsed.get("prompt").and_then(|v| v.as_str());
            if let (Some(lane), Some(prompt)) = (lane, prompt) {
                let _ = proxy.send_event(AppEvent::EchoesSubmit {
                    lane: lane.to_string(),
                    prompt: prompt.to_string(),
                });
            }
        }
        // Echoes Act II HITL (doc 35 PR1): PromptCard の回答。 lane + request_id 必須。
        // answers（allow）or behavior+message（deny）を運ぶ。
        Some("echoes:respond") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let request_id = parsed.get("request_id").and_then(|v| v.as_str());
            if let (Some(lane), Some(request_id)) = (lane, request_id) {
                let _ = proxy.send_event(AppEvent::EchoesRespond {
                    lane: lane.to_string(),
                    request_id: request_id.to_string(),
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
        // Echoes Act II HITL (doc 35 §5 / PR2): 実行中 turn の中断。 lane 必須。
        Some("echoes:interrupt") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                let _ = proxy.send_event(AppEvent::EchoesInterrupt {
                    lane: lane.to_string(),
                });
            }
        }
        // Echoes Act II HITL (doc 35 §2.5 / PR3): permission mode 切替。 lane + mode 必須。
        Some("echoes:set_permission_mode") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let mode = parsed.get("mode").and_then(|v| v.as_str());
            if let (Some(lane), Some(mode)) = (lane, mode) {
                let _ = proxy.send_event(AppEvent::EchoesSetPermissionMode {
                    lane: lane.to_string(),
                    mode: mode.to_string(),
                });
            }
        }
        // doc 33 C2: Act toggle。 console mode 切替要求を event loop へ。
        Some("console:set_mode") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let mode = parsed.get("mode").and_then(|v| v.as_str());
            if let (Some(lane), Some(mode)) = (lane, mode) {
                let _ = proxy.send_event(AppEvent::ConsoleSetMode {
                    lane: lane.to_string(),
                    mode: mode.to_string(),
                });
            }
        }
        // 新セッション開始（console の New Session ボタン）。 lane 必須。
        Some("console:new_session") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                // doc 46 P2 要件 4: engine / act は **任意**。省略時は従来の継承挙動
                // （現 focused の engine / lane の Act）。空文字は未指定に畳む —
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
                    act: opt("act"),
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
        // Act II モデル切替（ChatView の model picker）。 lane 必須、 model 省略/null = default。
        Some("console:set_model") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                let model = parsed
                    .get("model")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let _ = proxy.send_event(AppEvent::ConsoleSetModel {
                    lane: lane.to_string(),
                    model,
                });
            }
        }
        // doc 38 Phase 2: session tab strip。lane は常に別 field で運び、session を lane 名に
        // 埋めない（doc 38 落とし穴①）。一覧取得 / 作成 / focused 切替 / stands 取得。
        Some("echoes:sessions_fetch") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                let _ = proxy.send_event(AppEvent::EchoesSessionsFetch {
                    lane: lane.to_string(),
                });
            }
        }
        Some("echoes:session_create") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                // stand 省略 = lane の stand（backend 既定）。
                let stand = parsed
                    .get("stand")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let _ = proxy.send_event(AppEvent::EchoesSessionCreate {
                    lane: lane.to_string(),
                    stand,
                });
            }
        }
        Some("echoes:session_focus") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let session = parsed.get("session").and_then(|v| v.as_u64());
            if let (Some(lane), Some(session)) = (lane, session) {
                let _ = proxy.send_event(AppEvent::EchoesSessionFocus {
                    lane: lane.to_string(),
                    session: session as u32,
                });
            }
        }
        // doc 38 Phase 3: session tab の × による close。lane + session 必須。
        Some("echoes:session_remove") => {
            let lane = parsed.get("lane").and_then(|v| v.as_str());
            let session = parsed.get("session").and_then(|v| v.as_u64());
            if let (Some(lane), Some(session)) = (lane, session) {
                let _ = proxy.send_event(AppEvent::EchoesSessionRemove {
                    lane: lane.to_string(),
                    session: session as u32,
                });
            }
        }
        Some("echoes:stands_fetch") => {
            if let Some(lane) = parsed.get("lane").and_then(|v| v.as_str()) {
                // doc 47 §6: 要求元の相関 id（省略可 = 応答を誰も拾わない）。
                let req = parsed
                    .get("req")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let _ = proxy.send_event(AppEvent::EchoesStandsFetch {
                    lane: lane.to_string(),
                    req,
                });
            }
        }
        Some("lanes:ensure-all") => {
            // VP-140: JS 側が DOMContentLoaded 後に送る lane catch-up 要求。
            // 起動 race で silent drop された ensureLane を再発行させるための signal。
            tracing::info!("[ipc] lanes:ensure-all (JS DOMContentLoaded catch-up)");
            let _ = proxy.send_event(AppEvent::LanesEnsureAll);
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
            // に届ける → event loop が main_view の window.deliverPaste(text) を evaluate_script。
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
            // board モデル: thumbnail ✕。 SP の board_delete_item に forward（app.rs で World ask）。
            let _ = proxy.send_event(AppEvent::BoardMutate {
                method: "board_delete_item".to_string(),
                body: parsed.clone(),
            });
        }
        Some("board:clear") => {
            // board モデル: Clear ボタン。 SP の board_clear に forward。
            let _ = proxy.send_event(AppEvent::BoardMutate {
                method: "board_clear".to_string(),
                body: parsed.clone(),
            });
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
