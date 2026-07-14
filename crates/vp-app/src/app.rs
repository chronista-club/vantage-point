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

use std::time::Duration;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{
    Rect, WebView, WebViewBuilder, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize,
};

use crate::client::{ProjectInfo, TheWorldClient};
use crate::main_area::{self, ActivePaneInfo, MAIN_AREA_HTML, SlotRect};
use crate::pane::{ActiveStand, ActivitySnapshot, ProjectPaneState, SidebarState};
use crate::project_dialog::{
    resolve_default_project_root, spawn_add_project_picker, spawn_clone_path_picker,
    spawn_clone_project,
};
use crate::session_state::SessionState;
use crate::settings::Settings;
use crate::terminal::{self, AppEvent};

/// Sidebar の固定幅 (LogicalPixel)。
/// WebView 統合 (step 3a) 後は HTML 側 CSS (#sidebar-root width:280px) が司るため Rust 側は未使用
/// (MIN_WINDOW_WIDTH 算出の参照値として comment でのみ言及)。
#[allow(dead_code)]
const SIDEBAR_WIDTH: f64 = 280.0;

/// 起動時の window default size (LogicalPixel)。 with_inner_size と clamp 矯正後の値で
/// 共用するため定数化。
const DEFAULT_WINDOW_WIDTH: f64 = 1200.0;
const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;

/// 最低 window size (LogicalPixel)。 SIDEBAR_WIDTH (280) + 余裕ある main 領域 (820+) を
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

/// WebView 統合 (step 3a) 後の唯一の webview が `vp-asset://` で配信する asset。
/// `MAIN_AREA_HTML` (sidebar bundle + editor-host bundle を inline 済) を `app/index.html` で配信。
///
/// ## なぜ with_html ではなく custom protocol か (統合 origin fix)
/// `with_html` で load した document は **about:blank = 不透明 (opaque) オリジン**になり、
/// `localStorage` 等 origin 依存 API が `SecurityError` を throw する。統合で sidebar bundle を
/// 同 document に inline した結果、`Shell()` が render 時に踏む `localStorage.getItem`
/// (タブ状態の永続) で sidebar bundle が boot 中に落ち、`<Shell/>` が mount されず sidebar が
/// 空になっていた。custom protocol で load すれば document origin = `vp-asset://app` の
/// 実オリジンになり、統合前 (sidebar が `vp-asset://app/sidebar.html` を load していた頃) と
/// 同じく localStorage が使える。
const MAIN_VIEW_ASSETS: &[(&str, &[u8], &str)] = &[(
    "app/index.html",
    MAIN_AREA_HTML.as_bytes(),
    "text/html; charset=utf-8",
)];

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

/// WebView 統合 (step 3a): 統合 ipc_handler の dispatch 判定。
/// main (terminal / pane) IPC tag なら true、 sidebar IpcEnvelope tag (project: / lane: 系)
/// なら false。 tag 集合は `terminal::handle_ipc_message` の match arm と一致 (disjoint)。
/// terminal の fall-through に頼ると sidebar tag を silent drop するため、 ここで明示判定する。
fn is_main_ipc_tag(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    matches!(
        v.get("t").and_then(|t| t.as_str()),
        Some(
            "ready"
                | "term:write"
                | "term:resize"
                | "lanes:ensure-all"
                | "copy"
                | "paste:request"
                | "debug"
                | "osc:notification"
                | "slot:rect"
                | "pp:state:save"
                | "pp:state:load"
                | "console"
                | "open-url"
                | "echoes:submit"
                | "echoes:respond"
                | "echoes:interrupt"
                | "echoes:set_permission_mode"
                | "console:set_mode"
                | "console:new_session"
                | "console:set_model"
        )
    )
}

/// muda の `MenuEvent::receiver()` channel を polling して `AppEvent::MenuClicked` に
/// 変換する pump スレッドを起動する。muda の menu event は global channel (single
/// receiver) なので 1 thread だけ起動する。
///
/// channel は sync (`rx.recv()` が blocking) なので shared runtime の blocking pool に逃す。
fn spawn_menu_event_pump(rt_handle: &tokio::runtime::Handle, proxy: EventLoopProxy<AppEvent>) {
    rt_handle.spawn_blocking(move || {
        let rx = muda::MenuEvent::receiver();
        while let Ok(ev) = rx.recv() {
            if proxy.send_event(AppEvent::MenuClicked(ev.id)).is_err() {
                tracing::debug!("EventLoop 終了、menu pump も終了");
                break;
            }
        }
    });
}

/// F6 (doc 27 §3.4): active_lane_address から対応する project_path を引く。
///
/// active_lane_address (`<project>/conductor` or `<project>/performer/<name>`) から、 対応する
/// project_path を引く。 World process-proxy は SP port 不問・project_path を path_key に正規化して
/// routing するので、 ask 系 (pp:state) は port でなく path で引く。 解決失敗 (lane 未選択 / SP 未起動)
/// なら `None`。 caller: `PpStateSaveRequest` / `PpStateLoadRequest` の process-proxy ask。
pub(crate) fn resolve_active_project_path(state: &crate::pane::SidebarState) -> Option<String> {
    let active = state.active_lane_address.as_deref()?;
    for proc in &state.processes {
        if let Some(lanes) = state.lanes_by_project.get(&proc.path)
            && lanes.iter().any(|l| l.address.key() == active)
        {
            return Some(proc.path.clone());
        }
    }
    None
}

pub(crate) fn merge_ports_from_running(
    projects: &mut [crate::client::ProjectInfo],
    running: &[crate::client::RunningProcess],
) {
    let port_by_name: std::collections::HashMap<String, u16> = running
        .iter()
        .map(|r| (r.project_name.clone(), r.port))
        .collect();
    for p in projects.iter_mut() {
        if let Some(&port) = port_by_name.get(&p.name) {
            p.port = Some(port);
        }
    }
}

/// 各 `ProjectInfo.port` に runtime port を merge した list を返す。
///
/// `list_projects()` を直接呼んでそのまま `ProjectsLoaded` に乗せると、 config に port を
/// 書いていない project (= 大多数) の port が `None` で来てしまい、 sidebar_state.processes
/// の port を全潰しする。 これが起きると以降の `LanesLoaded` で `ensureLane` が skip され
/// terminal が表示されなくなる (= restart / stop / delete 後の conductor console 消失 bug)。
/// **全 fetch 経路はこのヘルパ 1 本に集約**して同じ join をかける。
///
/// `list_processes` 側のみエラーなら空 map 扱い (= port は config 値のまま) で degrade、
/// `list_projects` 側エラーは bubble up する。
pub(crate) async fn fetch_projects_with_ports(
    client: &TheWorldClient,
) -> anyhow::Result<Vec<ProjectInfo>> {
    let (proj_res, run_res) = tokio::join!(client.list_projects(), client.list_processes());
    let mut projects = proj_res?;
    match run_res {
        Ok(runs) => merge_ports_from_running(&mut projects, &runs),
        Err(e) => {
            tracing::warn!("list_processes 失敗 (port 不明、 config 値のみ): {}", e);
        }
    };
    Ok(projects)
}

/// 起動時に TheWorld の Process list を別スレッドで fetch。
///
/// **Phase A4-3b bug fix (mem_1CaTpCQH8iLJ2PasRcPjHv Architecture v4)**:
/// `fetch_projects_with_ports` で registered + running を join して、各 Process に
/// `port` と `state` を解決した状態で `ProjectsLoaded` event に乗せる。
///
/// これにより handler 側で `if let Some(port) = p.port { spawn_lanes_subscription(...) }` が動く経路完成。
fn spawn_processes_fetch(rt_handle: &tokio::runtime::Handle, proxy: EventLoopProxy<AppEvent>) {
    rt_handle.spawn(async move {
        let client = TheWorldClient::default();
        match fetch_projects_with_ports(&client).await {
            Ok(processes) => {
                // polling 毎回発火するため log omit (= loop noise)。
                let _ = proxy.send_event(AppEvent::ProjectsLoaded(processes));
            }
            Err(e) => {
                tracing::warn!("TheWorld fetch 失敗 (daemon 未起動?): {}", e);
                let _ = proxy.send_event(AppEvent::ProjectsError(e.to_string()));
            }
        }
    });
}

/// 1 回の Unison channel 接続セッションの終わり方 ("lanes" / "canvas" 購読が共用)。
enum SubscriptionOutcome {
    /// セッション確立後に切断 (SP restart / channel close)。即再接続の対象。
    Disconnected,
    /// event loop が閉じた (= app 終了)。購読スレッドを畳む。
    AppClosing,
}

/// F1b (doc 27 §3.4.4): vp-app → World :32000 の全 persistent session (lanes / canvas /
/// terminal / device) を **1 QUIC connection に集約**するための共有ハンドル。
///
/// `current` watch は現 epoch の `ProtocolClient` (= 1 connection) を全 session に配る
/// (None = 未接続 / 再接続中)。 session は `wait_client()` で接続を待ち、 得た client で
/// `open_channel` して自分の stream を張る (= 1 conn × N streams)。 reconnect は manager task が
/// 一手に所有し、 epoch ごとに fresh client を connect → publish する (F1a SP uplink と同パターン)。
///
/// 旧構成は session ごと (lanes / canvas は project ごと、 terminal は lane ごと) に別 QUIC
/// connection を張り、 QUIC の多重化を使えていなかった (§3.4.4 負債)。 これを 1 connection に畳む。
#[derive(Clone)]
struct SharedWorldConn {
    current: tokio::sync::watch::Receiver<Option<std::sync::Arc<unison::ProtocolClient>>>,
}

impl SharedWorldConn {
    /// 共有 connection が確立する (current = Some) まで待ち、 その client を返す。
    /// watch sender が drop された (= app 終了) 場合は None。
    async fn wait_client(&mut self) -> Option<std::sync::Arc<unison::ProtocolClient>> {
        loop {
            if let Some(client) = self.current.borrow().clone() {
                return Some(client);
            }
            // None の間は変化を待つ。 sender drop で Err = app 終了。
            if self.current.changed().await.is_err() {
                return None;
            }
        }
    }
}

/// 共有 World connection を connect / reconnect し続ける manager を spawn し、 ハンドルを返す。
///
/// epoch ごとに fresh `ProtocolClient` を build → connect → `current` に publish → 切断検知で
/// None に戻して exp backoff reconnect。 全 session が `wait_client` で追従する。 reconnect 機構を
/// ここに一元化することで、 各 session は channel logic だけを持てば良くなる (関心分離)。
fn spawn_world_conn_manager(
    rt_handle: &tokio::runtime::Handle,
    world_port: u16,
) -> SharedWorldConn {
    let (current_tx, current_rx) =
        tokio::sync::watch::channel::<Option<std::sync::Arc<unison::ProtocolClient>>>(None);

    rt_handle.spawn(async move {
        use unison::ProtocolClient;
        use unison::network::ClientConnectionEvent;
        use unison::network::TrustAnchors;
        use unison::network::quic::QuicClient;

        let addr = format!("[::1]:{}", world_port);
        const INITIAL_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(16);
        let mut backoff = INITIAL_BACKOFF;
        let mut generation: u64 = 0;

        loop {
            // epoch ごとに fresh client (F1a SP uplink と同じ「再接続 = 新 client」パターン)。
            let transport = match QuicClient::builder()
                .trust_anchors(TrustAnchors::SkipVerification)
                .build()
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("world conn: QUIC client build 失敗: {} (リトライ)", e);
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
                    continue;
                }
            };
            let client = std::sync::Arc::new(ProtocolClient::new(transport));

            match client.connect(&addr).await {
                Ok(()) => {
                    backoff = INITIAL_BACKOFF;
                    generation += 1;
                    tracing::info!(
                        "world conn: 共有 connection 確立 (gen={}, addr={})",
                        generation,
                        addr
                    );
                    let mut conn_events = client.subscribe_connection_events();
                    // F1b heartbeat: vp-app は passive subscriber (recv 待ち) のみで能動送信が無いため、
                    // connection 死を QUIC idle timeout (60s) でしか検知できない。 15s ごとに
                    // world-control へ ping して liveness を能動確認する (client→server 一方向、 server は
                    // 応答のみ = 両端 heartbeat にしない)。 open 失敗時は None で conn_events (60s) に degrade。
                    let heartbeat = client.open_channel("world-control").await.ok();
                    // session に新 client を配る。 receiver 全滅 (= app 終了) なら manager も終了。
                    if current_tx.send(Some(client.clone())).is_err() {
                        return;
                    }
                    let mut hb_tick = tokio::time::interval(std::time::Duration::from_secs(15));
                    hb_tick.tick().await; // 最初の tick (即時) をスキップ
                    // 切断を待つ (conn_events か heartbeat 失敗のどちらか早い方で再接続へ抜ける)。
                    loop {
                        tokio::select! {
                            conn_ev = conn_events.recv() => {
                                match conn_ev {
                                    Ok(ClientConnectionEvent::Disconnected { reason }) => {
                                        tracing::warn!("world conn: 切断検知 ({}) → 再接続", reason);
                                        break;
                                    }
                                    Ok(_) => {}
                                    Err(_) => break, // event channel closed = client 異常、 再接続へ
                                }
                            }
                            _ = hb_tick.tick() => {
                                if let Some(hb) = &heartbeat {
                                    // 5s 以内に pong が返らなければ connection 死と判断 (idle timeout 60s を待たない)。
                                    let pong = tokio::time::timeout(
                                        std::time::Duration::from_secs(5),
                                        hb.request::<serde_json::Value, serde_json::Value>("ping", &serde_json::json!({})),
                                    )
                                    .await;
                                    if !matches!(pong, Ok(Ok(_))) {
                                        tracing::warn!("world conn: heartbeat 応答なし → 再接続");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    // 再接続前に None を配る (session は wait_client で次 client を待つ)。
                    if current_tx.send(None).is_err() {
                        return;
                    }
                    drop(heartbeat);
                    // 旧 connection を明示 close する。 session は同じ `Arc<ProtocolClient>` を握って
                    // recv() でブロックしているため、 manager 側の drop だけでは refcount>0 で
                    // connection が閉じず、 session の recv() は old connection の idle timeout (60s)
                    // まで Err にならない (= heartbeat で manager を 15s 再接続させても session が 60s
                    // migrate しない)。 disconnect() で即 stream reset → session の recv() が即 Err →
                    // wait_client で次 client へ移る。
                    let _ = client.disconnect().await;
                    drop(client); // 次 loop で fresh client
                }
                Err(e) => {
                    tracing::debug!(
                        "world conn: 接続失敗 ({}), {}ms 後 retry",
                        e,
                        backoff.as_millis()
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
                }
            }
        }
    });

    SharedWorldConn {
        current: current_rx,
    }
}

/// wiremsg Stage 1 consumer: SP の "lanes" Unison channel を購読し、retained Lane
/// snapshot を受信して `AppEvent::LanesLoaded` を emit する。旧 `spawn_lanes_fetch`
/// (one-shot HTTP poll) を置換する long-lived 購読。F1b: 共有 connection 上の stream で、
/// reconnect は `SharedWorldConn` の manager が所有するので give-up せず追従する。
/// 設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
///
/// L0 SP-portless (lanes slice): 接続先は SP 直結ではなく **World :32000 の集約 "lanes" channel**。
/// World は registry channel 経由で各 SP の lane snapshot/diff を受けて lane_registry に集約済で、
/// 本購読は project_path で scope して当該 project の snapshot を受ける (繋ぎ先が変わっただけで
/// consumer ロジックは不変)。
fn spawn_lanes_subscription(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    process_path: String,
    conn: SharedWorldConn,
) {
    rt_handle.spawn(lanes_subscription_loop(proxy, process_path, conn));
}

/// lanes 購読の各フェーズ (wait_client / open / subscribe / 初回 snapshot) の stall 判定 timeout。
/// これを超えたら World lanes channel 無応答 (half-alive) or QUIC 未接続とみなし Err 化 →
/// `LanesError` surface (UI が stalled 表示) + retry (self-heal)。 retained topic は本来即応するので
/// 余裕を見て 12s。 doc 30 §5-3 (loading lanes の状態区別)。
const LANES_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// "lanes" channel の購読 → 再購読を司る long-lived ループ (F1b: 共有 connection 上の stream)。
///
/// reconnect は `SharedWorldConn` の manager が一手に所有するので、 本ループは
/// `wait_client` で接続を待ち、 得た client で session を回すだけ。 SP unreachable でも諦めず
/// 共有 connection に追従する (旧 10 連続失敗 give-up + `LanesSubscriptionEnded` は廃止)。
async fn lanes_subscription_loop(
    proxy: EventLoopProxy<AppEvent>,
    process_path: String,
    mut conn: SharedWorldConn,
) {
    loop {
        // 共有 connection が確立するまで待つ。 self-heal: World QUIC が長時間 未接続 (dead World)
        // だと wait_client が永久ブロックし「loading lanes」が silent 滞留する。 timeout を張って
        // 未接続を LanesError として surface し (UI が stalled 表示 → user が daemon restart できる)、
        // 待ち直す。 App 終了 (sender drop) は None で即抜ける。
        let client = match tokio::time::timeout(LANES_STALL_TIMEOUT, conn.wait_client()).await {
            Ok(Some(c)) => c,
            Ok(None) => return, // app 終了
            Err(_) => {
                let _ = proxy.send_event(AppEvent::LanesError {
                    process_path: process_path.clone(),
                    message: "world QUIC 未接続 (wait_client timeout)".to_string(),
                });
                continue;
            }
        };
        match run_lanes_session(&proxy, &process_path, &client).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            // 切断は共有 manager が面倒を見るので、 次の client を待つだけ (per-session error 扱い無し)。
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                // open_channel / handshake 失敗。 surface に通知しつつ give-up せず次の接続機会を待つ。
                tracing::warn!("lanes subscription error: project={}: {}", process_path, e);
                let _ = proxy.send_event(AppEvent::LanesError {
                    process_path: process_path.clone(),
                    message: e,
                });
                // connected だが open_channel が連続失敗するケースの busy loop を避ける小休止。
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
    client: &unison::ProtocolClient,
) -> Result<SubscriptionOutcome, String> {
    // F1b: 共有 connection 上に "lanes" stream を開く (旧: session ごと別 connect)。
    // self-heal: open を LANES_STALL_TIMEOUT で括る。 timeout 時は channel 未確立 (recv_task も
    // 未起動) なので、 raw stream の drop = implicit reset で片付く (close 不要)。
    let channel = tokio::time::timeout(LANES_STALL_TIMEOUT, client.open_channel("lanes"))
        .await
        .map_err(|_| "open lanes channel: timeout".to_string())?
        .map_err(|e| format!("open lanes channel: {}", e))?;

    // ここから先の全 early-return は **確立済み channel** を残すため、 内側で結果を作ってから
    // 抜けに 1 度だけ `channel.close()` する (recv_task abort + stream close)。 close せず drop すると
    // recv_task と QUIC stream がリークし、 half-alive 障害の 12.5s retry ごとに積み上がって
    // MAX_STREAMS 枯渇 → この fix が直そうとした症状が再発する (Moody Blues #1)。
    let outcome = lanes_session_after_open(proxy, process_path, &channel).await;
    let _ = channel.close().await;
    outcome
}

/// `run_lanes_session` の channel 確立後のロジック (subscribe → recv loop)。 呼び出し元が
/// 戻り後に必ず `channel.close()` するため、 本体は close を気にせず早期 return してよい。
async fn lanes_session_after_open(
    proxy: &EventLoopProxy<AppEvent>,
    process_path: &str,
    channel: &unison::network::UnisonChannel,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // L0 SP-portless: World "lanes" channel は project 単位なので、 接続後に subscribe
    // handshake で project_path を渡す (World 側で path_key に正規化されて lane_registry と突合)。
    // ack 後に当該 project の snapshot が `send_event("snapshot", ...)` で初期配信される。
    // self-heal: subscribe を LANES_STALL_TIMEOUT で括る (half-alive で永久ブロックしない)。
    tokio::time::timeout(
        LANES_STALL_TIMEOUT,
        channel.request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": process_path }),
        ),
    )
    .await
    .map_err(|_| "lanes subscribe handshake: timeout".to_string())?
    .map_err(|e| format!("lanes subscribe handshake: {}", e))?;
    tracing::info!(
        "lanes subscription connected (via World): project={}",
        process_path
    );

    // 初回 snapshot deadline: retained topic なので即届くはず。 来なければ stall とみなし Err (retry)。
    // 初回受信後は deadline を外し、 steady-state の変化 push を無期限に待つ。
    let mut first_snapshot_deadline = Some(tokio::time::Instant::now() + LANES_STALL_TIMEOUT);
    loop {
        let msg = match first_snapshot_deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, channel.recv()).await {
                Ok(Ok(m)) => m,
                Ok(Err(_)) => return Ok(SubscriptionOutcome::Disconnected),
                Err(_) => return Err("lanes first snapshot timeout".to_string()),
            },
            // セッション確立後の切断 (SP 停止 / channel close)。再接続対象。
            None => match channel.recv().await {
                Ok(m) => m,
                Err(_) => return Ok(SubscriptionOutcome::Disconnected),
            },
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
        // 初回 snapshot を受けたら deadline 解除 (以降は変化 push を無期限に待つ = steady-state)。
        first_snapshot_deadline = None;
        // LanesLoaded push (= retained snapshot + delta) は project × frequency で
        // ループする systematic event なので log omit (= info / debug どちらでも noise)。
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
///
/// L0 SP-portless (canvas slice): 接続先は SP 直結ではなく **World :32000 の集約 "canvas" channel**。
/// 各 SP が paisley-park topic を World に push し、 World が project の TopicRouter に集約済なので、
/// 本購読は project_path で scope して当該 project の canvas (retained + live) を受ける。
fn spawn_canvas_subscription(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    process_path: String,
    conn: SharedWorldConn,
) {
    rt_handle.spawn(canvas_subscription_loop(proxy, process_path, conn));
}

/// "canvas" channel の購読 → 再購読を司る long-lived ループ (F1b: 共有 connection 上の stream)。
///
/// reconnect は `SharedWorldConn` の manager が所有。 本ループは `wait_client` で接続を待ち
/// session を回すだけで、 give-up + `CanvasSubscriptionEnded` は廃止 (共有 conn に追従)。
async fn canvas_subscription_loop(
    proxy: EventLoopProxy<AppEvent>,
    process_path: String,
    mut conn: SharedWorldConn,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_canvas_session(&proxy, &process_path, &client).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("canvas subscription error: project={}: {}", process_path, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
    client: &unison::ProtocolClient,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // F1b: 共有 connection 上に "canvas" stream を開く (旧: session ごと別 connect)。
    let channel = client
        .open_channel("canvas")
        .await
        .map_err(|e| format!("open canvas channel: {}", e))?;
    // L0 SP-portless: World "canvas" channel は project 単位なので、 接続後に subscribe handshake で
    // project_path を渡す (World 側で path_key に正規化され TopicRouter と突合)。 ack 後に当該 project の
    // retained canvas (最新 Show 等) が `send_event("pane", ...)` で初期配信される。
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": process_path }),
        )
        .await
        .map_err(|e| format!("canvas subscribe handshake: {}", e))?;
    tracing::info!(
        "canvas subscription connected (via World): project={}",
        process_path
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

/// terminal S4 (doc 27 §4.1): per-lane terminal session への command (WebView → SP)。
#[derive(Debug)]
enum TermCmd {
    /// keystroke (base64)。 canvas channel 上り request `terminal_write` で SP に送る。
    Write(String),
    /// resize (cols, rows)。 `terminal_resize` で送る。
    Resize(u16, u16),
}

/// terminal S4: 1 lane の terminal session handle (event loop が保持)。
///
/// map から remove すると `cmd_tx` が drop され、 session loop の `cmd_rx.recv()` が None を返して
/// 停止 → canvas channel drop → World 側 demand stop → SP pump stop
/// (= 購読者が消えたら pump を畳む、 S2 demand-driven production の出口)。
struct LaneTerminal {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<TermCmd>,
}

/// terminal S4: lane の terminal を World "canvas" channel に乗せる per-lane session を spawn。
///
/// `lane_key` = `<project>/conductor` 等 (`LaneAddressWire::key()`)。 World :32000 の "canvas"
/// channel に `pattern: process/terminal/data/{lane_key}/out` で subscribe → World demand 発火 →
/// SP pump start。 受信した PTY 出力は `AppEvent::TerminalOutput` で event loop に流し、 cmd_rx
/// 経由の write/resize は同 channel の上り request で SP に forward する (S3 bidirectional)。
fn spawn_terminal_session(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedWorldConn,
    process_path: String,
    lane_key: String,
) -> LaneTerminal {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    rt_handle.spawn(terminal_session_loop(
        proxy,
        conn,
        process_path,
        lane_key,
        cmd_rx,
    ));
    LaneTerminal { cmd_tx }
}

/// "canvas" channel (terminal pattern) の購読 → 再購読を司る long-lived ループ
/// (F1b: 共有 connection 上の per-lane stream)。 `cmd_rx` は再接続を跨いで保持する
/// (= 切断中に積まれた write/resize は次接続で送れる)。 reconnect は共有 manager が所有するので
/// `wait_client` で接続を待ち、 give-up はしない (lane 消滅 = cmd_tx drop で AppClosing 終了)。
async fn terminal_session_loop(
    proxy: EventLoopProxy<AppEvent>,
    mut conn: SharedWorldConn,
    process_path: String,
    lane_key: String,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<TermCmd>,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_terminal_session(&proxy, &process_path, &lane_key, &client, &mut cmd_rx).await {
            // AppClosing = event loop 終了 or lane removed (cmd_tx drop) → session 終了。
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("terminal session error: lane={}: {}", lane_key, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の terminal session: connect → `open_channel("canvas")` → subscribe(terminal pattern) →
/// recv (出力) / cmd (入力・resize) の select ループ。
///
/// 出力 `channel.recv()` と 上り `channel.request()` を同一 select! で扱う。 unison の `request` の
/// response は pending map で解決され `recv()` には来ず、 また `recv()` は内部 buffer 由来で
/// cancel-safe (= concurrent recv+request は control/process-proxy で実証済) なので、 cmd 分岐で
/// recv future を drop しても出力欠落しない。
async fn run_terminal_session(
    proxy: &EventLoopProxy<AppEvent>,
    process_path: &str,
    lane_key: &str,
    client: &unison::ProtocolClient,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TermCmd>,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // F1b: 共有 connection 上に per-lane terminal 用 "canvas" stream を開く (旧: lane ごと別 connect)。
    let channel = client
        .open_channel("canvas")
        .await
        .map_err(|e| format!("open canvas channel: {}", e))?;
    // 当該 lane の terminal topic を pattern 指定で subscribe (= demand を立てて SP pump を起こす)。
    let topic = format!("process/terminal/data/{}/out", lane_key.replace('/', "~"));
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": process_path, "pattern": topic }),
        )
        .await
        .map_err(|e| format!("terminal subscribe handshake: {}", e))?;
    tracing::info!(
        "terminal session connected: lane={} topic={}",
        lane_key,
        topic
    );

    loop {
        tokio::select! {
            recvd = channel.recv() => {
                let msg = match recvd {
                    Ok(m) => m,
                    Err(_) => return Ok(SubscriptionOutcome::Disconnected),
                };
                if msg.msg_type != MessageType::Event || msg.method != "pane" {
                    continue;
                }
                let payload = match msg.payload_as_value() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // LaneTerminalOutput { lane, data(base64) }。 lane は subscription で確定済なので
                // data だけ抜いて lane_key 付きで JS に渡す。
                if let Some(data) = payload.get("data").and_then(|v| v.as_str())
                    && proxy
                        .send_event(AppEvent::TerminalOutput {
                            lane: lane_key.to_string(),
                            data: data.to_string(),
                        })
                        .is_err()
                {
                    return Ok(SubscriptionOutcome::AppClosing);
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(TermCmd::Write(data)) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "terminal_write",
                                &serde_json::json!({ "lane": lane_key, "data": data }),
                            )
                            .await;
                    }
                    Some(TermCmd::Resize(cols, rows)) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "terminal_resize",
                                &serde_json::json!({ "lane": lane_key, "cols": cols, "rows": rows }),
                            )
                            .await;
                    }
                    // cmd_tx drop = lane removed → session 終了 (channel drop で demand stop)。
                    None => return Ok(SubscriptionOutcome::AppClosing),
                }
            }
        }
    }
}

// =============================================================================
// Echoes Act II (doc 32): per-lane echoes session — 構造化イベント購読 + prompt 投入
// =============================================================================
//
// terminal session と同型だが **demand-driven**: lane reconcile には結合させず、
// EchoesChatPane を開いた lane で初回 submit された時に lazy spawn する (SP 側 host の
// lazy モデルと一致)。subscribe → submit の順で走るため取りこぼしなし。

/// Echoes session への command (WebView → SP)。
#[derive(Debug)]
enum EchoesCmd {
    /// プロンプト投入。 canvas channel 上り request `echoes_submit` で SP に送る。
    Submit(String),
    /// doc 35 PR1: PromptCard 回答。 canvas channel 上り request `echoes_respond` で SP に送る。
    Respond {
        request_id: String,
        answers: Option<serde_json::Value>,
        behavior: Option<String>,
        message: Option<String>,
    },
    /// doc 35 §5 / PR2: 実行中 turn の中断。 canvas channel 上り request `echoes_interrupt` で SP へ。
    Interrupt,
    /// doc 35 §2.5 / PR3: permission mode 切替。 canvas channel 上り request `echoes_set_permission_mode` で SP へ。
    SetPermissionMode { mode: String },
}

/// 1 lane の echoes session handle (event loop が保持)。map から remove で cmd_tx drop → 停止。
struct LaneEchoes {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<EchoesCmd>,
}

/// lane の echoes を World "canvas" channel に乗せる per-lane session を spawn。
///
/// `process/echoes/data/{lane_key}/event` を subscribe → SP host が emit する EchoesEvent を
/// `AppEvent::EchoesEvent` で event loop に流し、 cmd (submit) は同 channel の上り request
/// `echoes_submit` で SP に forward する (terminal session の Act II 対応)。
fn spawn_echoes_session(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedWorldConn,
    process_path: String,
    lane_key: String,
) -> LaneEchoes {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    rt_handle.spawn(echoes_session_loop(
        proxy,
        conn,
        process_path,
        lane_key,
        cmd_rx,
    ));
    LaneEchoes { cmd_tx }
}

/// echoes session の購読 → 再購読を司る long-lived ループ (terminal_session_loop と同型)。
async fn echoes_session_loop(
    proxy: EventLoopProxy<AppEvent>,
    mut conn: SharedWorldConn,
    process_path: String,
    lane_key: String,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<EchoesCmd>,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_echoes_session(&proxy, &process_path, &lane_key, &client, &mut cmd_rx).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("echoes session error: lane={}: {}", lane_key, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の echoes session: connect → `open_channel("canvas")` → subscribe(echoes pattern) →
/// recv (EchoesEvent) / cmd (submit) の select ループ (run_terminal_session と同型)。
async fn run_echoes_session(
    proxy: &EventLoopProxy<AppEvent>,
    process_path: &str,
    lane_key: &str,
    client: &unison::ProtocolClient,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<EchoesCmd>,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    let channel = client
        .open_channel("canvas")
        .await
        .map_err(|e| format!("open canvas channel (echoes): {}", e))?;
    let topic = format!("process/echoes/data/{}/event", lane_key.replace('/', "~"));
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": process_path, "pattern": topic }),
        )
        .await
        .map_err(|e| format!("echoes subscribe handshake: {}", e))?;
    tracing::info!(
        "echoes session connected: lane={} topic={}",
        lane_key,
        topic
    );

    loop {
        tokio::select! {
            recvd = channel.recv() => {
                let msg = match recvd {
                    Ok(m) => m,
                    Err(_) => return Ok(SubscriptionOutcome::Disconnected),
                };
                if msg.msg_type != MessageType::Event || msg.method != "pane" {
                    continue;
                }
                let payload = match msg.payload_as_value() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // ProcessMessage::EchoesEvent { lane, event } の生 JSON。 event (EchoesEvent) を
                // 抜いて lane_key 付きで JS に渡す (lane は subscription で確定済)。
                if let Some(event) = payload.get("event")
                    && proxy
                        .send_event(AppEvent::EchoesEvent {
                            lane: lane_key.to_string(),
                            event: event.clone(),
                        })
                        .is_err()
                {
                    return Ok(SubscriptionOutcome::AppClosing);
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(EchoesCmd::Submit(prompt)) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "echoes_submit",
                                &serde_json::json!({ "lane": lane_key, "prompt": prompt }),
                            )
                            .await;
                    }
                    Some(EchoesCmd::Respond { request_id, answers, behavior, message }) => {
                        // allow/deny のどちらの形も同 request に載せる（SP 側が behavior で分岐）。
                        let mut req = serde_json::json!({ "lane": lane_key, "request_id": request_id });
                        if let Some(a) = answers {
                            req["answers"] = a;
                        }
                        if let Some(b) = behavior {
                            req["behavior"] = serde_json::Value::String(b);
                        }
                        if let Some(m) = message {
                            req["message"] = serde_json::Value::String(m);
                        }
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>("echoes_respond", &req)
                            .await;
                    }
                    Some(EchoesCmd::Interrupt) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "echoes_interrupt",
                                &serde_json::json!({ "lane": lane_key }),
                            )
                            .await;
                    }
                    Some(EchoesCmd::SetPermissionMode { mode }) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "echoes_set_permission_mode",
                                &serde_json::json!({ "lane": lane_key, "mode": mode }),
                            )
                            .await;
                    }
                    None => return Ok(SubscriptionOutcome::AppClosing),
                }
            }
        }
    }
}

/// F6 (doc 27 §3.4): vp-app → World process-proxy → SP の one-shot ask。
///
/// 旧 SP HTTP 直結 (`reqwest http://127.0.0.1:{sp_port}/api/...`) の置換。 surface は World :32000
/// だけに繋ぐ (§6)。 低頻度 ask 専用 (pp:state debounce save / lane ops) なので 1 回ごとに
/// connect → `open_channel("process-proxy")` → handshake({project_path}) → request(method) → drop。
/// (connection 共有は F1 で畳む。) method は SP `dispatch_process_method` に届き、 戻り値が返る。
async fn world_process_request(
    world_port: u16,
    process_path: &str,
    method: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use unison::ProtocolClient;
    use unison::network::TrustAnchors;
    use unison::network::quic::QuicClient;

    let addr = format!("[::1]:{}", world_port);
    let transport = QuicClient::builder()
        .trust_anchors(TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client build: {}", e))?;
    let client = ProtocolClient::new(transport);
    client
        .connect(&addr)
        .await
        .map_err(|e| format!("connect {}: {}", addr, e))?;
    let channel = client
        .open_channel("process-proxy")
        .await
        .map_err(|e| format!("open process-proxy: {}", e))?;
    // handshake: project_path → World が path_key 正規化 → 当該 SP control へ routing。
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "project_path": process_path }),
        )
        .await
        .map_err(|e| format!("process-proxy handshake: {}", e))?;
    // ask: method を World が SP dispatch_process_method へ forward し応答を relay。
    let resp = channel
        .request::<serde_json::Value, serde_json::Value>(method, &payload)
        .await
        .map_err(|e| format!("process-proxy {}: {}", method, e))?;
    // SP は dispatch の Err を `{"error": ...}` の**正常応答**として返す（discovery.rs の
    // World uplink/control）。transport 成功 = 処理成功ではないので、ここで Err に戻す。
    // これが無いと呼び手は全員「ok」と読み、未実装 method を旧 binary の SP に投げた時などに
    // 「成功ログが出るのに何も起きない」silent success になる。
    if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
        return Err(format!("process-proxy {}: {}", method, err));
    }
    Ok(resp)
}

/// Bastet 🧲 device event 購読: daemon (32000) の "world-device" channel を購読して
/// `AppEvent::DeviceEvent` を emit する。 daemon に 1 本のみ (canvas/lanes は per-SP だが
/// device は World scope = singleton)。 F1b で共有 connection 上の stream に集約。
fn spawn_device_subscription(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedWorldConn,
) {
    rt_handle.spawn(device_subscription_loop(proxy, conn));
}

/// "world-device" channel の購読 → 再購読を司る long-lived ループ (F1b: 共有 connection 上の stream)。
///
/// device channel は **optional** (daemon が feature midi 無効 / Bastet 不在なら未登録)。 connection
/// 自体は共有 manager が維持するので、 「接続済なのに open_channel が連続失敗」= channel 未提供と
/// 判断して graceful give-up する (= device 機能なしで app は動く)。 connection-down (Disconnected)
/// は失敗カウントに含めない (channel は在った)。
async fn device_subscription_loop(proxy: EventLoopProxy<AppEvent>, mut conn: SharedWorldConn) {
    const MAX_FAILURES: u32 = 10;
    let mut failures: u32 = 0;

    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_device_session(&proxy, &client).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {
                // channel は在った (= 接続できた)。 失敗カウントを reset し次 client を待つ。
                failures = 0;
            }
            Err(e) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    // 接続済なのに open_channel が連続失敗 = daemon が world-device を出さない
                    // (feature midi 無効 / Bastet 不在) → graceful degrade。
                    tracing::warn!(
                        "world-device subscription giving up (no midi / Bastet absent): {}",
                        e
                    );
                    return;
                }
                let delay_ms = std::cmp::min(500u64 << (failures - 1), 16_000);
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// 1 回の "world-device" channel 接続セッション: connect → `open_channel("world-device")` →
/// recv ループ。 world-device は接続即購読 (canvas 方式)。 各 device event は daemon が
/// `send_event("event", <DeviceEvent JSON>)` で push する。
async fn run_device_session(
    proxy: &EventLoopProxy<AppEvent>,
    client: &unison::ProtocolClient,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // F1b: 共有 connection 上に "world-device" stream を開く (旧: 専用 connect)。
    let channel = client
        .open_channel("world-device")
        .await
        .map_err(|e| format!("open world-device channel: {}", e))?;
    tracing::info!("world-device subscription connected");

    loop {
        let msg = match channel.recv().await {
            Ok(m) => m,
            Err(_) => return Ok(SubscriptionOutcome::Disconnected),
        };
        if msg.msg_type != MessageType::Event || msg.method != "event" {
            continue;
        }
        let payload = match msg.payload_as_value() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("world-device payload parse failed: {}", e);
                continue;
            }
        };
        if proxy.send_event(AppEvent::DeviceEvent { payload }).is_err() {
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
    /// Lane address は通常 ASCII safe (`<project>/conductor`) だが、 一貫性と future-proof のため統一。
    fn js_str(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
    }

    /// `window.ensureLane(address)` を呼ぶ — 既存ならば no-op (idempotent)。
    ///
    /// terminal S4: SP port は不要になった (xterm の transport は World "canvas" channel +
    /// per-lane terminal session、 旧 `/ws/terminal?port=` 直結を撤去)。 JS は xterm instance を
    /// 作るだけで socket は持たない。 出力/入力は Rust の terminal session が IPC で橋渡しする。
    pub fn ensure_lane(main_view: &WebView, address: &str) {
        let script = format!("window.ensureLane({})", js_str(address));
        if let Err(e) = main_view.evaluate_script(&script) {
            tracing::warn!("ensureLane script failed (addr={}): {}", address, e);
        }
    }

    /// `window.showLane(address, isChat)` を呼ぶ — active な 1 Lane を表示。 None なら empty placeholder。
    ///
    /// `is_chat` = Act II (console_mode="chat")。 chat lane は xterm を持たない (ChatView が内容) ため、
    /// これを渡さないと JS 側が「xterm 無し = 内容無し」と誤判定して placeholder を被せる。
    pub fn show_lane(main_view: &WebView, address: Option<&str>, is_chat: bool) {
        let script = match address {
            Some(a) => format!("window.showLane({}, {})", js_str(a), is_chat),
            None => "window.showLane(null, false)".into(),
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

    /// `window.vpBastet.renderDevices(devices)` を呼ぶ — Bastet pane に device 一覧を render。
    /// Phase 2: world-device bridge の出口 (= AppEvent::DeviceEvent handler から呼ぶ)。
    pub fn render_bastet_devices(main_view: &WebView, devices: &[crate::pane::DeviceSnapshot]) {
        let json = serde_json::to_string(devices).unwrap_or_else(|_| "[]".into());
        let script = format!("window.vpBastet && window.vpBastet.renderDevices({json})");
        if let Err(e) = main_view.evaluate_script(&script) {
            tracing::warn!("renderBastetDevices script failed: {}", e);
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
fn spawn_sp_start(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    project_name: String,
    project_path: String,
) {
    rt_handle.spawn(async move {
        let client = TheWorldClient::default();
        match client.start_process(&project_name).await {
            Ok(()) => {
                tracing::info!(
                    "SP auto-spawn 要求成功: project={} path={}",
                    project_name,
                    project_path
                );
                // TheWorld の polling が新 SP を pick up すると、 既存の
                // spawn_processes_fetch / spawn_activity_poller が ProjectsLoaded を再送、
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
}

/// VP-95: Activity widget の定期更新。
///
/// 5 秒間隔で `/api/health` + `/api/world/projects` + `/api/world/processes` を
/// fetch し、`AppEvent::ActivityUpdate` として main thread に push する。
/// daemon 未起動時は world_online=false で穏やかに通る。
///
/// VP-100 follow-up (B1 / MB1 / PH#7): daemon が **後発で online 復帰** した時、
/// `world_online: false → true` の遷移を検知して `/api/world/projects` を
/// 再 fetch し `AppEvent::ProjectsLoaded` を再送する。これにより sidebar
/// projects accordion が永遠に空のまま、という UX バグを防ぐ。
/// 起動初回 (`prev_online == None`) では `spawn_processes_fetch` 側が担当するので
/// 二重 fetch を避けるため transition 検知をスキップする。
fn spawn_activity_poller(rt_handle: &tokio::runtime::Handle, proxy: EventLoopProxy<AppEvent>) {
    rt_handle.spawn(async move {
        let client = TheWorldClient::default();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut prev_online: Option<bool> = None;
        let mut prev_running: Option<usize> = None;
        loop {
            tick.tick().await;
            let snap = collect_activity(&client).await;
            let became_online = matches!(prev_online, Some(false)) && snap.world_online;
            let running_changed = prev_running.is_some_and(|p| p != snap.running_process_count);
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
            // どちらも port join 経由で ProjectsLoaded 再送 → sidebar state badge 更新
            if (became_online || running_changed)
                && snap.world_online
                && let Ok(projects) = fetch_projects_with_ports(&client).await
            {
                // polling tick で再 fetch → ProjectsLoaded を送るが、 log は omit
                // (= loop で noise)。 失敗時のみ warn にして残す。
                if proxy
                    .send_event(AppEvent::ProjectsLoaded(projects))
                    .is_err()
                {
                    break;
                }
            }
        }
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
fn spawn_session_title_poller(rt_handle: &tokio::runtime::Handle, proxy: EventLoopProxy<AppEvent>) {
    rt_handle.spawn(async move {
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
}

/// VP-147 PR-P2-3: 5s 間隔で `AppEvent::ResolveLaneInboxes` を fire する background poller。
///
/// `spawn_session_title_poller` と同 pattern (tokio current_thread runtime + interval tick)。
/// main thread が `sidebar_state.lanes_by_project` を walk して各 lane の MessageState を
/// build し、 sidebar に push back する trigger となる。 Phase 2 PR-P2-3 では default 値の
/// placeholder を populate し、 sidebar UI で `.vp-message-icon` 表示の signal として動く。
/// 後続 PR で backend peek API + Whitesnake query を実装して actual 値を populate する。
fn spawn_lane_inbox_poller(rt_handle: &tokio::runtime::Handle, proxy: EventLoopProxy<AppEvent>) {
    rt_handle.spawn(async move {
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
        // hub federation 接続状態（World 横の Hub インジケータ用）+ available worlds リスト。
        snap.hub = h.hub;
        snap.hub_worlds = h.hub_worlds;
        // in-app update: daemon の定期チェック結果（「更新する」ボタンの表示 gate + label）。
        snap.update_available = h.update_available;
        snap.latest_version = h.latest_version;
        // L1 lifecycle: SP presence map（project 行の ●◐○ dot 用、path → presence）。
        snap.presence = h
            .processes
            .into_iter()
            .map(|p| (p.path, p.presence))
            .collect();
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
///   1. `active_stand` Some → kind = "paisley_park" / "gold_experience" / "bastet"
///   2. `active_lane_address` Some → kind = "terminal"、 pane_id = Lane address
///   3. 両方 None → kind=None で empty placeholder
///
/// Lane address ごとの terminal 接続は per-Lane xterm.js (Phase 2.5) が JS-side で管理。
/// 指定 lane address が Act II (console_mode="chat") かを `lanes_by_project` から引く。
///
/// 未知 address (LanesLoaded 未着 等) は false (= Act I 扱い) に倒す。 chat lane は
/// engine-less (pid=None) が正常形なので、 pid では判定できない — mode が唯一の真実源。
fn lane_is_chat(state: &SidebarState, address: &str) -> bool {
    state
        .lanes_by_project
        .values()
        .flatten()
        .find(|l| l.address.key() == address)
        .map(|l| l.console_mode == "chat")
        .unwrap_or(false)
}

/// Act II: active になった chat lane を echoes topic に attach する（`terminal_sessions` の対）。
///
/// 購読 0→1 が World の demand hook を撃ち、SP が **transcript replay**（過去会話）を返す。
/// これが無いと echoes topic は非 retained なので「submit するまで ChatView が空」になる
/// （app 再起動で会話が消えたように見える）。 idempotent — 既に session があれば no-op。
///
/// tui lane では何もしない（Act I の履歴は PtySlot の terminal replay が担う）。
fn ensure_echoes_attach(
    address: &str,
    sidebar_state: &SidebarState,
    echoes_sessions: &mut std::collections::HashMap<String, LaneEchoes>,
    rt_handle: &tokio::runtime::Handle,
    proxy: &EventLoopProxy<AppEvent>,
    world_conn: &SharedWorldConn,
) {
    if !lane_is_chat(sidebar_state, address) || echoes_sessions.contains_key(address) {
        return;
    }
    let Some(process_path) = resolve_project_path_for_lane(sidebar_state, address) else {
        return; // project 未解決 (LanesLoaded 未着) — 後続の LanesLoaded で再評価される
    };
    tracing::info!("echoes attach (chat lane): {}", address);
    let session = spawn_echoes_session(
        rt_handle,
        proxy.clone(),
        world_conn.clone(),
        process_path,
        address.to_string(),
    );
    echoes_sessions.insert(address.to_string(), session);
}

fn push_active_view(main_view: &WebView, state: &SidebarState) {
    let info = if let Some(stand) = state.active_stand.as_ref() {
        ActivePaneInfo {
            kind: Some(stand.kind.as_str()),
            pane_id: None,
            preview_url: None,
            chat: false,
            // 非 lane pane (Stand) は Echoes ヘッダの lane 情報を持たない。
            cwd: None,
            branch: None,
            lane_name: None,
        }
    } else if let Some(addr) = state.active_lane_address.as_deref() {
        // Echoes 共通ヘッダ用: active lane の LaneInfo から cwd / branch を引く。cwd は
        // address (pane_id) から導出できない唯一の lane 情報なので、setActivePane に相乗り
        // させて運ぶ (新しい配信チャネルは増やさない)。branch は performer のみ (安価に取れる時)。
        let lane = state
            .lanes_by_project
            .values()
            .flatten()
            .find(|l| l.address.key() == addr);
        ActivePaneInfo {
            kind: Some("terminal"),
            pane_id: Some(addr),
            preview_url: None,
            // doc 33: chat lane は xterm を持たない (ChatView が内容)。 これを JS に伝えないと
            // showLane が「xterm 無し = 内容無し」と誤判定し placeholder が ChatView を覆う。
            chat: lane_is_chat(state, addr),
            cwd: lane.map(|l| l.cwd.as_str()).filter(|c| !c.is_empty()),
            branch: lane
                .and_then(|l| l.performer_status.as_ref())
                .and_then(|p| p.branch.as_deref()),
            // 現状 LaneInfo.name は常に None（JS は addr 短縮名に fallback）だが、
            // 将来 populate された時にヘッダが取り残されないよう cwd/branch と同経路で供給。
            lane_name: lane.and_then(|l| l.name.as_deref()),
        }
    } else {
        ActivePaneInfo {
            kind: None,
            pane_id: None,
            preview_url: None,
            chat: false,
            cwd: None,
            branch: None,
            lane_name: None,
        }
    };
    let script = main_area::build_set_active_pane_script(&info);
    if let Err(e) = main_view.evaluate_script(&script) {
        tracing::warn!("main setActivePane 失敗: {}", e);
    }
}

/// Lane address (Display 形 `"<project>/conductor"` 等) から所属 project path を逆引きする。
///
/// `lanes_by_project` (= project_path → LaneInfo list) を走査し、 `address.key()` が一致する
/// lane を持つ project の path を返す。 `lane:select` 経路 (= JS から path を受け取る) の鏡像で、
/// focus 経路は address しか持たないためここで path を解決する。 一致なしは None。
fn resolve_project_path_for_lane(state: &SidebarState, address: &str) -> Option<String> {
    state
        .lanes_by_project
        .iter()
        .find(|(_path, lanes)| lanes.iter().any(|l| l.address.key() == address))
        .map(|(path, _)| path.clone())
}

/// Active Lane を切替える — 全副作用を 1 箇所に集約（Simplicity 原則）。
///
/// sidebar click / switch_lane (QUIC) / auto-select の 3 入口すべてがこの関数を呼ぶ。
/// 副作用:
///   1. `sidebar_state.active_lane_address` + `active_stand` (排他 clear)
///   2. `session_state` 永続化
///   3. notification / awaiting_input reset
///   4. sidebar UI push (`renderSidebarState`)
///   5. main area push (`setActivePane` → `showLane`)
///   6. dead lane respawn
#[allow(clippy::too_many_arguments)]
fn activate_lane(
    address: &str,
    sidebar_state: &mut SidebarState,
    session_state: &mut crate::session_state::SessionState,
    webview: &wry::WebView,
    lane_respawn_triggered: &mut std::collections::HashSet<String>,
    rt_handle: &tokio::runtime::Handle,
    respawn_proxy: &EventLoopProxy<AppEvent>,
) {
    // 1. State
    sidebar_state.active_lane_address = Some(address.to_string());
    if sidebar_state.active_stand.is_some() {
        sidebar_state.active_stand = None;
    }

    // 2. Session persistence
    session_state.active_lane_address = Some(address.to_string());
    session_state.save();

    // 3. Notification reset (同 lane click 連打でも badge を消す)
    sidebar_state.unread_notifications.remove(address);
    sidebar_state.awaiting_input.remove(address);
    // canvas 着信 badge (D) も active 化で消す (unread_notifications と同 lifecycle)。
    sidebar_state.canvas_unread.remove(address);

    // 4-6. UI push + dead lane respawn
    // BUG#3: 旧実装は push_active_view / respawn を `view_changed` (active_lane_address が
    // 変わった時だけ) に gate していたが、 address 一致だが main area 未表示 / pump 未成立
    // (restart 直後の楽観反映 × canonical desync) の状態で同一 lane を再 click すると no-op に
    // なり「切り替えられない」。 setActivePane → showLane は冪等 (setWantedLane 撤去済で WS 付替
    // churn 無し)、 respawn は triggered set で dedup 済なので、 view_changed に依らず毎回実行して
    // desync を確定的に解消する。 activate_lane の 3 caller (初回 auto-select は
    // active_lane_address.is_none() gate で 1 回 / switch_lane / sidebar click) はいずれも
    // genuine activation で高頻度発火しないため、 毎回 push しても focus 奪取 flood は起きない。
    push_sidebar_state(webview, sidebar_state);
    push_active_view(webview, sidebar_state);
    // doc 33 C2: この lane の console_mode を WebView に同期する（xterm⇄chat 表示を確定 +
    // Act toggle の宛先 consoleActiveLane を初期化）。lane の mode は LaneInfo から引く。
    let mode = sidebar_state
        .lanes_by_project
        .values()
        .flatten()
        .find(|l| l.address.key() == address)
        .map(|l| l.console_mode.clone())
        .unwrap_or_else(|| {
            // snapshot 欠落時は tui に落ちる = chat lane が Act I 表示で開く。起動レースでしか
            // 起きないはずなので、黙って既定値を使わず観測できるようにしておく。
            tracing::warn!("activate_lane: lane が snapshot に不在、console_mode を tui と仮定 (lane={address})");
            "tui".to_string()
        });
    let script = format!(
        "window.vpConsole && window.vpConsole.setMode({}, {})",
        serde_json::to_string(address).unwrap_or_else(|_| "\"\"".into()),
        serde_json::to_string(&mode).unwrap_or_else(|_| "\"tui\"".into()),
    );
    let _ = webview.evaluate_script(&script);
    maybe_respawn_dead_lane(
        address,
        sidebar_state,
        lane_respawn_triggered,
        rt_handle,
        respawn_proxy,
    );
}

/// オンデマンド respawn: active にしようとする lane が Dead (pid:null) なら SP に restart_lane を
/// 発火して蘇らせる。 lane (conductor / performer) の Echoes プロセスが死ぬと SP の lifecycle monitor は
/// Dead を検知するだけで auto-respawn しない (server.rs の設計判断) ため、 user が lane を
/// 開いた時点でオンデマンドに復活させる。 これが無いと「一度死んだ lane は手動 restart するまで
/// Echoes が出ない」状態になる (= 全 project で console 非表示の真因)。
///
/// dedup: `triggered` set で同一 lane の連打を防ぐ (LanesLoaded は loop event で頻発するため必須)。
/// 解除タイミングは 2 つ: (a) lane が Running に戻った時 caller が `triggered.remove` する、
/// (b) restart_lane が失敗した時 `AppEvent::LaneRespawnFailed` 経由で caller が `triggered.remove`
/// する (= 失敗が永続 suppression にならないようにする、 Moody Blues Issue #1)。
fn maybe_respawn_dead_lane(
    addr: &str,
    state: &SidebarState,
    triggered: &mut std::collections::HashSet<String>,
    rt_handle: &tokio::runtime::Handle,
    proxy: &EventLoopProxy<AppEvent>,
) {
    // addr の lane を lanes_by_project から探し、 所属 project path と pid を取得。
    let entry = state.lanes_by_project.iter().find_map(|(path, lanes)| {
        lanes
            .iter()
            .find(|l| l.address.key() == addr)
            .map(|l| (path.clone(), l.pid, l.console_mode.clone()))
    });
    let Some((project_path, pid, console_mode)) = entry else {
        return; // lane 未知 (まだ LanesLoaded 来てない等) — 後続の LanesLoaded で再評価される
    };
    if pid.is_some() {
        return; // Running、 respawn 不要
    }
    // doc 33 §3: chat mode の lane は engine-less (pid=None) が正常形。
    // respawn 対象は「mode=tui かつ pid=None」のみ（chat lane を殺しに行かない — #683 再演防止）。
    if console_mode == "chat" {
        return;
    }
    // dedup: 既に respawn 進行中なら skip
    if !triggered.insert(addr.to_string()) {
        return;
    }
    // F6③: 旧 TheWorldClient.restart_lane (SP 直結 reqwest) を World process-proxy ask
    // (lane_restart) に移管。 SP port 解決は不要 (World :32000 固定 + project_path handshake)、
    // 旧「port 未解決 skip」分岐も消滅。 失敗時の trigger 解除は LaneRespawnFailed 経路に一本化。
    let addr_owned = addr.to_string();
    let proxy = proxy.clone();
    tracing::info!("auto-respawn dead lane (on-demand): addr={}", addr_owned);
    rt_handle.spawn(async move {
        // auto-respawn は Dead lane の復活なので会話を継ぐ (fresh=false)。
        let payload = serde_json::json!({ "address": &addr_owned, "fresh": false });
        match world_process_request(
            crate::client::default_world_port(),
            &project_path,
            "lane_restart",
            payload,
        )
        .await
        {
            Ok(_) => {
                // 成功時は LanesLoaded で Running 検出時に triggered から解除される。
                tracing::info!("auto-respawn lane_restart ok: {}", addr_owned);
            }
            Err(e) => {
                tracing::warn!("auto-respawn lane_restart failed: {}: {}", addr_owned, e);
                // 失敗を event loop に通知して triggered を解除する (永続 suppression 回避)。
                // これが無いと SP クラッシュ等で全 retry 失敗した lane は vp-app 再起動まで
                // auto-respawn 対象外になってしまう (Moody Blues Issue #1)。
                let _ = proxy.send_event(AppEvent::LaneRespawnFailed {
                    address: addr_owned,
                });
            }
        }
    });
}

/// window の現在の geometry + 表示モードを SessionState に write-through する (doc 30 §3.4a / §6.1)。
///
/// - **通常ウィンドウ**: 位置・サイズ・monitor・`display_mode=Windowed` を `set_window_geometry`。
/// - **全画面**: `inner_size()` は fullscreen frame を返し windowed 座標を潰すため、 `set_display_mode`
///   で mode + monitor のみ更新し、 直前の windowed 座標を保持する (全画面解除で元の窓サイズに戻せる)。
///
/// `save()` は呼ばない (caller が open flag 等とまとめて save する)。 outer_position 取得失敗時は
/// windowed 座標を更新できないので geometry を触らず返る (mode 更新は全画面時のみで別経路)。
fn persist_window_geometry(session_state: &mut SessionState, window: &tao::window::Window) {
    let monitor_name = window.current_monitor().and_then(|m| m.name());
    if window.fullscreen().is_some() {
        session_state.set_display_mode(crate::session_state::DisplayMode::Fullscreen, monitor_name);
        return;
    }
    let scale = window.scale_factor();
    match window.outer_position() {
        Ok(pos) => {
            let inner = window.inner_size().to_logical::<f64>(scale);
            let logical_pos = pos.to_logical::<f64>(scale);
            session_state.set_window_geometry(crate::session_state::WindowGeometry {
                width: inner.width,
                height: inner.height,
                x: logical_pos.x,
                y: logical_pos.y,
                monitor: monitor_name,
                display_mode: crate::session_state::DisplayMode::Windowed,
            });
        }
        Err(e) => {
            tracing::warn!("outer_position() 取得失敗 (geometry save skip): {}", e);
        }
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

/// lane を「入力待ち（要注意）」として記録し、sidebar の unread count / 黄 dot を更新する。
///
/// active lane（今まさに見ている lane）は即読扱いで skip する（見ている lane に dot を出さない）。
/// これは通知の**単一 sink** で、2 つのソースがここに合流する。Act I は OSC 99/9/777
/// notification（`AppEvent::OscNotification`、xterm が parse）。Act II は
/// `EchoesEvent::turn_completed`（headless stream-json は Notification hook を発火しないため、
/// stream `result` 由来の turn_completed が「Claude が返し終えた＝入力待ち」の唯一のシグナル。
/// memory echoes-act2-notification-signal 参照）。
/// `source` はログ用ラベル（`"osc:notification"` / `"act2:turn_completed"` 等）。
fn mark_lane_awaiting_input(
    lane: &str,
    source: &str,
    sidebar_state: &mut SidebarState,
    webview: &WebView,
) {
    if sidebar_state.active_lane_address.as_deref() == Some(lane) {
        tracing::debug!("{source} skip (active lane): lane={lane}");
        return;
    }
    let count = sidebar_state
        .unread_notifications
        .entry(lane.to_string())
        .or_insert(0);
    *count += 1;
    // 「入力待ち」 = 行右端に黄 dot。 active 切替で reset される。
    sidebar_state.awaiting_input.insert(lane.to_string(), true);
    tracing::info!("{source} lane={lane} unread={}", *count);
    push_sidebar_state(webview, sidebar_state);
}

/// lane に Canvas (PP) show が着信したことを sidebar の canvas_unread に計上する。
///
/// `mark_lane_awaiting_input` (HITL/OSC = 黄 dot) とは**別 sink**。Canvas 着信は sidebar 行に
/// Canvas 専用 icon (Phosphor easel) として出し、「用事(黄 dot)」と「絵が届いた(easel)」の
/// 語彙を分離する (bug: canvas 可観測性 D、show 偽 success の viewer 文脈対策)。
/// active lane（今見ている lane）宛の show は panel 側 (pp-overlay auto-open) で解決するので、
/// ここでは badge を出さない（呼び出し側で active 判定済だが二重防御で skip）。
fn mark_lane_canvas_unread(lane: &str, sidebar_state: &mut SidebarState, webview: &WebView) {
    if sidebar_state.active_lane_address.as_deref() == Some(lane) {
        return;
    }
    let count = sidebar_state
        .canvas_unread
        .entry(lane.to_string())
        .or_insert(0);
    *count += 1;
    tracing::info!("canvas:show lane={lane} canvas_unread={}", *count);
    push_sidebar_state(webview, sidebar_state);
}

/// sidebar IPC を解釈した結果
#[derive(Debug, Default)]
struct SidebarIpcOutcome {
    /// SidebarState が変化したか (true なら push_sidebar_state を呼ぶ)
    changed: bool,
    /// active Lane/Stand が変わったか (true なら push_active_view を呼ぶ)。
    /// Lane 選択の場合は `activate_lane` を使うこと（こちらは Stand 選択・Lane 削除用）。
    active_changed: bool,
    /// Lane activation 要求 — caller が `activate_lane()` を呼ぶ。
    /// `active_changed` とは排他（こちらが Some なら active_changed は不要）。
    activate_lane: Option<String>,
    /// SP auto-spawn が必要な project (= 「Current」 になった dead な project)。
    /// `(name, path)` を返し、 caller が `spawn_sp_start` を呼ぶ。
    /// dedup は caller の `sp_spawn_triggered: HashSet<String>` (path key) で行う。
    sp_spawn_request: Option<(String, String)>,
    /// Phase 3-A: Performer Lane 作成要求 `(project_path, name, branch, stand)`。
    /// doc 24 §10 B-create: caller が daemon (:32000) の `create_performer_lane`
    /// (`POST /api/world/lanes`) を呼ぶ (SP port 解決は不要)。
    /// `stand` は doc 11 PR-C で追加 (None なら daemon-side default)。
    add_performer_request: Option<(String, String, Option<String>, Option<String>)>,
    /// doc 11 PR-C / F6④: 利用可能 Stand 一覧 fetch 要求 `(project_path)`。
    /// caller が World process-proxy ask (`stands_list`) を呼ぶ → `AppEvent::StandsResult` で push back。
    list_stands_request: Option<String>,
    /// Phase 4-A: Performer Lane 削除要求 `(project_path, address)`。
    /// caller が SP port を解決して `client.delete_lane` を呼ぶ。
    delete_lane_request: Option<(String, String)>,
    /// Lane Conductor Stand restart 要求 `(project_path, address, fresh)`。
    /// caller が SP port を解決して `client.restart_lane` を呼ぶ。
    /// fresh=true は "New Conductor Session" (resume/continue 回避の fresh 起動)。
    restart_lane_request: Option<(String, String, bool)>,
    /// Phase 5-C: Process restart 要求 `(project_name)`。
    /// caller が TheWorld の `/api/world/processes/{name}/restart` を呼ぶ。
    restart_process_request: Option<String>,
    /// Process stop 要求 `(project_name)`。
    /// caller が TheWorld の `/api/world/processes/{name}/stop` を呼ぶ。
    /// project は registered のまま (停止しても sidebar リストに残り ▶ 起動が出る)。
    stop_process_request: Option<String>,
    /// Project delete 要求 `(project_name, project_path)`。
    /// caller が SP を stop してから `/api/world/projects/remove` を呼ぶ。
    /// `project_name` は stop 用、 `project_path` は remove 用 (registry key)。
    delete_project_request: Option<(String, String)>,
    /// Phase 1 (doc 24): project 並び替えを daemon に永続化する要求 (path の順序列)。
    /// caller が `client.reorder_projects` を呼び、成功後に re-fetch → `ProjectsLoaded` で
    /// canonical 順を反映する。これで sidebar の D&D が daemon `project_order` に一本化される。
    reorder_request: Option<Vec<String>>,
    /// Phase 5-D fix: SP auto-spawn dedup HashSet から path を release する要求。
    /// 「accordion を閉じる」 = 「ユーザが retry を望んでいる」 と解釈、 失敗ループの
    /// dedup deadlock を抜けられるようにする。 caller は `sp_spawn_triggered.remove(path)` を呼ぶ。
    sp_spawn_release: Option<String>,
    /// Sidebar File Explorer: `files:list` 要求 `(project_path, address)`。
    /// caller (event loop) で lane cwd を解決して `file_explorer::list_entries` を
    /// blocking thread で実行 → `AppEvent::FilesListResult` で push back。
    files_list_request: Option<(String, String)>,
    /// Sidebar File Explorer: `files:open` 要求 `(project_path, address, rel_path)`。
    /// caller (event loop) で lane cwd を解決して `file_explorer::open_file` を
    /// blocking thread で実行 → `AppEvent::FilesOpenResult` で push back。
    files_open_request: Option<(String, String, String)>,
    /// Model Q: active lane を daemon canonical に永続する要求 `(project_path, lane_address)`。
    /// caller が `client.set_active_lane` を fire-and-forget で呼ぶ (optimistic local は適用済)。
    set_active_lane_request: Option<(String, String)>,
    /// Wire inbox (doc 34 §4 V1): `wire:fetch` 要求 `(address)`。 caller が World "wire" channel
    /// へ read-only request (wire/history + wire/unread-count) を投げ、
    /// `AppEvent::WireHistoryResult` で push back する (cursor 不触り)。
    wire_fetch_request: Option<String>,
    /// Wire inbox: `wire:ack` 要求 `(address, message_id)`。 lane の agent として ack した後、
    /// 再 fetch して `AppEvent::WireHistoryResult` で最新状態を push back する。
    wire_ack_request: Option<(String, String)>,
    /// in-app update: sidebar footer の「更新する」ボタン click 要求 `(latest_version)`。
    /// caller (event loop) が `update_flow::spawn_update_flow` を呼び、native 確認ダイアログ →
    /// self-update → `vp daemon restart` → GUI relaunch を専用スレッドで実行する。
    update_apply_request: Option<String>,
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
            // Phase 4-A: Performer Lane 削除要求。 caller (event loop) で SP port を解決して
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
                out.restart_lane_request = Some((m.path, m.address, m.fresh.unwrap_or(false)));
            }
        }
        IpcEnvelope::LaneAddPerformer(m) => {
            // Phase 3-A: sidebar から Performer Lane 作成要求。 doc 24 §10 B-create:
            // caller (event loop) が daemon (:32000) の create_performer_lane を呼ぶ。
            // doc 11 PR-C: branch / stand は optional。 空文字は None に畳んで
            // daemon-side default にフォールバックさせる。
            let branch = m.branch.filter(|s| !s.is_empty());
            let stand = m.stand.filter(|s| !s.is_empty());
            if !m.path.is_empty() && !m.name.is_empty() {
                out.add_performer_request = Some((m.path, m.name, branch, stand));
            }
        }
        IpcEnvelope::StandsFetch(m) => {
            // doc 11 PR-C: sidebar の + Add Performer form 開閉時に利用可能 Stand 一覧を取得。
            // caller (event loop) で World process-proxy ask (`stands_list`) → window.handleStandsResult で push back。
            if !m.path.is_empty() {
                out.list_stands_request = Some(m.path);
            }
        }
        IpcEnvelope::StandSelect(m) => {
            // Phase 5-A: Project-scope Stand row click → main area に対応 pane を表示
            // (Lane と mutually exclusive、 active_lane_address は preemptively clear)
            // Bastet 🧲 は World-scope Stand (device = daemon 共通) なので path="" で来る。
            // World-scope stand は path 空を許可、 それ以外 (Project-scope) は path 必須。
            if m.kind.is_empty() || (m.path.is_empty() && m.kind != "bastet") {
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
            if m.address.is_empty() {
                tracing::warn!("lane:select with empty address: {}", msg);
                return out;
            }
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
            tracing::info!("lane:select {} address={}", m.path, m.address);
            out.activate_lane = Some(m.address.clone());
            // Model Q: active lane を daemon canonical に永続 (optimistic local は activate_lane で適用)。
            out.set_active_lane_request = Some((m.path.clone(), m.address.clone()));
        }
        IpcEnvelope::ProcessReorder(m) => {
            // Currents セクションを drag-and-drop で並び替えた時の通知。
            // payload: `{"t":"process:reorder","order":["/path/a","/path/b",...]}`。
            tracing::info!("process:reorder: {} entries", m.order.len());
            // optimistic 反映: session 保存 + SidebarState（次回 push で JS 側 sort に使う）。
            // changed フラグは立てない (DOM 順は user 操作で既に変わっている、re-push で flash を避ける)。
            session.currents_order = Some(m.order.clone());
            session.save();
            state.currents_order = Some(m.order.clone());
            // Phase 1 (doc 24): daemon の project_order にも永続化する。
            // caller が client.reorder_projects → re-fetch → ProjectsLoaded で canonical を反映し、
            // sidebar / ROTO / CLI vp projects を 1 つの順序源に揃える。
            out.reorder_request = Some(m.order);
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
            // SP を停止する (project は registered のまま sidebar リストに残る)。
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
        IpcEnvelope::FilesList(m) => {
            // Sidebar File Explorer: lane workdir 配下を walk して entries を返す要求。
            // caller (event loop) で SidebarState から cwd を解決して blocking thread で実行する。
            if !m.path.is_empty() && !m.address.is_empty() {
                out.files_list_request = Some((m.path, m.address));
            }
        }
        IpcEnvelope::FilesOpen(m) => {
            // Sidebar File Explorer: 選択されたファイルを Canvas (PP) に表示する要求。
            // rel_path は workdir 相対 (TS 側で list_entries の戻り値そのまま投げる想定)。
            if !m.path.is_empty() && !m.address.is_empty() && !m.rel_path.is_empty() {
                out.files_open_request = Some((m.path, m.address, m.rel_path));
            }
        }
        IpcEnvelope::WireFetch(m) => {
            // Wire inbox (doc 34 §4 V1): 選択 lane の wire 履歴 fetch 要求。
            if !m.address.is_empty() {
                out.wire_fetch_request = Some(m.address);
            }
        }
        IpcEnvelope::WireAck(m) => {
            // Wire inbox: lane の agent としての ack 要求 (ack 後に再 fetch)。
            if !m.address.is_empty() && !m.message_id.is_empty() {
                out.wire_ack_request = Some((m.address, m.message_id));
            }
        }
        IpcEnvelope::UpdateApply(m) => {
            // in-app update: sidebar footer の「更新する」ボタン click。version は
            // ダイアログ文言用の latest version。caller (event loop) が native 確認ダイアログ →
            // self-update → daemon restart → relaunch の破壊的フローを専用スレッドで起動する。
            if !m.version.is_empty() {
                out.update_apply_request = Some(m.version);
            }
        }
    }
    out
}

/// sidebar の lane address key (`P/conductor` / `P/performer/N`) → wire agent address。
///
/// `LaneAddressWire::key()` の逆写像 (delivery_actor の `wire_agent_to_lane_display` と対)。
/// 未知 kind (magic 等) は wire address を持たないので `None`。
fn lane_key_to_wire_agent(address: &str) -> Option<String> {
    let (project, rest) = address.split_once('/')?;
    if project.is_empty() {
        return None;
    }
    match rest.split_once('/') {
        None if rest == "conductor" => Some(format!("agent@{project}")),
        // "<unnamed>" は spawning 中(name 未確定)の placeholder(`LaneAddressWire::key()`)で
        // 実在の wire agent ではない — 偽 address で空 inbox を開かないよう除外する。
        Some(("performer", name)) if !name.is_empty() && name != "<unnamed>" => {
            Some(format!("agent@{project}/{name}"))
        }
        _ => None,
    }
}

/// Wire inbox (doc 34 §4 V1): World "wire" channel に read-only request を投げて
/// `{address, agent, history, unread}` payload を組み立てる (エラーは `{address, error}`)。
///
/// **wire/recv は使わない** — per-agent 単一 cursor を GUI が進めると lane の claude から
/// 未読を横取りするため、 cursor 不触りの wire/history + wire/unread-count のみを叩く。
/// `ack_message_id` が Some なら先に wire/ack を実行してから fetch する (ack → 最新状態の
/// 再描画を 1 往復に畳む)。
async fn wire_fetch_payload(
    mut conn: SharedWorldConn,
    address: String,
    ack_message_id: Option<String>,
) -> serde_json::Value {
    let Some(agent) = lane_key_to_wire_agent(&address) else {
        return serde_json::json!({ "address": address, "error": "wire address を持たない lane" });
    };
    let Some(client) = conn.wait_client().await else {
        return serde_json::json!({ "address": address, "error": "World 未接続" });
    };
    let channel = match client.open_channel("wire").await {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({ "address": address, "error": format!("wire channel: {e}") });
        }
    };
    if let Some(id) = ack_message_id {
        // ack は台帳の意味論どおり「処理済み宣言」。 GUI からの手動 ack は dogfood の
        // オペレーション手段 (needs_user relay 等の規約整合は doc 34 §7 で継続検討)。
        let _ = channel
            .request::<serde_json::Value, serde_json::Value>(
                "wire/ack",
                &serde_json::json!({ "message_id": id, "agent": agent }),
            )
            .await;
    }
    let history = channel
        .request::<serde_json::Value, serde_json::Value>(
            "wire/history",
            &serde_json::json!({ "agent": agent }),
        )
        .await
        .unwrap_or_else(|e| serde_json::json!({ "error": format!("wire/history: {e}") }));
    let unread = channel
        .request::<serde_json::Value, serde_json::Value>(
            "wire/unread-count",
            &serde_json::json!({ "agent": agent }),
        )
        .await
        .unwrap_or_else(|e| serde_json::json!({ "error": format!("wire/unread-count: {e}") }));
    serde_json::json!({ "address": address, "agent": agent, "history": history, "unread": unread })
}

/// SidebarState の `lanes_by_project` から (project_path, address) の組に
/// 対応する Lane の workdir 絶対パスを引く。 見つからなければ `None`。
///
/// File Explorer の `files:list` / `files:open` で使う。 address は
/// `LaneAddressWire::key()` 形式 (= `lane:select` 等で使われている wire 文字列)。
fn lookup_lane_cwd(
    state: &SidebarState,
    project_path: &str,
    address: &str,
) -> Option<std::path::PathBuf> {
    let lanes = state.lanes_by_project.get(project_path)?;
    lanes
        .iter()
        .find(|l| l.address.key() == address)
        .map(|l| std::path::PathBuf::from(&l.cwd))
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
    vp_paths::migrate_legacy_paths();

    // Windows taskbar の identity。 **window を作る前**に設定する必要がある
    // (既存 window の AUMID は後から変えられない)。 非 Windows は no-op。
    crate::icon::set_app_user_model_id();

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // 根治: vp-app 共有 Tokio runtime (multi-thread)。
    //
    // tao の event_loop は macOS main thread を専有し、 closure 内には Tokio
    // runtime context が無いため、 bare `tokio::spawn` を呼ぶと
    // 「no reactor running」 panic で即死する (= 過去事故、 PP 永続化 #456241e 等)。
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

    // F1b (doc 27 §3.4.4): vp-app → World :32000 の全 persistent session を 1 QUIC connection に
    // 集約する共有ハンドル。 manager task が connect/reconnect を一手に所有し、 各 session
    // (device/lanes/canvas/terminal) は `wait_client` で得た共有 client に open_channel する。
    // event loop closure が move capture するので、 closure 内の spawn は `world_conn.clone()` を渡す。
    let world_conn = spawn_world_conn_manager(&rt_handle, crate::client::default_world_port());

    // Bastet 🧲 device event を daemon (world-device channel) から購読する (daemon に 1 本)。
    // canvas/lanes は per-SP だが device は World scope (= daemon singleton) なので起動時 1 回。
    spawn_device_subscription(&rt_handle, event_loop.create_proxy(), world_conn.clone());

    // vp-app instance index 判定 (= multi-window 復元)。 per-instance file load に先立って
    // 必要なので session_state より前に確定する。
    // 新 env `VP_APP_INSTANCE` (= "0", "1", ...) が primary、 旧 `VP_APP_SECONDARY="1"`
    // は backward compat で `VP_APP_INSTANCE="1"` 相当に map。 未設定 / "0" = primary。
    let instance_index: usize = std::env::var("VP_APP_INSTANCE")
        .ok()
        .or_else(|| {
            std::env::var("VP_APP_SECONDARY")
                .ok()
                .filter(|v| v == "1")
                .map(|_| "1".to_string())
        })
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
    // 後段で active_lane_address / projects / currents_order 等の mutate + save にも使う。
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
    // 1. `with_min_inner_size`: SIDEBAR_WIDTH + 余裕ある main 領域を構造的に確保する OS
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
    // 子は `VP_APP_INSTANCE=<idx>` で自分の file を read (旧 `VP_APP_SECONDARY=1` も
    // backward compat で渡す)。 spawn 失敗は warn して continue (= primary 起動は阻害しない)。
    if is_primary {
        let to_spawn = SessionState::open_secondary_indices();
        if !to_spawn.is_empty() {
            match std::env::current_exe() {
                Ok(exe) => {
                    for idx in to_spawn {
                        match std::process::Command::new(&exe)
                            .env("VP_APP_INSTANCE", idx.to_string())
                            .env("VP_APP_SECONDARY", "1")
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
    let world_url = std::env::var("VP_WORLD_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", crate::client::default_world_port()));
    if let Err(e) = crate::daemon_launcher::ensure_daemon_ready(&world_url) {
        tracing::warn!(
            "TheWorld auto-launch 失敗 (continue with offline state): {}",
            e
        );
    }

    // TheWorld から project list を非同期 fetch (起動初回)
    spawn_processes_fetch(&rt_handle, event_loop.create_proxy());
    // VP-95: Activity widget の定期更新 (5s 間隔)
    spawn_activity_poller(&rt_handle, event_loop.create_proxy());
    // VP-143: cc session display name (custom-title) の 5s 周期 resolve
    spawn_session_title_poller(&rt_handle, event_loop.create_proxy());
    // VP-147 PR-P2-3: per-Lane mailbox inbox 状況の 5s 周期 resolve (sidebar message icon 用 signal)
    spawn_lane_inbox_poller(&rt_handle, event_loop.create_proxy());

    // WebView 統合 (step 3a): sidebar + main を 1 WebView (1 DOM, CSS flex) に統合。
    // sidebar.bundle.js は MAIN_AREA_HTML 内に inline 済 (#sidebar-root に mount)。
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
            crate::web_assets::serve(id, request, MAIN_VIEW_ASSETS)
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
            // project: / lane: 系) → SidebarIpc。 terminal の fall-through に頼らない。
            let body = req.body();
            if is_main_ipc_tag(body) {
                terminal::handle_ipc_message(body, &ipc_proxy);
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

    // Phase 2.x-d: 旧 single-PTY 経路 (`xterm_ready` / `pending` / `PENDING_MAX`) は撤去。
    // per-Lane instance + browser-native WebSocket では各 Lane の xterm.js が独立に
    // WS から bytes を受けるので、 Rust 側で buffer / flush 同期する必要が無い。
    // VP-95: sidebar 全体 state (projects + widget + activity)
    let mut sidebar_state = SidebarState::default();
    // session_state は WindowBuilder 上で既に load 済 (= window geometry を先に必要)。
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
    // オンデマンド respawn: active にする lane が Dead (pid:null) の時に restart_lane を 1 回だけ
    // 発火するための guard。 lane address をキーにする。 lane が Running に戻ったら (LanesLoaded で
    // pid あり検出時) entry を解除し、 再度 Dead 化した時に再 respawn できるようにする。
    let mut lane_respawn_triggered: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // maybe_respawn_dead_lane の async restart_lane が失敗した時に event loop へ
    // 通知を返し lane_respawn_triggered を解除するための proxy (永続 suppression 回避)。
    let respawn_proxy = event_loop.create_proxy();
    // wiremsg Stage 1: per-SP の "lanes" Unison 購読を 1 本だけ張るための guard。
    // path をキーにする。F1b: 購読は共有 connection に追従して give-up しないので、 一度
    // spawn したら app 終了まで張りっぱなし (= guard から除去されない)。
    let mut lanes_sub_active: std::collections::HashSet<String> = std::collections::HashSet::new();
    // wiremsg Stage 2: per-SP の "canvas" Unison 購読 guard (lanes_sub_active と同型)。
    let mut canvas_sub_active: std::collections::HashSet<String> = std::collections::HashSet::new();
    // terminal S4: per-lane terminal session registry (lane key → LaneTerminal)。
    // LanesLoaded で live lane に対し start、 消えた lane / app 終了で stop (= map から remove)。
    let mut terminal_sessions: std::collections::HashMap<String, LaneTerminal> =
        std::collections::HashMap::new();
    // Echoes Act II (doc 32): per-lane echoes session registry (lane key → LaneEchoes)。
    // terminal と違い demand-driven: EchoesSubmit の初回で lazy spawn (reconcile 非結合)。
    let mut echoes_sessions: std::collections::HashMap<String, LaneEchoes> =
        std::collections::HashMap::new();
    // VP-100 follow-up (1Password 風): runtime 開発者モード state
    let mut dev_mode = initial_dev_mode;
    // project:add 等の async 操作で event loop に project list 再 fetch を kick するための proxy
    let async_action_proxy = event_loop.create_proxy();

    // 起動時 size clamp 用 once-flag。 macOS state restoration の `restorableState` は
    // EventLoop 起動後の async phase で frame に反映され、 初回の `WindowEvent::Resized`
    // として届く。 この flag が false のうちに来た Resized が「restoration 適用直後」と
    // みなして min 制約と照合し、 必要なら force-resize する (#428 Moody Blues Issue #1)。
    // PR #458 fix: 保存 geometry を復元した path では起動時 clamp を skip。
    // 復元値 (with_inner_size apply 済) を macOS state restoration race 由来の小 size で
    // 上書きしないため、 復元 path 中は最初の Resized event を「正常な user-driven resize」
    // 扱いにする。 default path (= restored_geometry None) では従来通り clamp logic を走らせる。
    let mut initial_size_clamp_done = restored_geometry.is_some();

    // PR #459 throttled save: window resize / move 中も 500ms throttle で session save。
    // CloseRequested の force save に依存しない (= `ge app:stop` の SIGTERM kill や crash
    // でも直近 state が persistent)。 dogfood で「ge app で再起動すると save 走らない」
    // bug を解消。
    const GEOMETRY_SAVE_THROTTLE: std::time::Duration = std::time::Duration::from_millis(500);
    let mut last_geometry_save = std::time::Instant::now() - std::time::Duration::from_secs(1);

    // dock app icon (portal favicon) の再アサート用。 bare binary は .app bundle が無いため
    // macOS が launch 完了時に generic icon を被せ、 run() 前 (window.build 直後) の
    // setApplicationIconImage を上書きする。 event loop 開始後 ~1.5s 間 set_app_icon() を
    // 呼び続けて (WaitUntil で loop を起こす) portal icon を定着させる。 .dmg 版は冪等。
    let icon_launch_at = std::time::Instant::now();
    let mut icon_settled = false;

    // Model B (focus = 操舵ポインタ): この vp-app instance が OS の key window かを追跡する。
    // multi-window は別プロセス (VP_APP_INSTANCE = primary 0 / secondary N) なので、ROTO の
    // switch_lane broadcast は全 instance の "canvas" 購読に届く。両 window が一斉に切り替わるのを
    // 防ぐため、**focused instance だけ**が switch_lane を適用する (B-local self-filter)。
    // with_focused(true) で起動するので初期値は true。
    let mut is_focused = true;
    // Model B #2: 直近 daemon に報告した active_lane。 focus が高速に flip しても
    // 同じ lane への重複報告 (= reqwest::Client 新規構築 + 無駄 POST) を抑止する。
    let mut last_focus_reported_lane: Option<String> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // launch settle まで dock icon を再設定 (bare binary 対策)。 settle 後は通常の Wait。
        if !icon_settled {
            crate::icon::set_app_icon();
            if icon_launch_at.elapsed() < std::time::Duration::from_millis(1500) {
                *control_flow = ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(150),
                );
            } else {
                icon_settled = true;
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                tracing::info!("Window close requested");
                // この window は **明示的に閉じられた** → 次回 primary 起動時に auto-respawn
                // しないよう自 instance file に open=false を記録する。 強制 kill (= SIGTERM /
                // crash) では CloseRequested が来ないので open=true のまま残り、 復元される。
                session_state.set_open(false);
                // window geometry + 表示モード (position/size/monitor/fullscreen) も自 instance file に
                // save。 起動時に WindowBuilder + set_fullscreen で apply されて前回の配置に復元される。
                persist_window_geometry(&mut session_state, &window);
                if let Some(g) = session_state.window_geometry() {
                    tracing::info!(
                        "session save [instance={}]: window geometry ({}x{} @ {},{}, monitor={:?}, mode={:?}), open=false",
                        instance_index,
                        g.width,
                        g.height,
                        g.x,
                        g.y,
                        g.monitor.as_deref(),
                        g.display_mode
                    );
                }
                // open=false (+ geometry) を確実に書き出す (outer_position 失敗でも open は残す)。
                session_state.save();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                let scale = window.scale_factor();
                // 初回 Resized = macOS state restoration 適用後の frame。 min 未満なら
                // force-resize して default に揃える (#428 Moody Blues Issue #1 fix)。
                // 2 回目以降は user resize / clamp 由来の通常 resize として update_pane_bounds 走らせる。
                if !initial_size_clamp_done {
                    initial_size_clamp_done = true;
                    let logical = size.to_logical::<f64>(scale);
                    if logical.width < MIN_WINDOW_WIDTH || logical.height < MIN_WINDOW_HEIGHT {
                        tracing::info!(
                            "vp-app: 起動時 window size ({}x{}) が min 未満 → {}x{} に矯正",
                            logical.width,
                            logical.height,
                            DEFAULT_WINDOW_WIDTH,
                            DEFAULT_WINDOW_HEIGHT
                        );
                        window.set_inner_size(LogicalSize::new(
                            DEFAULT_WINDOW_WIDTH,
                            DEFAULT_WINDOW_HEIGHT,
                        ));
                        // set_inner_size → 後続 Resized event で update_pane_bounds が正しく走る。
                        // この event は restoration の小 size なので bounds 更新 skip。
                        return;
                    }
                }
                update_pane_bounds(&webview, size, scale);
                // PR #459 throttled save: resize 中も 500ms throttle で geometry + 表示モードを save。
                // 全画面 enter/exit も Resized を撃つので、 helper 内の fullscreen 判定で mode が追従する。
                let now = std::time::Instant::now();
                if now.duration_since(last_geometry_save) > GEOMETRY_SAVE_THROTTLE {
                    last_geometry_save = now;
                    persist_window_geometry(&mut session_state, &window);
                    session_state.save();
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Moved(_),
                ..
            } => {
                // PR #459 throttled save: window 移動中も 500ms throttle で geometry + 表示モードを save。
                // Resized と pair (= drag による size 変更だけでなく位置変更も capture)。
                let now = std::time::Instant::now();
                if now.duration_since(last_geometry_save) > GEOMETRY_SAVE_THROTTLE {
                    last_geometry_save = now;
                    persist_window_geometry(&mut session_state, &window);
                    session_state.save();
                }
            }
            // Model B (focus = 操舵ポインタ): focus 状態を追跡する。OS の key window は全プロセス間で
            // 1 つだけなので、ちょうど 1 つの instance が is_focused=true になる。これにより ROTO の
            // switch_lane broadcast を「今見ている window」だけが適用し、focus を切り替えるだけで
            // 操舵対象 window が移る (seamless)。
            Event::WindowEvent {
                event: WindowEvent::Focused(focused),
                ..
            } => {
                is_focused = focused;
                tracing::debug!("window focus changed: is_focused={}", focused);
                // Model B #2: focus を得た瞬間、 この window の display lane を daemon canonical の
                // active_lane に報告する。 daemon active_lane が focused window に追従 → ROTO LCD
                // follows focus (#4) が「active_lane を映すだけ」 で自動成立する。 focus-loss (false)
                // は無視 ── 次に focus を得た window が上書きするため (lane 未選択 window も skip)。
                if focused
                    && let Some(address) = sidebar_state.active_lane_address.clone()
                    && last_focus_reported_lane.as_deref() != Some(address.as_str())
                    && let Some(path) = resolve_project_path_for_lane(&sidebar_state, &address)
                {
                    // 重複報告抑止: 報告する lane を記録してから spawn。 同 lane への
                    // 連続 focus event は上の guard で弾かれ、 Client 構築は lane 切替時のみ。
                    last_focus_reported_lane = Some(address.clone());
                    rt_handle.spawn(async move {
                        let client = crate::client::TheWorldClient::new(crate::client::default_world_port());
                        if let Err(e) = client.set_active_lane(path, address).await {
                            tracing::warn!("focus→set_active_lane failed: {}", e);
                        }
                    });
                }
            }
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
                    if let Err(e) = webview.evaluate_script(&script) {
                        tracing::warn!("paste deliver script failed: {}", e);
                    }
                }
            }
            Event::UserEvent(AppEvent::OscNotification { lane, code: _ }) => {
                // Phase 5-D Sprint C P2.1: per-Lane HD notification（Act I / OSC 由来）。
                // active lane は即読 skip。共通 sink（Act II の turn_completed と合流）。
                mark_lane_awaiting_input(&lane, "osc:notification", &mut sidebar_state, &webview);
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
                    push_sidebar_state(&webview, &sidebar_state);
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
                    push_sidebar_state(&webview, &sidebar_state);
                }
            }
            Event::UserEvent(AppEvent::ProjectsLoaded(projects)) => {
                // 既存 SidebarState とマージ:
                //  - 同じ path があれば既存 state を維持 (expanded / panes / active 保持)
                //  - 新規は ProjectPaneState::new (Conductor Agent 1 つ)
                //  - サーバから消えた project は除外
                //
                // VP-101 follow-up: register 後の auto-expand。
                // auto-select は LanesLoaded 側で扱う (Architecture v4: 真の selection unit は Lane)。
                // 「prev (旧 sidebar_state.processes) には port があった、 新 projects には port が無い」
                // 形の merge は port を不用意に消すので、 sidebar_state の port は新側 (port_by_name 反映済)
                // で上書きされる。 retroactive ensureLane (= 後段) で None→Some 遷移を補う。
                let prev: std::collections::HashMap<String, ProjectPaneState> = sidebar_state
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
                // Model Q: daemon canonical の active lane (presence、 boot 復元用)。
                // 注: app の active_lane_address は単一 global (pane.rs) なので、 daemon の
                // per-project active のうち **order 先頭の 1 つ**を採用する (意図的な単純化、
                // doc 24 §12-H)。 project ごとに最後の active を復元する per-project 化は
                // Phase 3 (app 側を per-project active に拡張、 daemon は既に per-project 保持)。
                let daemon_active_lane: Option<String> =
                    projects.iter().find_map(|p| p.active_lane.clone());
                sidebar_state.processes = projects
                    .into_iter()
                    .map(|p| {
                        // ProjectInfo.state / .port を ProjectPaneState に merge
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
                            let mut s = ProjectPaneState::new(p.path.clone(), p.name.clone());
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
                // Phase 1 (doc 24): currents_order を daemon の project_order (= fetch 順) の
                // mirror にする。これで currents_order は独立 SSOT ではなく canonical の派生となり、
                // JS resolveProjectOrder は実質 passthrough（sidebar = daemon = ROTO = CLI で一致）。
                sidebar_state.currents_order =
                    Some(project_ports.iter().map(|(path, _)| path.clone()).collect());
                // Model Q: 初回 load で active lane を daemon canonical から復元 (session.json でなく daemon が源)。
                if is_initial_load
                    && let Some(addr) = daemon_active_lane
                {
                    sidebar_state.active_lane_address = Some(addr.clone());
                    session_state.active_lane_address = Some(addr);
                }
                // wiremsg: 各 project の SP の Unison channel を購読する (per-SP 1 本ずつ)。
                // - Stage 1: "lanes" channel → sidebar Lane ツリー
                // - Stage 2: "canvas" channel → main area の Paisley Park body
                // retained topic なので接続直後に現スナップショットが届き、以降変化のたび
                // push される。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
                for (path, _port) in &project_ports {
                    // L0 SP-portless: lanes / canvas とも World :32000 の集約 channel から購読する
                    // (SP 直結を剥がす)。 どちらも World 側で per-project に集約済
                    // (lanes=lane_registry / canvas=TopicRouter) なので SP port 不問 = SP が down
                    // (port=None) でも「前回の続き」を表示でき、 port None→Some race で購読が始まらない
                    // 旧 gating の穴も解消する。 SP 復帰時は register / canvas push で各 channel が更新。
                    if lanes_sub_active.insert(path.clone()) {
                        spawn_lanes_subscription(
                            &rt_handle,
                            async_action_proxy.clone(),
                            path.clone(),
                            world_conn.clone(),
                        );
                    }
                    if canvas_sub_active.insert(path.clone()) {
                        spawn_canvas_subscription(
                            &rt_handle,
                            async_action_proxy.clone(),
                            path.clone(),
                            world_conn.clone(),
                        );
                    }
                }
                // terminal S4: ensureLane / terminal session は SP port に依存しなくなった
                // (xterm transport は World "canvas" channel)。 port None→Some race のための
                // retroactive ensureLane block は撤去 — lane の出現/消滅は LanesLoaded reconcile
                // が SSOT として扱う (= ensureLane + terminal session start/stop)。
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
                push_sidebar_state(&webview, &sidebar_state);
            }
            // Phase A4-3b: SP の Lane fetch 結果を sidebar_state に反映
            Event::UserEvent(AppEvent::LanesLoaded {
                process_path,
                lanes,
            }) => {
                // ループする event なので log omit (= LanesLoaded push と pair で noise 源)。
                // Architecture v4: active_lane_address が未設定なら最初の Lane を auto-select。
                // 「初回起動 → Conductor Lane が main area に出る」UX を Lane SSOT で保つ。
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
                // F.8 B Convergent: auto-select は pid あり (= Active = Pane 起動済) な Lane のみ対象。
                //  Dead Lane (pid:null、 spawn 失敗) を選ぶと WS 確立先が無く「lane not found」 reconnect ループに陥る。
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
                    lane_js::remove_lane(&webview, addr);
                    // terminal S4: 消えた lane の terminal session を停止 (= map から remove で
                    // cmd_tx drop → canvas channel close → World demand stop → SP pump stop)。
                    terminal_sessions.remove(addr);
                    // echoes session も対で停止（terminal_sessions と同寿命）。remove が無いと
                    // 削除済 lane の購読 task が demand を立てたまま永久残留する。
                    echoes_sessions.remove(addr);
                    // VP-147 PR-P2-3 Moody Blues fix #1: lane delete 検出時に lane_inboxes
                    // も即時 cleanup (= 5s polling tick 待たずに stale state 解消)。
                    sidebar_state.lane_inboxes.remove(addr);
                }
                sidebar_state.lanes_by_project.insert(process_path, lanes);
                // 購読フェーズを "ready" に (= snapshot を 1 度でも受けた)。 stalled から復帰した場合も
                // ここで解消。 absent(初期 loading) / stalled と区別して hintFor が lane 0本 を
                // 「📡 lane なし」 と正しく出せる (doc 30 §5-3)。
                sidebar_state
                    .lane_sub_state
                    .insert(path_key.clone(), "ready".to_string());
                // terminal S4: per-lane instance — SP port には依存しない (xterm transport は
                // World "canvas" channel)。 live lane (pid あり) ごとに ensureLane (JS xterm 作成) +
                // terminal session start (World 購読 → demand → SP pump)。 どちらも idempotent。
                if let Some(lanes_for_proj) = sidebar_state.lanes_by_project.get(&path_key) {
                    for lane in lanes_for_proj {
                        // pid:null = PtySlot 不在 → xterm を作らない。 内訳は 2 種で、 どちらも
                        // ここでは対象外にするのが正しい:
                        //  - Dead Lane (spawn 失敗、 F.8 B Convergent) → 別途 on-demand respawn
                        //  - chat lane (Act II) → 内容は ChatView が描く
                        // ⚠️ 「pid=null = 死」ではない。逆に「pid あり = tui」でもない — chat lane
                        //    は engine 稼働中 pid=Some になる（ensure_chat_engine が host pid を
                        //    記録）ため、pid だけで gate すると chat lane に xterm と terminal
                        //    購読を作ってしまう。console_mode の除外を必ず併用（#702 と同じ教訓）。
                        if lane.pid.is_none() || lane.console_mode == "chat" {
                            continue;
                        }
                        // Running に戻った lane は respawn guard を解除 (再 Dead 化時に再 respawn 可能に)。
                        let addr_str = lane.address.key();
                        lane_respawn_triggered.remove(&addr_str);
                        lane_js::ensure_lane(&webview, &addr_str);
                        // terminal session 未起動なら start (idempotent)。
                        terminal_sessions
                            .entry(addr_str.clone())
                            .or_insert_with(|| {
                                spawn_terminal_session(
                                    &rt_handle,
                                    async_action_proxy.clone(),
                                    world_conn.clone(),
                                    path_key.clone(),
                                    addr_str.clone(),
                                )
                            });
                    }
                }
                if let Some(addr) = first_addr {
                    tracing::info!("auto-select first lane: {}", addr);
                    activate_lane(
                        &addr,
                        &mut sidebar_state,
                        &mut session_state,
                        &webview,
                        &mut lane_respawn_triggered,
                        &rt_handle,
                        &respawn_proxy,
                    );
                } else {
                    push_sidebar_state(&webview, &sidebar_state);
                }
                // Act II: active chat lane を echoes topic に attach（→ demand → transcript replay）。
                // LanesLoaded は lane snapshot 到着のたび走るので、 起動直後の session 復元
                // (activate は LanesLoaded 前に済んでいる場合がある) もここで確実に拾える。
                if let Some(addr) = sidebar_state.active_lane_address.clone() {
                    ensure_echoes_attach(
                        &addr,
                        &sidebar_state,
                        &mut echoes_sessions,
                        &rt_handle,
                        &async_action_proxy,
                        &world_conn,
                    );
                }
            }
            // VP-140: JS 側が DOMContentLoaded 後に送る lane catch-up 要求。
            // 起動 race で silent drop された ensureLane を再発行する (WebView HTML load 完了
            // 後なので、 evaluate_script は確実に実行される)。 idempotent (ensureLane 内で既存なら no-op)。
            Event::UserEvent(AppEvent::LanesEnsureAll) => {
                // terminal S4: JS xterm instance の catch-up 再発行のみ (SP port 不要)。
                // terminal session 自体は LanesLoaded reconcile が管理するのでここでは触らない。
                for (_project_path, lanes) in sidebar_state.lanes_by_project.clone().iter() {
                    for lane in lanes {
                        // pid:null = PtySlot 不在 (Dead Lane / chat lane) → xterm 不要。
                        if lane.pid.is_none() {
                            continue;
                        }
                        lane_js::ensure_lane(&webview, &lane.address.key());
                    }
                }
                // 現在 active な Lane を再度 show する (lane-empty placeholder を解除する保険)
                if let Some(addr) = sidebar_state.active_lane_address.clone() {
                    let is_chat = lane_is_chat(&sidebar_state, &addr);
                    lane_js::show_lane(&webview, Some(&addr), is_chat);
                    // 起動 race で silent drop されるのは ensureLane だけではない。 auto-select の
                    // activate_lane が撃つ setActivePane / vpConsole.setMode も同じ窓で落ちるが、
                    // この 2 つは JS 側の「active lane」(= Act toggle の宛先) を埋める唯一の経路。
                    // showLane だけ再発行しても JS の active lane は null のままなので、 Act II 押下が
                    // "active lane 不明" で早期 return し「Act II に移行できない」になる (lane を手で
                    // 選び直すと activate_lane が再走して直る、が user から見れば不可解)。 catch-up は
                    // 3 つとも再発行して JS 側 state を確定させる。 いずれも冪等。
                    push_active_view(&webview, &sidebar_state);
                    let script = format!(
                        "window.vpConsole && window.vpConsole.setMode({}, {})",
                        serde_json::to_string(&addr).unwrap_or_else(|_| "\"\"".into()),
                        if is_chat { "\"chat\"" } else { "\"tui\"" },
                    );
                    let _ = webview.evaluate_script(&script);
                }
                // LanesLoaded のたびに follow up 発火する loop event のため log omit。
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
                // SP 接続失敗 / lanes channel stall — lanes_by_project は更新しない (前回値を保持) が、
                // 購読フェーズを "stalled" に倒して UI に surface する (doc 30 §5-3)。 hintFor が
                // `📡 loading lanes…` ではなく「⚠️ lane 接続が停滞 — restart で復帰」を出す。 復帰時の
                // snapshot 受信 (LanesLoaded) で "ready" に上書きされて自動解消する (self-heal と連動)。
                sidebar_state
                    .lane_sub_state
                    .insert(process_path, "stalled".to_string());
                push_sidebar_state(&webview, &sidebar_state);
            }
            // オンデマンド respawn の restart_lane が失敗した lane を guard から解除する。
            // 解除しておくと、 次に同 lane を active にした (or LanesLoaded for Dead の) 時点で
            // 再 respawn を試行できる (= SP クラッシュ後の復帰でも auto-respawn が効く)。
            // 即ループにはならない: クリック起点は user 操作、 起動時 first_addr は active 設定後
            // None になるため LanesLoaded loop event での連続発火は起きない。
            Event::UserEvent(AppEvent::LaneRespawnFailed { address }) => {
                if lane_respawn_triggered.remove(&address) {
                    tracing::info!("auto-respawn guard 解除 (restart 失敗): {}", address);
                }
            }
            Event::UserEvent(AppEvent::DeviceEvent { payload }) => {
                tracing::debug!("🧲 device event: {}", payload);
                // Phase 2: device 一覧を registry 更新 → sidebar (Devices badge) + main area
                // (Bastet pane の device list) の両方に push。
                if crate::pane::apply_device_event(&mut sidebar_state.bastet_devices, &payload) {
                    push_sidebar_state(&webview, &sidebar_state);
                    lane_js::render_bastet_devices(&webview, &sidebar_state.bastet_devices);
                }
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
                // B1 + cross-project: switch_lane は PP content ではなく active Lane 切替コマンド。
                // active を「変える」コマンドなので、active project guard の **外**で処理する
                // （別 project の SP から来た switch_lane こそ通す）。送信元 SP の project
                // (= msg_project) の lane を activate し、sidebar / main area を追随させる。
                if message.get("type").and_then(|t| t.as_str()) == Some("switch_lane") {
                    if let (Some(project), Some(token)) = (
                        msg_project,
                        message.get("lane").and_then(|l| l.as_str()),
                    ) {
                        // token → lane address (`<project>/conductor` or `<project>/performer/<name>`)
                        let address = if token.is_empty() || token == "conductor" {
                            format!("{}/conductor", project)
                        } else {
                            format!("{}/performer/{}", project, token)
                        };
                        // Model B (focus = 操舵ポインタ): switch_lane は全 instance に broadcast される
                        // が、適用するのは **focused instance だけ**。非 focus の window はこの event を
                        // 無視し、自分の lane に park されたまま (= 2 window が別々の lane を同時に見られる)。
                        if is_focused {
                            activate_lane(
                                &address,
                                &mut sidebar_state,
                                &mut session_state,
                                &webview,
                                &mut lane_respawn_triggered,
                                &rt_handle,
                                &respawn_proxy,
                            );
                            // Act II: chat lane なら echoes topic に attach（→ transcript replay）。
                            ensure_echoes_attach(
                                &address,
                                &sidebar_state,
                                &mut echoes_sessions,
                                &rt_handle,
                                &async_action_proxy,
                                &world_conn,
                            );
                        } else {
                            tracing::debug!(
                                "switch_lane skip (not focused): address={}",
                                address
                            );
                        }
                    }
                } else if active_project.is_some() && active_project == msg_project {
                    // PP content (非 switch_lane) は active project の分のみ main area に転送する。
                    match serde_json::to_string(&message) {
                        Ok(json) => {
                            let script = format!(
                                "window.vpCanvas && window.vpCanvas.handleMessage({})",
                                json
                            );
                            if let Err(e) = webview.evaluate_script(&script) {
                                tracing::warn!("vpCanvas.handleMessage 失敗: {}", e);
                            }
                            // message ごとに loop 発火するため成功 log は omit (= warn のみ keep)。
                        }
                        Err(e) => {
                            tracing::warn!("CanvasMessage serialize 失敗: {}", e);
                        }
                    }
                }

                // Canvas 着信 badge (bug: canvas 可観測性 D): show が現在 active でない lane に
                // 着いたら sidebar に canvas_unread を計上する。別 project / 別 lane（同 project
                // だが別 lane）の両ケースを 1 箇所で拾う（上の forward guard とは独立）。active lane
                // 宛の show は panel 側 (pp-overlay auto-open, canvas-handler.ts) で解決する。
                if message.get("type").and_then(|t| t.as_str()) == Some("show")
                    && let Some(project) = msg_project
                {
                    let token = message
                        .get("lane")
                        .and_then(|l| l.as_str())
                        .unwrap_or("conductor");
                    // token → lane address（switch_lane と同じ変換）。
                    let address = if token.is_empty() || token == "conductor" {
                        format!("{}/conductor", project)
                    } else {
                        format!("{}/performer/{}", project, token)
                    };
                    if sidebar_state.active_lane_address.as_deref() != Some(address.as_str()) {
                        mark_lane_canvas_unread(&address, &mut sidebar_state, &webview);
                    }
                }
            }
            // terminal S4 (doc 27 §4.1): per-lane terminal session 由来の PTY 出力を当該 lane の
            // xterm に inject する。 data は base64 (JS 側で decode → term.write)。
            Event::UserEvent(AppEvent::TerminalOutput { lane, data }) => {
                let script = format!(
                    "window.vpTerminal && window.vpTerminal.handleOutput({}, {})",
                    serde_json::to_string(&lane).unwrap_or_else(|_| "\"\"".into()),
                    serde_json::to_string(&data).unwrap_or_else(|_| "\"\"".into()),
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("vpTerminal.handleOutput 失敗 (lane={}): {}", lane, e);
                }
            }
            // terminal S4: xterm onData → 当該 lane の terminal session に渡す (上り request)。
            Event::UserEvent(AppEvent::TerminalWrite { lane, data }) => {
                vp_paths::term_trace("A:app-dispatch(b64)", &lane, data.as_bytes());
                if let Some(session) = terminal_sessions.get(&lane) {
                    let _ = session.cmd_tx.send(TermCmd::Write(data));
                }
            }
            // terminal S4: xterm resize → 当該 lane の terminal session に渡す (上り request)。
            Event::UserEvent(AppEvent::TerminalResize { lane, cols, rows }) => {
                if let Some(session) = terminal_sessions.get(&lane) {
                    let _ = session.cmd_tx.send(TermCmd::Resize(cols, rows));
                }
            }
            // Echoes Act II (doc 32): SP から受信した構造化イベントを当該 lane の Console pane に渡す。
            Event::UserEvent(AppEvent::EchoesEvent { lane, event }) => {
                let script = format!(
                    "window.vpConsole && window.vpConsole.handleEvent({}, {})",
                    serde_json::to_string(&lane).unwrap_or_else(|_| "\"\"".into()),
                    serde_json::to_string(&event).unwrap_or_else(|_| "null".into()),
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("vpConsole.handleEvent 失敗 (lane={}): {}", lane, e);
                }
                // 路 A（memory echoes-act2-notification-signal）: Act II の完了/エラーを Act I の
                // OSC 通知と同じ sink に流す。headless stream-json は Notification hook を発火しない
                // ため、turn_completed（stream `result` 由来）が「Claude が返し終えた＝入力待ち」の
                // 唯一のシグナル。question（PR1）/ permission_request（PR3 tool 承認 + PR4 plan の
                // ExitPlanMode）は engine が turn を pause して人の判断を待つ明示的 HITL なので、同じ
                // awaiting_input 機構で conn-hitl（magenta diamond）を点灯する（doc 35 §4 の契約）。
                // active lane は helper が即読 skip し、切替（activate_lane）で reset される。
                if let Some(kind) = event.get("kind").and_then(|k| k.as_str())
                    && (kind == "turn_completed"
                        || kind == "error"
                        || kind == "question"
                        || kind == "permission_request")
                {
                    mark_lane_awaiting_input(
                        &lane,
                        &format!("act2:{kind}"),
                        &mut sidebar_state,
                        &webview,
                    );
                }
            }
            // Echoes Act II: EchoesChatPane の submit → 当該 lane の echoes session に渡す。
            // demand-driven: 未起動なら lazy spawn (subscribe → submit の順で取りこぼしなし)。
            Event::UserEvent(AppEvent::EchoesSubmit { lane, prompt }) => {
                let session = echoes_sessions.entry(lane.clone()).or_insert_with(|| {
                    // process_path は active project から解決 (echoes pane = active lane 前提)。
                    let process_path =
                        resolve_active_project_path(&sidebar_state).unwrap_or_default();
                    spawn_echoes_session(
                        &rt_handle,
                        async_action_proxy.clone(),
                        world_conn.clone(),
                        process_path,
                        lane.clone(),
                    )
                });
                let _ = session.cmd_tx.send(EchoesCmd::Submit(prompt));
            }
            // Echoes Act II HITL (doc 35 PR1): PromptCard の回答 → 当該 lane の echoes session へ。
            // 質問は submit 済み engine 由来なので session は既存のはずだが、防御的に lazy spawn。
            Event::UserEvent(AppEvent::EchoesRespond { lane, request_id, answers, behavior, message }) => {
                let session = echoes_sessions.entry(lane.clone()).or_insert_with(|| {
                    let process_path =
                        resolve_active_project_path(&sidebar_state).unwrap_or_default();
                    spawn_echoes_session(
                        &rt_handle,
                        async_action_proxy.clone(),
                        world_conn.clone(),
                        process_path,
                        lane.clone(),
                    )
                });
                let _ = session.cmd_tx.send(EchoesCmd::Respond {
                    request_id,
                    answers,
                    behavior,
                    message,
                });
            }
            // doc 35 §5 / PR2: 実行中 turn の中断を当該 lane の echoes session に渡す。
            // interrupt は走行中 turn 前提なので session が居るはず（lazy spawn しない）。
            Event::UserEvent(AppEvent::EchoesInterrupt { lane }) => {
                if let Some(session) = echoes_sessions.get(&lane) {
                    let _ = session.cmd_tx.send(EchoesCmd::Interrupt);
                } else {
                    tracing::warn!("echoes:interrupt skip — session 未起動 (lane={lane})");
                }
            }
            // doc 35 §2.5 / PR3: permission mode 切替を当該 lane の echoes session に渡す。
            Event::UserEvent(AppEvent::EchoesSetPermissionMode { lane, mode }) => {
                if let Some(session) = echoes_sessions.get(&lane) {
                    let _ = session.cmd_tx.send(EchoesCmd::SetPermissionMode { mode });
                } else {
                    tracing::warn!("echoes:set_permission_mode skip — session 未起動 (lane={lane})");
                }
            }
            // doc 33 C2: Act toggle → SP console_set_mode。成功したら vpConsole.setMode で
            // WebView の表示を切替える（成功後に反映 = SP が真実源）。
            Event::UserEvent(AppEvent::ConsoleSetMode { lane, mode }) => {
                let Some(path) = resolve_active_project_path(&sidebar_state) else {
                    tracing::warn!("console:set_mode skip — active project 解決失敗");
                    return;
                };
                let proxy = async_action_proxy.clone();
                let mode_for_js = mode.clone();
                let lane_for_js = lane.clone();
                rt_handle.spawn(async move {
                    match world_process_request(
                        crate::client::default_world_port(),
                        &path,
                        "console_set_mode",
                        serde_json::json!({ "lane": lane, "mode": mode }),
                    )
                    .await
                    {
                        Ok(_) => {
                            tracing::info!("console_set_mode ok: lane={lane_for_js} mode={mode_for_js}");
                            // 表示切替は main thread の evaluate_script で行う必要があるため
                            // ConsoleModeApplied event を投げ直す。
                            let _ = proxy.send_event(AppEvent::ConsoleModeApplied {
                                lane: lane_for_js,
                                mode: mode_for_js,
                            });
                        }
                        Err(e) => tracing::warn!("console_set_mode 失敗: {e}"),
                    }
                });
            }
            // doc 33 C2: console_set_mode 成功後、WebView に mode を反映（xterm⇄chat 表示切替）。
            Event::UserEvent(AppEvent::ConsoleModeApplied { lane, mode }) => {
                // SP が Ok を返した = mode は確定。だが lanes snapshot への反映は 5s periodic
                // 頼み（SystemEvent::Lane は Add/Remove しか fire しない）で最大 5 秒 stale が
                // 残るため、手元 snapshot に即時反映する。これが無いと (a) 5 秒以内に lane を
                // 離れて戻ると activate_lane が旧 mode で開く（間欠の「戻ると Act I で開く」）、
                // (b) 下の ensure_echoes_attach が旧 mode を読んで skip する。
                for lanes in sidebar_state.lanes_by_project.values_mut() {
                    if let Some(l) = lanes.iter_mut().find(|l| l.address.key() == lane) {
                        l.console_mode = mode.clone();
                    }
                }
                push_sidebar_state(&webview, &sidebar_state);
                // Act I 復帰では xterm と terminal session を mode 反映の前に用意する。
                // 起動時に chat だった lane は LanesLoaded の pid=None 分岐で ensure_lane も
                // session start も素通りしており、mode を反映しただけでは購読者が居ないまま
                // SP の pump が出力を route する。terminal topic は非 retained なので、その間の
                // PTY 出力は復元されず xterm が空のままになる（II→I で何も出ない の真因）。
                // subscribe すると demand 0→1 が World の hook を撃ち、SP が pump を張り直して
                // replay を先頭配送する。どちらも idempotent（起動時 tui の lane は entry 既存）。
                let is_tui = mode == "tui";
                if is_tui {
                    lane_js::ensure_lane(&webview, &lane);
                    // SP 応答待ちの間に user が別 lane / 別 project へ移り得るため、project は
                    // 「今の active lane」ではなく対象 lane 自身から逆引きする（chat 分岐の
                    // ensure_echoes_attach と同じ resolver — 揃えないと遅着応答が別 project の
                    // path で購読を張る）。
                    match resolve_project_path_for_lane(&sidebar_state, &lane) {
                        Some(path) => {
                            terminal_sessions.entry(lane.clone()).or_insert_with(|| {
                                spawn_terminal_session(
                                    &rt_handle,
                                    async_action_proxy.clone(),
                                    world_conn.clone(),
                                    path,
                                    lane.clone(),
                                )
                            });
                        }
                        // 購読を張れない = PTY 出力が届かず xterm が空のままになる。切替は
                        // 成立しているので黙って落とさず、原因を残す。
                        None => tracing::warn!(
                            "console:mode_applied — lane の project 解決失敗、terminal session を張れず (lane={lane})"
                        ),
                    }
                }
                let script = format!(
                    "window.vpConsole && window.vpConsole.setMode({}, {})",
                    serde_json::to_string(&lane).unwrap_or_else(|_| "\"\"".into()),
                    serde_json::to_string(&mode).unwrap_or_else(|_| "\"tui\"".into()),
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("vpConsole.setMode 失敗 (lane={}): {}", lane, e);
                }
                // Act I 復帰は xterm container を active 化しないと見えない（applyConsoleMode の
                // tui 分岐は laneHost の console-hidden を外すだけ = chat で生まれた lane の
                // container は非 active のまま）。showLane は active 化に加えて rAF 2 段で
                // fit / sendResize / focus まで行う。⚠️ setMode より後に呼ぶこと — console-hidden
                // が残ったままだと clientWidth=0 で fit が見送られ 80×24 に固定される。
                if is_tui {
                    // SP 応答待ちの間に別 lane へ移っていたら表示は奪わない。mode は上で手元
                    // snapshot に反映済みなので、戻った時の activate_lane が正しい mode で開く。
                    if sidebar_state.active_lane_address.as_deref() == Some(lane.as_str()) {
                        lane_js::show_lane(&webview, Some(&lane), false);
                    }
                } else {
                    // I→II の対称: toggle 経路でも echoes topic に即 attach（→ demand 0→1 →
                    // transcript replay）。attach は lane 選択時と LanesLoaded にしか無く、
                    // tui 起点の lane を初めて chat に切替えた場合は periodic snapshot（最大
                    // 5 秒）まで会話が出ない。上の手元 snapshot 反映が先に要る（attach の
                    // gate が lane_is_chat を読む）。attach 済みなら no-op（idempotent）。
                    ensure_echoes_attach(
                        &lane,
                        &sidebar_state,
                        &mut echoes_sessions,
                        &rt_handle,
                        &async_action_proxy,
                        &world_conn,
                    );
                }
            }
            // 新セッション開始（New Session ボタン）: lane_restart(fresh=true) で SP に forward。
            // fresh = cc_session 破棄 + Act I は素の claude respawn / Act II は engine drop →
            // restart_lane_orchestrated が eager 再 spawn（新 session_init が即届く）。
            Event::UserEvent(AppEvent::ConsoleNewSession { lane }) => {
                // project は対象 lane 自身から逆引き（#705 のレース教訓 — SP 応答待ちの間に
                // active lane が変わり得るため resolve_active_project_path は使わない）。
                let Some(path) = resolve_project_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("console:new_session skip — lane の project 解決失敗 (lane={lane})");
                    return;
                };
                let proxy = async_action_proxy.clone();
                rt_handle.spawn(async move {
                    let payload = serde_json::json!({ "address": &lane, "fresh": true });
                    match world_process_request(
                        crate::client::default_world_port(),
                        &path,
                        "lane_restart",
                        payload,
                    )
                    .await
                    {
                        Ok(_) => {
                            tracing::info!("console:new_session ok: lane={lane}");
                            let _ = proxy.send_event(AppEvent::ConsoleSessionRenewed { lane });
                        }
                        Err(e) => tracing::warn!("console:new_session 失敗 (lane={lane}): {e}"),
                    }
                });
            }
            // fresh restart 成功 → ChatView の会話表示をクリアする。replay_start は foldInto が
            // 「会話 clear + header 保持」で畳む既存意味論（chatview.tsx）— 新 engine の
            // session_init が届けば header も新しくなる。tui lane は追加処理不要（新 PtySlot の
            // pump replay が clear prefix 付きで xterm を拭く）。
            Event::UserEvent(AppEvent::ConsoleSessionRenewed { lane }) => {
                let script = format!(
                    "window.vpConsole && window.vpConsole.handleEvent({}, {{kind:'replay_start'}})",
                    serde_json::to_string(&lane).unwrap_or_else(|_| "\"\"".into()),
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("console:new_session の ChatView クリア失敗 (lane={lane}): {e}");
                }
            }
            // Act II モデル切替: console_set_model で SP に forward（fire & forget）。
            // 適用の視覚確認は新 engine の session_init が header.model を更新することで得る。
            Event::UserEvent(AppEvent::ConsoleSetModel { lane, model }) => {
                let Some(path) = resolve_project_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("console:set_model skip — lane の project 解決失敗 (lane={lane})");
                    return;
                };
                rt_handle.spawn(async move {
                    let payload = serde_json::json!({ "lane": &lane, "model": model });
                    match world_process_request(
                        crate::client::default_world_port(),
                        &path,
                        "console_set_model",
                        payload,
                    )
                    .await
                    {
                        Ok(_) => tracing::info!("console:set_model ok: lane={lane}"),
                        Err(e) => tracing::warn!("console:set_model 失敗 (lane={lane}): {e}"),
                    }
                });
            }
            Event::UserEvent(AppEvent::PpStateSaveRequest { body }) => {
                // F6: WebView の save IPC を World process-proxy ask (pp_state_save) で SP に forward。
                // 旧 SP HTTP 直結を撤去。 active project 解決失敗は silent skip (空 canvas の debounce save)。
                let Some(path) = resolve_active_project_path(&sidebar_state) else {
                    tracing::debug!("pp:state:save skip — active project 解決失敗 (lane 未選択 or SP 未起動)");
                    return;
                };
                rt_handle.spawn(async move {
                    match world_process_request(
                        crate::client::default_world_port(),
                        &path,
                        "pp_state_save",
                        body,
                    )
                    .await
                    {
                        Ok(_) => tracing::debug!("pp:state:save → World OK"),
                        Err(e) => tracing::warn!("pp:state:save 失敗: {}", e),
                    }
                });
            }
            Event::UserEvent(AppEvent::PpStateLoadRequest { lane, pane_id }) => {
                // F6: WebView の load IPC を World process-proxy ask (pp_state_load) で SP に forward。
                // 結果 record を AppEvent::PpStateLoaded で event loop に戻し、 次の arm で WebView に push。
                let Some(path) = resolve_active_project_path(&sidebar_state) else {
                    tracing::debug!("pp:state:load skip — active project 解決失敗");
                    return;
                };
                let load_proxy = async_action_proxy.clone();
                rt_handle.spawn(async move {
                    let mut payload = serde_json::json!({ "pane_id": pane_id });
                    if let Some(name) = lane {
                        payload["lane"] = serde_json::Value::String(name);
                    }
                    let record = match world_process_request(
                        crate::client::default_world_port(),
                        &path,
                        "pp_state_load",
                        payload,
                    )
                    .await
                    {
                        // SP は {status:ok, record} | {status:empty} を返す。 record だけ抜く。
                        Ok(v) => v.get("record").filter(|r| !r.is_null()).cloned(),
                        Err(e) => {
                            tracing::warn!("pp:state:load 失敗: {}", e);
                            None
                        }
                    };
                    let _ = load_proxy.send_event(AppEvent::PpStateLoaded { record });
                });
            }
            Event::UserEvent(AppEvent::PpStateLoaded { record }) => {
                // pp-content-persist: SP から取った PP state を WebView に push back する。
                // record は pane_contents の 1 行 (stack/ui_state 等を含む) か None (= 未保存)。
                // WebView 側は `pp:state:loaded` を canvas-handler.handleMessage で受けて
                // applyPersistedState を呼ぶ。 SurrealDB row の `stack` field のみ取り出して渡す。
                let stack = record.as_ref().and_then(|r| r.get("stack").cloned());
                let payload = serde_json::json!({
                    "type": "pp:state:loaded",
                    "stack": stack,
                });
                match serde_json::to_string(&payload) {
                    Ok(json) => {
                        let script = format!(
                            "window.vpCanvas && window.vpCanvas.handleMessage({})",
                            json
                        );
                        if let Err(e) = webview.evaluate_script(&script) {
                            tracing::warn!("pp:state:loaded inject 失敗: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("pp:state:loaded serialize 失敗: {}", e);
                    }
                }
            }
            Event::UserEvent(AppEvent::ProjectsError(msg)) => {
                let js_msg = serde_json::to_string(&msg).unwrap_or_else(|_| "\"error\"".into());
                let script = format!("window.renderError({})", js_msg);
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("sidebar renderError 失敗: {}", e);
                }
            }
            // R5 Performer create flow: spawn_blocking thread からの結果を sidebar に push back。
            // success → form を閉じる + addPerformerOpen から削除。
            // error → form 下に inline error 表示 + form は開いたまま (再 submit 可能)。
            Event::UserEvent(AppEvent::PerformerCreateResult {
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
                let script = format!("window.handleAddPerformerResult({})", payload_str);
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("sidebar handleAddPerformerResult 失敗: {}", e);
                }
            }
            Event::UserEvent(AppEvent::StandsResult {
                project_path,
                stands,
                error,
            }) => {
                // doc 11 PR-C: + Add Performer form の dropdown を populate するための push back。
                let payload = serde_json::json!({
                    "project_path": project_path,
                    "stands": stands,
                    "error": error,
                });
                let payload_str = serde_json::to_string(&payload)
                    .unwrap_or_else(|_| "{}".to_string());
                let script = format!("window.handleStandsResult({})", payload_str);
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("sidebar handleStandsResult 失敗: {}", e);
                }
            }
            // Sidebar File Explorer: walk 結果を sidebar webview に push back。
            // JS 側 (`FileExplorer.tsx`) が `window.vpFiles.handleListResult` で受信。
            Event::UserEvent(AppEvent::FilesListResult {
                address,
                entries,
                truncated,
            }) => {
                let payload = serde_json::json!({
                    "address": address,
                    "entries": entries,
                    "truncated": truncated,
                });
                let payload_str =
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                let script = format!(
                    "window.vpFiles && window.vpFiles.handleListResult({})",
                    payload_str
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("sidebar vpFiles.handleListResult 失敗: {}", e);
                }
            }
            // Wire inbox (doc 34 §4 V1): fetch 結果を sidebar の vpWire 受け口へ push back。
            Event::UserEvent(AppEvent::WireHistoryResult { address, payload }) => {
                let payload_str =
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
                let script = format!(
                    "window.vpWire && window.vpWire.handleResult({})",
                    payload_str
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("sidebar vpWire.handleResult 失敗 (address={}): {}", address, e);
                }
            }
            // Sidebar File Explorer: file 読み込み結果を Canvas (PP) に inject。
            // 既存 MCP `show` ルートを QUIC を経由せず WebView 直注入 (= ephemeral / local-only) で
            // 再現するため、 `ProcessMessage::Show` 相当の JSON を main_view にそのまま渡す。
            Event::UserEvent(AppEvent::FilesOpenResult { content }) => {
                // doc 19 PP Canvas Stack Model: append field は omit (= stack push に
                // 統一)。 pane_id は dead field だが backward compat で keep。
                let msg = serde_json::json!({
                    "type": "show",
                    "pane_id": "main",
                    "content": content,
                });
                let msg_str =
                    serde_json::to_string(&msg).unwrap_or_else(|_| "{}".to_string());
                let script = format!(
                    "window.vpCanvas && window.vpCanvas.handleMessage({})",
                    msg_str
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("main_view vpCanvas.handleMessage (files:open) 失敗: {}", e);
                }
            }
            Event::UserEvent(AppEvent::ActivityUpdate(snap)) => {
                sidebar_state.activity = snap;
                push_sidebar_state(&webview, &sidebar_state);
            }
            Event::UserEvent(AppEvent::ClonePathPicked(path)) => {
                // user キャンセル時 (None) は JS 状態を変更しない (= 既存 override を保持)
                if let Some(p) = path {
                    let js_arg = serde_json::to_string(&p).unwrap_or_else(|_| "null".into());
                    let script =
                        format!("window.setClonePath && window.setClonePath({})", js_arg);
                    if let Err(e) = webview.evaluate_script(&script) {
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
                // Lane activation — activate_lane() が全副作用を処理
                if let Some(addr) = outcome.activate_lane {
                    activate_lane(
                        &addr,
                        &mut sidebar_state,
                        &mut session_state,
                        &webview,
                        &mut lane_respawn_triggered,
                        &rt_handle,
                        &respawn_proxy,
                    );
                    // Act II: chat lane なら echoes topic に attach（→ transcript replay）。
                    ensure_echoes_attach(
                        &addr,
                        &sidebar_state,
                        &mut echoes_sessions,
                        &rt_handle,
                        &async_action_proxy,
                        &world_conn,
                    );
                } else {
                    if outcome.changed {
                        push_sidebar_state(&webview, &sidebar_state);
                    }
                    if outcome.active_changed {
                        push_active_view(&webview, &sidebar_state);
                    }
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
                        spawn_sp_start(&rt_handle, async_action_proxy.clone(), name, path);
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
                // Phase 5-C: Process restart 要求 (sidebar の 🔄 button から)。
                // 全 async work は shared runtime (rt_handle) 経由 — bare `tokio::spawn` は禁止
                // (.clippy.toml で compile gate)、 tao event loop closure に runtime context が
                // 無いので必ず `rt_handle.spawn` を使う。
                if let Some(project_name) = outcome.restart_process_request {
                    let proxy = async_action_proxy.clone();
                    rt_handle.spawn(async move {
                        // TheWorld port は profile 依存 (brew=32000 / dev=32100、 client::default_world_port() と同期)
                        let client = crate::client::TheWorldClient::new(crate::client::default_world_port());
                        match client.restart_process(&project_name).await {
                            Ok(()) => {
                                tracing::info!("restart_process OK: {}", project_name);
                                // 完了 → projects 再 fetch → sidebar state badge 更新。
                                // 必ず `fetch_projects_with_ports` 経由 (= runtime port merge)
                                // で送る。 list_projects() だけだと restart 直後に全 project の
                                // port が None で潰れ、 後続 LanesLoaded で ensureLane が
                                // 全件 skip され conductor terminal が消失する。
                                if let Ok(projects) = fetch_projects_with_ports(&client).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ProjectsLoaded(projects));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "restart_process failed for {}: {}",
                                    project_name,
                                    e
                                );
                            }
                        }
                    });
                }
                // Process stop 要求 (project context menu の Stop project から)。
                if let Some(project_name) = outcome.stop_process_request {
                    let proxy = async_action_proxy.clone();
                    rt_handle.spawn(async move {
                        let client = crate::client::TheWorldClient::new(crate::client::default_world_port());
                        match client.stop_process(&project_name).await {
                            Ok(()) => {
                                tracing::info!("stop_process OK: {}", project_name);
                                // 完了 → projects 再 fetch → 停止 state を sidebar に反映。
                                // restart と同じく `fetch_projects_with_ports` 経由で
                                // 他 project の runtime port を保つ。
                                if let Ok(projects) = fetch_projects_with_ports(&client).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ProjectsLoaded(projects));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "stop_process failed for {}: {}",
                                    project_name,
                                    e
                                );
                            }
                        }
                    });
                }
                // Project delete 要求 (project context menu の Delete project から、
                // UI で 2-click 確認済)。 daemon の remove_project は稼働中 SP があると
                // エラーになるため、 先に stop → grace → remove と chain する
                // (restart_process が capability 内でやっているのと同じ順序)。
                if let Some((project_name, project_path)) = outcome.delete_project_request {
                    let proxy = async_action_proxy.clone();
                    rt_handle.spawn(async move {
                        let client = crate::client::TheWorldClient::new(crate::client::default_world_port());
                        // stop は best-effort: SP が未起動 (= 停止中) なら
                        // 「No running Process」 エラーが返るが、 続行して remove する。
                        match client.stop_process(&project_name).await {
                            Ok(()) => {
                                tracing::info!("delete: stop_process OK: {}", project_name);
                                // shutdown 伝播 + port release を待つ grace period
                                tokio::time::sleep(std::time::Duration::from_millis(500))
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
                                tracing::info!("remove_project OK: {}", project_path);
                                // 完了 → projects 再 fetch → sidebar から除去。
                                // 削除対象以外の project の runtime port を保つため
                                // `fetch_projects_with_ports` 経由で送る。
                                if let Ok(projects) = fetch_projects_with_ports(&client).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ProjectsLoaded(projects));
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
                }
                // Phase 1 (doc 24): project 並び替えを daemon の project_order に永続化する。
                // restart/stop と同じ「操作 → re-fetch → ProjectsLoaded」パターン。成功後の
                // ProjectsLoaded で currents_order が canonical 順に reconcile される。
                if let Some(order) = outcome.reorder_request {
                    let proxy = async_action_proxy.clone();
                    rt_handle.spawn(async move {
                        let client = crate::client::TheWorldClient::new(crate::client::default_world_port());
                        match client.reorder_projects(order).await {
                            Ok(()) => {
                                tracing::info!("reorder_projects OK");
                                // 完了 → projects 再 fetch → canonical 順で sidebar reconcile。
                                if let Ok(projects) = fetch_projects_with_ports(&client).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ProjectsLoaded(projects));
                                }
                            }
                            Err(e) => {
                                tracing::warn!("reorder_projects failed: {}", e);
                            }
                        }
                    });
                }
                // Model Q: active lane を daemon canonical に永続 (fire-and-forget、 optimistic 適用済)。
                if let Some((project_path, address)) = outcome.set_active_lane_request {
                    rt_handle.spawn(async move {
                        let client = crate::client::TheWorldClient::new(crate::client::default_world_port());
                        if let Err(e) = client.set_active_lane(project_path, address).await {
                            tracing::warn!("set_active_lane failed: {}", e);
                        }
                    });
                }
                // Phase 4-A: Performer Lane 削除要求 (sidebar の × button から)
                if let Some((project_path, address)) = outcome.delete_lane_request {
                    // F6②: 旧 TheWorldClient.delete_lane (SP 直結 reqwest) を World process-proxy
                    // ask (lane_delete) に移管。 SP port 解決は不要になり project_path を handshake で渡す。
                    // JS-side からも先 removeLane を呼ぶ (= xterm 即時 dispose、 server 反映は
                    // SP の "lanes" topic snapshot 経由で sidebar に届く)。
                    lane_js::remove_lane(&webview, &address);
                    rt_handle.spawn(async move {
                        let payload = serde_json::json!({ "address": &address });
                        match world_process_request(
                            crate::client::default_world_port(),
                            &project_path,
                            "lane_delete",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => {
                                tracing::info!(
                                    "Lane deleted: project={} address={}",
                                    project_path,
                                    address
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "lane_delete failed: project={} address={}: {}",
                                    project_path,
                                    address,
                                    e
                                );
                            }
                        }
                    });
                }
                // Lane Conductor Stand restart 要求 (sidebar の restart icon → confirm dialog から)
                if let Some((project_path, address, fresh)) = outcome.restart_lane_request {
                    // F6③: 旧 TheWorldClient.restart_lane (SP 直結 reqwest) を World process-proxy
                    // ask (lane_restart) に移管。 SP port 解決は不要、 project_path を handshake で渡す。
                    rt_handle.spawn(async move {
                        let payload = serde_json::json!({ "address": &address, "fresh": fresh });
                        match world_process_request(
                            crate::client::default_world_port(),
                            &project_path,
                            "lane_restart",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => {
                                // 新 pid / state は SP の "lanes" topic snapshot で購読側に push され、
                                // 端末は canvas channel demand 経由で新 PtySlot に再 attach し直す。
                                tracing::info!(
                                    "Lane restarted: project={} address={}",
                                    project_path,
                                    address
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "lane_restart failed: project={} address={}: {}",
                                    project_path,
                                    address,
                                    e
                                );
                            }
                        }
                    });
                }
                // Phase 3-A: Performer Lane 作成要求 (sidebar の + Add Performer から)
                // doc 24 §10 Phase 2 B-create: create は daemon-canonical (§5.3 ground は daemon が
                // provision + descriptor 所有)。 SP port 解決は不要 — daemon (:32000) に投げる。
                // PtySlot は worktree dir 作成を検知した lane_watcher が SP に依頼して spawn する
                // (= set_active_lane / reorder と同じ daemon-command パターン)。
                // doc 11 PR-C: stand 指定 を tuple 4 番目に保持 (None なら daemon-side default)。
                if let Some((project_path, name, branch, stand)) = outcome.add_performer_request {
                    let proxy = async_action_proxy.clone();
                    let name_clone = name.clone();
                    let branch_clone = branch.clone();
                    let stand_clone = stand.clone();
                    let path_clone = project_path.clone();
                    rt_handle.spawn(async move {
                        let client = TheWorldClient::new(crate::client::default_world_port());
                        match client
                            .create_performer_lane(
                                &path_clone,
                                &name_clone,
                                branch_clone.as_deref(),
                                stand_clone.as_deref(),
                            )
                            .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    "Performer Lane created (daemon): project={} name={} branch={:?}",
                                    path_clone,
                                    name_clone,
                                    branch_clone
                                );
                                // 新 Lane descriptor は daemon-canonical。 PtySlot spawn 後に
                                // SP の "lanes" topic snapshot で購読側に push される。
                                // R5: 成功通知を sidebar に push back (form を閉じる)
                                let _ = proxy.send_event(AppEvent::PerformerCreateResult {
                                    project_path: path_clone,
                                    name: name_clone,
                                    error: None,
                                });
                            }
                            Err(e) => {
                                // R5: 失敗通知を sidebar に push back (form 下に inline error 表示)。
                                // daemon からは "create_performer_lane HTTP <code>: <body>" 形式で
                                // 返ってくるので、 そのまま流す (UI 側で trim)。
                                let msg = format!("{}", e);
                                tracing::warn!(
                                    "create_performer_lane failed: project={} name={}: {}",
                                    path_clone,
                                    name_clone,
                                    msg
                                );
                                let _ = proxy.send_event(AppEvent::PerformerCreateResult {
                                    project_path: path_clone,
                                    name: name_clone,
                                    error: Some(msg),
                                });
                            }
                        }
                    });
                }

                // doc 11 PR-C / F6④: 利用可能 Stand 一覧 fetch 要求 (sidebar の + Add Performer 開閉から)。
                // 旧 SP 直結 (client.list_stands) を撤去し World process-proxy ask (`stands_list`) に移管。
                // SP port 解決が消滅し、 surface は World :32000 だけを知れば済む (L1 portless 前進)。
                if let Some(project_path) = outcome.list_stands_request {
                    let proxy = async_action_proxy.clone();
                    rt_handle.spawn(async move {
                        let (stands, error) = match world_process_request(
                            crate::client::default_world_port(),
                            &project_path,
                            "stands_list",
                            serde_json::json!({}),
                        )
                        .await
                        {
                            // SP は {stands:[...]} を返す。 stands 配列だけ Vec<StandInfo> に deserialize。
                            Ok(v) => {
                                let stands = v
                                    .get("stands")
                                    .and_then(|s| {
                                        serde_json::from_value::<Vec<crate::client::StandInfo>>(
                                            s.clone(),
                                        )
                                        .ok()
                                    })
                                    .unwrap_or_default();
                                tracing::debug!(
                                    "stands listed: project={} count={}",
                                    project_path,
                                    stands.len()
                                );
                                (stands, None)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "stands_list failed: project={}: {}",
                                    project_path,
                                    e
                                );
                                (Vec::new(), Some(e))
                            }
                        };
                        let _ = proxy.send_event(AppEvent::StandsResult {
                            project_path,
                            stands,
                            error,
                        });
                    });
                }

                // Wire inbox (doc 34 §4 V1): World "wire" channel への read-only fetch
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
                    let conn = world_conn.clone();
                    rt_handle.spawn(async move {
                        let payload = wire_fetch_payload(conn, address.clone(), ack_id).await;
                        let _ =
                            proxy.send_event(AppEvent::WireHistoryResult { address, payload });
                    });
                }

                // Sidebar File Explorer: lane workdir 配下を walk して entries を返す要求。
                // walk は I/O blocking のため main thread で実行せず、 dedicated thread に逃す。
                // 結果は AppEvent::FilesListResult で event loop に戻して sidebar に push back。
                if let Some((project_path, address)) = outcome.files_list_request {
                    match lookup_lane_cwd(&sidebar_state, &project_path, &address) {
                        Some(cwd) => {
                            let proxy = async_action_proxy.clone();
                            let addr_clone = address.clone();
                            // sync I/O (walk_dir) は spawn_blocking で Tokio runtime の
                            // dedicated blocking pool に逃す (主 worker thread を専有しない)。
                            rt_handle.spawn_blocking(move || {
                                let (entries, truncated) =
                                    crate::file_explorer::list_entries(&cwd);
                                let _ = proxy.send_event(AppEvent::FilesListResult {
                                    address: addr_clone,
                                    entries,
                                    truncated,
                                });
                            });
                        }
                        None => {
                            tracing::warn!(
                                "files:list: lane cwd unknown for path={} address={} (skip)",
                                project_path,
                                address
                            );
                        }
                    }
                }

                // Sidebar File Explorer: 選択されたファイルを Canvas (PP) に表示する要求。
                // file 読み込み + base64 (画像) も blocking thread に逃す。 結果の Content JSON は
                // AppEvent::FilesOpenResult で main thread に戻して main_view へ inject。
                if let Some((project_path, address, rel_path)) = outcome.files_open_request {
                    match lookup_lane_cwd(&sidebar_state, &project_path, &address) {
                        Some(cwd) => {
                            let proxy = async_action_proxy.clone();
                            let rel_clone = rel_path.clone();
                            // sync I/O (file read + base64 encode) は spawn_blocking で
                            // Tokio runtime の dedicated blocking pool に逃す。
                            rt_handle.spawn_blocking(move || {
                                let content =
                                    crate::file_explorer::open_file(&cwd, &rel_clone);
                                let _ = proxy.send_event(AppEvent::FilesOpenResult { content });
                            });
                        }
                        None => {
                            tracing::warn!(
                                "files:open: lane cwd unknown for path={} address={} rel_path={} (skip)",
                                project_path,
                                address,
                                rel_path
                            );
                        }
                    }
                }
                // in-app update: sidebar footer の「更新する」ボタン click 要求。
                // native 確認ダイアログ → self-update → daemon restart → relaunch を
                // 専用スレッドで起動する（event loop = main thread は塞がない）。
                if let Some(version) = outcome.update_apply_request {
                    crate::update_flow::spawn_update_flow(version);
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
            //  - "Open Developer Tools" → dev_mode == true なら webview.open_devtools()
            Event::UserEvent(AppEvent::MenuClicked(id)) => {
                if id == menu_ids.new_window {
                    // Cmd+N: 新規 vp-app process を spawn = 新しい MainWindow が独立 process で立つ。
                    // 同 EventLoop に重ねるのではなく fork-style で別 process 化することで、
                    // state 干渉ゼロ + crash isolation + multi-instance 並行開発が可能に。
                    // TheWorld daemon (port 32000) は process 横断 shared なので projects 一覧は同期。
                    //
                    // instance index を明示採番する (= 旧 bug 修正)。 採番しないと子は
                    // `VP_APP_SECONDARY=1` の backward-compat map で全員 instance 1 に落ち、
                    // `session.1.json` を共有して per-window state (active_lane / geometry) を
                    // 互いに clobber していた。 採番直後に open=true で予約 save しておくと、
                    // 連打 (= 複数 Cmd+N) でも次の採番が同 index を避ける (= race 防止)。
                    let new_idx = SessionState::next_free_secondary_index();
                    let mut reserved = SessionState::load(new_idx);
                    reserved.set_open(true);
                    reserved.save();
                    match std::env::current_exe() {
                        Ok(exe) => {
                            match std::process::Command::new(&exe)
                                // 子 process は auto-select を skip ── 元 vp-app と active_lane
                                // が衝突して両方の terminal WS が壊れるのを防ぐ。
                                // 起動後 user が手動で lane 選択するまで main_area は empty。
                                .env("VP_APP_INSTANCE", new_idx.to_string())
                                .env("VP_APP_SECONDARY", "1")
                                .spawn()
                            {
                                Ok(child) => {
                                    tracing::info!(
                                        "Cmd+N: spawned new vp-app process (pid={}, instance_index={})",
                                        child.id(),
                                        new_idx
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Cmd+N: failed to spawn new process at {}: {}",
                                        exe.display(),
                                        e
                                    );
                                    // spawn 失敗 → 予約した open=true を解放 (= 次回 primary 起動の
                                    // auto-spawn が存在しない secondary を起こすのを防ぐ)。
                                    reserved.set_open(false);
                                    reserved.save();
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Cmd+N: current_exe() failed: {}", e);
                            // 同上: spawn に至らなかったので予約を解放。
                            reserved.set_open(false);
                            reserved.save();
                        }
                    }
                } else if id == menu_ids.open_file {
                    // Cmd+O: File menu → "Open File..." accelerator。 active lane の workdir に
                    // 対して File Explorer overlay picker を sidebar に開かせる。
                    //
                    // menu accelerator は OS-level で global に発火するため、 Pane (terminal /
                    // Canvas) focus 中の Cmd+O でも到達する (これが要件の本質)。 active lane が
                    // ない (= 未選択) 時は no-op + warn log。
                    match sidebar_state.active_lane_address.as_deref() {
                        Some(addr) => {
                            // sidebar 側の `window.vpFilePicker.open(address)` を呼ぶ。
                            // 文字列は JSON エンコードして address に特殊文字が混ざっても安全に。
                            let addr_json = serde_json::to_string(addr)
                                .unwrap_or_else(|_| "\"\"".into());
                            let script = format!(
                                "window.vpFilePicker && window.vpFilePicker.open({})",
                                addr_json
                            );
                            if let Err(e) = webview.evaluate_script(&script) {
                                tracing::warn!(
                                    "Cmd+O: sidebar vpFilePicker.open inject 失敗: {}",
                                    e
                                );
                            } else {
                                tracing::info!("Cmd+O: File Explorer opened for {}", addr);
                            }
                        }
                        None => {
                            tracing::warn!("Cmd+O: active lane なし、 picker open skip");
                        }
                    }
                } else if id == menu_ids.developer_mode {
                    dev_mode = !dev_mode;
                    dev_mode_item.set_checked(dev_mode);
                    open_devtools_item.set_enabled(dev_mode);
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
                        webview.open_devtools();
                        tracing::info!("DevTools open");
                    } else {
                        tracing::warn!("Open DevTools clicked but dev_mode=false (gated)");
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
mod port_merge_tests {
    //! `fetch_projects_with_ports` の core logic (= `merge_ports_from_running`) の unit test。
    //!
    //! HTTP 呼び出しを含む `fetch_projects_with_ports` 自体は integration test の領域だが、
    //! merge logic は pure calculation なので Small Test として検証する。

    use super::*;
    use crate::client::{ProcessStatus, ProjectInfo, RunningProcess};

    fn make_project(name: &str, port: Option<u16>) -> ProjectInfo {
        ProjectInfo {
            name: name.to_string(),
            path: format!("/repos/{name}"),
            port,
            state: ProcessStatus::Running,
            ..ProjectInfo::default()
        }
    }

    fn make_running(name: &str, port: u16) -> RunningProcess {
        RunningProcess {
            project_name: name.to_string(),
            port,
        }
    }

    /// 正常系: running list の name と project name が一致した場合に port が inject される。
    #[test]
    fn merge_injects_port_for_matched_project() {
        let mut projects = vec![make_project("vp", None), make_project("creo", None)];
        let running = vec![make_running("vp", 33000), make_running("creo", 33001)];
        merge_ports_from_running(&mut projects, &running);
        assert_eq!(projects[0].port, Some(33000));
        assert_eq!(projects[1].port, Some(33001));
    }

    /// 正常系: running list に無い project は port を変更しない (= None のまま)。
    #[test]
    fn merge_leaves_unmatched_project_port_unchanged() {
        let mut projects = vec![make_project("vp", None), make_project("creo", None)];
        let running = vec![make_running("vp", 33000)]; // creo は running にない
        merge_ports_from_running(&mut projects, &running);
        assert_eq!(projects[0].port, Some(33000), "vp は inject される");
        assert_eq!(projects[1].port, None, "creo は変更されない");
    }

    /// 正常系: running list が空の場合、全 project の port は変更されない。
    /// (= list_processes がエラーの場合の degrade path と同等)
    #[test]
    fn merge_with_empty_running_leaves_all_ports_unchanged() {
        let mut projects = vec![make_project("vp", None), make_project("creo", Some(33000))];
        merge_ports_from_running(&mut projects, &[]);
        assert_eq!(projects[0].port, None);
        assert_eq!(
            projects[1].port,
            Some(33000),
            "config の static port は維持"
        );
    }

    /// 正常系: project list が空の場合、panic しない。
    #[test]
    fn merge_with_empty_projects_is_noop() {
        let mut projects: Vec<ProjectInfo> = vec![];
        let running = vec![make_running("vp", 33000)];
        merge_ports_from_running(&mut projects, &running);
        assert!(projects.is_empty());
    }

    /// 正常系: running に同名 project が複数あっても最後 (HashMap 上書き) で一意に決まる。
    /// 実際の daemon は重複を持たないが、defensive に動作することを確認。
    #[test]
    fn merge_with_duplicate_running_entry_picks_one() {
        let mut projects = vec![make_project("vp", None)];
        // HashMap なので同名は上書きされる — どちらかが選ばれれば OK
        let running = vec![make_running("vp", 33000), make_running("vp", 33001)];
        merge_ports_from_running(&mut projects, &running);
        assert!(projects[0].port.is_some(), "どちらか一方の port が入る");
    }

    /// 境界値: port が既に Some の project も running の port で上書きされる。
    /// (= TheWorld の config port より runtime port が正確)
    #[test]
    fn merge_overwrites_existing_config_port_with_runtime_port() {
        let mut projects = vec![make_project("vp", Some(9999))]; // config に static port
        let running = vec![make_running("vp", 33000)]; // runtime は別 port
        merge_ports_from_running(&mut projects, &running);
        assert_eq!(projects[0].port, Some(33000), "runtime port で上書きされる");
    }

    /// 異常系: name が大文字小文字違いの場合は match しない (= case-sensitive)。
    #[test]
    fn merge_is_case_sensitive() {
        let mut projects = vec![make_project("VP", None)];
        let running = vec![make_running("vp", 33000)];
        merge_ports_from_running(&mut projects, &running);
        assert_eq!(projects[0].port, None, "大文字小文字違いは match しない");
    }
}

#[cfg(test)]
mod main_view_asset_tests {
    //! 統合 WebView (step 3a) の単一 HTML が vp-asset:// で配信でき、sidebar を inline mount すること。
    //! Bundle font / serve handler のテストは `web_assets` module 側に分離。
    use super::*;

    /// `MAIN_VIEW_ASSETS` で統合 HTML が `vp-asset://app/index.html` から取れる。
    #[test]
    fn main_view_html_servable_via_vp_asset() {
        let html = crate::web_assets::lookup_asset("vp-asset://app/index.html", MAIN_VIEW_ASSETS);
        assert!(html.is_some(), "index.html not lookupable");
        let (bytes, ct) = html.unwrap();
        assert_eq!(ct, "text/html; charset=utf-8");
        assert_eq!(bytes, MAIN_AREA_HTML.as_bytes());
    }

    /// 統合 HTML が sidebar mount point を持ち、sidebar bundle を inline している。
    #[test]
    fn main_area_html_inlines_sidebar() {
        assert!(
            MAIN_AREA_HTML.contains(r#"id="sidebar-root""#),
            "統合 HTML に #sidebar-root mount point がない"
        );
        assert!(
            MAIN_AREA_HTML.contains("[vp-sidebar] booting"),
            "統合 HTML が sidebar bundle を inline していない (boot marker 不在)"
        );
    }
}
