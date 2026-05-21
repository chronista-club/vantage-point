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
//! │ │ (Creo)   │   ┌─ pane-terminal (xterm.js)─────┐   │ │
//! │ │ project  │   ├─ pane-canvas (placeholder)─────┤   │ │
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

use std::thread;
use std::time::Duration;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{
    Rect, WebView, WebViewBuilder, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize,
};

use crate::client::TheWorldClient;
use crate::main_area::{self, ActivePaneInfo, MAIN_AREA_HTML, SlotRect};
use crate::pane::{ActiveStand, ActivitySnapshot, ProcessPaneState, SidebarState};
use crate::project_dialog::{
    resolve_default_project_root, spawn_add_project_picker, spawn_clone_path_picker,
    spawn_clone_project,
};
use crate::session_state::SessionState;
use crate::settings::Settings;
use crate::terminal::{self, AppEvent};

/// Sidebar の固定幅 (LogicalPixel)
const SIDEBAR_WIDTH: f64 = 280.0;

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
    if let Ok(v) = std::env::var("VP_DEVELOPER_MODE") {
        match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => return true,
            "0" | "false" | "no" | "off" => return false,
            _ => {}
        }
    }
    if let Some(b) = settings.developer_mode {
        return b;
    }
    cfg!(debug_assertions)
}

/// Creo UI design tokens (CSS custom properties、mint-dark default)
///
/// <https://github.com/chronista-club/creo-ui> packages/web が source。
/// vp-app の 3 ペインすべてに inline して共通 token で描画する。
pub const CREO_TOKENS_CSS: &str = include_str!("../assets/creo-tokens.css");

/// 柱 2: SolidJS + creoui で構築した sidebar の JS bundle。
/// `crates/vp-app/web-bundle/` で `bun run build` → `assets/sidebar.bundle.js` 生成。
/// `SIDEBAR_HTML` 内の `<script src="vp-asset://app/sidebar.bundle.js">` で load される。
const SIDEBAR_BUNDLE: &[u8] = include_bytes!("../assets/sidebar.bundle.js");

/// sidebar (SolidJS) を mount する最小 HTML shell。
/// 描画ロジックは持たず `sidebar.bundle.js` に全て委ねる。creoui token
/// (`var(--color-*)`) 解決のため `creo-tokens.css` のみ inline する。
const SIDEBAR_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="ja" data-theme="contrast-dark">
<head><meta charset="utf-8"><style>"#,
    include_str!("../assets/creo-tokens.css"),
    r#"</style></head>
<body>
<div id="sidebar-root"></div>
<script src="vp-asset://app/sidebar.bundle.js"></script>
</body>
</html>"#,
);

/// sidebar webview の custom protocol closure に渡す asset 群。
/// `app/sidebar.html` (shell HTML) と `app/sidebar.bundle.js` (SolidJS bundle)。
const SIDEBAR_ASSETS: &[(&str, &[u8], &str)] = &[
    (
        "app/sidebar.html",
        SIDEBAR_HTML.as_bytes(),
        "text/html; charset=utf-8",
    ),
    (
        "app/sidebar.bundle.js",
        SIDEBAR_BUNDLE,
        "application/javascript; charset=utf-8",
    ),
];

/// Sidebar + Main area の bounds をウィンドウサイズから計算 (VP-100 Phase 2)
///
/// Phase 2 で canvas + terminal の 2 WebView を main_view 1 つに統合。
/// レイアウトは sidebar (左固定 280px) + main (右側全部) のシンプル構造。
fn update_pane_bounds(
    sidebar: &WebView,
    main_view: &WebView,
    window_size: tao::dpi::PhysicalSize<u32>,
    scale: f64,
) {
    let logical = window_size.to_logical::<f64>(scale);
    let width = logical.width;
    let height = logical.height;
    let right_x = SIDEBAR_WIDTH;
    let right_w = (width - SIDEBAR_WIDTH).max(0.0);

    let _ = sidebar.set_bounds(Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: WryLogicalSize::new(SIDEBAR_WIDTH, height).into(),
    });
    let _ = main_view.set_bounds(Rect {
        position: LogicalPosition::new(right_x, 0.0).into(),
        size: WryLogicalSize::new(right_w, height).into(),
    });
}

/// muda の `MenuEvent::receiver()` channel を polling して `AppEvent::MenuClicked` に
/// 変換する pump スレッドを起動する。muda の menu event は global channel (single
/// receiver) なので 1 thread だけ起動する。
fn spawn_menu_event_pump(proxy: EventLoopProxy<AppEvent>) {
    let _ = thread::Builder::new()
        .name("menu-event-pump".into())
        .spawn(move || {
            let rx = muda::MenuEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if proxy.send_event(AppEvent::MenuClicked(ev.id)).is_err() {
                    tracing::debug!("EventLoop 終了、menu pump も終了");
                    break;
                }
            }
        });
}

/// 起動時に TheWorld の Process list を別スレッドで fetch。
///
/// **Phase A4-3b bug fix (mem_1CaTpCQH8iLJ2PasRcPjHv Architecture v4)**:
/// `/api/world/projects` (registered Process list、port は持たない) と
/// `/api/world/processes` (running Process list、port + pid 持つ) を **併行 fetch + join** して、
/// 各 Process に `port` と `state` を解決した状態で `ProcessesLoaded` event に乗せる。
///
/// これにより handler 側で `if let Some(port) = p.port { spawn_lanes_subscription(...) }` が動く経路完成。
fn spawn_processes_fetch(proxy: EventLoopProxy<AppEvent>) {
    let _ = thread::Builder::new()
        .name("processes-fetch".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = proxy.send_event(AppEvent::ProcessesError(format!(
                        "tokio runtime 作成失敗: {}",
                        e
                    )));
                    return;
                }
            };
            rt.block_on(async {
                let client = TheWorldClient::default();
                // 併行 fetch: registered list + running list
                let (proj_res, run_res) = tokio::join!(
                    client.list_projects(),
                    client.list_processes(),
                );
                match proj_res {
                    Ok(mut processes) => {
                        // running list から (name → port) map を作って join
                        let port_by_name: std::collections::HashMap<String, u16> = match run_res {
                            Ok(runs) => runs.into_iter().map(|r| (r.project_name, r.port)).collect(),
                            Err(e) => {
                                tracing::warn!(
                                    "list_processes (running) 失敗 (port 不明、Lane fetch skip): {}",
                                    e
                                );
                                std::collections::HashMap::new()
                            }
                        };
                        // ProcessInfo に port を merge。
                        // state は daemon の process_status が SSOT ── join で上書き
                        // しない (旧実装は running list の有無で Running/Dead を上書き
                        // していたが、 add_project 経路が join を通らず default state が
                        // 露出するバグの温床だった)。
                        for p in &mut processes {
                            if let Some(&port) = port_by_name.get(&p.name) {
                                p.port = Some(port);
                            }
                        }
                        let running_count = processes.iter().filter(|p| p.port.is_some()).count();
                        tracing::info!(
                            "TheWorld Processes: {} 件 (running={} 件)",
                            processes.len(),
                            running_count
                        );
                        let _ = proxy.send_event(AppEvent::ProcessesLoaded(processes));
                    }
                    Err(e) => {
                        tracing::warn!("TheWorld fetch 失敗 (daemon 未起動?): {}", e);
                        let _ = proxy.send_event(AppEvent::ProcessesError(e.to_string()));
                    }
                }
            });
        });
}

/// 1 回の Unison channel 接続セッションの終わり方 ("lanes" / "canvas" 購読が共用)。
enum SubscriptionOutcome {
    /// セッション確立後に切断 (SP restart / channel close)。即再接続の対象。
    Disconnected,
    /// event loop が閉じた (= app 終了)。購読スレッドを畳む。
    AppClosing,
}

/// wiremsg Stage 1 consumer: SP の "lanes" Unison channel を購読し、retained Lane
/// snapshot を受信して `AppEvent::LanesLoaded` を emit する。旧 `spawn_lanes_fetch`
/// (one-shot HTTP poll) を置換する long-lived 購読。接続が切れたら指数バックオフで
/// 再接続し、10 連続失敗で諦めて `AppEvent::LanesSubscriptionEnded` を emit する。
/// SP が同じ project を再 spawn すれば次の `ProcessesLoaded` で購読も再 spawn される。
/// 設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
fn spawn_lanes_subscription(proxy: EventLoopProxy<AppEvent>, process_path: String, sp_port: u16) {
    let _ = thread::Builder::new()
        .name(format!("lanes-sub-{}", sp_port))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = proxy.send_event(AppEvent::LanesError {
                        process_path: process_path.clone(),
                        message: format!("tokio runtime: {}", e),
                    });
                    let _ = proxy.send_event(AppEvent::LanesSubscriptionEnded { process_path });
                    return;
                }
            };
            rt.block_on(lanes_subscription_loop(proxy, process_path, sp_port));
        });
}

/// "lanes" channel への接続 → 購読 → 再接続を司る long-lived ループ。
async fn lanes_subscription_loop(
    proxy: EventLoopProxy<AppEvent>,
    process_path: String,
    sp_port: u16,
) {
    // QUIC ポート = HTTP ポート (QUIC_PORT_OFFSET = 0、TCP/UDP は同一ポートで共存)。
    let addr = format!("[::1]:{}", sp_port);
    const MAX_FAILURES: u32 = 10;
    let mut failures: u32 = 0;

    loop {
        match run_lanes_session(&proxy, &process_path, &addr).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {
                // セッション確立後の切断 (SP restart 等)。失敗カウンタをリセットし、
                // 短い固定 delay を挟んで即再接続する (確立直後の即切断による busy loop を防ぐ)。
                failures = 0;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(
                    "lanes subscription failed ({}/{}): project={}: {}",
                    failures,
                    MAX_FAILURES,
                    process_path,
                    e
                );
                let _ = proxy.send_event(AppEvent::LanesError {
                    process_path: process_path.clone(),
                    message: e,
                });
                if failures >= MAX_FAILURES {
                    tracing::warn!(
                        "lanes subscription giving up: project={} (SP unreachable)",
                        process_path
                    );
                    let _ = proxy.send_event(AppEvent::LanesSubscriptionEnded { process_path });
                    return;
                }
                // 指数バックオフ 500ms〜16s (TUI→Process reconnect と同じカーブ)。
                let delay_ms = std::cmp::min(500u64 << (failures - 1), 16_000);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// 1 回の接続セッション: QUIC connect → `open_channel("lanes")` → recv ループ。
///
/// retained topic なので接続直後に現スナップショットが届き、以降 LanePool 変化のたび
/// push される。`Ok` = セッション確立後の終了 (再接続 or app 終了)、`Err` = 接続 or
/// channel open に失敗 (失敗カウンタの対象)。
async fn run_lanes_session(
    proxy: &EventLoopProxy<AppEvent>,
    process_path: &str,
    addr: &str,
) -> Result<SubscriptionOutcome, String> {
    use unison::ProtocolClient;
    use unison::network::MessageType;
    use unison::network::TrustAnchors;
    use unison::network::quic::QuicClient;

    let transport = QuicClient::builder()
        .trust_anchors(TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client build: {}", e))?;
    let client = ProtocolClient::new(transport);
    client
        .connect(addr)
        .await
        .map_err(|e| format!("connect {}: {}", addr, e))?;
    let channel = client
        .open_channel("lanes")
        .await
        .map_err(|e| format!("open lanes channel: {}", e))?;
    tracing::info!(
        "lanes subscription connected: project={} addr={}",
        process_path,
        addr
    );

    loop {
        let msg = match channel.recv().await {
            Ok(m) => m,
            // セッション確立後の切断 (SP 停止 / channel close)。再接続対象。
            Err(_) => return Ok(SubscriptionOutcome::Disconnected),
        };
        // SP 側 "lanes" channel は `send_event("snapshot", ...)` で push する。
        if msg.msg_type != MessageType::Event || msg.method != "snapshot" {
            continue;
        }
        let payload = match msg.payload_as_value() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("lanes snapshot payload parse failed: {}", e);
                continue;
            }
        };
        // payload = ProcessMessage::LanesSnapshot = {"type":"lanes_snapshot","lanes":[...]}。
        // topic は `process/star-platinum/state/#` の wildcard 購読なので、将来別 message
        // 種別が同 subtree に publish されても無視する。
        if payload.get("type").and_then(|t| t.as_str()) != Some("lanes_snapshot") {
            continue;
        }
        let lanes: Vec<crate::client::LaneInfo> =
            match serde_json::from_value(payload.get("lanes").cloned().unwrap_or_default()) {
                Ok(lanes) => lanes,
                Err(e) => {
                    tracing::warn!("lanes snapshot decode failed: {}", e);
                    continue;
                }
            };
        tracing::info!(
            "LanesLoaded (wiremsg): project={} ({} lanes)",
            process_path,
            lanes.len()
        );
        if proxy
            .send_event(AppEvent::LanesLoaded {
                process_path: process_path.to_string(),
                lanes,
            })
            .is_err()
        {
            // event loop が閉じた = app 終了。購読スレッドを畳む。
            return Ok(SubscriptionOutcome::AppClosing);
        }
    }
}

/// wiremsg Stage 2 consumer: SP の "canvas" Unison channel を購読し、Canvas (Paisley Park)
/// ProcessMessage を受信して `AppEvent::CanvasMessage` を emit する。`spawn_lanes_subscription`
/// と同型（QUIC 購読 + 指数バックオフ再接続）。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
fn spawn_canvas_subscription(proxy: EventLoopProxy<AppEvent>, process_path: String, sp_port: u16) {
    let _ = thread::Builder::new()
        .name(format!("canvas-sub-{}", sp_port))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("canvas-sub tokio runtime: {}", e);
                    let _ = proxy.send_event(AppEvent::CanvasSubscriptionEnded { process_path });
                    return;
                }
            };
            rt.block_on(canvas_subscription_loop(proxy, process_path, sp_port));
        });
}

/// "canvas" channel への接続 → 購読 → 再接続を司る long-lived ループ。
async fn canvas_subscription_loop(
    proxy: EventLoopProxy<AppEvent>,
    process_path: String,
    sp_port: u16,
) {
    let addr = format!("[::1]:{}", sp_port);
    const MAX_FAILURES: u32 = 10;
    let mut failures: u32 = 0;

    loop {
        match run_canvas_session(&proxy, &process_path, &addr).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {
                failures = 0;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(
                    "canvas subscription failed ({}/{}): project={}: {}",
                    failures,
                    MAX_FAILURES,
                    process_path,
                    e
                );
                if failures >= MAX_FAILURES {
                    tracing::warn!(
                        "canvas subscription giving up: project={} (SP unreachable)",
                        process_path
                    );
                    let _ = proxy.send_event(AppEvent::CanvasSubscriptionEnded { process_path });
                    return;
                }
                let delay_ms = std::cmp::min(500u64 << (failures - 1), 16_000);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// 1 回の "canvas" channel 接続セッション: connect → `open_channel("canvas")` → recv ループ。
///
/// "canvas" channel は `process/paisley-park/#` retained topic を購読しており、接続直後に
/// 現スナップショット（最新 Show 等）が届く。各メッセージは `send_event("pane", <JSON>)` で
/// 来る（payload = ProcessMessage の生 JSON）。
async fn run_canvas_session(
    proxy: &EventLoopProxy<AppEvent>,
    process_path: &str,
    addr: &str,
) -> Result<SubscriptionOutcome, String> {
    use unison::ProtocolClient;
    use unison::network::MessageType;
    use unison::network::TrustAnchors;
    use unison::network::quic::QuicClient;

    let transport = QuicClient::builder()
        .trust_anchors(TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client build: {}", e))?;
    let client = ProtocolClient::new(transport);
    client
        .connect(addr)
        .await
        .map_err(|e| format!("connect {}: {}", addr, e))?;
    let channel = client
        .open_channel("canvas")
        .await
        .map_err(|e| format!("open canvas channel: {}", e))?;
    tracing::info!(
        "canvas subscription connected: project={} addr={}",
        process_path,
        addr
    );

    loop {
        let msg = match channel.recv().await {
            Ok(m) => m,
            Err(_) => return Ok(SubscriptionOutcome::Disconnected),
        };
        // SP 側 "canvas" channel は `send_event("pane", <ProcessMessage JSON>)` で push する。
        if msg.msg_type != MessageType::Event || msg.method != "pane" {
            continue;
        }
        let payload = match msg.payload_as_value() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("canvas payload parse failed: {}", e);
                continue;
            }
        };
        if proxy
            .send_event(AppEvent::CanvasMessage {
                process_path: process_path.to_string(),
                message: payload,
            })
            .is_err()
        {
            // event loop が閉じた = app 終了。
            return Ok(SubscriptionOutcome::AppClosing);
        }
    }
}

/// Phase 2.5 (per-Lane instance): main_view の JS API を呼ぶ helper 群。
/// xterm.js + WebSocket は **JS-side で per-Lane に管理** され、 Rust は thin trigger を出すだけ。
mod lane_js {
    use wry::WebView;

    /// JS string literal にする (Phase review fix #3 と同設計: serde_json::to_string で
    /// 全 UTF-8 + null byte + surrogate を JSON spec で escape、 JS の valid string literal に)。
    /// Lane address は通常 ASCII safe (`<project>/lead`) だが、 一貫性と future-proof のため統一。
    fn js_str(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
    }

    /// `window.ensureLane(address, port)` を呼ぶ — 既存ならば no-op (idempotent)。
    pub fn ensure_lane(main_view: &WebView, address: &str, port: u16) {
        let script = format!("window.ensureLane({}, {})", js_str(address), port);
        if let Err(e) = main_view.evaluate_script(&script) {
            tracing::warn!("ensureLane script failed (addr={}): {}", address, e);
        }
    }

    /// `window.showLane(address)` を呼ぶ — active な 1 Lane を表示。 None / 不在の address なら empty placeholder。
    pub fn show_lane(main_view: &WebView, address: Option<&str>) {
        let script = match address {
            Some(a) => format!("window.showLane({})", js_str(a)),
            None => "window.showLane(null)".into(),
        };
        if let Err(e) = main_view.evaluate_script(&script) {
            tracing::warn!("showLane script failed: {}", e);
        }
    }

    /// `window.removeLane(address)` を呼ぶ — Lane が消えた時に xterm + WS を dispose。
    pub fn remove_lane(main_view: &WebView, address: &str) {
        let script = format!("window.removeLane({})", js_str(address));
        if let Err(e) = main_view.evaluate_script(&script) {
            tracing::warn!("removeLane script failed (addr={}): {}", address, e);
        }
    }
}

/// 「Current project が dead 状態」 のとき TheWorld に SP spawn を要求する fire-and-forget task。
///
/// State は TheWorld が持つ (mem_1CaTpCQH8iLJ2PasRcPjHv) ので、 vp-app は再起動しても
/// 既存 SP がいれば自動で続行 (state == running なので spawn 不要)。 dead のときだけ trigger。
///
/// 重複防止: 呼び出し側が `triggered: HashSet<String>` で path の dedup を担う。
/// (TheWorld 側でも `Process already running` で弾かれるが、 余計な POST を避けるため。)
fn spawn_sp_start(proxy: EventLoopProxy<AppEvent>, project_name: String, project_path: String) {
    let _ = thread::Builder::new()
        .name(format!("sp-start-{}", project_name))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("sp-start tokio runtime 失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async {
                let client = TheWorldClient::default();
                match client.start_process(&project_name).await {
                    Ok(()) => {
                        tracing::info!(
                            "SP auto-spawn 要求成功: project={} path={}",
                            project_name,
                            project_path
                        );
                        // TheWorld の polling が新 SP を pick up すると、 既存の
                        // spawn_processes_fetch / spawn_activity_poller が ProcessesLoaded を再送、
                        // その流れで spawn_lanes_subscription が走って "lanes" channel を購読、
                        // retained snapshot を受信して sidebar に Lane が出る。
                        // ここで明示的に trigger する必要はない (polling が 5s で SP を拾う)。
                        let _ = proxy; // 将来 spawn 完了通知 event を入れるなら使う
                    }
                    Err(e) => {
                        tracing::warn!(
                            "SP auto-spawn 失敗: project={} path={}: {}",
                            project_name,
                            project_path,
                            e
                        );
                    }
                }
            });
        });
}

/// VP-95: Activity widget の定期更新。
///
/// 5 秒間隔で `/api/health` + `/api/world/projects` + `/api/world/processes` を
/// fetch し、`AppEvent::ActivityUpdate` として main thread に push する。
/// daemon 未起動時は world_online=false で穏やかに通る。
///
/// VP-100 follow-up (B1 / MB1 / PH#7): daemon が **後発で online 復帰** した時、
/// `world_online: false → true` の遷移を検知して `/api/world/projects` を
/// 再 fetch し `AppEvent::ProcessesLoaded` を再送する。これにより sidebar
/// projects accordion が永遠に空のまま、という UX バグを防ぐ。
/// 起動初回 (`prev_online == None`) では `spawn_processes_fetch` 側が担当するので
/// 二重 fetch を避けるため transition 検知をスキップする。
fn spawn_activity_poller(proxy: EventLoopProxy<AppEvent>) {
    let _ = thread::Builder::new()
        .name("activity-poller".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("activity poller tokio runtime 作成失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let client = TheWorldClient::default();
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                let mut prev_online: Option<bool> = None;
                let mut prev_running: Option<usize> = None;
                loop {
                    tick.tick().await;
                    let snap = collect_activity(&client).await;
                    let became_online = matches!(prev_online, Some(false)) && snap.world_online;
                    let running_changed =
                        prev_running.is_some_and(|p| p != snap.running_process_count);
                    prev_online = Some(snap.world_online);
                    prev_running = Some(snap.running_process_count);
                    if proxy
                        .send_event(AppEvent::ActivityUpdate(snap.clone()))
                        .is_err()
                    {
                        tracing::debug!("EventLoop 終了、activity poller も終了");
                        break;
                    }
                    // 再 fetch trigger (Architecture v4 fix、 mem_1CaTpCQH8iLJ2PasRcPjHv):
                    // - daemon online 復帰 (false → true)
                    // - running 数変化 (SP 起動 / 停止)
                    // どちらも port join 経由で ProcessesLoaded 再送 → sidebar state badge 更新
                    if (became_online || running_changed) && snap.world_online {
                        let (proj_res, run_res) = tokio::join!(
                            client.list_projects(),
                            client.list_processes(),
                        );
                        if let Ok(mut processes) = proj_res {
                            let port_by_name: std::collections::HashMap<String, u16> =
                                match run_res {
                                    Ok(runs) => runs
                                        .into_iter()
                                        .map(|r| (r.project_name, r.port))
                                        .collect(),
                                    Err(_) => std::collections::HashMap::new(),
                                };
                            // state は daemon の process_status が SSOT ── port のみ merge。
                            for p in &mut processes {
                                if let Some(&port) = port_by_name.get(&p.name) {
                                    p.port = Some(port);
                                }
                            }
                            let running_count =
                                processes.iter().filter(|p| p.port.is_some()).count();
                            tracing::info!(
                                "polling re-fetch (online={} running_changed={}): processes={} running={}",
                                became_online,
                                running_changed,
                                processes.len(),
                                running_count
                            );
                            if proxy
                                .send_event(AppEvent::ProcessesLoaded(processes))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            });
        });
}

/// VP-143: 5s 間隔で `AppEvent::ResolveSessionTitles` を fire する background poller。
///
/// task 自体は state を持たず、 ただ tick を main thread に届ける役割。 main thread の
/// handler が `sidebar_state.lanes_by_project` を walk して
/// `session_title::resolve_title_for_cwd` を呼び、 結果を `session_titles` map に diff/update
/// + sidebar に push する。
///
/// `proxy.send_event` 失敗 (= EventLoop 終了) で task を終了する。 polling 周期は
/// `spawn_activity_poller` と揃えた 5s (`/rename` 反映までの max latency)。 file watch
/// (notify crate) に切り替えればリアルタイム化可能だが、 現時点は 1 lane / 1 cwd 仮定下では
/// polling で十分 (read-only mtime check + 末尾 grep のみ、 CPU 影響 minimal)。
fn spawn_session_title_poller(proxy: EventLoopProxy<AppEvent>) {
    let _ = thread::Builder::new()
        .name("session-title-poller".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("session title poller tokio runtime 作成失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                // tokio::time::interval は 1 回目即発火、 起動 burst を避けるため空打ち skip
                tick.tick().await;
                loop {
                    tick.tick().await;
                    if proxy.send_event(AppEvent::ResolveSessionTitles).is_err() {
                        tracing::debug!("EventLoop 終了、session title poller も終了");
                        break;
                    }
                }
            });
        });
}

/// VP-147 PR-P2-3: 5s 間隔で `AppEvent::ResolveLaneInboxes` を fire する background poller。
///
/// `spawn_session_title_poller` と同 pattern (tokio current_thread runtime + interval tick)。
/// main thread が `sidebar_state.lanes_by_project` を walk して各 lane の MessageState を
/// build し、 sidebar に push back する trigger となる。 Phase 2 PR-P2-3 では default 値の
/// placeholder を populate し、 sidebar UI で `.vp-message-icon` 表示の signal として動く。
/// 後続 PR で backend peek API + Whitesnake query を実装して actual 値を populate する。
fn spawn_lane_inbox_poller(proxy: EventLoopProxy<AppEvent>) {
    let _ = thread::Builder::new()
        .name("lane-inbox-poller".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("lane inbox poller tokio runtime 作成失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                tick.tick().await;
                loop {
                    tick.tick().await;
                    if proxy.send_event(AppEvent::ResolveLaneInboxes).is_err() {
                        tracing::debug!("EventLoop 終了、lane inbox poller も終了");
                        break;
                    }
                }
            });
        });
}

/// `/api/health` + `/api/world/projects` + `/api/world/processes` を集約して
/// `ActivitySnapshot` を組み立てる。各 endpoint 失敗時は default で穏当に通す。
async fn collect_activity(client: &TheWorldClient) -> ActivitySnapshot {
    let mut snap = ActivitySnapshot::default();
    if let Ok(h) = client.world_health().await {
        snap.world_online = !h.status.is_empty();
        if !h.version.is_empty() {
            snap.world_version = Some(h.version);
        }
        if !h.started_at.is_empty() {
            snap.world_started_at = Some(h.started_at);
        }
    }
    if let Ok(projects) = client.list_projects().await {
        snap.project_count = projects.len();
    }
    if let Ok(procs) = client.list_processes().await {
        snap.running_process_count = procs.len();
    }
    snap
}

/// Architecture v4: sidebar の active selection に応じて main area の表示 kind を切替。
///
/// Phase 5-A 拡張: Lane と Stand が **mutually exclusive** な active 軸として扱われる。
/// 優先順位:
///   1. `active_stand` Some → kind = "paisley_park" / "gold_experience" / "hermit_purple"
///   2. `active_lane_address` Some → kind = "terminal"、 pane_id = Lane address
///   3. 両方 None → kind=None で empty placeholder
///
/// Lane address ごとの terminal 接続は per-Lane xterm.js (Phase 2.5) が JS-side で管理。
fn push_active_view(main_view: &WebView, state: &SidebarState) {
    let info = if let Some(stand) = state.active_stand.as_ref() {
        ActivePaneInfo {
            kind: Some(stand.kind.as_str()),
            pane_id: None,
            preview_url: None,
        }
    } else if let Some(addr) = state.active_lane_address.as_deref() {
        ActivePaneInfo {
            kind: Some("terminal"),
            pane_id: Some(addr),
            preview_url: None,
        }
    } else {
        ActivePaneInfo {
            kind: None,
            pane_id: None,
            preview_url: None,
        }
    };
    let script = main_area::build_set_active_pane_script(&info);
    if let Err(e) = main_view.evaluate_script(&script) {
        tracing::warn!("main setActivePane 失敗: {}", e);
    }
}

/// SidebarState を JSON にして sidebar webview に push
fn push_sidebar_state(sidebar: &WebView, state: &SidebarState) {
    let json = match serde_json::to_string(state) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("SidebarState serialize 失敗: {}", e);
            return;
        }
    };
    let script = format!("window.renderSidebarState({})", json);
    if let Err(e) = sidebar.evaluate_script(&script) {
        tracing::warn!("sidebar renderSidebarState 失敗: {}", e);
    }
}

/// sidebar IPC を解釈した結果
#[derive(Debug, Default)]
struct SidebarIpcOutcome {
    /// SidebarState が変化したか (true なら push_sidebar_state を呼ぶ)
    changed: bool,
    /// active Lane が変わったか (true なら push_active_view を呼ぶ)
    active_changed: bool,
    /// SP auto-spawn が必要な project (= 「Current」 になった dead な project)。
    /// `(name, path)` を返し、 caller が `spawn_sp_start` を呼ぶ。
    /// dedup は caller の `sp_spawn_triggered: HashSet<String>` (path key) で行う。
    sp_spawn_request: Option<(String, String)>,
    /// Phase 3-A: Worker Lane 作成要求 `(project_path, name, branch, stand)`。
    /// caller が project の SP port を解決して `client.create_worker_lane` を呼ぶ。
    /// `stand` は doc 11 PR-C で追加 (None なら SP-side default)。
    add_worker_request: Option<(String, String, Option<String>, Option<String>)>,
    /// doc 11 PR-C: 利用可能 Stand 一覧 fetch 要求 `(project_path)`。
    /// caller が SP port を解決して `client.list_stands` を呼ぶ → `AppEvent::StandsResult` で push back。
    list_stands_request: Option<String>,
    /// Phase 4-A: Worker Lane 削除要求 `(project_path, address)`。
    /// caller が SP port を解決して `client.delete_lane` を呼ぶ。
    delete_lane_request: Option<(String, String)>,
    /// Lane Lead Stand restart 要求 `(project_path, address)`。
    /// caller が SP port を解決して `client.restart_lane` を呼ぶ。
    restart_lane_request: Option<(String, String)>,
    /// Phase 5-C: Process restart 要求 `(project_name)`。
    /// caller が TheWorld の `/api/world/processes/{name}/restart` を呼ぶ。
    restart_process_request: Option<String>,
    /// Process stop 要求 `(project_name)`。
    /// caller が TheWorld の `/api/world/processes/{name}/stop` を呼ぶ。
    /// project は registered のまま (一時停止中 tab へ移る)。
    stop_process_request: Option<String>,
    /// Project delete 要求 `(project_name, project_path)`。
    /// caller が SP を stop してから `/api/world/projects/remove` を呼ぶ。
    /// `project_name` は stop 用、 `project_path` は remove 用 (registry key)。
    delete_project_request: Option<(String, String)>,
    /// Phase 5-D fix: SP auto-spawn dedup HashSet から path を release する要求。
    /// 「accordion を閉じる」 = 「ユーザが retry を望んでいる」 と解釈、 失敗ループの
    /// dedup deadlock を抜けられるようにする。 caller は `sp_spawn_triggered.remove(path)` を呼ぶ。
    sp_spawn_release: Option<String>,
}

/// sidebar webview から IPC で受け取った JSON を解釈し、`SidebarState` を mutate。
///
/// VP-208 PR-3: 旧 手 JSON parse (`parsed.get("t")` の文字列 match) を、 KDL schema
/// (`schema/vp-sidebar.kdl`) から生成した `IpcEnvelope` enum での typed dispatch に
/// 置き換えた。 wire ↔ Rust の drift は schema を SSOT にすることで解消される。
fn handle_sidebar_ipc(
    msg: &str,
    state: &mut SidebarState,
    session: &mut SessionState,
) -> SidebarIpcOutcome {
    use crate::generated::sidebar_ipc::IpcEnvelope;

    let mut out = SidebarIpcOutcome::default();
    let envelope: IpcEnvelope = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("sidebar IPC のデシリアライズ失敗: {} (msg={})", e, msg);
            return out;
        }
    };

    match envelope {
        IpcEnvelope::ProcessToggle(m) => {
            // VP-101 Phase A1.b: native <details> が IPC で `expanded` の新状態を渡してくる。
            // DOM は既に user click で toggle 済なので、Rust state を silently sync するだけ。
            // `out.changed` は立てない (rebuild すると flash する)。
            //
            // auto-spawn: expand=true で state==stopped の project は
            // 「user が current として designate した未起動 project」 として扱い、
            // SP auto-spawn を request する (SP lifecycle は TheWorld 責務)。
            //
            // 条件の "stopped" は client::ProcessStatus::as_str() と一致させること。
            // 旧 ProcessState の "dead" 語彙から ProcessStatus の "stopped" へ移行した
            // (VP-189) 際にこの条件の追従が漏れ、 auto-spawn が発火しなくなっていた。
            if let Some(p) = state.processes.iter_mut().find(|p| p.path == m.path) {
                let new_state = m.expanded;
                if p.expanded != new_state {
                    p.expanded = new_state;
                    tracing::debug!(
                        "process:toggle {} → expanded={} (silent sync)",
                        m.path,
                        p.expanded
                    );
                    // session 永続化: vp-app 再起動時に accordion 状態を復元
                    session.set_project_expanded(m.path.clone(), new_state);
                    session.save();
                }
                if new_state && p.state.as_deref() == Some("stopped") {
                    out.sp_spawn_request = Some((p.name.clone(), p.path.clone()));
                }
                // Phase 5-D fix: accordion を閉じた = 「retry したい」signal と解釈、
                //  sp_spawn_triggered HashSet の entry を release。 これで spawn 失敗ループから
                //  抜けられる (collapse → expand で確実に retry が走る)。
                if !new_state {
                    out.sp_spawn_release = Some(p.path.clone());
                }
            }
        }
        IpcEnvelope::LaneDelete(m) => {
            // Phase 4-A: Worker Lane 削除要求。 caller (event loop) で SP port を解決して
            // client.delete_lane を呼ぶ。 active Lane を消した場合は active_lane_address を unset。
            if !m.path.is_empty() && !m.address.is_empty() {
                // active だった Lane が消えるなら preemptively clear (UI 反映を待たず)
                if state.active_lane_address.as_deref() == Some(m.address.as_str()) {
                    state.active_lane_address = None;
                    out.changed = true;
                    out.active_changed = true;
                }
                out.delete_lane_request = Some((m.path, m.address));
            }
        }
        IpcEnvelope::LaneRestart(m) => {
            // sidebar の restart icon → confirm dialog OK の連鎖。 caller が SP port を
            // 解決して `client.restart_lane` を呼ぶ。 active Lane を restart した場合は
            // WS が onclose → reconnect で新 PtySlot に attach し直す (PR #218)。
            if !m.path.is_empty() && !m.address.is_empty() {
                out.restart_lane_request = Some((m.path, m.address));
            }
        }
        IpcEnvelope::LaneAddWing(m) => {
            // Phase 3-A: sidebar から Wing Lane 作成要求。 caller (event loop) で
            // 該当 project の SP port を解決して client.create_wing_lane を呼ぶ。
            // doc 11 PR-C: branch / stand は optional。 空文字は None に畳んで
            // SP-side default にフォールバックさせる。
            let branch = m.branch.filter(|s| !s.is_empty());
            let stand = m.stand.filter(|s| !s.is_empty());
            if !m.path.is_empty() && !m.name.is_empty() {
                out.add_worker_request = Some((m.path, m.name, branch, stand));
            }
        }
        IpcEnvelope::StandsFetch(m) => {
            // doc 11 PR-C: sidebar の + Add Worker form 開閉時に利用可能 Stand 一覧を取得。
            // caller (event loop) で SP port 解決 → client.list_stands → window.handleStandsResult で push back。
            if !m.path.is_empty() {
                out.list_stands_request = Some(m.path);
            }
        }
        IpcEnvelope::StandSelect(m) => {
            // Phase 5-A: Project-scope Stand row click → main area に対応 pane を表示
            // (Lane と mutually exclusive、 active_lane_address は preemptively clear)
            if m.path.is_empty() || m.kind.is_empty() {
                tracing::warn!("stand:select with empty path/kind: {}", msg);
                return out;
            }
            let new_stand = ActiveStand {
                project_path: m.path.clone(),
                kind: m.kind.clone(),
            };
            // 既に同じ Stand が active なら no-op
            if state.active_stand.as_ref() == Some(&new_stand) {
                return out;
            }
            tracing::info!("stand:select project={} kind={}", m.path, m.kind);
            state.active_stand = Some(new_stand);
            // Lane を排他で clear (= main area の active 軸を Stand に切替)
            if state.active_lane_address.is_some() {
                state.active_lane_address = None;
            }
            out.changed = true;
            out.active_changed = true;
        }
        IpcEnvelope::LaneSelect(m) => {
            // Architecture v4: Lane row click → `address` (Display 形 "<project>/lead") を受信
            if m.address.is_empty() {
                tracing::warn!("lane:select with empty address: {}", msg);
                return out;
            }
            // 念のため: 該当 project の lanes_by_project に address が存在することを確認
            let lanes_exist = state
                .lanes_by_project
                .get(m.path.as_str())
                .map(|lanes| lanes.iter().any(|l| l.address.key() == m.address))
                .unwrap_or(false);
            if !lanes_exist {
                tracing::warn!(
                    "lane:select 対象 lane が見つからない: path={} address={}",
                    m.path,
                    m.address
                );
                return out;
            }
            if state.active_lane_address.as_deref() != Some(m.address.as_str()) {
                state.active_lane_address = Some(m.address.clone());
                tracing::info!("lane:select {} address={}", m.path, m.address);
                out.changed = true;
                out.active_changed = true;
                // session 永続化: vp-app 再起動時に直前 active Lane を復元
                session.active_lane_address = Some(m.address.clone());
                session.save();
            }
            // Phase 5-D Sprint C P2.1: Lane 切替時に対象 Lane の unread notification を 0 reset。
            //  user が Lane 開いた = 通知に応答した、 とみなして badge を消す。
            //  active 切替が無くても reset は走る (= 同 Lane を click 連打しても badge 消えるべき)。
            if state
                .unread_notifications
                .remove(m.address.as_str())
                .is_some()
            {
                out.changed = true;
            }
            // awaiting_input も同タイミングで reset (= user が Lane を開いたら入力待ち通知を消す)。
            if state.awaiting_input.remove(m.address.as_str()).is_some() {
                out.changed = true;
            }
            // Phase 5-A: Lane と Stand は排他なので active_stand を clear
            if state.active_stand.is_some() {
                state.active_stand = None;
                out.changed = true;
                out.active_changed = true;
            }
        }
        IpcEnvelope::ProcessReorder(m) => {
            // Currents セクションを drag-and-drop で並び替えた時の通知。
            // payload: `{"t":"process:reorder","order":["/path/a","/path/b",...]}`。
            // session_state に保存し、 次回起動時 + 現在の sidebar push に反映。
            tracing::info!("process:reorder: {} entries", m.order.len());
            session.currents_order = Some(m.order.clone());
            session.save();
            // SidebarState にも反映 (次回 push で JS 側 sort に使う)
            state.currents_order = Some(m.order);
            // changed フラグは立てない (DOM 順は user 操作で既に変わっている、
            // re-push で flash するのを避ける)。 次回 push 時に新 order が乗る。
        }
        IpcEnvelope::ProcessRestart(m) => {
            // Phase 5-C: project name (from p.path → leaf name) を抽出して async restart に投げる。
            // path は normalized full path、 SP の API は project name で識別する。
            if m.path.is_empty() {
                tracing::warn!("process:restart with empty path: {}", msg);
                return out;
            }
            let project_name = std::path::Path::new(&m.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(m.path.as_str())
                .to_string();
            tracing::info!("process:restart {} (project_name={})", m.path, project_name);
            out.restart_process_request = Some(project_name);
        }
        IpcEnvelope::ProcessStop(m) => {
            // SP を停止する (project は registered のまま → 一時停止中 tab へ)。
            // restart と同様 path の leaf name を project name として扱う。
            if m.path.is_empty() {
                tracing::warn!("process:stop with empty path: {}", msg);
                return out;
            }
            let project_name = std::path::Path::new(&m.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(m.path.as_str())
                .to_string();
            tracing::info!("process:stop {} (project_name={})", m.path, project_name);
            out.stop_process_request = Some(project_name);
        }
        IpcEnvelope::ProcessDelete(m) => {
            // project を完全に削除 (SP 停止 + projects.kdl から unregister)。
            // UI 側で 2-click 確認済。 stop 用に project_name、 remove 用に path を渡す。
            if m.path.is_empty() {
                tracing::warn!("process:delete with empty path: {}", msg);
                return out;
            }
            let project_name = std::path::Path::new(&m.path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(m.path.as_str())
                .to_string();
            tracing::info!("process:delete {} (project_name={})", m.path, project_name);
            out.delete_project_request = Some((project_name, m.path));
        }
        // process:add / project:clone:pickFolder は `AppEvent::SidebarIpc` の
        // dispatch 段で picker ルートに分岐済 (handle_sidebar_ipc には到達しない)。
        IpcEnvelope::ProcessAdd | IpcEnvelope::ProjectClonePickFolder => {
            tracing::debug!("sidebar IPC: picker 経路の message が handle_sidebar_ipc に到達");
        }
    }
    out
}

// R-0 (`docs/design/11-vp-app-refactor.md` § 3.0a / `mem_1CaaaDoXHZvhR46ZfLN6jx`):
//   旧 `lane_address_key(&LaneAddressWire) -> String` 関数は `lane.rs::LaneAddressWire::key()`
//   メソッドに移管 (G2 解消、 3 重実装の 1 元化)。 caller は `wire.key()` で同等の文字列を取れる。

/// App のエントリポイント
pub fn run() -> anyhow::Result<()> {
    // R-1 (`docs/design/11-vp-app-refactor.md` § 3.1 / `mem_1CaaaDoXHZvhR46ZfLN6jx`):
    //   tracing init を `crate::log_init::init_tracing()` に切り出し済。
    //   filter resilience (PR #235) + appender + KdlFormatter wiring + 起動ログを内包。
    let _log = crate::log_init::init_tracing();

    // VP-192: 旧 config/data パスからの冪等なデータ移行 (Settings/SessionState 読み込み前)
    crate::paths::migrate_legacy_paths();

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // VP-100 follow-up: 永続設定 + 1Password 風 開発者モード切替
    let mut settings = Settings::load();
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
    let dev_mode_item = menu_handles.developer_mode_item;
    let open_devtools_item = menu_handles.open_devtools_item;
    let open_sidebar_devtools_item = menu_handles.open_sidebar_devtools_item;
    let menu_ids = menu_handles.ids;
    let _tray = match crate::tray::build_tray() {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!("トレイ初期化失敗 (無効化): {}", e);
            None
        }
    };

    // muda の MenuEvent を main loop に橋渡しする thread を起動
    spawn_menu_event_pump(event_loop.create_proxy());

    let window = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .build(&event_loop)?;

    // Terminal backend 選択 (VP-93 Step 2a + auto-launch)
    // - VP_TERMINAL_MODE=local: 明示 opt-out で in-proc portable-pty
    // - それ以外 (default): TheWorld daemon の /ws/terminal 経由
    //   localhost URL かつ daemon が down なら `vp` binary を auto-spawn して待つ。
    //   spawn 失敗 or timeout なら local portable-pty にフォールバック (黙って落ちない)。
    let proxy = event_loop.create_proxy();
    // Phase 2.5 (per-Lane instance): startup の placeholder PTY 接続は撤去。
    // Lane が出現するまで main area は empty placeholder ("No Lane selected") のみ。
    // ただし TheWorld の auto-launch だけは継続 (sidebar の Activity widget や
    // /api/world/projects 取得に必要)。
    let _ = proxy; // 旧 spawn_shell / connect_daemon_terminal で proxy を消費していた、 互換用に残す
    let world_url =
        std::env::var("VP_WORLD_URL").unwrap_or_else(|_| "http://127.0.0.1:32000".into());
    if let Err(e) = crate::daemon_launcher::ensure_daemon_ready(&world_url) {
        tracing::warn!(
            "TheWorld auto-launch 失敗 (continue with offline state): {}",
            e
        );
    }

    // TheWorld から project list を非同期 fetch (起動初回)
    spawn_processes_fetch(event_loop.create_proxy());
    // VP-95: Activity widget の定期更新 (5s 間隔)
    spawn_activity_poller(event_loop.create_proxy());
    // VP-143: cc session display name (custom-title) の 5s 周期 resolve
    spawn_session_title_poller(event_loop.create_proxy());
    // VP-147 PR-P2-3: per-Lane mailbox inbox 状況の 5s 周期 resolve (sidebar message icon 用 signal)
    spawn_lane_inbox_poller(event_loop.create_proxy());

    // Sidebar
    let sidebar_ipc_proxy = event_loop.create_proxy();
    let sidebar = WebViewBuilder::new()
        // Phase 5-C: vp-asset:// custom protocol で bundled font (FONT_ASSETS) + sidebar.html を配信。
        // serve() に SIDEBAR_ASSETS を渡すと FONT_ASSETS と chain して両方 lookup される。
        // HTML 自体も同 scheme から読むことで page origin = vp-asset:// に統一、 font fetch も同一 origin。
        .with_custom_protocol("vp-asset".to_string(), move |id, request| {
            crate::web_assets::serve(id, request, SIDEBAR_ASSETS)
        })
        .with_url("vp-asset://app/sidebar.html")
        .with_devtools(true) // R5 dev: View → "Open Sidebar DevTools" で Web Inspector 起動可能
        .with_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: WryLogicalSize::new(SIDEBAR_WIDTH, 800.0).into(),
        })
        .with_ipc_handler(move |req| {
            // sidebar からのクリック等を main thread に飛ばす (state mutation は main で)
            let _ = sidebar_ipc_proxy.send_event(AppEvent::SidebarIpc(req.body().to_string()));
        })
        .build_as_child(&window)?;

    // VP-100 Phase 2: main area = 単一 WebView (canvas + terminal を統合)。
    // xterm.js + canvas placeholder + preview iframe を kind 別に切替表示する。
    // PTY ブリッジは旧 terminal_view と同じ IPC handler を引き継ぐ。
    let ipc_proxy = event_loop.create_proxy();
    // VP-100 follow-up (1Password 風 runtime 切替):
    // wry の DevTools 機能は **compile 時 always 有効** で固定。
    // 実際に開けるかどうかは menu の「Open Developer Tools」item から
    // `webview.open_devtools()` を呼ぶかで runtime 制御 (本番ビルドでも切替可)。
    // Mac App Store 審査が必要な配布では Cargo features で更に絞る予定 (Phase 4)。
    let main_view = WebViewBuilder::new()
        .with_html(MAIN_AREA_HTML)
        .with_bounds(Rect {
            position: LogicalPosition::new(SIDEBAR_WIDTH, 0.0).into(),
            size: WryLogicalSize::new(1200.0 - SIDEBAR_WIDTH, 800.0).into(),
        })
        .with_devtools(true)
        .with_ipc_handler(move |req| {
            // Phase 2.5 (per-Lane instance): IPC handler は ready / copy / debug / slot:rect
            // のみ処理する thin wrapper。 Lane の input / output は browser native WebSocket が
            // SP `/ws/terminal?lane=<addr>` に直接接続するので Rust 経路は不要。
            terminal::handle_ipc_message(req.body(), &ipc_proxy);
        })
        .with_focused(true)
        .build_as_child(&window)?;

    tracing::info!("メインウィンドウ + 2 ペイン (sidebar / main) 作成");

    // Phase 2.x-d: 旧 single-PTY 経路 (`xterm_ready` / `pending` / `PENDING_MAX`) は撤去。
    // per-Lane instance + browser-native WebSocket では各 Lane の xterm.js が独立に
    // WS から bytes を受けるので、 Rust 側で buffer / flush 同期する必要が無い。
    // VP-95: sidebar 全体 state (projects + widget + activity)
    let mut sidebar_state = SidebarState::default();
    // session 永続化: 起動を跨いで復元する UI state (expanded / active_lane / currents_order)
    let mut session_state = SessionState::load();
    // 直前 active Lane を初回 LanesLoaded で復元するための pending 値。
    // 1 度復元したら None にして、 後続 LanesLoaded で再復元しないように。
    let mut pending_session_active_lane: Option<String> = session_state.active_lane_address.clone();
    // SidebarState に currents_order を即反映 (renderProjects がこの順で並べる)
    sidebar_state.currents_order = session_state.currents_order.clone();
    // VP-100 γ-light: pane_id → slot rect。Phase 2 では蓄積するだけ、Phase 4+ で
    // native overlay の `set_position` 同期に使う。
    let mut slot_rects: std::collections::HashMap<String, SlotRect> =
        std::collections::HashMap::new();
    // SP auto-spawn: 1 セッションで同じ project を二重 trigger しないための guard。
    // path をキーにする (project_name は重複しうる、 path は正規化済 unique)。
    // TheWorld 側でも `Process already running` で弾かれるが、 無駄な POST を避ける。
    let mut sp_spawn_triggered: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // wiremsg Stage 1: per-SP の "lanes" Unison 購読を 1 本だけ張るための guard。
    // path をキーにする。購読が再接続上限で諦めると `LanesSubscriptionEnded` で除去され、
    // 次の `ProcessesLoaded` で SP がまだ生きていれば再 spawn される。
    let mut lanes_sub_active: std::collections::HashSet<String> = std::collections::HashSet::new();
    // wiremsg Stage 2: per-SP の "canvas" Unison 購読 guard (lanes_sub_active と同型)。
    let mut canvas_sub_active: std::collections::HashSet<String> = std::collections::HashSet::new();
    // VP-100 follow-up (1Password 風): runtime 開発者モード state
    let mut dev_mode = initial_dev_mode;
    // project:add 等の async 操作で event loop に project list 再 fetch を kick するための proxy
    let async_action_proxy = event_loop.create_proxy();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                tracing::info!("Window close requested");
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                update_pane_bounds(&sidebar, &main_view, size, window.scale_factor());
            }
            // Phase 2.x-d: AppEvent::Output / XtermReady は撤去済 (per-Lane browser native WS へ移行)。
            // 関連の `xterm_ready` / `pending` / `PENDING_MAX` も一括削除。
            // Phase 4-paste-fix: clipboard.readText の webview permission 問題への fallback。
            // IPC `paste:request` を Rust が受けて arboard で読み取り、 ここで JS に inject。
            Event::UserEvent(AppEvent::PasteText(text)) => {
                if text.is_empty() {
                    tracing::debug!("PasteText empty (clipboard 空 or 取得失敗)、 skip");
                } else {
                    // Phase review fix #3: 旧手書き escape (backslash/quote/newline/cr) は
                    // null byte (`\0`) や Unicode surrogate を見落とす可能性があった。
                    // serde_json::to_string で **JSON spec full escape** を使えば、
                    // 全 UTF-8 sequence が JS の string literal として安全に literalize される。
                    // 出力例: `"foo\nbar"` (ダブルクォート + JSON escape 込み) → JS で valid string literal。
                    let json_text = serde_json::to_string(&text)
                        .unwrap_or_else(|_| "\"\"".into());
                    let script = format!(
                        "if (window.deliverPaste) window.deliverPaste({});",
                        json_text
                    );
                    if let Err(e) = main_view.evaluate_script(&script) {
                        tracing::warn!("paste deliver script failed: {}", e);
                    }
                }
            }
            Event::UserEvent(AppEvent::OscNotification { lane, code }) => {
                // Phase 5-D Sprint C P2.1: per-Lane HD notification の unread count 加算。
                //  Skip increment if user is currently looking at this lane (即読扱い)。
                if sidebar_state.active_lane_address.as_deref() == Some(lane.as_str()) {
                    tracing::debug!("osc:notification skip (active lane): lane={} code={}", lane, code);
                } else {
                    let count = sidebar_state
                        .unread_notifications
                        .entry(lane.clone())
                        .or_insert(0);
                    *count += 1;
                    // 「入力待ち」 状態 = 行右端に黄 dot を表示。 active 切替で reset される。
                    sidebar_state.awaiting_input.insert(lane.clone(), true);
                    tracing::info!("osc:notification lane={} code={} unread={}", lane, code, *count);
                    push_sidebar_state(&sidebar, &sidebar_state);
                }
            }
            Event::UserEvent(AppEvent::ResolveSessionTitles) => {
                // VP-143: 全 lane の cwd を walk → cc custom-title resolve → diff → sidebar に push。
                //  poller (`spawn_session_title_poller`) が 5s 間隔で tick を送ってここに来る。
                //  resolve は read-only file I/O (ディレクトリ列挙 + 末尾 grep) なので
                //  数 lane × 数 ms 程度、 main thread blocking は無視できる範囲。
                let mut changed = false;
                let mut current_keys: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for lanes in sidebar_state.lanes_by_project.values() {
                    for lane in lanes {
                        let address = lane.address.key();
                        current_keys.insert(address.clone());
                        let cwd = std::path::Path::new(&lane.cwd);
                        let resolved = crate::session_title::resolve_title_for_cwd(cwd);
                        let prev = sidebar_state.session_titles.get(&address).cloned();
                        match (resolved, prev) {
                            (Some(new_title), Some(old)) if old == new_title => {}
                            (None, None) => {}
                            (Some(new_title), _) => {
                                sidebar_state.session_titles.insert(address, new_title);
                                changed = true;
                            }
                            (None, Some(_)) => {
                                sidebar_state.session_titles.remove(&address);
                                changed = true;
                            }
                        }
                    }
                }
                // 既に消えた lane の stale entry 掃除
                let stale: Vec<String> = sidebar_state
                    .session_titles
                    .keys()
                    .filter(|k| !current_keys.contains(k.as_str()))
                    .cloned()
                    .collect();
                for k in stale {
                    sidebar_state.session_titles.remove(&k);
                    changed = true;
                }
                if changed {
                    push_sidebar_state(&sidebar, &sidebar_state);
                }
            }
            Event::UserEvent(AppEvent::ResolveLaneInboxes) => {
                // VP-147 PR-P2-3: 全 lane の mailbox inbox 状況を resolve → sidebar に push。
                //  poller (`spawn_lane_inbox_poller`) が 5s 間隔で tick を送ってここに来る。
                //  Phase 2 (icon visibility のみ) では default MessageState を populate して
                //  sidebar UI で `.vp-message-icon` 表示の signal とする。 backend peek API
                //  + Whitesnake query は後続 PR で実装、 actual 値で MessageState を populate。
                use crate::pane::MessageState;
                let mut changed = false;
                let mut current_keys: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for lanes in sidebar_state.lanes_by_project.values() {
                    for lane in lanes {
                        let address = lane.address.key();
                        current_keys.insert(address.clone());
                        // Phase 2 placeholder: default MessageState (= unread_count 0、 has_persistent false)
                        // 既存 entry が無い (= 初回 tick or 新規 lane) 場合のみ insert、 上書きしない。
                        // TODO 後続 PR (Phase 2.5): backend peek API を叩いて actual 値で update。
                        //   `Vacant` ガードを `entry().and_modify(|s| *s = fetched).or_insert_with(...)`
                        //   に書き換えて、 既存 entry の `unread_count` 等を refresh する。 現状 (Phase 2)
                        //   は actual 値が無いので Vacant のみ insert で sufficient (icon visibility のみ)。
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            sidebar_state.lane_inboxes.entry(address)
                        {
                            e.insert(MessageState::default());
                            changed = true;
                        }
                    }
                }
                // 既に消えた lane の stale entry 掃除
                let stale: Vec<String> = sidebar_state
                    .lane_inboxes
                    .keys()
                    .filter(|k| !current_keys.contains(k.as_str()))
                    .cloned()
                    .collect();
                for k in stale {
                    sidebar_state.lane_inboxes.remove(&k);
                    changed = true;
                }
                if changed {
                    push_sidebar_state(&sidebar, &sidebar_state);
                }
            }
            Event::UserEvent(AppEvent::ProcessesLoaded(projects)) => {
                // 既存 SidebarState とマージ:
                //  - 同じ path があれば既存 state を維持 (expanded / panes / active 保持)
                //  - 新規は ProcessPaneState::new (Lead Agent 1 つ)
                //  - サーバから消えた project は除外
                //
                // VP-101 follow-up: register 後の auto-expand。
                // auto-select は LanesLoaded 側で扱う (Architecture v4: 真の selection unit は Lane)。
                let prev: std::collections::HashMap<String, ProcessPaneState> = sidebar_state
                    .processes
                    .drain(..)
                    .map(|p| (p.path.clone(), p))
                    .collect();
                let is_initial_load = prev.is_empty();
                // Phase A4-3b: drain 前に (path → port) を retain して fetch task に渡す
                let project_ports: Vec<(String, Option<u16>)> = projects
                    .iter()
                    .map(|p| (p.path.clone(), p.port))
                    .collect();
                sidebar_state.processes = projects
                    .into_iter()
                    .map(|p| {
                        // ProcessInfo.state / .port を ProcessPaneState に merge
                        // (sidebar JS が processStateMark で 🟢/🔴 badge 表示に使う、
                        //  port は Phase 2 で lane:select 時の WS 接続先決定に使う)
                        let state_str = p.state.as_str().to_string();
                        let port = p.port;
                        let mut pane_state = if let Some(existing) = prev.get(&p.path) {
                            existing.clone()
                        } else {
                            // 新規 project の expanded 解決:
                            //   1. session_state に saved 値があれば最優先 (vp-app 再起動の復元)
                            //   2. 上記 None かつ session 中の追加 (= 初回 fetch ではない) なら auto-expand
                            //   3. 初回 fetch の新規は閉じた状態
                            let mut s = ProcessPaneState::new(p.path.clone(), p.name.clone());
                            s.expanded = session_state
                                .project_expanded(&p.path)
                                .unwrap_or(!is_initial_load);
                            s
                        };
                        pane_state.state = Some(state_str);
                        pane_state.port = port;
                        pane_state
                    })
                    .collect();
                // wiremsg: 各 project の SP の Unison channel を購読する (per-SP 1 本ずつ)。
                // - Stage 1: "lanes" channel → sidebar Lane ツリー
                // - Stage 2: "canvas" channel → main area の Paisley Park body
                // retained topic なので接続直後に現スナップショットが届き、以降変化のたび
                // push される。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
                for (path, port) in &project_ports {
                    let Some(sp_port) = port else { continue };
                    if lanes_sub_active.insert(path.clone()) {
                        spawn_lanes_subscription(
                            async_action_proxy.clone(),
                            path.clone(),
                            *sp_port,
                        );
                    }
                    if canvas_sub_active.insert(path.clone()) {
                        spawn_canvas_subscription(
                            async_action_proxy.clone(),
                            path.clone(),
                            *sp_port,
                        );
                    }
                }
                // Phase 2.x-b: dead-respawn fix — SP が "running" になった時点で
                // sp_spawn_triggered から path を外す。 これで次に dead に落ちた時、
                // user が re-expand すれば再度 spawn が trigger される。
                // 注意: spawn 進行中 (state=="spawning") は外さない、 一連の spawn cycle が完了
                // (= "running") した時のみ。 こうすれば spawn 中の重複 POST も防げる。
                for proc in &sidebar_state.processes {
                    if proc.state.as_deref() == Some("running")
                        && sp_spawn_triggered.remove(&proc.path)
                    {
                        tracing::debug!(
                            "sp_spawn_triggered cleared (running): {}",
                            proc.path
                        );
                    }
                }
                push_sidebar_state(&sidebar, &sidebar_state);
            }
            // Phase A4-3b: SP の Lane fetch 結果を sidebar_state に反映
            Event::UserEvent(AppEvent::LanesLoaded {
                process_path,
                lanes,
            }) => {
                tracing::info!(
                    "AppEvent::LanesLoaded handled: project={} count={}",
                    process_path,
                    lanes.len()
                );
                // Architecture v4: active_lane_address が未設定なら最初の Lane を auto-select。
                // 「初回起動 → Lead Lane が main area に出る」UX を Lane SSOT で保つ。
                //
                // 例外: `VP_APP_SECONDARY=1` (Cmd+N で spawn された secondary instance) の場合は
                // auto-select を skip。 元 vp-app が既に同 lane の terminal WS を持ってる事が多く、
                // 衝突して両方の console が壊れるため。 Secondary は user が手動 lane 選択する前提。
                let is_secondary =
                    std::env::var("VP_APP_SECONDARY").map(|v| v == "1").unwrap_or(false);
                // session 復元優先: pending_session_active_lane が今回の lanes に含まれれば、
                // auto-select-first より先にそれを採用 (vp-app 再起動時に直前 active を維持)。
                let session_match: Option<String> = pending_session_active_lane
                    .as_ref()
                    .filter(|saved| {
                        lanes
                            .iter()
                            .any(|l| &l.address.key() == *saved)
                    })
                    .cloned();
                // Phase 5-E: auto-select は pid あり (= Active = Pane 起動済) な Lane のみ対象。
                //  disk-only Lane (pid:null) を選ぶと WS 確立先が無く 「lane not found」 reconnect ループに陥る。
                //  Active Lane が 1 件も無ければ auto-select はスキップ (user 明示選択を待つ)。
                let first_active = lanes.iter().find(|l| l.pid.is_some());
                let auto_select = !is_secondary
                    && sidebar_state.active_lane_address.is_none()
                    && session_match.is_none()
                    && first_active.is_some();
                let first_addr = if let Some(saved) = session_match {
                    // session 復元: 1 度限り、 復元済 marker として pending を消費
                    pending_session_active_lane = None;
                    tracing::info!("session 復元: active_lane = {}", saved);
                    Some(saved)
                } else if auto_select {
                    first_active.map(|l| l.address.key())
                } else {
                    None
                };
                let path_key = process_path.clone();
                // Phase 2.5: prev lanes との diff で「消えた Lane」 を判定 → removeLane 発行
                let removed_addrs: Vec<String> = sidebar_state
                    .lanes_by_project
                    .get(&path_key)
                    .map(|prev| {
                        let new_set: std::collections::HashSet<String> = lanes
                            .iter()
                            .map(|l| l.address.key())
                            .collect();
                        prev.iter()
                            .map(|l| l.address.key())
                            .filter(|addr| !new_set.contains(addr))
                            .collect()
                    })
                    .unwrap_or_default();
                for addr in &removed_addrs {
                    tracing::info!("Lane removed (LanesLoaded diff): {}", addr);
                    lane_js::remove_lane(&main_view, addr);
                    // VP-147 PR-P2-3 Moody Blues fix #1: lane delete 検出時に lane_inboxes
                    // も即時 cleanup (= 5s polling tick 待たずに stale state 解消)。
                    sidebar_state.lane_inboxes.remove(addr);
                }
                sidebar_state.lanes_by_project.insert(process_path, lanes);
                // Phase 2.5: per-Lane instance — このプロジェクトの SP port を引いて
                // 各 Lane に ensureLane を発行 (idempotent)。
                let sp_port_for_project = sidebar_state
                    .processes
                    .iter()
                    .find(|p| p.path == path_key)
                    .and_then(|p| p.port);
                if let Some(port) = sp_port_for_project {
                    if let Some(lanes_for_proj) = sidebar_state.lanes_by_project.get(&path_key) {
                        for lane in lanes_for_proj {
                            // Phase 5-E: pid:null = disk-only Lane (lane workspace dir のみ、 PtySlot 不在)。
                            //  ensureLane で WS 接続するとサーバ側が「lane not found」 を返し、
                            //  xterm.js が 1006 切断 → 500ms reconnect → 無限ループ に入る。
                            //  Activate 済 Lane (pid あり) のみ WS 確立対象とする。
                            if lane.pid.is_none() {
                                continue;
                            }
                            let addr_str = lane.address.key();
                            lane_js::ensure_lane(&main_view, &addr_str, port);
                        }
                    }
                } else {
                    tracing::warn!(
                        "LanesLoaded: SP port unknown for project_path={} (skip ensureLane)",
                        path_key
                    );
                }
                if let Some(addr) = first_addr {
                    tracing::info!("auto-select first lane: {}", addr);
                    sidebar_state.active_lane_address = Some(addr.clone());
                    push_active_view(&main_view, &sidebar_state);
                    // Phase 2.5: per-Lane instance を main area に表示。
                    // ensureLane は上のループで呼んだので、 ここでは show のみ。
                    lane_js::show_lane(&main_view, Some(&addr));
                }
                push_sidebar_state(&sidebar, &sidebar_state);
            }
            // VP-140: JS 側が DOMContentLoaded 後に送る lane catch-up 要求。
            // 起動 race で silent drop された ensureLane を再発行する (WebView HTML load 完了
            // 後なので、 evaluate_script は確実に実行される)。 idempotent (ensureLane 内で既存なら no-op)。
            Event::UserEvent(AppEvent::LanesEnsureAll) => {
                let mut total_lanes = 0usize;
                for (project_path, lanes) in sidebar_state.lanes_by_project.clone().iter() {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| &p.path == project_path)
                        .and_then(|p| p.port);
                    let Some(port) = sp_port else {
                        tracing::warn!(
                            "LanesEnsureAll: SP port unknown for {} (skip)",
                            project_path
                        );
                        continue;
                    };
                    for lane in lanes {
                        // Phase 5-E: pid:null = disk-only Lane は WS 確立対象外
                        if lane.pid.is_none() {
                            continue;
                        }
                        lane_js::ensure_lane(&main_view, &lane.address.key(), port);
                        total_lanes += 1;
                    }
                }
                // 現在 active な Lane を再度 show する (lane-empty placeholder を解除する保険)
                if let Some(addr) = &sidebar_state.active_lane_address {
                    lane_js::show_lane(&main_view, Some(addr));
                }
                tracing::info!(
                    "LanesEnsureAll: re-issued ensureLane for {} lane(s), active={:?}",
                    total_lanes,
                    sidebar_state.active_lane_address
                );
            }
            Event::UserEvent(AppEvent::LanesError {
                process_path,
                message,
            }) => {
                tracing::warn!(
                    "AppEvent::LanesError: project={} message={}",
                    process_path,
                    message
                );
                // SP 接続失敗 (Project SP 未起動等) — sidebar の lanes_by_project は更新しない
            }
            Event::UserEvent(AppEvent::LanesSubscriptionEnded { process_path }) => {
                // wiremsg Stage 1: "lanes" 購読が再接続上限に達して終了した。
                // guard から外し、SP が再び現れたら次の `ProcessesLoaded` で購読を再 spawn する。
                tracing::info!(
                    "AppEvent::LanesSubscriptionEnded: project={} (購読 guard 解除)",
                    process_path
                );
                lanes_sub_active.remove(&process_path);
            }
            Event::UserEvent(AppEvent::CanvasMessage {
                process_path,
                message,
            }) => {
                // wiremsg Stage 2: SP の "canvas" channel から受信した ProcessMessage。
                // active project の分のみ main area の Paisley Park body に転送する。
                // active 判定: active_lane_address の project segment == process_path の basename。
                let active_project = sidebar_state
                    .active_lane_address
                    .as_deref()
                    .and_then(|addr| addr.split('/').next());
                let msg_project = std::path::Path::new(&process_path)
                    .file_name()
                    .and_then(|s| s.to_str());
                let kind = message.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                if active_project.is_some() && active_project == msg_project {
                    match serde_json::to_string(&message) {
                        Ok(json) => {
                            let script = format!(
                                "window.vpCanvas && window.vpCanvas.handleMessage({})",
                                json
                            );
                            if let Err(e) = main_view.evaluate_script(&script) {
                                tracing::warn!("vpCanvas.handleMessage 失敗: {}", e);
                            } else {
                                tracing::info!(
                                    "CanvasMessage (wiremsg): project={} type={} → PP body へ転送",
                                    msg_project.unwrap_or("?"),
                                    kind
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("CanvasMessage serialize 失敗: {}", e);
                        }
                    }
                }
            }
            Event::UserEvent(AppEvent::CanvasSubscriptionEnded { process_path }) => {
                // wiremsg Stage 2: "canvas" 購読が再接続上限で終了。guard から外す。
                tracing::info!(
                    "AppEvent::CanvasSubscriptionEnded: project={} (購読 guard 解除)",
                    process_path
                );
                canvas_sub_active.remove(&process_path);
            }
            Event::UserEvent(AppEvent::ProcessesError(msg)) => {
                let js_msg = serde_json::to_string(&msg).unwrap_or_else(|_| "\"error\"".into());
                let script = format!("window.renderError({})", js_msg);
                if let Err(e) = sidebar.evaluate_script(&script) {
                    tracing::warn!("sidebar renderError 失敗: {}", e);
                }
            }
            // R5 Worker create flow: spawn_blocking thread からの結果を sidebar に push back。
            // success → form を閉じる + addWorkerOpen から削除。
            // error → form 下に inline error 表示 + form は開いたまま (再 submit 可能)。
            Event::UserEvent(AppEvent::WorkerCreateResult {
                project_path,
                name,
                error,
            }) => {
                let payload = serde_json::json!({
                    "project_path": project_path,
                    "name": name,
                    "error": error,
                });
                let payload_str = serde_json::to_string(&payload)
                    .unwrap_or_else(|_| "{}".to_string());
                let script = format!("window.handleAddWorkerResult({})", payload_str);
                if let Err(e) = sidebar.evaluate_script(&script) {
                    tracing::warn!("sidebar handleAddWorkerResult 失敗: {}", e);
                }
            }
            Event::UserEvent(AppEvent::StandsResult {
                project_path,
                stands,
                error,
            }) => {
                // doc 11 PR-C: + Add Worker form の dropdown を populate するための push back。
                let payload = serde_json::json!({
                    "project_path": project_path,
                    "stands": stands,
                    "error": error,
                });
                let payload_str = serde_json::to_string(&payload)
                    .unwrap_or_else(|_| "{}".to_string());
                let script = format!("window.handleStandsResult({})", payload_str);
                if let Err(e) = sidebar.evaluate_script(&script) {
                    tracing::warn!("sidebar handleStandsResult 失敗: {}", e);
                }
            }
            Event::UserEvent(AppEvent::ActivityUpdate(snap)) => {
                sidebar_state.activity = snap;
                push_sidebar_state(&sidebar, &sidebar_state);
            }
            Event::UserEvent(AppEvent::ClonePathPicked(path)) => {
                // user キャンセル時 (None) は JS 状態を変更しない (= 既存 override を保持)
                if let Some(p) = path {
                    let js_arg = serde_json::to_string(&p).unwrap_or_else(|_| "null".into());
                    let script =
                        format!("window.setClonePath && window.setClonePath({})", js_arg);
                    if let Err(e) = sidebar.evaluate_script(&script) {
                        tracing::warn!("sidebar setClonePath 失敗: {}", e);
                    }
                } else {
                    tracing::debug!("clone path picker canceled");
                }
            }
            Event::UserEvent(AppEvent::SidebarIpc(msg)) => {
                // VP-100 follow-up: project:add / project:clone は async picker → API → ProjectsLoaded ルート
                // (state 直接 mutate しないので handle_sidebar_ipc の前で分岐)
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
                    match parsed.get("t").and_then(|v| v.as_str()) {
                        Some("process:add") => {
                            let initial_dir =
                                resolve_default_project_root(&settings, &sidebar_state);
                            spawn_add_project_picker(async_action_proxy.clone(), initial_dir);
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
                                resolve_default_project_root(&settings, &sidebar_state);
                            spawn_clone_project(
                                async_action_proxy.clone(),
                                url,
                                default_root,
                                target_override,
                            );
                            return;
                        }
                        Some("project:clone:pickFolder") => {
                            let initial_dir =
                                resolve_default_project_root(&settings, &sidebar_state);
                            spawn_clone_path_picker(async_action_proxy.clone(), initial_dir);
                            return;
                        }
                        _ => {}
                    }
                }
                let outcome = handle_sidebar_ipc(&msg, &mut sidebar_state, &mut session_state);
                if outcome.changed {
                    push_sidebar_state(&sidebar, &sidebar_state);
                }
                if outcome.active_changed {
                    push_active_view(&main_view, &sidebar_state);
                    // Phase 2.5: lane:select は per-Lane instance の display 切替だけ。
                    // WebSocket は browser native で SP に直接繋がってる (ensure 済)。
                    lane_js::show_lane(
                        &main_view,
                        sidebar_state.active_lane_address.as_deref(),
                    );
                }
                // Architecture v4: dead な project が expand されたら SP を auto-spawn。
                // dedup: 同 session で同じ path を 2 回呼ばない (TheWorld 側でも弾かれるが
                // 余計な POST を避ける)。
                if let Some((name, path)) = outcome.sp_spawn_request {
                    if sp_spawn_triggered.insert(path.clone()) {
                        tracing::info!(
                            "SP auto-spawn 要求 (accordion expand trigger): name={} path={}",
                            name,
                            path
                        );
                        spawn_sp_start(async_action_proxy.clone(), name, path);
                    } else {
                        tracing::debug!("SP auto-spawn skip (既 trigger): {}", path);
                    }
                }
                // Phase 5-D fix: accordion 閉じた → dedup HashSet から path を release。
                //  spawn 失敗で entry が居残ったまま user が collapse → expand すれば確実に retry。
                if let Some(path) = outcome.sp_spawn_release
                    && sp_spawn_triggered.remove(&path)
                {
                    tracing::info!(
                        "SP auto-spawn dedup released (accordion collapse): {}",
                        path
                    );
                }
                // Phase 5-C: Process restart 要求 (sidebar の 🔄 button から)
                // Phase 5-D fix: bare `tokio::spawn` は wry main thread (= tokio runtime context 無)
                //   から呼ぶと panic 即死。 他の async handler と同じく
                //   `thread::Builder::spawn + Builder::new_current_thread + rt.block_on` にする。
                if let Some(project_name) = outcome.restart_process_request {
                    let proxy = async_action_proxy.clone();
                    let project_name_clone = project_name.clone();
                    thread::Builder::new()
                        .name(format!("restart-{}", project_name))
                        .spawn(move || {
                            let rt = match tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                            {
                                Ok(rt) => rt,
                                Err(e) => {
                                    tracing::warn!("restart_process tokio runtime: {}", e);
                                    return;
                                }
                            };
                            rt.block_on(async move {
                                // TheWorld port は固定 32000 (vantage_point::cli::WORLD_PORT と同期)
                                let client = crate::client::TheWorldClient::new(32000);
                                match client.restart_process(&project_name_clone).await {
                                    Ok(()) => {
                                        tracing::info!(
                                            "restart_process OK: {}",
                                            project_name_clone
                                        );
                                        // 完了 → projects 再 fetch → sidebar state badge 更新
                                        if let Ok(projects) = client.list_projects().await {
                                            let _ = proxy
                                                .send_event(AppEvent::ProcessesLoaded(projects));
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "restart_process failed for {}: {}",
                                            project_name_clone,
                                            e
                                        );
                                    }
                                }
                            });
                        })
                        .ok();
                }
                // Process stop 要求 (project context menu の Stop project から)。
                // restart と同じく current-thread runtime を立てて async client を回す。
                if let Some(project_name) = outcome.stop_process_request {
                    let proxy = async_action_proxy.clone();
                    let project_name_clone = project_name.clone();
                    thread::Builder::new()
                        .name(format!("stop-{}", project_name))
                        .spawn(move || {
                            let rt = match tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                            {
                                Ok(rt) => rt,
                                Err(e) => {
                                    tracing::warn!("stop_process tokio runtime: {}", e);
                                    return;
                                }
                            };
                            rt.block_on(async move {
                                let client = crate::client::TheWorldClient::new(32000);
                                match client.stop_process(&project_name_clone).await {
                                    Ok(()) => {
                                        tracing::info!("stop_process OK: {}", project_name_clone);
                                        // 完了 → projects 再 fetch → 一時停止中 tab へ反映
                                        if let Ok(projects) = client.list_projects().await {
                                            let _ = proxy
                                                .send_event(AppEvent::ProcessesLoaded(projects));
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "stop_process failed for {}: {}",
                                            project_name_clone,
                                            e
                                        );
                                    }
                                }
                            });
                        })
                        .ok();
                }
                // Project delete 要求 (project context menu の Delete project から、
                // UI で 2-click 確認済)。 daemon の remove_project は稼働中 SP があると
                // エラーになるため、 先に stop → grace → remove と chain する
                // (restart_process が capability 内でやっているのと同じ順序)。
                if let Some((project_name, project_path)) = outcome.delete_project_request {
                    let proxy = async_action_proxy.clone();
                    thread::Builder::new()
                        .name(format!("delete-project-{}", project_name))
                        .spawn(move || {
                            let rt = match tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                            {
                                Ok(rt) => rt,
                                Err(e) => {
                                    tracing::warn!("delete_project tokio runtime: {}", e);
                                    return;
                                }
                            };
                            rt.block_on(async move {
                                let client = crate::client::TheWorldClient::new(32000);
                                // stop は best-effort: SP が未起動 (= 一時停止中) なら
                                // 「No running Process」 エラーが返るが、 続行して remove する。
                                match client.stop_process(&project_name).await {
                                    Ok(()) => {
                                        tracing::info!(
                                            "delete: stop_process OK: {}",
                                            project_name
                                        );
                                        // shutdown 伝播 + port release を待つ grace period
                                        tokio::time::sleep(
                                            std::time::Duration::from_millis(500),
                                        )
                                        .await;
                                    }
                                    Err(e) => {
                                        tracing::info!(
                                            "delete: stop_process skipped for {} (continuing): {}",
                                            project_name,
                                            e
                                        );
                                    }
                                }
                                match client.remove_project(&project_path).await {
                                    Ok(()) => {
                                        tracing::info!(
                                            "remove_project OK: {}",
                                            project_path
                                        );
                                        // 完了 → projects 再 fetch → sidebar から除去
                                        if let Ok(projects) = client.list_projects().await {
                                            let _ = proxy
                                                .send_event(AppEvent::ProcessesLoaded(projects));
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "remove_project failed for {}: {}",
                                            project_path,
                                            e
                                        );
                                    }
                                }
                            });
                        })
                        .ok();
                }
                // Phase 4-A: Worker Lane 削除要求 (sidebar の × button から)
                if let Some((project_path, address)) = outcome.delete_lane_request {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| p.path == project_path)
                        .and_then(|p| p.port);
                    if let Some(port) = sp_port {
                        // JS-side からも先 removeLane を呼ぶ (= xterm + WS 即時 dispose、
                        // server side は polling で sidebar から消える前にこちらが先)
                        lane_js::remove_lane(&main_view, &address);
                        let path_clone = project_path.clone();
                        let addr_clone = address.clone();
                        thread::Builder::new()
                            .name(format!("delete-lane-{}", address))
                            .spawn(move || {
                                let rt =
                                    match tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                    {
                                        Ok(rt) => rt,
                                        Err(e) => {
                                            tracing::warn!("delete-lane runtime: {}", e);
                                            return;
                                        }
                                    };
                                rt.block_on(async {
                                    let client = TheWorldClient::new(port);
                                    match client.delete_lane(&addr_clone).await {
                                        Ok(()) => {
                                            tracing::info!(
                                                "Lane deleted: project={} address={}",
                                                path_clone,
                                                addr_clone
                                            );
                                            // wiremsg Stage 1: 明示的な再 fetch は不要。
                                            // SP が LanePool 変化を "lanes" topic に publish し、
                                            // 購読側が snapshot を受信して sidebar を更新する。
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "delete_lane failed: project={} address={}: {}",
                                                path_clone,
                                                addr_clone,
                                                e
                                            );
                                        }
                                    }
                                });
                            })
                            .ok();
                    } else {
                        tracing::warn!(
                            "lane:delete: SP port unknown for path={} (skip)",
                            project_path
                        );
                    }
                }
                // Lane Lead Stand restart 要求 (sidebar の restart icon → confirm dialog から)
                if let Some((project_path, address)) = outcome.restart_lane_request {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| p.path == project_path)
                        .and_then(|p| p.port);
                    if let Some(port) = sp_port {
                        let path_clone = project_path.clone();
                        let addr_clone = address.clone();
                        thread::Builder::new()
                            .name(format!("restart-lane-{}", address))
                            .spawn(move || {
                                let rt = match tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                {
                                    Ok(rt) => rt,
                                    Err(e) => {
                                        tracing::warn!("restart-lane runtime: {}", e);
                                        return;
                                    }
                                };
                                rt.block_on(async {
                                    let client = TheWorldClient::new(port);
                                    match client.restart_lane(&addr_clone).await {
                                        Ok(()) => {
                                            tracing::info!(
                                                "Lane restarted: project={} address={}",
                                                path_clone,
                                                addr_clone
                                            );
                                            // wiremsg Stage 1: 新 pid / state は SP の
                                            // "lanes" topic snapshot で購読側に push される。
                                            // WS は PR #218 の auto-reconnect で透過的に新 PtySlot に attach し直す。
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "restart_lane failed: project={} address={}: {}",
                                                path_clone,
                                                addr_clone,
                                                e
                                            );
                                        }
                                    }
                                });
                            })
                            .ok();
                    } else {
                        tracing::warn!(
                            "lane:restart: SP port unknown for path={} (skip)",
                            project_path
                        );
                    }
                }
                // Phase 3-A: Worker Lane 作成要求 (sidebar の + Add Worker から)
                // doc 11 PR-C: stand 指定 を tuple 4 番目に追加 (None なら SP-side default)
                if let Some((project_path, name, branch, stand)) = outcome.add_worker_request {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| p.path == project_path)
                        .and_then(|p| p.port);
                    if let Some(port) = sp_port {
                        let proxy = async_action_proxy.clone();
                        let name_clone = name.clone();
                        let branch_clone = branch.clone();
                        let stand_clone = stand.clone();
                        let path_clone = project_path.clone();
                        thread::Builder::new()
                            .name(format!("create-worker-{}", name))
                            .spawn(move || {
                                let rt =
                                    match tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                    {
                                        Ok(rt) => rt,
                                        Err(e) => {
                                            tracing::warn!(
                                                "create-worker tokio runtime: {}",
                                                e
                                            );
                                            return;
                                        }
                                    };
                                rt.block_on(async {
                                    let client = TheWorldClient::new(port);
                                    match client
                                        .create_wing_lane(
                                            &name_clone,
                                            branch_clone.as_deref(),
                                            stand_clone.as_deref(),
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            tracing::info!(
                                                "Worker Lane created: project={} name={} branch={:?}",
                                                path_clone,
                                                name_clone,
                                                branch_clone
                                            );
                                            // wiremsg Stage 1: 新 Lane は SP の "lanes"
                                            // topic snapshot で購読側に push される。
                                            // R5: 成功通知を sidebar に push back (form を閉じる)
                                            let _ = proxy.send_event(
                                                AppEvent::WorkerCreateResult {
                                                    project_path: path_clone,
                                                    name: name_clone,
                                                    error: None,
                                                },
                                            );
                                        }
                                        Err(e) => {
                                            // R5: 失敗通知を sidebar に push back (form 下に
                                            // inline error 表示)。 server からは
                                            // "create_worker_lane HTTP <code>: <body>" 形式で
                                            // 返ってくるので、 そのまま流す (UI 側で trim)。
                                            let msg = format!("{}", e);
                                            tracing::warn!(
                                                "create_worker_lane failed: project={} name={}: {}",
                                                path_clone,
                                                name_clone,
                                                msg
                                            );
                                            let _ = proxy.send_event(
                                                AppEvent::WorkerCreateResult {
                                                    project_path: path_clone,
                                                    name: name_clone,
                                                    error: Some(msg),
                                                },
                                            );
                                        }
                                    }
                                });
                            })
                            .ok();
                    } else {
                        tracing::warn!(
                            "lane:add_worker: SP port unknown for path={} (skip)",
                            project_path
                        );
                    }
                }

                // doc 11 PR-C: 利用可能 Stand 一覧 fetch 要求 (sidebar の + Add Worker 開閉から)
                if let Some(project_path) = outcome.list_stands_request {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| p.path == project_path)
                        .and_then(|p| p.port);
                    if let Some(port) = sp_port {
                        let proxy = async_action_proxy.clone();
                        let path_clone = project_path.clone();
                        thread::Builder::new()
                            .name(format!("list-stands-{}", port))
                            .spawn(move || {
                                let rt =
                                    match tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                    {
                                        Ok(rt) => rt,
                                        Err(e) => {
                                            tracing::warn!(
                                                "list-stands tokio runtime: {}",
                                                e
                                            );
                                            return;
                                        }
                                    };
                                rt.block_on(async {
                                    let client = TheWorldClient::new(port);
                                    match client.list_stands().await {
                                        Ok(stands) => {
                                            tracing::debug!(
                                                "stands listed: project={} count={}",
                                                path_clone,
                                                stands.len()
                                            );
                                            let _ = proxy.send_event(AppEvent::StandsResult {
                                                project_path: path_clone,
                                                stands,
                                                error: None,
                                            });
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "list_stands failed: project={}: {}",
                                                path_clone,
                                                e
                                            );
                                            let _ = proxy.send_event(AppEvent::StandsResult {
                                                project_path: path_clone,
                                                stands: Vec::new(),
                                                error: Some(e.to_string()),
                                            });
                                        }
                                    }
                                });
                            })
                            .ok();
                    } else {
                        tracing::warn!(
                            "stands:fetch: SP port unknown for path={} (skip)",
                            project_path
                        );
                    }
                }
            }
            // VP-100 γ-light: ResizeObserver からの slot 矩形通知を蓄積。
            // Phase 4+ で native overlay の `set_position` 同期に使う。
            Event::UserEvent(AppEvent::SlotRect {
                pane_id,
                kind,
                rect,
            }) => {
                if let Some(id) = pane_id {
                    slot_rects.insert(id.clone(), rect);
                    tracing::trace!("slot:rect kind={} pane={} rect={:?}", kind, id, rect);
                } else {
                    tracing::trace!("slot:rect kind={} (no pane_id) rect={:?}", kind, rect);
                }
            }
            // VP-100 follow-up: muda メニュー項目クリック処理
            //
            // 1Password 風 UX:
            //  - "Developer Mode" check item トグル → settings 永続化、Open DevTools の enabled 切替
            //  - "Open Developer Tools" → dev_mode == true なら main_view.open_devtools()
            Event::UserEvent(AppEvent::MenuClicked(id)) => {
                if id == menu_ids.new_window {
                    // Cmd+N: 新規 vp-app process を spawn = 新しい MainWindow が独立 process で立つ。
                    // 同 EventLoop に重ねるのではなく fork-style で別 process 化することで、
                    // state 干渉ゼロ + crash isolation + multi-instance 並行開発が可能に。
                    // TheWorld daemon (port 32000) は process 横断 shared なので projects 一覧は同期。
                    match std::env::current_exe() {
                        Ok(exe) => {
                            match std::process::Command::new(&exe)
                                // 子 process は auto-select を skip ── 元 vp-app と active_lane
                                // が衝突して両方の terminal WS が壊れるのを防ぐ。
                                // 起動後 user が手動で lane 選択するまで main_area は empty。
                                .env("VP_APP_SECONDARY", "1")
                                .spawn()
                            {
                                Ok(child) => {
                                    tracing::info!(
                                        "Cmd+N: spawned new vp-app process (pid={})",
                                        child.id()
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Cmd+N: failed to spawn new process at {}: {}",
                                        exe.display(),
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Cmd+N: current_exe() failed: {}", e);
                        }
                    }
                } else if id == menu_ids.developer_mode {
                    dev_mode = !dev_mode;
                    dev_mode_item.set_checked(dev_mode);
                    open_devtools_item.set_enabled(dev_mode);
                    open_sidebar_devtools_item.set_enabled(dev_mode);
                    settings.developer_mode = Some(dev_mode);
                    if let Err(e) = settings.save() {
                        tracing::warn!("Settings 保存失敗: {}", e);
                    }
                    tracing::info!("Developer Mode: {} (永続化)", dev_mode);
                    let body = if dev_mode {
                        "Developer Mode が有効になりました。View → Open Developer Tools で DevTools を開けます。"
                    } else {
                        "Developer Mode が無効になりました。"
                    };
                    if let Err(e) = notify_rust::Notification::new()
                        .summary("Vantage Point")
                        .body(body)
                        .show()
                    {
                        tracing::debug!("notification 表示失敗: {}", e);
                    }
                } else if id == menu_ids.open_devtools {
                    if dev_mode {
                        main_view.open_devtools();
                        tracing::info!("DevTools open (main_view)");
                    } else {
                        tracing::warn!("Open DevTools clicked but dev_mode=false (gated)");
                    }
                } else if id == menu_ids.open_sidebar_devtools {
                    if dev_mode {
                        sidebar.open_devtools();
                        tracing::info!("DevTools open (sidebar)");
                    } else {
                        tracing::warn!(
                            "Open Sidebar DevTools clicked but dev_mode=false (gated)"
                        );
                    }
                } else {
                    tracing::debug!("MenuClicked: 未処理の id = {:?}", id);
                }
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod sidebar_asset_tests {
    //! sidebar の shell HTML + JS bundle が vp-asset:// で配信できることの検証。
    //! Bundle font / serve handler のテストは `web_assets` module 側に分離。
    use super::*;

    /// `SIDEBAR_ASSETS` で sidebar.html / sidebar.bundle.js が
    /// `web_assets::lookup_asset` 経由で取れる。
    #[test]
    fn sidebar_assets_servable_via_vp_asset() {
        let html = crate::web_assets::lookup_asset("vp-asset://app/sidebar.html", SIDEBAR_ASSETS);
        assert!(html.is_some(), "sidebar.html not lookupable");
        let (bytes, ct) = html.unwrap();
        assert_eq!(ct, "text/html; charset=utf-8");
        assert_eq!(bytes, SIDEBAR_HTML.as_bytes());

        let js =
            crate::web_assets::lookup_asset("vp-asset://app/sidebar.bundle.js", SIDEBAR_ASSETS);
        assert!(js.is_some(), "sidebar.bundle.js not lookupable");
        assert_eq!(js.unwrap().1, "application/javascript; charset=utf-8");
    }

    /// shell HTML が SolidJS bundle を mount する骨格を持つ。
    #[test]
    fn sidebar_html_mounts_bundle() {
        assert!(
            SIDEBAR_HTML.contains(r#"<div id="sidebar-root">"#),
            "shell に #sidebar-root mount point がない"
        );
        assert!(
            SIDEBAR_HTML.contains("vp-asset://app/sidebar.bundle.js"),
            "shell が sidebar.bundle.js を load していない"
        );
    }
}
