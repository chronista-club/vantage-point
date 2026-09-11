//! main_area webview からの **IPC handler**（decode → `AppEvent` へ変換）
//!
//! `AppEvent` 自体は `crate::events` に移設（6-0）。本 module は IPC の decode だけを持つ。
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
//! - `handle_ipc_message`: main_area webview からの IPC で `ready` / `copy` / `debug` /
//!   `slot:rect` の **non-PTY** event だけを処理 (Lane の input/output は browser native WS で完結)
//!
//! 関連 memory: mem_1CaTpCQH8iLJ2PasRcPjHv (Architecture v4: Lane = Session Process)

use tao::event_loop::EventLoopProxy;

use crate::events::AppEvent;

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
            // `vp_app::webview::terminal_ipc=warn` で、PTY hot path の洪水を防ぐため意図的に絞ってある。
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
                    request_id: parsed
                        .get("request_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
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
        // を埋める。⚠️ webview/ipc_route.rs の is_main_ipc_tag allowlist と両方更新（片側だと sidebar IPC へ
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
                        rect: crate::webview::ink_snapshot::InkRect { x, y, w, h },
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
            // に届ける → event loop が `push_main::deliver_paste` で `term:paste` を push。
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
                let rect = crate::webview::main_area::SlotRect {
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
        // ⚠️ webview/ipc_route.rs `is_main_ipc_tag` の allowlist と対（片側更新は sidebar IPC へ silent drop）。
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
