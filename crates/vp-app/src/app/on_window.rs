//! event handler: **window**（OS window の lifecycle = close / resize / move / focus、shell layout、
//! slot rect、menu click と secondary window の起動）。
//!
//! 旧 `run()` の match arm を fn ごとに移したもの（doc 60 §6 6-2 PR-4、2026-09-08）。本体は arm の
//! 中身を 12 空白 dedent しただけ、arm の前のコメントは fn の doc に。
//! ⚠️ `resized` の途中 `return` は旧 arm の早期 return と同じ意味（match の後に共通処理は無い）:
//! 起動時 clamp の直後は pane bounds の更新と geometry 保存を **次の Resized に譲る**。
//!
//! 触る state: `ui.session_state` / `ui.win` / `ui.dev_mode`、`ui.sidebar_state`（read）。
//! resource: `boot.window` / `boot.webview` / `boot.menu_ids` / menu item / `boot.daemon_conn` /
//! `boot.rt_handle` / `boot.instance_index`。

use tao::dpi::LogicalSize;
use tao::event_loop::ControlFlow;

use super::boot::Boot;
use super::lane_view::resolve_repo_path_for_lane;
use super::state::{GEOMETRY_SAVE_THROTTLE, UiState};
use super::update_pane_bounds;
use super::{DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};
use crate::session_state::SessionState;
use crate::webview::push_main;

pub(super) fn close_requested(ui: &mut UiState, boot: &Boot, control_flow: &mut ControlFlow) {
    tracing::info!("Window close requested");
    // この window は **明示的に閉じられた** → 次回 primary 起動時に auto-respawn
    // しないよう自 instance file に open=false を記録する。 強制 kill (= SIGTERM /
    // crash) では CloseRequested が来ないので open=true のまま残り、 復元される。
    ui.session_state.set_open(false);
    // window geometry + 表示モード (position/size/monitor/fullscreen) も自 instance file に
    // save。 起動時に WindowBuilder + set_fullscreen で apply されて前回の配置に復元される。
    persist_window_geometry(&mut ui.session_state, &boot.window);
    if let Some(g) = ui.session_state.window_geometry() {
        tracing::info!(
            "session save [instance={}]: window geometry ({}x{} @ {},{}, monitor={:?}, mode={:?}), open=false",
            boot.instance_index,
            g.width,
            g.height,
            g.x,
            g.y,
            g.monitor.as_deref(),
            g.display_mode
        );
    }
    // open=false (+ geometry) を確実に書き出す (outer_position 失敗でも open は残す)。
    ui.session_state.save();
    *control_flow = ControlFlow::Exit;
}

pub(super) fn resized(ui: &mut UiState, boot: &Boot, size: tao::dpi::PhysicalSize<u32>) {
    let scale = boot.window.scale_factor();
    // 初回 Resized = macOS state restoration 適用後の frame。 min 未満なら
    // force-resize して default に揃える (#428 Moody Blues Issue #1 fix)。
    // 2 回目以降は user resize / clamp 由来の通常 resize として update_pane_bounds 走らせる。
    if !ui.win.initial_size_clamp_done {
        ui.win.initial_size_clamp_done = true;
        let logical = size.to_logical::<f64>(scale);
        if logical.width < MIN_WINDOW_WIDTH || logical.height < MIN_WINDOW_HEIGHT {
            tracing::info!(
                "vp-app: 起動時 window size ({}x{}) が min 未満 → {}x{} に矯正",
                logical.width,
                logical.height,
                DEFAULT_WINDOW_WIDTH,
                DEFAULT_WINDOW_HEIGHT
            );
            boot.window.set_inner_size(LogicalSize::new(
                DEFAULT_WINDOW_WIDTH,
                DEFAULT_WINDOW_HEIGHT,
            ));
            // set_inner_size → 後続 Resized event で update_pane_bounds が正しく走る。
            // この event は restoration の小 size なので bounds 更新 skip。
            return;
        }
    }
    update_pane_bounds(&boot.webview, size, scale);
    // PR #459 throttled save: resize 中も 500ms throttle で geometry + 表示モードを save。
    // 全画面 enter/exit も Resized を撃つので、 helper 内の fullscreen 判定で mode が追従する。
    let now = std::time::Instant::now();
    if now.duration_since(ui.win.last_geometry_save) > GEOMETRY_SAVE_THROTTLE {
        ui.win.last_geometry_save = now;
        persist_window_geometry(&mut ui.session_state, &boot.window);
        ui.session_state.save();
    }
}

pub(super) fn moved(ui: &mut UiState, boot: &Boot) {
    // PR #459 throttled save: window 移動中も 500ms throttle で geometry + 表示モードを save。
    // Resized と pair (= drag による size 変更だけでなく位置変更も capture)。
    let now = std::time::Instant::now();
    if now.duration_since(ui.win.last_geometry_save) > GEOMETRY_SAVE_THROTTLE {
        ui.win.last_geometry_save = now;
        persist_window_geometry(&mut ui.session_state, &boot.window);
        ui.session_state.save();
    }
}

/// window の現在の geometry + 表示モードを SessionState に write-through する (doc 30 §3.4a / §6.1)。
///
/// - **通常ウィンドウ**: 位置・サイズ・monitor・`display_mode=Windowed` を `set_window_geometry`。
/// - **全画面**: `inner_size()` は fullscreen frame を返し windowed 座標を潰すため、 `set_display_mode`
///   で mode + monitor のみ更新し、 直前の windowed 座標を保持する (全画面解除で元の窓サイズに戻せる)。
///
/// `save()` は呼ばない (caller が open flag 等とまとめて save する)。 outer_position 取得失敗時は
/// windowed 座標を更新できないので geometry を触らず返る (mode 更新は全画面時のみで別経路)。
pub(super) fn persist_window_geometry(
    session_state: &mut SessionState,
    window: &tao::window::Window,
) {
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

/// Model B (focus = 操舵ポインタ): focus 状態を追跡する。OS の key window は全プロセス間で
/// 1 つだけなので、ちょうど 1 つの instance が is_focused=true になる。これにより ROTO の
/// switch_lane broadcast を「今見ている window」だけが適用し、focus を切り替えるだけで
/// 操舵対象 window が移る (seamless)。
pub(super) fn focused(ui: &mut UiState, boot: &Boot, focused: bool) {
    ui.win.is_focused = focused;
    tracing::debug!("window focus changed: is_focused={}", focused);
    // Model B #2: focus を得た瞬間、 この window の display lane を daemon canonical の
    // active_lane に報告する。 daemon active_lane が focused window に追従 → ROTO LCD
    // follows focus (#4) が「active_lane を映すだけ」 で自動成立する。 focus-loss (false)
    // は無視 ── 次に focus を得た window が上書きするため (lane 未選択 window も skip)。
    if focused
        && let Some(address) = ui.sidebar_state.active_lane_address.clone()
        && ui.win.last_focus_reported_lane.as_deref() != Some(address.as_str())
        && let Some(path) = resolve_repo_path_for_lane(&ui.sidebar_state, &address)
    {
        // 重複報告抑止: 報告する lane を記録してから spawn。 同 lane への
        // 連続 focus event は上の guard で弾かれ、 RPC は lane 切替時のみ。
        ui.win.last_focus_reported_lane = Some(address.clone());
        let conn = boot.daemon_conn.clone();
        boot.rt_handle.spawn(async move {
            let result = match conn.control().await {
                Ok(control) => control.set_active_lane(path, address).await,
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                tracing::warn!("focus→set_active_lane failed: {}", e);
            }
        });
    }
}

pub(super) fn shell_layout(
    ui: &mut UiState,
    _boot: &Boot,
    sidebar_width: f64,
    right_sidebar_width: f64,
    sidebar_form: String,
    right_sidebar_open: bool,
) {
    // shell の形（幅 / full-slim / R 開閉）を **この instance の** session file に保存。
    // 「window をどう開いていたか」なので window_geometry と同じ箱に入れる。
    // ⚠️ 値の検証は `set_shell_layout` の clamp が持つ（webview の値を信用しない）。
    use crate::session_state::{ShellLayout, SidebarForm};
    ui.session_state.set_shell_layout(ShellLayout {
        sidebar_width,
        right_sidebar_width,
        // 未知の形は full に倒す（版ズレで「開けない sidebar」を作らない）
        sidebar_form: if sidebar_form == "slim" {
            SidebarForm::Slim
        } else {
            SidebarForm::Full
        },
        right_sidebar_open,
    });
    ui.session_state.save();
}

/// VP-100 γ-light: ResizeObserver からの slot 矩形通知を蓄積。
/// Phase 4+ で native overlay の `set_position` 同期に使う。
pub(super) fn slot_rect(
    ui: &mut UiState,
    _boot: &Boot,
    pane_id: Option<String>,
    kind: String,
    rect: crate::webview::main_area::SlotRect,
) {
    if let Some(id) = pane_id {
        ui.win.slot_rects.insert(id.clone(), rect);
        tracing::trace!("slot:rect kind={} pane={} rect={:?}", kind, id, rect);
    } else {
        tracing::trace!("slot:rect kind={} (no pane_id) rect={:?}", kind, rect);
    }
}

/// VP-100 follow-up: muda メニュー項目クリック処理
///
/// ⚠️ **"Developer Mode" の toggle は設定ページへ移設した**（doc 59 P1）。ここに
/// 残るのは dev_mode で gate される 2 項目で、gate 自体の切替は
/// `settings:save` の arm が担う（両 item の `set_enabled` もそちら）。
///  - "Open Developer Tools" → dev_mode == true なら webview.open_devtools()
pub(super) fn menu_clicked(ui: &mut UiState, boot: &Boot, id: muda::MenuId) {
    if id == boot.menu_ids.new_window {
        // Cmd+N: 新規 vp-app process を spawn = 新しい MainWindow が独立 process で立つ。
        // 同 EventLoop に重ねるのではなく fork-style で別 process 化することで、
        // state 干渉ゼロ + crash isolation + multi-instance 並行開発が可能に。
        // daemon (port 32000) は process 横断 shared なので repos 一覧は同期。
        //
        // instance index を明示採番する (= 旧 bug 修正)。 採番しないと子は
        // 全員 instance 0 相当に落ち、 `session.0.json` を共有して per-window state
        // (active_lane / geometry) を互いに clobber していた。 採番直後に open=true で
        // 予約 save しておくと、 連打 (= 複数 Cmd+N) でも次の採番が同 index を避ける
        // (= race 防止)。
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
    } else if id == boot.menu_ids.open_file {
        // File menu → "Code Browser": code pane の toggle を webview に要求。
        //
        // menu click は OS-level で発火するため、 Pane (terminal / Canvas) focus 中
        // でも到達する (これが要件の本質)。 active lane の有無は webview 側
        // （code-view.ts — lane 不在なら no-op）に委譲し、 Rust に判定を持たない
        // （旧 File Explorer は Rust 側で active 判定していたが、 判定が 2 箇所に
        // なる & sidebar_state を menu 経路が読む結合が残るため一本化した）。
        tracing::info!("File menu: code pane toggle 要求");
        push_main::code_toggle(&boot.webview);
    } else if id == boot.menu_ids.open_devtools {
        if ui.dev_mode {
            boot.webview.open_devtools();
            tracing::info!("DevTools open");
        } else {
            tracing::warn!("Open DevTools clicked but dev_mode=false (gated)");
        }
    } else if id == boot.menu_ids.reload_webview {
        // doc 48 Phase 1: HMR loop の reload 側。VP_WEBVIEW_DEV 設定時は
        // reload で *.bundle.js が disk から fresh に取り直される。
        if ui.dev_mode {
            if let Err(e) = boot.webview.evaluate_script("location.reload()") {
                tracing::warn!("Reload WebView 失敗: {}", e);
            } else {
                tracing::info!("Reload WebView (location.reload)");
            }
        } else {
            tracing::warn!("Reload WebView clicked but dev_mode=false (gated)");
        }
    } else {
        tracing::debug!("MenuClicked: 未処理の id = {:?}", id);
    }
}
