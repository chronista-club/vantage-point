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
/// event handler: terminal（PTY 出力 / keystroke / resize / paste）。
mod on_terminal;
/// event handler: window（close / resize / move / focus / shell layout / menu / secondary window）。
mod on_window;
/// sidebar IPC の解釈（state 遷移 + 効果要求）。
mod sidebar_ipc;
/// event loop の可変 state（`UiState`）。
mod state;

use tao::event::{Event, WindowEvent};
use tao::event_loop::ControlFlow;
use wry::{Rect, WebView, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize};

use crate::daemon::conn::daemon_repo_request;
use crate::daemon::pollers::{fetch_repos_with_ports, spawn_sp_start};
use crate::daemon::wire::wire_fetch_payload;
use crate::events::AppEvent;
use crate::flows::repo_dialog::{
    resolve_default_repo_root, spawn_add_repo_picker, spawn_clone_repo, spawn_repo_root_picker,
};
use crate::pane::SidebarState;
use crate::settings::Settings;
use crate::webview::main_area::{self, MAIN_AREA_HTML};
use crate::webview::push_main;
use crate::webview::push_sidebar::{self, push_sidebar_state};
use lane_view::{
    activate_lane, ensure_conversation_attach, lane_is_chat, push_active_view, push_session_list,
    resolve_repo_path_for_lane, session_list_payload, term_sessions_of,
};
use sidebar_ipc::handle_sidebar_ipc;

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
fn developer_mode_env() -> Option<bool> {
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

/// daemon 側（settings.kdl）から取れた設定。**取れなかった場合と未設定を区別する**ため
/// `Option` を包んでいる（doc 59 P3）。
///
/// `None` = daemon に届かなかった（オフライン / 旧 binary）。この時 UI は該当区画を
/// 「daemon に接続すると編集できます」に落とす — 空欄を編集可能に見せると、押しても
/// 保存できない**行き止まり**になる。
#[derive(Debug, Clone, Default)]
struct DaemonSettings {
    log_level: Option<String>,
    idle_timeout_minutes: Option<i64>,
    /// 既定 agent × model（doc 59 P4）。**組で保持する** — 別々に持つと
    /// 「codex なのに claude の model」を UI 側で再構成できてしまう。
    default_agent: Option<String>,
    default_model: Option<String>,
    /// 実効 agent が model 指定を受け付けるか。**daemon が判定した結果**をそのまま持つ
    /// （vp-app は engine の能力表明を知らない = 一覧を複製しない）。
    default_agent_takes_model: bool,
}

/// 設定 overlay へ返す確定値を組み立てる（doc 59 P1 + P3）。
///
/// `developer_mode` は **実効値**（env > vp-app.toml > `debug_assertions` の解決後）を渡す —
/// event loop が持っている `dev_mode` がその値なので、それをそのまま映す。
/// `resolved_repo_root` は明示値が無いときに実際に使われるパスで、入力欄の placeholder に
/// なる（「空欄だが実際はここ」を見せるため）。
///
/// ⚠️ **真実源が 2 つある**面なので、それぞれの持ち主を分けて扱う:
/// - `vp-app.toml`（GUI 固有）= developer_mode / default_repo_root — この関数が同期で読む
/// - `settings.kdl`（好み、daemon 所有）= log_level / idle_timeout — `daemon` 引数で渡る
fn settings_snapshot(
    settings: &Settings,
    sidebar_state: &SidebarState,
    dev_mode: bool,
    daemon: Option<&DaemonSettings>,
) -> crate::generated::sidebar_ipc::SettingsResult {
    crate::generated::sidebar_ipc::SettingsResult {
        developer_mode: dev_mode,
        developer_mode_locked: developer_mode_env().is_some(),
        default_repo_root: settings.default_repo_root.clone(),
        resolved_repo_root: resolve_default_repo_root(settings, sidebar_state)
            .map(|p| p.display().to_string()),
        daemon_reachable: daemon.is_some(),
        log_level: daemon.and_then(|d| d.log_level.clone()),
        idle_timeout_minutes: daemon.and_then(|d| d.idle_timeout_minutes),
        default_agent: daemon.and_then(|d| d.default_agent.clone()),
        default_model: daemon.and_then(|d| d.default_model.clone()),
        // daemon が判定した結果をそのまま流す。UI はこれが false なら model 欄を出さない
        // （codex は VP から model を渡さない = 押しても効かない欄を並べない）。
        default_agent_takes_model: daemon.is_some_and(|d| d.default_agent_takes_model),
    }
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
            }) => {
                on_lanes::lanes_loaded(
                    &mut ui,
                    &boot,
                    &respawn_proxy,
                    &async_action_proxy,
                    repo_path,
                    lanes,
                    origin,
                )
            }
            // webview が「受け口を全部生やした」と名乗った（`entry.tsx` の `t:"ready"`）。
            //
            // ## これは catch-up ではなく **replay**
            //
            // bundle 評価前に Rust が撃った押し込みは、受け口 (`window.vpDispatch`) が居ないので
            // 届かない。ここで**現在の状態を丸ごと撃ち直す**ことでそれを埋める。全部 idempotent /
            // 全量置き換えなので、二重に撃っても壊れない（level 駆動）。
            //
            // ⚠️ 以前は同じことを **feature ごとの pull 3 本**（`lanes:ensure-all` /
            // `bastet:devices_fetch` / `board:demand`）でやっていた。面を足すたびに pull を 1 本
            // 足す形で、しかも**その面が install された後**に撃つ順序制約が JS 側に散っていた。
            // 「webview が生まれた」という事実は 1 つなので、signal も 1 本に畳んである。
            // 新しい面を足したら **ここに replay を 1 行足す**（新しい IPC tag は要らない）。
            Event::UserEvent(AppEvent::WebviewReady) => {
                // terminal S4: JS xterm instance の catch-up 再発行のみ (repo port 不要)。
                // terminal session 自体は LanesLoaded reconcile が管理するのでここでは触らない。
                for (_repo_path, lanes) in ui.sidebar_state.lanes_by_repo.clone().iter() {
                    for lane in lanes {
                        // doc 50 §4.6 A6: gate は term session の有無（LanesLoaded と同じ規則 —
                        // lane 単位の pid / mode で切ると root=chat の lane の非 root term が
                        // 落ちる）。ensureLane は idempotent なので catch-up で撃ち直してよい。
                        let addr_str = lane.address.key();
                        for (session, is_root) in term_sessions_of(lane) {
                            push_main::ensure_lane(&boot.webview, &addr_str, session, is_root);
                        }
                        // doc 53 §11: **roster も同じ窓で落ちる**（team-b 指摘 2026-07-25）。
                        //
                        // roster が push 型になった以上、`ensure_lane` / `push_active_view` /
                        // device 一覧と同じ boot race を持つ: bundle **評価前**の押し込みは
                        // 受け口（`window.vpDispatch`）が居ないので届かないのに、Rust 側は
                        // 「送った」として指紋を残す → その lane の roster が**実際に変わるまで
                        // 二度と push されない**（tab strip / pane grid / picker が空のまま）。
                        // 供給が fetch だった頃は「JS が能動的に取りに行く」ので原理的に
                        // 起きなかった窓 — 供給路を変えたことの随伴。
                        //
                        // ここは JS が ready を名乗った後なので、**指紋を無視して撃ち直す**
                        // （送った値は同じなので指紋の更新は不要）。
                        if let Some(sessions) = lane.sessions.as_ref() {
                            push_session_list(
                                &boot.webview,
                                &addr_str,
                                &session_list_payload(&addr_str, sessions),
                            );
                        }
                        // **terminal の replay も同じ窓で落ちる**（2026-07-26 実測）。
                        //
                        // 上のコメントが列挙する「同じ boot race を持つもの」に terminal が
                        // 並んでいなかった。実測した時刻:
                        //   02:11:42.886  replay が client に到着（= evaluate_script が撃たれる）
                        //   02:11:43.284  bundle init complete（**0.4 秒後**）
                        // `window.vpTerminal` が未定義の間の `evaluate_script` は silent no-op で、
                        // terminal の replay は**一度きり**なので二度と来ない → console が黒いまま。
                        //
                        // ここは JS が ready を名乗った後なので、demand を撃ち直して replay を
                        // 取り直す（server 側は `terminal_demand_start` → `reconcile_lane` で
                        // 冪等 — 既に張られていれば pump は kept、replay だけが流れ直す）。
                        if !term_sessions_of(lane).is_empty()
                            && let Some(path) =
                                resolve_repo_path_for_lane(&ui.sidebar_state, &addr_str)
                        {
                            let lane_for_req = addr_str.clone();
                            let conn = boot.daemon_conn.clone();
                            boot.rt_handle.spawn(async move {
                                if let Err(e) = daemon_repo_request(
                                    &conn,
                                    &path,
                                    "terminal_demand_start",
                                    // `replay: true` = 「画面を持っていないので流し直して」。
                                    // 準備前に届いた replay を捨てているので、server 側の
                                    // 「変化なし」判定を明示要求で越える（doc 53 §6.5.0）。
                                    serde_json::json!({ "lane": lane_for_req, "replay": true }),
                                )
                                .await
                                {
                                    tracing::debug!(
                                        "WebviewReady: terminal demand 再要求に失敗（次の契機で再試行）: {e}"
                                    );
                                }
                            });
                        }
                    }
                }
                // 現在 active な Lane を再度 show する (lane-empty placeholder を解除する保険)
                if let Some(addr) = ui.sidebar_state.active_lane_address.clone() {
                    let is_chat = lane_is_chat(&ui.sidebar_state, &addr);
                    push_main::show_lane(&boot.webview, Some(&addr), is_chat);
                    // 起動 race で silent drop されるのは ensureLane だけではない。 auto-select の
                    // activate_lane が撃つ setActivePane も同じ窓で落ちるが、これが JS 側の
                    // 「active lane」を埋める唯一の経路 — showLane だけ再発行しても JS の active
                    // lane は null のままなので、lane 文脈を要する操作が「active lane 不明」で
                    // 早期 return する。冪等なので毎回再発行して JS 側 state を確定させる。
                    //
                    // doc 50 §4.6 A6: lane 単位 mode の catch-up は退役（lane 単位 mode が
                    // 消滅）。roster の catch-up は上の lane ループが撃つ（doc 53 §11 — push 型に
                    // なって以降、この経路にも roster が要る）。
                    push_active_view(&boot.webview, &ui.sidebar_state);
                }
                // 計器盤: daemon-device の接続時 snapshot は bundle ロード前に届いて落ちている
                // （sidebar の Devices badge は state 再 push で生きるが pane だけ空、2026-07-23
                // 実機で確認）。保持済み state から全量で撃ち直す。
                push_main::render_devices(&boot.webview, &ui.sidebar_state.devices);
                // shell (L|main|R) の形: 保存があれば復元する。無ければ撃たない
                // （webview の既定値が残る = 既定を 2 箇所に書かない）。
                // ⚠️ 撃った/撃たなかったを**両方**残す。「行が無い」は「保存が無かった」とも
                // 「ここに来ていない」とも読めてしまい、実機の切り分けで 1 往復損する
                // （2026-08-06 に実際に損した）。
                match ui.session_state.shell_layout().cloned() {
                    Some(layout) => {
                        tracing::info!("shell layout 復元: {layout:?}");
                        push_main::shell_layout(&boot.webview, &layout);
                    }
                    None => tracing::info!("shell layout 復元: 保存なし（既定のまま）"),
                }
                // 掲示板: retained BoardUpdated も同じ窓で落ちる（doc 52 §10 wave 0）。
                // 保持分を撃ち直す（落ちたままだと reopen で board pane が出ず、次の live show
                // まで空のまま）。
                //
                // ⚠️ **全 repo 分を撃つ**。active repo だけに絞っていた旧実装は、bundle 再評価後に
                // 別 repo へ切り替えると board が空のままだった（webview は `(repo, lane)` で
                // 箱を持つので、届いていない repo の箱は作られない）。message には repo が
                // stamp 済なので、まとめて配っても混ざらない。
                for boards in ui.board_snapshots.values() {
                    for message in boards.values() {
                        push_main::board_message(&boot.webview, message.clone());
                    }
                }
                // LanesLoaded のたびに follow up 発火する loop event のため log omit。
            }
            Event::UserEvent(AppEvent::LanesError {
                repo_path,
                message,
            }) => on_lanes::lanes_error(&mut ui, &boot, repo_path, message),
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
            }) => {
                on_window::shell_layout(
                    &mut ui,
                    &boot,
                    sidebar_width,
                    right_sidebar_width,
                    sidebar_form,
                    right_sidebar_open,
                )
            }
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
            Event::UserEvent(AppEvent::CanvasMessage {
                repo_path,
                message,
            }) => {
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
            Event::UserEvent(AppEvent::ConversationSubmit { lane, prompt, session: chat_session, images, request_id }) => {
                on_conversation::conversation_submit(
                    &mut ui,
                    &boot,
                    &async_action_proxy,
                    lane,
                    prompt,
                    chat_session,
                    images,
                    request_id,
                )
            }
            Event::UserEvent(AppEvent::ConversationRespond {
                lane,
                request_id,
                answers,
                behavior,
                message,
                session: chat_session,
            }) => {
                on_conversation::conversation_respond(
                    &mut ui,
                    &boot,
                    &async_action_proxy,
                    lane,
                    request_id,
                    answers,
                    behavior,
                    message,
                    chat_session,
                )
            }
            Event::UserEvent(AppEvent::ConversationInterrupt { lane, session: chat_session }) => {
                on_conversation::conversation_interrupt(&mut ui, &boot, lane, chat_session)
            }
            Event::UserEvent(AppEvent::ConversationSetPermissionMode {
                lane,
                mode,
                session: chat_session,
            }) => {
                on_conversation::conversation_set_permission_mode(
                    &mut ui,
                    &boot,
                    lane,
                    mode,
                    chat_session,
                )
            }
            Event::UserEvent(AppEvent::SessionSetMode { lane, session, mode }) => {
                on_conversation::session_set_mode(
                    &mut ui,
                    &boot,
                    &async_action_proxy,
                    lane,
                    session,
                    mode,
                )
            }
            Event::UserEvent(AppEvent::SessionModeApplied { lane, session, mode }) => {
                on_conversation::session_mode_applied(
                    &mut ui,
                    &boot,
                    &async_action_proxy,
                    lane,
                    session,
                    mode,
                )
            }
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
                on_board::board_mutate(&mut ui, &boot, &proxy, method, body)
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
                ui.update_applying = applying;
                ui.sidebar_state.activity.update_applying = applying;
                push_sidebar_state(&boot.webview, &ui.sidebar_state);
            }
            Event::UserEvent(AppEvent::SettingsRepoRootPicked(path)) => {
                // キャンセル (None) は**書かない**（既存値を保持）。ただし overlay の表示は
                // 現実に合わせたいので、選ばれた / 選ばれなかったに関わらず確定値を返す。
                if let Some(p) = path {
                    ui.settings.default_repo_root = Some(p);
                    if let Err(e) = ui.settings.save() {
                        tracing::warn!("Settings 保存失敗: {e}");
                    }
                }
                // picker は vp-app.toml しか触らないので daemon 側は引き直さない
                // （`last_daemon_settings` に前回の結果が残っている）。
                push_sidebar::settings_result(
                    &boot.webview,
                    settings_snapshot(
                        &ui.settings,
                        &ui.sidebar_state,
                        ui.dev_mode,
                        ui.last_daemon_settings.as_ref(),
                    ),
                );
            }
            Event::UserEvent(AppEvent::SettingsDaemonFetched(fetched)) => {
                // daemon 側（settings.kdl）が揃ったので、vp-app.toml 側と合流させて
                // **1 回だけ** push する。`None` = 接続できなかった（UI は該当区画を
                // 「daemon に接続すると編集できます」に落とす）。
                ui.last_daemon_settings = fetched.map(|v| {
                    let text = |k: &str| {
                        v.get(k)
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                    };
                    DaemonSettings {
                        log_level: text("log_level"),
                        idle_timeout_minutes: v
                            .get("idle_timeout_minutes")
                            .and_then(|x| x.as_i64()),
                        default_agent: text("default_agent"),
                        default_model: text("default_model"),
                        default_agent_takes_model: v
                            .get("default_agent_takes_model")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                    }
                });
                push_sidebar::settings_result(
                    &boot.webview,
                    settings_snapshot(
                        &ui.settings,
                        &ui.sidebar_state,
                        ui.dev_mode,
                        ui.last_daemon_settings.as_ref(),
                    ),
                );
            }
            Event::UserEvent(AppEvent::SidebarIpc(msg)) => {
                // VP-100 follow-up: repo:add / repo:clone は async picker → API → ReposLoaded ルート
                // (state 直接 mutate しないので handle_sidebar_ipc の前で分岐)
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
                    match parsed.get("t").and_then(|v| v.as_str()) {
                        Some("repo:add") => {
                            let initial_dir =
                                resolve_default_repo_root(&ui.settings, &ui.sidebar_state);
                            spawn_add_repo_picker(
                                async_action_proxy.clone(),
                                initial_dir,
                                boot.rt_handle.clone(),
                                boot.daemon_conn.clone(),
                            );
                            return;
                        }
                        Some("process:clone") => {
                            let url = parsed
                                .get("url")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if url.is_empty() {
                                tracing::warn!("process:clone with empty url");
                                return;
                            }
                            let target_override = parsed
                                .get("target_dir")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(std::path::PathBuf::from);
                            let default_root =
                                resolve_default_repo_root(&ui.settings, &ui.sidebar_state);
                            spawn_clone_repo(
                                async_action_proxy.clone(),
                                url,
                                default_root,
                                target_override,
                                boot.rt_handle.clone(),
                                boot.daemon_conn.clone(),
                            );
                            return;
                        }
                        _ => {}
                    }
                }
                let outcome = handle_sidebar_ipc(&msg, &mut ui.sidebar_state, &mut ui.session_state);
                // 解釈は純粋（doc 60 §6 A-2）: session の file 書き込みは要求を見てここで行う。
                // in-memory の更新は handle 側で済んでいるので、他の効果より先に書いて
                // 旧実装（handle 内で save）と同じ順序を保つ。
                if outcome.session_save {
                    ui.session_state.save();
                }
                // Lane activation — activate_lane() が全副作用を処理
                if let Some(addr) = outcome.activate_lane {
                    activate_lane(
                        &addr,
                        &mut ui.sidebar_state,
                        &mut ui.session_state,
                        &boot.webview,
                        &mut ui.guards.lane_respawn_triggered,
                        &boot.rt_handle,
                        &respawn_proxy,
                        &boot.daemon_conn,
                    );
                    // gui: chat lane なら conversation topic に attach（→ transcript replay）。
                    ensure_conversation_attach(
                        &addr,
                        &ui.sidebar_state,
                        &mut ui.sessions.conversation_sessions,
                        &boot.rt_handle,
                        &async_action_proxy,
                        &boot.daemon_conn,
                    );
                } else {
                    if outcome.changed {
                        push_sidebar_state(&boot.webview, &ui.sidebar_state);
                    }
                    if outcome.active_changed {
                        push_active_view(&boot.webview, &ui.sidebar_state);
                    }
                }
                // Architecture v4: dead な repo が expand されたら repo を auto-spawn。
                // dedup: 同 session で同じ path を 2 回呼ばない (daemon 側でも弾かれるが
                // 余計な POST を避ける)。
                if let Some((name, path)) = outcome.repo_spawn_request {
                    if ui.guards.repo_spawn_triggered.insert(path.clone()) {
                        tracing::info!(
                            "repo auto-spawn 要求 (accordion expand trigger): name={} path={}",
                            name,
                            path
                        );
                        spawn_sp_start(
                            &boot.rt_handle,
                            async_action_proxy.clone(),
                            name,
                            path,
                            boot.daemon_conn.clone(),
                        );
                    } else {
                        tracing::debug!("repo auto-spawn skip (既 trigger): {}", path);
                    }
                }
                // Phase 5-D fix: accordion 閉じた → dedup HashSet から path を release。
                //  spawn 失敗で entry が居残ったまま user が collapse → expand すれば確実に retry。
                if let Some(path) = outcome.repo_spawn_release
                    && ui.guards.repo_spawn_triggered.remove(&path)
                {
                    tracing::info!(
                        "repo auto-spawn dedup released (accordion collapse): {}",
                        path
                    );
                }
                // 「見えている Lane だけ生きている」: accordion の開閉で購読を張り直す。
                // 全 lane を回すのは LanesLoaded の再評価と同じ形（冪等・数十 lane 規模）。
                if outcome.conversation_reattach {
                    let all_addrs: Vec<String> = ui.sidebar_state
                        .lanes_by_repo
                        .values()
                        .flatten()
                        .map(|l| l.address.key().to_string())
                        .collect();
                    for addr in all_addrs {
                        ensure_conversation_attach(
                            &addr,
                            &ui.sidebar_state,
                            &mut ui.sessions.conversation_sessions,
                            &boot.rt_handle,
                            &async_action_proxy,
                            &boot.daemon_conn,
                        );
                    }
                }
                // Phase 5-C: Process restart 要求 (sidebar の 🔄 button から)。
                // 全 async work は shared runtime (rt_handle) 経由 — bare `tokio::spawn` は禁止
                // (.clippy.toml で compile gate)、 tao event loop closure に runtime context が
                // 無いので必ず `rt_handle.spawn` を使う。
                if let Some(repo_name) = outcome.restart_process_request {
                    let proxy = async_action_proxy.clone();
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        // doc 45 段 3: 旧 `POST /api/daemon/processes/{name}/restart` を
                        // Unison `daemon-control.repos/restart` に差し替え。 接続先は共有
                        // QUIC connection (port 解決は conn manager が持つ)。
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("restart_process: {}", e);
                                return;
                            }
                        };
                        match control.restart_process(&repo_name).await {
                            Ok(()) => {
                                tracing::info!("restart_process OK: {}", repo_name);
                                // 完了 → repos 再 fetch → sidebar state badge 更新。
                                // 必ず `fetch_repos_with_ports` 経由 (= runtime port merge)
                                // で送る。 list_repos() だけだと restart 直後に全 repo の
                                // port が None で潰れ、 後続 LanesLoaded で ensureLane が
                                // 全件 skip され main terminal が消失する。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "restart_process failed for {}: {}",
                                    repo_name,
                                    e
                                );
                            }
                        }
                    });
                }
                // Process stop 要求 (repo context menu の Stop repo から)。
                if let Some(repo_name) = outcome.stop_process_request {
                    let proxy = async_action_proxy.clone();
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("stop_process: {}", e);
                                return;
                            }
                        };
                        match control.stop_process(&repo_name).await {
                            Ok(()) => {
                                tracing::info!("stop_process OK: {}", repo_name);
                                // 完了 → repos 再 fetch → 停止 state を sidebar に反映。
                                // restart と同じく `fetch_repos_with_ports` 経由で
                                // 他 repo の runtime port を保つ。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "stop_process failed for {}: {}",
                                    repo_name,
                                    e
                                );
                            }
                        }
                    });
                }
                // Repo delete 要求 (repo context menu の Delete repo から、
                // UI で 2-click 確認済)。 daemon の remove_repo は稼働中 repo があると
                // エラーになるため、 先に stop → grace → remove と chain する
                // (restart_process が capability 内でやっているのと同じ順序)。
                if let Some((repo_name, repo_path)) = outcome.delete_repo_request {
                    let proxy = async_action_proxy.clone();
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("delete_repo: {}", e);
                                return;
                            }
                        };
                        // stop は best-effort: repo が未起動 (= 停止中) なら
                        // 「No running Process」 エラーが返るが、 続行して remove する。
                        match control.stop_process(&repo_name).await {
                            Ok(()) => {
                                tracing::info!("delete: stop_process OK: {}", repo_name);
                                // shutdown 伝播 + port release を待つ grace period
                                tokio::time::sleep(std::time::Duration::from_millis(500))
                                    .await;
                            }
                            Err(e) => {
                                tracing::info!(
                                    "delete: stop_process skipped for {} (continuing): {}",
                                    repo_name,
                                    e
                                );
                            }
                        }
                        match control.remove_repo(&repo_path).await {
                            Ok(()) => {
                                tracing::info!("remove_repo OK: {}", repo_path);
                                // 完了 → repos 再 fetch → sidebar から除去。
                                // 削除対象以外の repo の runtime port を保つため
                                // `fetch_repos_with_ports` 経由で送る。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "remove_repo failed for {}: {}",
                                    repo_path,
                                    e
                                );
                            }
                        }
                    });
                }
                // Phase 1 (doc 24): repo 並び替えを daemon の repo_order に永続化する。
                // restart/stop と同じ「操作 → re-fetch → ReposLoaded」パターン。成功後の
                // ReposLoaded で currents_order が canonical 順に reconcile される。
                if let Some(order) = outcome.reorder_request {
                    let proxy = async_action_proxy.clone();
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("reorder_repos: {}", e);
                                return;
                            }
                        };
                        match control.reorder_repos(order).await {
                            Ok(()) => {
                                tracing::info!("reorder_repos OK");
                                // 完了 → repos 再 fetch → canonical 順で sidebar reconcile。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!("reorder_repos failed: {}", e);
                            }
                        }
                    });
                }
                // Model Q: active lane を daemon canonical に永続 (fire-and-forget、 optimistic 適用済)。
                if let Some((repo_path, address)) = outcome.set_active_lane_request {
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let result = match conn.control().await {
                            Ok(control) => control.set_active_lane(repo_path, address).await,
                            Err(e) => Err(e),
                        };
                        if let Err(e) = result {
                            tracing::warn!("set_active_lane failed: {}", e);
                        }
                    });
                }
                // Phase 4-A: Sub Lane 削除要求 (sidebar の × button から)
                if let Some((repo_path, address)) = outcome.delete_lane_request {
                    // F6②: 旧 DaemonRpcClient.delete_lane (repo 直結 reqwest) を daemon repo-proxy
                    // ask (lane_delete) に移管。 repo port 解決は不要になり repo_path を handshake で渡す。
                    // JS-side からも先 removeLane を呼ぶ (= xterm 即時 dispose、 server 反映は
                    // repo の "lanes" topic snapshot 経由で sidebar に届く)。
                    push_main::remove_lane(&boot.webview, &address);
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let payload = serde_json::json!({ "address": &address });
                        match daemon_repo_request(
                            &conn,
                            &repo_path,
                            "lane_delete",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => {
                                tracing::info!(
                                    "Lane deleted: repo={} address={}",
                                    repo_path,
                                    address
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "lane_delete failed: repo={} address={}: {}",
                                    repo_path,
                                    address,
                                    e
                                );
                            }
                        }
                    });
                }
                // Lane Main Agent restart 要求 (sidebar の restart icon → confirm dialog から)
                if let Some((repo_path, address, fresh)) = outcome.restart_lane_request {
                    // F6③: 旧 DaemonRpcClient.restart_lane (repo 直結 reqwest) を daemon repo-proxy
                    // ask (lane_restart) に移管。 repo port 解決は不要、 repo_path を handshake で渡す。
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let payload = serde_json::json!({ "address": &address, "fresh": fresh });
                        match daemon_repo_request(
                            &conn,
                            &repo_path,
                            "lane_restart",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => {
                                // 新 pid / state は repo の "lanes" topic snapshot で購読側に push され、
                                // 端末は canvas channel demand 経由で新 PtySlot に再 attach し直す。
                                tracing::info!(
                                    "Lane restarted: repo={} address={}",
                                    repo_path,
                                    address
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "lane_restart failed: repo={} address={}: {}",
                                    repo_path,
                                    address,
                                    e
                                );
                            }
                        }
                    });
                }
                // doc 39 §8.4 提案 2: 新しい root conversation（sidebar lane 行の context menu）。
                // backend が新 session を採番して root に向ける。旧 root の pane / 会話は残る
                //（= Reset Lane との違い）。反映は lanes snapshot / session list が運ぶので
                // 楽観更新しない。
                if let Some((repo_path, address)) = outcome.new_root_request {
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        match daemon_repo_request(
                            &conn,
                            &repo_path,
                            "conversation_session_new_root",
                            serde_json::json!({ "lane": &address }),
                        )
                        .await
                        {
                            Ok(res) => {
                                let session =
                                    res.get("session").and_then(serde_json::Value::as_u64);
                                tracing::info!(
                                    "new root conversation: repo={repo_path} lane={address} session={session:?}"
                                );
                            }
                            Err(e) => tracing::warn!(
                                "conversation_session_new_root failed: repo={repo_path} lane={address}: {e}"
                            ),
                        }
                    });
                }
                // doc 44 D4/D5: 開発起点の再指定 (sidebar lane 行の context menu から)。
                // Host の帳簿のポインタを書き換えるだけ — cwd も active lane も engine も動かない。
                // 反映は次の lanes snapshot の `origin` で戻る（楽観更新しない = 帳簿が真実源）。
                if let Some((repo_path, address)) = outcome.set_origin_request {
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        // 帳簿は lane **名**で受ける（起点は repo ごとに 1 本なので
                        // address の `<repo>` 部分は冗長）。address からは末尾を取る。
                        let lane_name = address.rsplit('/').next().unwrap_or("").to_string();
                        if lane_name.is_empty() {
                            tracing::warn!("lane_origin_set: address から lane 名を取れない: {address}");
                            return;
                        }
                        let payload = serde_json::json!({ "lane": lane_name });
                        match daemon_repo_request(
                            &conn,
                            &repo_path,
                            "lane_origin_set",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => tracing::info!(
                                "開発起点を変更: repo={} lane={}",
                                repo_path,
                                lane_name
                            ),
                            Err(e) => tracing::warn!(
                                "lane_origin_set failed: repo={} lane={}: {}",
                                repo_path,
                                lane_name,
                                e
                            ),
                        }
                    });
                }
                // doc 44 §12: lane の並び順を帳簿に保存する（sidebar の DnD）。
                // address 列を lane 名の列に畳んでから投げる（帳簿は lane 名で受け、
                // 境界で lane_id に解決する — 起点と同じ規律）。
                if let Some((repo_path, order)) = outcome.reorder_lanes_request {
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let names: Vec<String> = order
                            .iter()
                            .filter_map(|a| a.rsplit('/').next())
                            .filter(|n| !n.is_empty())
                            .map(|n| n.to_string())
                            .collect();
                        if names.is_empty() {
                            tracing::warn!("lane_order_set: address 列から lane 名を取れない");
                            return;
                        }
                        let payload = serde_json::json!({ "order": names });
                        match daemon_repo_request(
                            &conn,
                            &repo_path,
                            "lane_order_set",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => tracing::info!(
                                "lane の並び順を保存: repo={} count={}",
                                repo_path,
                                names.len()
                            ),
                            Err(e) => tracing::warn!(
                                "lane_order_set failed: repo={}: {}",
                                repo_path,
                                e
                            ),
                        }
                    });
                }
                // Phase 3-A: Sub Lane 作成要求 (sidebar の + Add Sub から)
                // 投げ先は Daemon (:32000) の `daemon-control.lanes/create` 1 本 (repo port 解決は不要、
                // set_active_lane / reorder と同じ daemon-command パターン)。
                // doc 44 §9.4: daemon 側はそこで自前の provision をせず repo runtime の
                // lane 作成 core に委譲する — worktree も PtySlot も**この 1 往復で揃う**。
                // 旧構成は descriptor だけ作って PtySlot を lane_watcher の到達に賭けており、
                // 「+ で作った lane だけ engine 指定が別経路で伝わる」等の経路差が生じていた。
                // doc 11 PR-C: agent 指定 を tuple 4 番目に保持 (None なら daemon-side default)。
                if let Some((repo_path, name, branch, agent)) = outcome.add_sub_request {
                    let proxy = async_action_proxy.clone();
                    let name_clone = name.clone();
                    let branch_clone = branch.clone();
                    let stand_clone = agent.clone();
                    let path_clone = repo_path.clone();
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                let msg = e.to_string();
                                tracing::warn!("create_sub_lane: {}", msg);
                                let _ = proxy.send_event(AppEvent::SubCreateResult {
                                    repo_path: path_clone,
                                    name: name_clone,
                                    error: Some(msg),
                                });
                                return;
                            }
                        };
                        match control
                            .create_sub_lane(
                                &path_clone,
                                &name_clone,
                                branch_clone.as_deref(),
                                stand_clone.as_deref(),
                            )
                            .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    "Sub Lane created (daemon): repo={} name={} branch={:?}",
                                    path_clone,
                                    name_clone,
                                    branch_clone
                                );
                                // 応答が返った時点で lane は既に spawn 済（doc 44 §9.4）。
                                // sidebar への反映は "lanes" topic snapshot の push を待つ
                                // （楽観更新しない = 真実源は 1 つ、doc 44 §10.3 と同じ規律）。
                                // R5: 成功通知を sidebar に push back (form を閉じる)
                                let _ = proxy.send_event(AppEvent::SubCreateResult {
                                    repo_path: path_clone,
                                    name: name_clone,
                                    error: None,
                                });
                            }
                            Err(e) => {
                                // R5: 失敗通知を sidebar に push back (form 下に inline error 表示)。
                                // doc 45 段 3 以降は Unison の error 慣習 (VP-163) に従い
                                // "daemon-control.lanes/create: <daemon 側の理由>" が返る
                                // (旧 HTTP の "... HTTP 500: {json}" より読める)。 そのまま流す。
                                let msg = format!("{}", e);
                                tracing::warn!(
                                    "create_sub_lane failed: repo={} name={}: {}",
                                    path_clone,
                                    name_clone,
                                    msg
                                );
                                let _ = proxy.send_event(AppEvent::SubCreateResult {
                                    repo_path: path_clone,
                                    name: name_clone,
                                    error: Some(msg),
                                });
                            }
                        }
                    });
                }

                // doc 11 PR-C / F6④: 利用可能 Agent 一覧 fetch 要求 (sidebar の + Add Sub 開閉から)。
                // 旧 SP 直結 (client.list_agents) を撤去し daemon repo-proxy ask (`agents_list`) に移管。
                // repo port 解決が消滅し、 surface は Daemon :32000 だけを知れば済む (L1 portless 前進)。
                if let Some(repo_path) = outcome.list_stands_request {
                    let proxy = async_action_proxy.clone();
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let (agents, error) = match daemon_repo_request(
                            &conn,
                            &repo_path,
                            "agents_list",
                            serde_json::json!({}),
                        )
                        .await
                        {
                            // repo は {agents:[...]} を返す。 agents 配列だけ Vec<AgentInfo> に deserialize。
                            Ok(v) => {
                                let agents = v
                                    .get("agents")
                                    .and_then(|s| {
                                        serde_json::from_value::<Vec<crate::daemon_wire::AgentInfo>>(
                                            s.clone(),
                                        )
                                        .ok()
                                    })
                                    .unwrap_or_default();
                                tracing::debug!(
                                    "agents listed: repo={} count={}",
                                    repo_path,
                                    agents.len()
                                );
                                (agents, None)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "agents_list failed: repo={}: {}",
                                    repo_path,
                                    e
                                );
                                (Vec::new(), Some(e))
                            }
                        };
                        let _ = proxy.send_event(AppEvent::AgentsResult {
                            repo_path,
                            agents,
                            error,
                        });
                    });
                }

                // Wire inbox (doc 34 §4 V1): Daemon "wire" channel への read-only fetch
                // (ack 要求は「ack → 再 fetch」に畳む)。 async I/O なので tokio task に逃し、
                // 結果は AppEvent::WireHistoryResult で event loop に戻して
                // window.vpWire.handleResult へ push back する。
                // fetch と ack は別 IPC で、 IpcEnvelope の単一 variant match により同一
                // outcome で両立しない — 単純な合成で足りる (ack は「ack → 再 fetch」に畳む)。
                let wire_req = outcome
                    .wire_ack_request
                    .map(|(addr, id)| (addr, Some(id)))
                    .or_else(|| outcome.wire_fetch_request.map(|a| (a, None)));
                if let Some((address, ack_id)) = wire_req {
                    let proxy = async_action_proxy.clone();
                    let conn = boot.daemon_conn.clone();
                    boot.rt_handle.spawn(async move {
                        let payload = wire_fetch_payload(conn, address.clone(), ack_id).await;
                        let _ =
                            proxy.send_event(AppEvent::WireHistoryResult { address, payload });
                    });
                }

                // in-app update: sidebar footer の「更新する」ボタン click 要求。
                // native 確認ダイアログ → self-update → daemon restart → relaunch を
                // 専用スレッドで起動する（event loop = main thread は塞がない）。
                // on_phase は AppEvent 経由で event loop に戻し、「更新中…」表示に使う。
                if let Some(version) = outcome.update_apply_request {
                    let phase_proxy = proxy.clone();
                    crate::flows::update::spawn_update_flow(version, move |applying| {
                        let _ = phase_proxy.send_event(AppEvent::UpdateFlowPhase(applying));
                    });
                }

                // 設定 overlay（doc 59 P1）。fetch / save は最後に必ず「確定値を push back」で
                // 合流する — client が楽観更新をしないので、**真実は vp-app.toml 1 本**で決まり、
                // 保存失敗時の巻き戻しを client に持たせなくてよい。
                let settings_saved = outcome.settings_save_request.is_some();
                // daemon 側（settings.kdl）へ中継する分。**vp-app は書かない** — 書き手を
                // daemon 唯一にしてある（doc 59 §3）ので、ここは payload を組むだけ。
                let mut daemon_payload = serde_json::Map::new();
                if let Some(save) = outcome.settings_save_request {
                    // ⚠️ `None` の field は**不変**（「変えた分だけ送る」契約）。
                    if let Some(dev) = save.developer_mode {
                        ui.dev_mode = dev;
                        boot.open_devtools_item.set_enabled(dev);
                        boot.reload_webview_item.set_enabled(dev);
                        ui.settings.developer_mode = Some(dev);
                    }
                    if let Some(root) = save.default_repo_root {
                        // 空文字 = **未設定に戻す**（推定へのフォールバックを復活させる）。
                        // 消し方を別 UI にしないための約束 — 入力欄を空にすれば戻る。
                        let trimmed = root.trim();
                        ui.settings.default_repo_root =
                            (!trimmed.is_empty()).then(|| trimmed.to_string());
                    }
                    if let Err(e) = ui.settings.save() {
                        tracing::warn!("Settings 保存失敗: {e}");
                    }
                    if let Some(level) = save.log_level {
                        daemon_payload.insert("log_level".into(), serde_json::json!(level));
                    }
                    if let Some(minutes) = save.idle_timeout_minutes {
                        daemon_payload
                            .insert("idle_timeout_minutes".into(), serde_json::json!(minutes));
                    }
                    if let Some(agent) = save.default_agent {
                        daemon_payload.insert("default_agent".into(), serde_json::json!(agent));
                    }
                    if let Some(model) = save.default_model {
                        daemon_payload.insert("default_model".into(), serde_json::json!(model));
                    }
                }
                if outcome.settings_pick_repo_root_request {
                    // rfd は blocking なので専用スレッド → 結果は
                    // `AppEvent::SettingsRepoRootPicked` で戻る（そこで保存 + push back）。
                    let initial = resolve_default_repo_root(&ui.settings, &ui.sidebar_state);
                    spawn_repo_root_picker(async_action_proxy.clone(), initial);
                }
                if outcome.settings_fetch_request || settings_saved {
                    // ⚠️ **書いてから読む**を 1 つの task に閉じ込める。2 本に分けると
                    // 「保存より先に読み終えて古い値を表示する」順序が生まれる。
                    // 読めたら `SettingsDaemonFetched` で戻り、そこで vp-app.toml 側と
                    // 合流させて 1 回だけ push する。
                    let conn = boot.daemon_conn.clone();
                    let ev_proxy = proxy.clone();
                    let payload = (!daemon_payload.is_empty())
                        .then_some(serde_json::Value::Object(daemon_payload));
                    boot.rt_handle.spawn(async move {
                        let fetched = match conn.control().await {
                            Ok(control) => {
                                if let Some(p) = payload
                                    && let Err(e) = control.settings_set(p).await
                                {
                                    tracing::warn!("settings/set 失敗: {e}");
                                }
                                control.settings_get().await.ok()
                            }
                            Err(e) => {
                                tracing::debug!("settings: daemon に接続できない: {e}");
                                None
                            }
                        };
                        let _ = ev_proxy.send_event(AppEvent::SettingsDaemonFetched(fetched));
                    });
                }
                if outcome.daemon_restart_request {
                    // ⚠️ **全 repo = 全 lane の claude が落ちる**（doc 44 P1 fold-in）。
                    // 確認ダイアログは flow 側（rfd が blocking なので専用スレッド）。
                    crate::daemon::restart::spawn_daemon_restart();
                }
                // Hub 行の Login / Logout ボタン click 要求。blocking フロー（browser OAuth
                // 待ち / 確認ダイアログ / CLI spawn）を blocking pool で実行し、成功したら
                // `daemon-control.hub/reconnect` で daemon の hub 接続に credential 変化を
                // 即反映する（= 押した結果が数秒後の health poll で Hub 行に現れる）。
                // ACTIONS の永続化要求（doc 57 Phase 4）。watch は latest-wins なので、
                // 打鍵ごとに来ても debounce task が静まった 1 回だけを daemon へ撃つ。
                if let Some(payload) = outcome.actions_persist_request {
                    let _ = boot.actions_persist_tx.send(Some(payload));
                }
                if outcome.auth_login_request.is_some() || outcome.auth_logout_request.is_some() {
                    let login_target = outcome.auth_login_request.clone();
                    let logout_target = outcome.auth_logout_request.clone();
                    let conn = boot.daemon_conn.clone();
                    let rt = boot.rt_handle.clone();
                    boot.rt_handle.spawn(async move {
                        let flow = rt.spawn_blocking(move || match login_target {
                            Some(t) => crate::flows::auth::run_login_blocking(&t),
                            None => crate::flows::auth::run_logout_blocking(
                                logout_target.as_deref().unwrap_or(""),
                            ),
                        });
                        // false = キャンセル / 失敗 / 二重起動 → credentials 不変なので反映不要。
                        if !matches!(flow.await, Ok(true)) {
                            return;
                        }
                        match conn.control().await {
                            Ok(control) => {
                                if let Err(e) = control.hub_reconnect().await {
                                    tracing::warn!(
                                        "auth flow: hub/reconnect 要求に失敗（次の自然な再接続で反映される）: {}",
                                        e
                                    );
                                }
                            }
                            Err(e) => tracing::warn!(
                                "auth flow: daemon 接続に失敗（hub/reconnect 未送信）: {}",
                                e
                            ),
                        }
                    });
                }
            }
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
