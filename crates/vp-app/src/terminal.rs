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
    /// TheWorld から Process list 取得成功 (Architecture v4: 旧 ProjectsLoaded)
    ProcessesLoaded(Vec<crate::client::ProcessInfo>),
    /// TheWorld への接続失敗 (Architecture v4: 旧 ProjectsError)
    ProcessesError(String),
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
    },
    /// Phase A4-3b: Lane fetch 失敗 (SP 未起動 / 接続失敗)
    LanesError {
        process_path: String,
        message: String,
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
    /// R5 Worker create flow: Add Worker form が送信した `lane:add_worker` の結果を sidebar に
    /// push back する。 `error` Some の時 form 下に inline error 表示、 None の時 form を閉じる。
    /// 例: 名前重複 (CONFLICT)、 ccws clone 失敗、 SP 未起動 等。
    WorkerCreateResult {
        project_path: String,
        name: String,
        error: Option<String>,
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
        Some("in") | Some("resize") | Some("ready") => {
            // Phase 2.x-d: Lane WS が直接 SP に送信するので Rust 経路は使わない。
            // 旧 single-term の互換のため受け取りは続けるが silent no-op。
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
        other => {
            tracing::debug!("terminal IPC: unknown type {:?}", other);
        }
    }
}
