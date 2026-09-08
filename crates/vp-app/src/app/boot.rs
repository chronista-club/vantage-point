//! 起動 = resource の構築（doc 60 §6 6-2、Codex 再レビュー ⑤「boot() は resource の寿命を明示」）。
//!
//! `run()` は `boot()` で [`Boot`] を受け取り、event loop の閉包に move して **最後まで所有する**。
//! `event_loop.run` は `-> !` なので、旧実装では `_rt` / `_tray` / `_log` は `run()` の frame に
//! 居座るだけで生きていた。struct にして所有者を名前で示す。
//!
//! 本 module の関数本体は旧 `run()` 冒頭（app/mod.rs L1047–1340、2026-09-08）を順序保持で移したもの。
//! 差分は proxy 3 本（`proxy` / `respawn_proxy` / `async_action_proxy`）の生成を `run()` 側に残した
//! こと（閉包内で `let proxy = …` の shadow が多く、struct field にすると読みにくい。統合は後続）。
//!
//! ⚠️ UI thread 専用。`Boot` は `Send` ではない（`muda::MenuItem` / `wry::WebView` / `tao::Window`）。
//! tao の `EventLoop::run` は `'static + FnMut` しか要求しないので、閉包が値で持てばよい。

use std::time::Duration;

use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoop, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{
    Rect, WebView, WebViewBuilder, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize,
};

use super::state::UiState;
use super::{
    DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, MAIN_VIEW_ASSETS, MIN_WINDOW_HEIGHT,
    MIN_WINDOW_WIDTH, initial_developer_mode, update_pane_bounds,
};
use crate::daemon::conn::{SharedDaemonConn, spawn_daemon_conn_manager};
use crate::daemon::pollers::{
    ActionsPersistPayload, spawn_actions_persist_writer, spawn_activity_poller,
    spawn_lane_inbox_poller, spawn_menu_event_pump, spawn_processes_fetch,
    spawn_session_title_poller,
};
use crate::daemon::subscriptions::spawn_device_subscription;
use crate::events::AppEvent;
use crate::session_state::SessionState;
use crate::settings::Settings;
use crate::webview::editor_bridge::fleet_feedback_payload;
use crate::webview::ipc_route::is_main_ipc_tag;
use crate::webview::terminal_ipc;

/// 起動時に構築した resource。`run()` の閉包が値で持ち、process の寿命と一致する。
///
/// 宣言順 = drop 順（今は `run()` が戻らないので関係しないが、将来の graceful exit のため）:
/// webview → window → menu / tray → daemon 接続 → runtime handle → runtime 本体 → log guard
/// （旧 local の逆順 drop と同じ: runtime を止めてから log を flush する）。
pub(super) struct Boot {
    /// sidebar + main を 1 枚に統合した WebView（`build_as_child(&window)`）。
    pub(super) webview: WebView,
    /// main window。
    pub(super) window: tao::window::Window,
    /// menu の id 表（`MenuClicked` の dispatch 用）。
    pub(super) menu_ids: crate::menu::MenuIds,
    /// View → Open Developer Tools（developer mode で有効化）。
    pub(super) open_devtools_item: muda::MenuItem,
    /// View → Reload WebView（同上）。
    pub(super) reload_webview_item: muda::MenuItem,
    /// menu bar 本体（drop すると消えるので保持）。
    _menu: muda::Menu,
    /// tray icon（初期化失敗時は None、機能は縮退）。
    _tray: Option<tray_icon::TrayIcon>,
    /// F1b: vp-app → Daemon :32000 の共有 QUIC connection ハンドル（再接続は manager が所有）。
    pub(super) daemon_conn: SharedDaemonConn,
    /// ACTIONS の永続化要求を 400ms coalesce writer へ流す watch（doc 57 Phase 4）。
    pub(super) actions_persist_tx: tokio::sync::watch::Sender<Option<ActionsPersistPayload>>,
    /// 共有 Tokio runtime の Handle。全 async work はここに乗せる（bare `tokio::spawn` は clippy で禁止）。
    pub(super) rt_handle: tokio::runtime::Handle,
    /// vp-app instance index（0 = primary / N≥1 = secondary、`VP_APP_INSTANCE`）。
    pub(super) instance_index: usize,
    /// 共有 Tokio runtime 本体。drop すると全 task が止まる。
    _rt: tokio::runtime::Runtime,
    /// tracing の guard（drop で appender が flush される）。runtime の後に落とす。
    _log: crate::log_init::LogInitResult,
}

/// resource を構築し、event loop と初期 [`UiState`] と共に返す。
///
/// `EventLoop` は window の構築に借り、`run()` が `.run(self)` で消費するので別に返す。
pub(super) fn boot() -> anyhow::Result<(EventLoop<AppEvent>, Boot, UiState)> {
    let _log = crate::log_init::init_tracing();

    // VP-192: 旧 config/data パスからの冪等なデータ移行 (Settings/SessionState 読み込み前)
    vp_paths::migrate_legacy_paths();

    // ink（対話面, doc 52 §3）: 送信済み snapshot は ephemeral だが disk に残るので、起動時に
    // 7 日超を掃除する（「消し手のないファイルを作らない」— terminal replay disk leak の轍）。
    crate::webview::ink_snapshot::prune_old(Duration::from_secs(7 * 24 * 3600));

    // Windows taskbar の identity。 **window を作る前**に設定する必要がある
    // (既存 window の AUMID は後から変えられない)。 非 Windows は no-op。
    crate::icon::set_app_user_model_id();

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // 根治: vp-app 共有 Tokio runtime (multi-thread)。
    //
    // tao の event_loop は macOS main thread を専有し、 closure 内には Tokio
    // runtime context が無いため、 bare `tokio::spawn` を呼ぶと
    // 「no reactor running」 panic で即死する (= 過去事故、 board 永続化 #456241e 等)。
    //
    // 全 async work はここで作る共有 runtime の `Handle::spawn` に乗せる。
    // closure / helper 関数には `rt_handle.clone()` を move-capture で配る。
    // `tokio::spawn` 直書きは `crates/vp-app/.clippy.toml` の
    // `disallowed-methods` で compile-time block。
    //
    // `_rt` は `run()` の戻りまで生存させる (= drop すると runtime が止まる)。
    let _rt = tokio::runtime::Runtime::new()?;
    let rt_handle = _rt.handle().clone();

    // VP-100 follow-up: 永続設定 + 1Password 風 開発者モード切替
    let settings = Settings::load();
    let initial_dev_mode = initial_developer_mode(&settings);
    tracing::info!("Settings: developer_mode = {} (initial)", initial_dev_mode);

    // メニューバー (View → Developer Mode / Open Developer Tools を含む) + トレイ
    let menu_handles = crate::menu::build_menu_bar(initial_dev_mode);
    let _menu = menu_handles.menu.clone();
    // macOS: NSApp に menu を attach、 accelerator (Cmd+N 等) を NSApplication menu hotkey 化。
    // これを呼ばないと MenuItem::new() の accelerator が NSResponder chain で発火しない。
    // 既存の PredefinedMenuItem (close_window/undo/copy 等) は muda 内部で auto-attach されるが、
    // user-defined MenuItem は明示の init_for_nsapp が要る。
    #[cfg(target_os = "macos")]
    {
        // muda 0.17: Menu::init_for_nsapp() でメニューバーに attach
        menu_handles.menu.init_for_nsapp();
    }
    let open_devtools_item = menu_handles.open_devtools_item;
    let reload_webview_item = menu_handles.reload_webview_item;
    let menu_ids = menu_handles.ids;
    let _tray = match crate::tray::build_tray() {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!("トレイ初期化失敗 (無効化): {}", e);
            None
        }
    };

    // muda の MenuEvent を main loop に橋渡しする pump を起動
    spawn_menu_event_pump(&rt_handle, event_loop.create_proxy());

    // F1b (doc 27 §3.4.4): vp-app → Daemon :32000 の全 persistent session を 1 QUIC connection に
    // 集約する共有ハンドル。 manager task が connect/reconnect を一手に所有し、 各 session
    // (device/lanes/canvas/terminal) は `wait_client` で得た共有 client に open_channel する。
    // event loop closure が move capture するので、 closure 内の spawn は `daemon_conn.clone()` を渡す。
    let daemon_conn = spawn_daemon_conn_manager(&rt_handle, crate::daemon::default_daemon_port());

    // フィードバック方向 (doc 49 LE-19): webview の場の状態 → daemon-device 上り event。
    // watch = latest-wins (webview が throttle 済みでも Rust 側で自然に coalesce)。
    // 送り手 = ipc_handler の "fleet:feedback" 分岐 / 受け手 = device session の sender task。
    let (fleet_feedback_tx, fleet_feedback_rx) =
        tokio::sync::watch::channel(serde_json::Value::Null);

    // ACTIONS の永続化 (doc 57 Phase 4)。同じく watch = latest-wins。
    // 送り手 = `handle_sidebar_ipc` の `actions:persist` 分岐 / 受け手 = 下の debounce task。
    let (actions_persist_tx, actions_persist_rx) =
        tokio::sync::watch::channel::<Option<ActionsPersistPayload>>(None);
    spawn_actions_persist_writer(&rt_handle, actions_persist_rx, daemon_conn.clone());

    // DeviceRegistry 🧲 device event を daemon (daemon-device channel) から購読する (daemon に 1 本)。
    // canvas/lanes は per-repo だが device は machine scope (= daemon singleton) なので起動時 1 回。
    spawn_device_subscription(
        &rt_handle,
        event_loop.create_proxy(),
        daemon_conn.clone(),
        fleet_feedback_rx,
    );

    // vp-app instance index 判定 (= multi-window 復元)。 per-instance file load に先立って
    // 必要なので session_state より前に確定する。
    // `VP_APP_INSTANCE` (= "0", "1", ...) が instance 番号。 未設定 / "0" = primary。
    let instance_index: usize = std::env::var("VP_APP_INSTANCE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let is_primary = instance_index == 0;
    tracing::info!(
        "vp-app boot: instance_index={} (= {})",
        instance_index,
        if is_primary { "primary" } else { "secondary" }
    );

    // session_state を WindowBuilder より前に load して、 window geometry (= 前回終了時の
    // position + size + monitor) を起動時に復元できるようにする。 per-instance 分離後は
    // **自分の instance file** (`session.json` / `session.<N>.json`) を読む。 `mut` で keep し、
    // 後段で active_lane_address / repos / currents_order 等の mutate + save にも使う。
    let mut session_state = SessionState::load(instance_index);
    // この instance window を「開いている」 と記録する (= 次回 primary 起動時の auto-spawn
    // signal)。 clean close (`CloseRequested`) で `open=false` に上書きするので、 明示的に
    // 閉じた window は復活せず、 kill された window は復元される。
    session_state.set_open(true);
    session_state.save();

    // PR #458: invalid geometry (= MIN 未満 / NaN / Inf) は None に fallback。
    // per-instance 分離後は自分の file の geometry を使う。
    let restored_geometry = session_state.window_geometry().cloned();

    // 最低サイズ + 起動時 size 強制矯正 — sidebar (固定 280px) 圧縮 bug の構造的防御。
    //
    // 1. `with_min_inner_size`: sidebar 幅 (280) + 余裕ある main 領域を構造的に確保する OS
    //    レベル下限 (NSWindow.setMinSize)。 手動 drag による narrow 化を防ぐ。
    // 2. 起動時 clamp: macOS state restoration は `applicationDidFinishLaunching` 後の
    //    async phase で `restorableState` を frame に反映するため、 build 直後の同期
    //    `inner_size()` チェックは race する (#428 Moody Blues Issue #1 で発覚)。
    //    EventLoop が走り始めた**最初の Resized event** (= restoration 適用後) で
    //    min 未満を検出して `set_inner_size(DEFAULT)` で force-resize する経路に移行。
    //    詳細は event loop の Resized handler 側コメント。
    // 3. window geometry 復元: `session_state.window_geometry` Some なら前回の size +
    //    position を apply (= 個別位置)。 None なら default。 monitor 復元は EventLoop
    //    走り始め後に `available_monitors()` で確認、 disconnect されてれば primary 内に clamp。
    let mut builder = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_min_inner_size(LogicalSize::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT));
    // window icon (Windows: titlebar + taskbar / Linux: WM)。 Windows は exe に焼いた icon
    // resource が主役だが、 window 単位の icon を明示しておくと起動経路によらず確実に出る。
    // mac は dock icon (icon::set_app_icon) が担当で window icon の概念が無いため素通り。
    if let Some((rgba, w, h)) = crate::icon::icon_rgba(256)
        && let Ok(icon) = tao::window::Icon::from_rgba(rgba, w, h)
    {
        builder = builder.with_window_icon(Some(icon));
    }
    if let Some(geom) = &restored_geometry {
        builder = builder
            .with_inner_size(LogicalSize::new(geom.width, geom.height))
            .with_position(LogicalPosition::new(geom.x, geom.y));
    } else {
        builder = builder.with_inner_size(LogicalSize::new(
            DEFAULT_WINDOW_WIDTH,
            DEFAULT_WINDOW_HEIGHT,
        ));
    }
    let window = builder.build(&event_loop)?;

    // 表示モード復元 (doc 30 §6.1): windowed 座標で build した後、 保存が Fullscreen なら全画面化する。
    // windowed frame を base に残すことで全画面解除時に元の窓サイズへ戻せる。 monitor 精密指定は
    // EventLoop 走行後の `available_monitors()` race を避け、 current monitor (= 復元位置の display) で
    // 全画面化する (`Borderless(None)`)。 monitor 相対の厳密復元は doc 30 §6.2 の将来課題。
    if restored_geometry
        .as_ref()
        .is_some_and(|g| g.display_mode == crate::session_state::DisplayMode::Fullscreen)
    {
        tracing::info!(
            "session restore [instance={}]: 全画面モードを復元",
            instance_index
        );
        window.set_fullscreen(Some(tao::window::Fullscreen::Borderless(None)));
    }

    // primary 起動時、 前回「開いていた」 secondary instance (= `session.<N>.json` で
    // open==true、 N≥1) を **child process として auto-spawn** する。 これで「複数 window を
    // 開いて再起動 → 全 window 復元」 が動く。 明示的に閉じた (= clean close で open=false)
    // instance は復活しない ─ per-instance file 分離 + open flag 管理によって、 共有 1 file
    // 時代の「close しても slot が残り再 spawn される」 bug を根治した。
    //
    // 子は `VP_APP_INSTANCE=<idx>` で自分の file を read する。
    // spawn 失敗は warn して continue (= primary 起動は阻害しない)。
    if is_primary {
        let to_spawn = SessionState::open_secondary_indices();
        if !to_spawn.is_empty() {
            match std::env::current_exe() {
                Ok(exe) => {
                    for idx in to_spawn {
                        match std::process::Command::new(&exe)
                            .env("VP_APP_INSTANCE", idx.to_string())
                            .spawn()
                        {
                            Ok(child) => tracing::info!(
                                "auto-spawned secondary instance (pid={}, instance_index={})",
                                child.id(),
                                idx
                            ),
                            Err(e) => tracing::warn!(
                                "auto-spawn secondary (instance={}) failed (起動は継続): {}",
                                idx,
                                e
                            ),
                        }
                    }
                }
                Err(e) => tracing::warn!("current_exe() 失敗 (auto-spawn skip): {}", e),
            }
        }
    }

    // Terminal backend: daemon を auto-launch (down なら `vp` binary を spawn)。
    let node_url = std::env::var("VP_DAEMON_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", crate::daemon::default_daemon_port()));
    if let Err(e) = crate::daemon::launcher::ensure_daemon_ready(&node_url) {
        tracing::warn!(
            "daemon auto-launch 失敗 (continue with offline state): {}",
            e
        );
    }

    // daemon から repo list を非同期 fetch (起動初回)
    spawn_processes_fetch(&rt_handle, event_loop.create_proxy(), daemon_conn.clone());
    // VP-95: Activity widget の定期更新 (5s 間隔)
    spawn_activity_poller(&rt_handle, event_loop.create_proxy(), daemon_conn.clone());
    // VP-143: cc session display name (custom-title) の 5s 周期 resolve
    spawn_session_title_poller(&rt_handle, event_loop.create_proxy());
    // VP-147 PR-P2-3: per-Lane mailbox inbox 状況の 5s 周期 resolve (sidebar message icon 用 signal)
    spawn_lane_inbox_poller(&rt_handle, event_loop.create_proxy());

    // WebView 統合 (step 3a): sidebar + main を 1 WebView (1 DOM, CSS flex) に統合。
    // sidebar.bundle.js は vp-asset://app/sidebar.bundle.js の外部 script として load される
    // (doc 48 Phase 1 で inline → 外部化。#sidebar-root に mount)。
    // 旧 2 WebView (cross-WebView IPC bridge で keyboard を 2 往復させていた) を廃し、
    // sidebar↔main の event / state が同一 DOM 内で直接流れる。
    let sidebar_ipc_proxy = event_loop.create_proxy();
    let ipc_proxy = event_loop.create_proxy();
    // DevTools は compile 時 always 有効。menu の「Open Developer Tools」から
    // `webview.open_devtools()` を呼ぶかで runtime 制御 (本番ビルドでも切替可)。
    // echo probe trigger (Unison 北極星 step 2/3): VP_UNISON_ECHO_CERT が set なら
    // webview load 前に cert を global へ注入する。 entry.tsx が load 時に検出して
    // window.vpUnisonEcho を auto-run し、 結果は console bridge 経由で app.kdl.log に出る
    // (= agent が DevTools なしで round-trip を観測する経路)。 未 set なら空 script で no-op。
    let echo_init = std::env::var("VP_UNISON_ECHO_CERT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|cert| {
            format!(
                "window.__VP_ECHO_CERT__ = {};",
                serde_json::to_string(&cert).unwrap_or_else(|_| "\"\"".into())
            )
        })
        .unwrap_or_default();
    let webview = WebViewBuilder::new()
        // 統合 origin fix: with_html (about:blank = 不透明オリジン) だと localStorage が
        // SecurityError を throw し sidebar bundle が boot 中に落ちる。custom protocol で
        // 実オリジン (vp-asset://app) を与え、MAIN_AREA_HTML を app/index.html として配信する。
        .with_custom_protocol("vp-asset".to_string(), move |id, request| {
            crate::webview::assets::serve(id, request, MAIN_VIEW_ASSETS)
        })
        .with_initialization_script(&echo_init)
        .with_url("vp-asset://app/index.html")
        .with_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: WryLogicalSize::new(DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT).into(),
        })
        .with_devtools(true)
        .with_ipc_handler(move |req| {
            // 統合 ipc dispatch: t 値で明示分岐 (sidebar tag と main tag は disjoint)。
            // main tag (terminal / pane 系) → terminal、 それ以外 (sidebar IpcEnvelope:
            // repo: / lane: 系) → SidebarIpc。 terminal の fall-through に頼らない。
            let body = req.body();
            // fleet feedback (LE-19) は event loop を経由せず watch へ直行 (高頻度 + 状態量)
            if let Some(fb) = fleet_feedback_payload(body) {
                let _ = fleet_feedback_tx.send(fb);
                return;
            }
            if is_main_ipc_tag(body) {
                terminal_ipc::handle_ipc_message(body, &ipc_proxy);
            } else {
                let _ = sidebar_ipc_proxy.send_event(AppEvent::SidebarIpc(body.to_string()));
            }
        })
        .with_focused(true)
        .build_as_child(&window)?;

    tracing::info!("メインウィンドウ作成 (sidebar + main を 1 WebView に統合)");

    // 起動直後の bounds 明示同期 — 「下部が空く」 bug の構造的 fix。
    // WebView の初期 `with_bounds` は DEFAULT_WINDOW_HEIGHT (800) 固定なので、 復元 geometry が
    // DEFAULT より大きい (= 前回 window を縦に広げていた) 場合、 起動後に `WindowEvent::Resized`
    // が発火しない限り content が 800px のまま下部が黒く空く。 macOS は `with_inner_size` で
    // born した window に初回 Resized を出さないことがあるため、 ここで実 inner_size に
    // 明示同期して初回 paint から content view を全面に張る (Resized handler と idempotent)。
    update_pane_bounds(&webview, window.inner_size(), window.scale_factor());

    let ui = UiState::new(
        settings,
        session_state,
        initial_dev_mode,
        restored_geometry.is_some(),
    );
    let boot = Boot {
        webview,
        window,
        menu_ids,
        open_devtools_item,
        reload_webview_item,
        _menu,
        _tray,
        daemon_conn,
        actions_persist_tx,
        rt_handle,
        instance_index,
        _rt,
        _log,
    };
    Ok((event_loop, boot, ui))
}
