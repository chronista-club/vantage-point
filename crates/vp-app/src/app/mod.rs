//! Main EventLoop + window lifecycle
//!
//! ## アーキテクチャ方針 (Mac 版と同等 + Creo UI 統一)
//!
//! 「ネイティブ層ベース + WebUI on top」のハイブリッド構成。
//! デザインシステムは **Creo UI** (mint-dark theme) を全ペインで共有。
//!
//! ```text
//! ┌─── tao ネイティブウィンドウ (native chrome, menu, tray) ──┐
//! │ ┌──────────┬───────────────────────────────────────┐ │
//! │ │ sidebar  │   main area (単一 wry WebView)          │ │
//! │ │ (Creo)   │   ┌─ pane-lane (xterm.js)─────┐   │ │
//! │ │ repo  │   ├─ pane-canvas (placeholder)─────┤   │ │
//! │ │ + Activ. │   ├─ pane-preview (iframe)─────────┤   │ │
//! │ │ widget   │   └─ pane-empty   (no selection)───┘   │ │
//! │ │ (~280px) │   active pane を kind 別に切替表示       │ │
//! │ └──────────┴───────────────────────────────────────┘ │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! - **ウィンドウ・メニュー・トレイ・レイアウト境界** は Rust (tao + muda + tray-icon)
//! - **sidebar** は wry WebView (accordion + Activity widget、VP-95)
//! - **main area** は単一 wry WebView (β 戦略、VP-100 Phase 2)。
//!   PaneKind 別の content を全部 mount しておき、`window.setActivePane` で表示切替
//! - **Creo UI tokens.css (mint-dark)** を各 WebView に inline して token 統一
//! - **γ-light readiness**: main area の slot rect を ResizeObserver 経由で Rust に
//!   push (`AppEvent::SlotRect`)、Phase 4+ で native overlay の `set_position` 同期に使用

/// 起動 = resource の構築（`Boot`）。`run()` が最後まで所有する。
mod boot;
/// boot 窓の catch-up（`WebviewReady` で state を 1 つの list で撃ち直す）。
mod catch_up;
/// lane の見え方の調整役（activate_lane / conversation attach / roster / 未読印）。
mod lane_view;
/// event handler: board / canvas / editor（board snapshot の投影、switch_lane、editor bridge、board mutate）。
mod on_board;
/// event handler: conversation（chat の submit / respond / mode / session 操作 / console / agents / OSC）。
mod on_conversation;
/// event handler: lanes（repo 一覧 / lane snapshot の到着 = model 更新・購読・session reconcile）。
mod on_lanes;
/// event handler: misc（session title / inbox / ink / debug log / device / code / wire / activity）。
mod on_misc;
/// event handler: sidebar（sidebar IPC の効果実行 / update phase / settings overlay）。
mod on_sidebar;
/// event handler: terminal（PTY 出力 / keystroke / resize / paste）。
mod on_terminal;
/// event handler: window（close / resize / move / focus / shell layout / menu / secondary window）。
mod on_window;
/// 復元と保存の 1 箇所（`Persist` = SessionState の所有者、doc 60 §8）。
mod persist;
/// sidebar IPC の解釈（state 遷移 + 効果要求）。
mod sidebar_ipc;
/// event loop の可変 state（`UiState`）。
mod state;

use tao::event::{Event, WindowEvent};
use tao::event_loop::ControlFlow;
use wry::{Rect, WebView, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize};

use crate::events::AppEvent;
use crate::settings::Settings;
use crate::webview::main_area::{self, MAIN_AREA_HTML};

/// 起動時の window default size (LogicalPixel)。 with_inner_size と clamp 矯正後の値で
/// 共用するため定数化。
const DEFAULT_WINDOW_WIDTH: f64 = 1200.0;
const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;

/// 最低 window size (LogicalPixel)。 sidebar 幅 280 (HTML 側 CSS `#sidebar-root` が司る) + 余裕ある main 領域 (820+) を
/// 構造的に確保。 これ未満になる window は使用に耐えないため、 OS の min 制約 (drag 防止)
/// と起動時 clamp (state restoration 後の矯正) の両方で下限として参照する。
const MIN_WINDOW_WIDTH: f64 = 1100.0;
const MIN_WINDOW_HEIGHT: f64 = 700.0;

/// 開発者モード判定 (起動時の初期値計算に使用、runtime 切替は menu 経由)
///
/// 優先順位 (1Password 風の挙動):
/// 1. `VP_DEVELOPER_MODE` env var が `1`/`true`/`yes`/`on` → 強制 ON
/// 2. `VP_DEVELOPER_MODE` env var が `0`/`false`/`no`/`off` → 強制 OFF
/// 3. Settings ファイル (`vp_config_dir()/vp-app.toml`) の `developer_mode` フィールド
/// 4. それ以外 (未設定) → `cfg!(debug_assertions)` (debug ビルドは ON、release は OFF)
///
/// 起動後の runtime 切替 (View → Developer Mode メニュー) は app.rs の event loop で
/// settings ファイルを更新しつつ、対応する menu item の状態を即時反映する。
fn initial_developer_mode(settings: &Settings) -> bool {
    if let Some(b) = developer_mode_env() {
        return b;
    }
    if let Some(b) = settings.developer_mode {
        return b;
    }
    cfg!(debug_assertions)
}

/// `VP_DEVELOPER_MODE` の解釈。`Some` = **env が実効値を固定している**（= 設定ページで
/// 変えても効かない）。
///
/// [`initial_developer_mode`] から切り出したのは、設定ページが「この toggle は今 env に
/// 固定されているか」を知る必要があるため（doc 59 P1）。受理する綴りの一覧を 2 箇所に
/// 書くと、片方だけ増やしたときに「env は効いているのに UI は編集可能に見える」がすぐ生える。
pub(super) fn developer_mode_env() -> Option<bool> {
    let v = std::env::var("VP_DEVELOPER_MODE").ok()?;
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        // 綴り違いは「指定なし」に倒す（file / debug_assertions のフォールバックへ）。
        _ => None,
    }
}

/// Creo UI design tokens (CSS custom properties、mint-dark default)
///
/// <https://github.com/chronista-club/creo-ui> packages/web が source。
/// vp-app の 3 ペインすべてに inline して共通 token で描画する。
pub const CREO_TOKENS_CSS: &str = include_str!("../../assets/creo-tokens.css");

/// WebView 統合 (step 3a) 後の唯一の webview が `vp-asset://` で配信する asset。
/// `MAIN_AREA_HTML` を `app/index.html` で、SolidJS bundle 2 本を外部 script として配信
/// (doc 48 Phase 1 で inline → `<script src>` 化。`VP_WEBVIEW_DEV` 設定時は
/// `webview::assets::serve` の disk-read が baked より優先され、cargo build なしの HMR になる)。
///
/// ## なぜ with_html ではなく custom protocol か (統合 origin fix)
/// `with_html` で load した document は **about:blank = 不透明 (opaque) オリジン**になり、
/// `localStorage` 等 origin 依存 API が `SecurityError` を throw する。統合で sidebar bundle を
/// 同 document に inline した結果、`Shell()` が render 時に踏む `localStorage.getItem`
/// (タブ状態の永続) で sidebar bundle が boot 中に落ち、`<Shell/>` が mount されず sidebar が
/// 空になっていた。custom protocol で load すれば document origin = `vp-asset://app` の
/// 実オリジンになり、統合前 (sidebar が `vp-asset://app/sidebar.html` を load していた頃) と
/// 同じく localStorage が使える。
const MAIN_VIEW_ASSETS: &[(&str, &[u8], &str)] = &[
    (
        "app/index.html",
        MAIN_AREA_HTML.as_bytes(),
        "text/html; charset=utf-8",
    ),
    (
        "app/editor-host.bundle.js",
        main_area::EDITOR_HOST_BUNDLE_JS.as_bytes(),
        "application/javascript; charset=utf-8",
    ),
    (
        "app/sidebar.bundle.js",
        main_area::SIDEBAR_BUNDLE_JS.as_bytes(),
        "application/javascript; charset=utf-8",
    ),
];

/// Sidebar + Main area の bounds をウィンドウサイズから計算 (VP-100 Phase 2)
///
/// WebView 統合 (step 3a): sidebar + main を統合した 1 WebView を window 全面に張る。
/// sidebar(280px) | main の横分割は HTML 側 CSS flex (#app-shell) が司る。
fn update_pane_bounds(webview: &WebView, window_size: tao::dpi::PhysicalSize<u32>, scale: f64) {
    let logical = window_size.to_logical::<f64>(scale);
    let _ = webview.set_bounds(Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: WryLogicalSize::new(logical.width, logical.height).into(),
    });
}

// R-0 (`docs/design/11-vp-app-refactor.md` § 3.0a / `mem_1CaaaDoXHZvhR46ZfLN6jx`):
//   旧 `lane_address_key(&LaneAddressWire) -> String` 関数は `lane_address.rs::LaneAddressWire::key()`
//   メソッドに移管 (G2 解消、 3 重実装の 1 元化)。 caller は `wire.key()` で同等の文字列を取れる。

/// App のエントリポイント
pub fn run() -> anyhow::Result<()> {
    // resource（runtime / window / webview / menu / tray / daemon 接続）と初期 state を作る。
    // `boot` と `ui` は閉包に move し、process の寿命と一致させる（doc 60 §6 6-2）。
    let (event_loop, boot, mut ui) = boot::boot()?;

    let proxy = event_loop.create_proxy();
    // Phase 2.5 (per-Lane instance): startup の placeholder PTY 接続は撤去。
    // Lane が出現するまで main area は empty placeholder ("No Lane selected") のみ。
    // ただし daemon の auto-launch だけは継続 (sidebar の Activity widget や
    // /api/daemon/repos 取得に必要)。
    let _ = proxy; // 旧 spawn_shell / connect_daemon_terminal で proxy を消費していた、 互換用に残す
    // maybe_respawn_dead_lane の async restart_lane が失敗した時に event loop へ
    // 通知を返し lane_respawn_triggered を解除するための proxy (永続 suppression 回避)。
    let respawn_proxy = event_loop.create_proxy();
    // repo:add 等の async 操作で event loop に repo list 再 fetch を kick するための proxy
    let async_action_proxy = event_loop.create_proxy();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // launch settle まで dock icon を再設定 (bare binary 対策)。 settle 後は通常の Wait。
        if !ui.win.icon_settled {
            crate::icon::set_app_icon();
            if ui.win.icon_launch_at.elapsed() < std::time::Duration::from_millis(1500) {
                *control_flow = ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(150),
                );
            } else {
                ui.win.icon_settled = true;
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => on_window::close_requested(&mut ui, &boot, control_flow),
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => on_window::resized(&mut ui, &boot, size),
            Event::WindowEvent {
                event: WindowEvent::Moved(_),
                ..
            } => on_window::moved(&mut ui, &boot),
            Event::WindowEvent {
                event: WindowEvent::Focused(focused),
                ..
            } => on_window::focused(&mut ui, &boot, focused),
            Event::UserEvent(AppEvent::PasteText(text)) => {
                on_terminal::paste_text(&mut ui, &boot, text)
            }
            Event::UserEvent(AppEvent::OscNotification { lane, code: _ }) => {
                on_conversation::osc_notification(&mut ui, &boot, lane)
            }
            Event::UserEvent(AppEvent::ResolveSessionTitles) => {
                on_misc::resolve_session_titles(&mut ui, &boot)
            }
            Event::UserEvent(AppEvent::ResolveLaneInboxes) => {
                on_misc::resolve_lane_inboxes(&mut ui, &boot)
            }
            Event::UserEvent(AppEvent::ReposLoaded(repos)) => {
                on_lanes::repos_loaded(&mut ui, &boot, &async_action_proxy, repos)
            }
            Event::UserEvent(AppEvent::LanesLoaded {
                repo_path,
                lanes,
                origin,
            }) => on_lanes::lanes_loaded(
                &mut ui,
                &boot,
                &respawn_proxy,
                &async_action_proxy,
                repo_path,
                lanes,
                origin,
            ),
            Event::UserEvent(AppEvent::WebviewReady) => catch_up::webview_ready(&mut ui, &boot),
            Event::UserEvent(AppEvent::LanesError { repo_path, message }) => {
                on_lanes::lanes_error(&mut ui, &boot, repo_path, message)
            }
            Event::UserEvent(AppEvent::LaneRespawnFailed { address }) => {
                on_lanes::lane_respawn_failed(&mut ui, &boot, address)
            }
            Event::UserEvent(AppEvent::InkSnapshot { rect }) => {
                on_misc::ink_snapshot(&mut ui, &boot, &proxy, rect)
            }
            Event::UserEvent(AppEvent::InkSnapshotReady { path, error }) => {
                on_misc::ink_snapshot_ready(&mut ui, &boot, path, error)
            }
            Event::UserEvent(AppEvent::ShellLayout {
                sidebar_width,
                right_sidebar_width,
                sidebar_form,
                right_sidebar_open,
            }) => on_window::shell_layout(
                &mut ui,
                &boot,
                sidebar_width,
                right_sidebar_width,
                sidebar_form,
                right_sidebar_open,
            ),
            Event::UserEvent(AppEvent::DebugLogWatch { source }) => {
                on_misc::debug_log_watch(&mut ui, &boot, &proxy, source)
            }
            Event::UserEvent(AppEvent::DebugLogUnwatch) => {
                on_misc::debug_log_unwatch(&mut ui, &boot)
            }
            Event::UserEvent(AppEvent::DebugLogChunk {
                source,
                reset,
                lines,
                generation,
            }) => on_misc::debug_log_chunk(&mut ui, &boot, source, reset, lines, generation),
            Event::UserEvent(AppEvent::DeviceEvent { payload }) => {
                on_misc::device_event(&mut ui, &boot, payload)
            }
            Event::UserEvent(AppEvent::EditorEval { js, resp }) => {
                on_board::editor_eval(&mut ui, &boot, js, resp)
            }
            Event::UserEvent(AppEvent::CanvasMessage { repo_path, message }) => {
                on_board::canvas_message(
                    &mut ui,
                    &boot,
                    &respawn_proxy,
                    &async_action_proxy,
                    repo_path,
                    message,
                )
            }
            Event::UserEvent(AppEvent::TerminalOutput {
                lane,
                session,
                data,
            }) => on_terminal::terminal_output(&mut ui, &boot, lane, session, data),
            Event::UserEvent(AppEvent::TerminalWrite {
                lane,
                session,
                data,
            }) => on_terminal::terminal_write(&mut ui, &boot, lane, session, data),
            Event::UserEvent(AppEvent::TerminalResize {
                lane,
                session,
                cols,
                rows,
            }) => on_terminal::terminal_resize(&mut ui, &boot, lane, session, cols, rows),
            Event::UserEvent(AppEvent::ConversationEvent {
                lane,
                event,
                session,
            }) => on_conversation::conversation_event(&mut ui, &boot, lane, event, session),
            Event::UserEvent(AppEvent::ConversationSubmit {
                lane,
                prompt,
                session: chat_session,
                images,
                request_id,
            }) => on_conversation::conversation_submit(
                &mut ui,
                &boot,
                &async_action_proxy,
                lane,
                prompt,
                chat_session,
                images,
                request_id,
            ),
            Event::UserEvent(AppEvent::ConversationRespond {
                lane,
                request_id,
                answers,
                behavior,
                message,
                session: chat_session,
            }) => on_conversation::conversation_respond(
                &mut ui,
                &boot,
                &async_action_proxy,
                lane,
                request_id,
                answers,
                behavior,
                message,
                chat_session,
            ),
            Event::UserEvent(AppEvent::ConversationInterrupt {
                lane,
                session: chat_session,
            }) => on_conversation::conversation_interrupt(&mut ui, &boot, lane, chat_session),
            Event::UserEvent(AppEvent::ConversationSetPermissionMode {
                lane,
                mode,
                session: chat_session,
            }) => on_conversation::conversation_set_permission_mode(
                &mut ui,
                &boot,
                lane,
                mode,
                chat_session,
            ),
            Event::UserEvent(AppEvent::SessionSetMode {
                lane,
                session,
                mode,
            }) => on_conversation::session_set_mode(
                &mut ui,
                &boot,
                &async_action_proxy,
                lane,
                session,
                mode,
            ),
            Event::UserEvent(AppEvent::SessionModeApplied {
                lane,
                session,
                mode,
            }) => on_conversation::session_mode_applied(
                &mut ui,
                &boot,
                &async_action_proxy,
                lane,
                session,
                mode,
            ),
            Event::UserEvent(AppEvent::ConsoleNewSession { lane, engine, mode }) => {
                on_conversation::console_new_session(&mut ui, &boot, lane, engine, mode)
            }
            Event::UserEvent(AppEvent::ConsoleSwitchRoot { lane, session }) => {
                on_conversation::console_switch_root(&mut ui, &boot, lane, session)
            }
            Event::UserEvent(AppEvent::ConversationSetModel {
                lane,
                session,
                model,
            }) => on_conversation::conversation_set_model(&mut ui, &boot, lane, session, model),
            Event::UserEvent(AppEvent::ConversationSessionCreate { lane, agent }) => {
                on_conversation::conversation_session_create(&mut ui, &boot, lane, agent)
            }
            Event::UserEvent(AppEvent::ConversationDemandStart { lane }) => {
                on_conversation::conversation_demand_start(&mut ui, &boot, lane)
            }
            Event::UserEvent(AppEvent::ConversationSessionFocus { lane, session }) => {
                on_conversation::conversation_session_focus(&mut ui, &boot, lane, session)
            }
            Event::UserEvent(AppEvent::ConversationSessionRemove { lane, session }) => {
                on_conversation::conversation_session_remove(&mut ui, &boot, lane, session)
            }
            Event::UserEvent(AppEvent::AgentsFetch { lane, req }) => {
                on_conversation::agents_fetch(&mut ui, &boot, &async_action_proxy, lane, req)
            }
            Event::UserEvent(AppEvent::Agents { lane, payload, req }) => {
                on_conversation::agents(&mut ui, &boot, lane, payload, req)
            }
            Event::UserEvent(AppEvent::BoardMutate { method, body }) => {
                on_board::board_mutate(&mut ui, &boot, method, body)
            }
            Event::UserEvent(AppEvent::ReposError(msg)) => {
                on_lanes::repos_error(&mut ui, &boot, msg)
            }
            Event::UserEvent(AppEvent::SubCreateResult {
                repo_path,
                name,
                error,
            }) => on_lanes::sub_create_result(&mut ui, &boot, repo_path, name, error),
            Event::UserEvent(AppEvent::AgentsResult {
                repo_path,
                agents,
                error,
            }) => on_conversation::agents_result(&mut ui, &boot, repo_path, agents, error),
            Event::UserEvent(AppEvent::CodeList { lane }) => {
                on_misc::code_list(&mut ui, &boot, &async_action_proxy, lane)
            }
            Event::UserEvent(AppEvent::CodeRead { lane, rel_path }) => {
                on_misc::code_read(&mut ui, &boot, &async_action_proxy, lane, rel_path)
            }
            Event::UserEvent(AppEvent::CodeEntriesResult {
                lane,
                entries,
                truncated,
            }) => on_misc::code_entries_result(&mut ui, &boot, lane, entries, truncated),
            Event::UserEvent(AppEvent::CodeFileResult {
                lane,
                rel_path,
                payload,
            }) => on_misc::code_file_result(&mut ui, &boot, lane, rel_path, payload),
            Event::UserEvent(AppEvent::WireHistoryResult { address, payload }) => {
                on_misc::wire_history_result(&mut ui, &boot, address, payload)
            }
            Event::UserEvent(AppEvent::ActivityUpdate(snap)) => {
                on_misc::activity_update(&mut ui, &boot, snap)
            }
            Event::UserEvent(AppEvent::UpdateFlowPhase(applying)) => {
                on_sidebar::update_flow_phase(&mut ui, &boot, applying)
            }
            Event::UserEvent(AppEvent::SettingsRepoRootPicked(path)) => {
                on_sidebar::settings_repo_root_picked(&mut ui, &boot, path)
            }
            Event::UserEvent(AppEvent::SettingsDaemonFetched(fetched)) => {
                on_sidebar::settings_daemon_fetched(&mut ui, &boot, fetched)
            }
            Event::UserEvent(AppEvent::SidebarIpc(msg)) => on_sidebar::sidebar_ipc(
                &mut ui,
                &boot,
                &proxy,
                &respawn_proxy,
                &async_action_proxy,
                msg,
            ),
            Event::UserEvent(AppEvent::SlotRect {
                pane_id,
                kind,
                rect,
            }) => on_window::slot_rect(&mut ui, &boot, pane_id, kind, rect),
            Event::UserEvent(AppEvent::MenuClicked(id)) => {
                on_window::menu_clicked(&mut ui, &boot, id)
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod main_view_asset_tests {
    //! 統合 WebView (step 3a) の単一 HTML が vp-asset:// で配信でき、SolidJS bundle を
    //! 外部 script (vp-asset://app/*.bundle.js) として参照・配信できること (doc 48 Phase 1)。
    //! Bundle font / serve handler のテストは `webview::assets` module 側に分離。
    use super::*;

    /// `MAIN_VIEW_ASSETS` で統合 HTML が `vp-asset://app/index.html` から取れる。
    #[test]
    fn main_view_html_servable_via_vp_asset() {
        let html =
            crate::webview::assets::lookup_asset("vp-asset://app/index.html", MAIN_VIEW_ASSETS);
        assert!(html.is_some(), "index.html not lookupable");
        let (bytes, ct) = html.unwrap();
        assert_eq!(ct, "text/html; charset=utf-8");
        assert_eq!(bytes, MAIN_AREA_HTML.as_bytes());
    }

    /// 統合 HTML が sidebar mount point を持ち、bundle を外部 script として参照する
    /// (doc 48 Phase 1: inline → `<script src>` 化。相対 src は page origin
    /// `vp-asset://app/` により `app/*.bundle.js` に解決される)。
    #[test]
    fn main_area_html_references_external_bundles() {
        assert!(
            MAIN_AREA_HTML.contains(r#"id="sidebar-root""#),
            "統合 HTML に #sidebar-root mount point がない"
        );
        assert!(
            MAIN_AREA_HTML.contains(r#"<script src="editor-host.bundle.js"></script>"#),
            "統合 HTML が editor-host bundle を外部 script 参照していない"
        );
        assert!(
            MAIN_AREA_HTML.contains(r#"<script src="sidebar.bundle.js"></script>"#),
            "統合 HTML が sidebar bundle を外部 script 参照していない"
        );
    }

    /// 外部化した bundle が `vp-asset://app/*.bundle.js` から配信できる
    /// (baked 経路 = `VP_WEBVIEW_DEV` 未設定時の prod 挙動)。
    #[test]
    fn bundles_servable_via_vp_asset() {
        for (path, marker) in [
            ("vp-asset://app/sidebar.bundle.js", "[vp-sidebar] booting"),
            ("vp-asset://app/editor-host.bundle.js", "EditorHost"),
        ] {
            let asset = crate::webview::assets::lookup_asset(path, MAIN_VIEW_ASSETS);
            assert!(asset.is_some(), "{path} not lookupable");
            let (bytes, ct) = asset.unwrap();
            assert_eq!(ct, "application/javascript; charset=utf-8");
            assert!(
                String::from_utf8_lossy(bytes).contains(marker),
                "{path} の中身に marker `{marker}` が無い (bundle 生成物が想定と違う)"
            );
        }
    }
}
