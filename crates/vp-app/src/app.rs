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
use crate::pane::{ActivitySnapshot, ProcessPaneState, SidebarState};
use crate::settings::Settings;
use crate::terminal::{self, AppEvent};

/// Sidebar の固定幅 (LogicalPixel)
const SIDEBAR_WIDTH: f64 = 280.0;

/// 開発者モード判定 (起動時の初期値計算に使用、runtime 切替は menu 経由)
///
/// 優先順位 (1Password 風の挙動):
/// 1. `VP_DEVELOPER_MODE` env var が `1`/`true`/`yes`/`on` → 強制 ON
/// 2. `VP_DEVELOPER_MODE` env var が `0`/`false`/`no`/`off` → 強制 OFF
/// 3. Settings ファイル (`~/.config/vantage/vp-app.toml` 等) の `developer_mode` フィールド
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

/// VP-95: Sidebar accordion HTML
///
/// 上から:
/// 1. **Widget slot** (Activity / Stand status — `/api/health` + `/api/world/processes`)
/// 2. **Projects accordion** (project ヘッダー ▶/▼ + 子 pane 一覧)
///
/// state は `window.renderSidebarState(state)` で Rust → JS に push される。
/// クリック操作は `window.ipc.postMessage(JSON)` で Rust に送信:
///   - `{"t":"process:toggle","path":"...","expanded":true|false}`
///   - `{"t":"lane:select","path":"...","address":"<project>/lead"}`
///   - `{"t":"process:add"}` / `{"t":"process:clone","url":"..."}`
const SIDEBAR_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="ja" data-theme="contrast-dark">
<head><meta charset="utf-8"><style>"#,
    include_str!("../assets/creo-tokens.css"),
    r#"</style><style>"#,
    include_str!("../assets/creo-components.css"),
    r#"</style><style>
  html,body{margin:0;height:100%;background:var(--color-surface-bg-subtle);color:var(--color-text-primary);font-family:var(--typography-family-sans);font-size:13px;overflow:hidden;}
  body{display:flex;flex-direction:column;height:100%;}

  /* Widget slot (top) */
  .widget-slot{padding:10px 12px;border-bottom:1px solid var(--color-surface-border,#1f2233);background:var(--color-surface-bg-base);}
  .widget-slot .widget-title{font-size:10px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:.08em;display:flex;justify-content:space-between;align-items:center;margin-bottom:6px;}
  .widget-slot .stat{display:flex;justify-content:space-between;font-size:11px;padding:2px 0;color:var(--color-text-secondary);}
  .widget-slot .stat .label{color:var(--color-text-tertiary);}
  .widget-slot .stat .value{font-weight:500;color:var(--color-text-primary);font-variant-numeric:tabular-nums;}

  /* Projects accordion */
  .processes-section{flex:1;overflow-y:auto;padding:6px 0;}
  .processes-section .section-header{padding:10px 16px 6px;font-size:10px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:.08em;display:flex;justify-content:space-between;align-items:center;}

  /* Bottom Add ボタン (single trigger) と展開後の sub-actions */
  .add-trigger{margin:6px 12px 10px;padding:6px 8px;border-radius:var(--radius-sm,6px);cursor:pointer;color:var(--color-text-tertiary);font-size:11px;text-align:center;border:1px dashed var(--color-surface-border,#1f2233);background:transparent;transition:background .12s ease,color .12s ease,border-color .12s ease;user-select:none;}
  .add-trigger:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-secondary);border-color:var(--color-text-tertiary);}
  .add-trigger.expanded{color:var(--color-text-secondary);border-color:var(--color-text-tertiary);background:var(--color-surface-bg-emphasis);}

  /* sub-actions (Select / Clone) — 展開時に max-height + opacity トランジション */
  .add-actions{margin:0 12px 10px;display:flex;flex-direction:column;gap:4px;max-height:0;opacity:0;overflow:hidden;transition:max-height .22s ease, opacity .22s ease, margin-top .22s ease;margin-top:0;pointer-events:none;}
  .add-actions.expanded{max-height:120px;opacity:1;margin-top:-6px;pointer-events:auto;}
  .add-action{padding:6px 10px;border-radius:var(--radius-sm,6px);cursor:pointer;color:var(--color-text-tertiary);font-size:11px;text-align:left;background:var(--color-surface-bg-subtle);border:1px solid transparent;transition:background .12s ease,color .12s ease,border-color .12s ease,transform .15s ease;user-select:none;display:flex;align-items:center;gap:6px;transform:translateY(-2px);}
  .add-actions.expanded .add-action{transform:translateY(0);}
  .add-action:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);border-color:var(--color-surface-border,#1f2233);}
  .add-action .icon{width:16px;text-align:center;color:var(--color-brand-primary);font-size:12px;}

  /* Clone inline form — sidebar 内で展開する form (modal でなく inline) */
  .vp-clone-inline{margin:0 12px 10px;display:flex;flex-direction:column;gap:6px;max-height:0;opacity:0;overflow:hidden;transition:max-height .22s ease, opacity .22s ease, margin-top .22s ease;margin-top:0;pointer-events:none;}
  .vp-clone-inline.expanded{max-height:140px;opacity:1;margin-top:-6px;pointer-events:auto;}
  .vp-clone-inline label{font-size:10px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:.06em;}
  .vp-clone-inline input{width:100%;padding:6px 8px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:inherit;font-size:12px;box-sizing:border-box;}
  .vp-clone-inline input:focus{outline:none;border-color:var(--color-brand-primary);}
  .vp-clone-inline .actions{display:flex;justify-content:flex-end;gap:6px;}
  .vp-clone-inline button{padding:4px 10px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:transparent;color:var(--color-text-secondary);cursor:pointer;font-size:11px;font-family:inherit;transition:background .12s ease,color .12s ease;}
  .vp-clone-inline button:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);}
  .vp-clone-inline button.primary{background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);border-color:var(--color-brand-primary-subtle);}
  .vp-clone-inline button.primary:hover{background:var(--color-brand-primary);color:var(--color-surface-bg-base);}

  /* creo-accordion を sidebar 用に override (default の bordered card 風 → flush) */
  .processes-section .creo-accordion{margin:0 6px 2px;background:transparent;border:none;border-radius:var(--radius-sm,6px);overflow:visible;}
  .processes-section .creo-accordion-summary{padding:6px 8px;min-height:auto;font-size:13px;border-radius:var(--radius-sm,6px);}
  .processes-section .creo-accordion-summary:hover{background:var(--color-surface-bg-emphasis);}
  .processes-section .creo-accordion-summary::before{font-size:9px;color:var(--color-text-tertiary);width:10px;}
  .processes-section .creo-accordion-title{font-weight:500;font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
  .processes-section .creo-accordion-content{padding:2px 0 4px 18px;}
  .processes-section .creo-accordion-content > * + * {margin-top:0;}

  /* Architecture v4: Lane row (Project → Lane → Stand 階層の中段) */
  .vp-lane-row{display:flex;align-items:center;gap:6px;padding:5px 8px 5px 14px;border-radius:var(--radius-sm,6px);cursor:pointer;transition:background .1s ease;font-size:12px;}
  .vp-lane-row:hover{background:var(--color-surface-bg-emphasis);}
  .vp-lane-row.active{background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);}
  .vp-lane-row .icon{width:18px;text-align:center;font-size:13px;font-family:var(--typography-family-icon);}
  .vp-lane-row .label{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
  .vp-lane-row .state{font-size:10px;}

  /* Stand row (Lane の中身、 HD/TH 等) — read-only 表示 */
  .vp-stand-row{display:flex;align-items:center;gap:6px;padding:2px 8px 2px 34px;font-size:11px;color:var(--color-text-tertiary);}
  .vp-stand-row .icon{width:18px;text-align:center;font-family:var(--typography-family-icon);}
  .vp-stand-row .label{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}

  /* SP 未起動 / Lane loading 等の hint 表示 */
  .vp-empty-hint{padding:6px 12px 6px 14px;font-size:11px;color:var(--color-text-tertiary);font-style:italic;}

  .empty,.loading,.error{padding:8px 16px;color:var(--color-text-tertiary);font-style:italic;font-size:12px;}
</style></head>
<body>
  <div class="widget-slot" id="widget-slot">
    <div class="widget-title">Activity <span class="creo-badge" data-size="sm" id="world-badge">…</span></div>
    <div class="stat"><span class="label">Version</span><span class="value" id="world-version">—</span></div>
    <div class="stat"><span class="label">Started</span><span class="value" id="world-uptime">—</span></div>
    <div class="stat"><span class="label">Projects</span><span class="value" id="proj-count">0</span></div>
    <div class="stat"><span class="label">Processes</span><span class="value" id="proc-count">0</span></div>
  </div>
  <div class="projects-section">
    <div class="section-header">Projects</div>
    <div id="projects"><div class="loading">読込中…</div></div>
    <div class="add-trigger" id="add-trigger" title="Add Project">＋ Add</div>
    <div class="add-actions" id="add-actions">
      <div class="add-action" id="select-project-btn" title="Select existing folder"><span class="icon">📁</span> Select Folder</div>
      <div class="add-action" id="clone-project-btn" title="Clone repository from URL"><span class="icon">🌱</span> Clone Repository</div>
    </div>
  </div>
  <!-- Clone inline form (sidebar 内 expand、modal でなく inline) -->
  <div class="vp-clone-inline" id="clone-inline">
    <label for="clone-url">Repository URL</label>
    <input type="text" id="clone-url" placeholder="https://github.com/user/repo.git" />
    <div class="actions">
      <button type="button" id="clone-cancel">Cancel</button>
      <button type="button" class="primary" id="clone-confirm">Clone</button>
    </div>
  </div>
<script>
  // Rust から push される sidebar state を保持
  let state = null;
  let pendingState = null;
  let domReady = false;

  // ipc 送信 wrapper (window.ipc は wry が提供)
  function send(msg) {
    if (window.ipc && window.ipc.postMessage) {
      window.ipc.postMessage(JSON.stringify(msg));
    }
  }

  // unix 時刻 ISO → "Xh Ym ago" 風文字列
  function formatStartedAt(iso) {
    if (!iso) return '—';
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return iso;
    const sec = Math.max(0, Math.floor((Date.now() - t) / 1000));
    if (sec < 60) return sec + 's ago';
    const m = Math.floor(sec / 60);
    if (m < 60) return m + 'm ago';
    const h = Math.floor(m / 60);
    const rem = m % 60;
    return h + 'h ' + rem + 'm ago';
  }

  function renderActivity(activity) {
    const badge = document.getElementById('world-badge');
    const ver = document.getElementById('world-version');
    const upt = document.getElementById('world-uptime');
    const pc = document.getElementById('proj-count');
    const rc = document.getElementById('proc-count');
    if (!badge || !ver || !upt || !pc || !rc) return;
    if (activity && activity.world_online) {
      badge.textContent = 'online';
      badge.setAttribute('data-variant', 'success');
    } else {
      badge.textContent = 'offline';
      badge.removeAttribute('data-variant');
    }
    ver.textContent = (activity && activity.world_version) || '—';
    upt.textContent = formatStartedAt(activity && activity.world_started_at);
    pc.textContent = String((activity && activity.project_count) || 0);
    rc.textContent = String((activity && activity.running_process_count) || 0);
  }

  function renderProjects(projects) {
    const root = document.getElementById('projects');
    if (!root) return;
    root.innerHTML = '';
    if (!projects || projects.length === 0) {
      root.innerHTML = '<div class="empty">(no projects)</div>';
      return;
    }
    for (const p of projects) {
      // creo-accordion: native <details> ベース。expand/collapse + chevron + ARIA は creo-ui 側 CSS。
      const proj = document.createElement('details');
      proj.className = 'creo-accordion';
      if (p.expanded) proj.setAttribute('open', '');
      // 'toggle' イベントで Rust に永続化 IPC を送る (native toggle は即時、IPC は state 同期用)
      proj.addEventListener('toggle', () => {
        send({t: 'process:toggle', path: p.path, expanded: proj.open});
      });

      const summary = document.createElement('summary');
      summary.className = 'creo-accordion-summary';
      // Sprint 2 (Idea 1 tree visualization): ProcessKind icon (Architecture v4)
      const kindIcon = document.createElement('span');
      kindIcon.className = 'icon';
      kindIcon.style.cssText = 'margin-right:6px;';
      kindIcon.textContent = processKindIcon(p.kind || 'runtime');
      summary.appendChild(kindIcon);
      const title = document.createElement('span');
      title.className = 'creo-accordion-title';
      title.textContent = p.name;
      summary.appendChild(title);
      // Sprint 2: ProcessState badge (running 🟢 / dead 🔴 等)
      const stateBadge = document.createElement('span');
      stateBadge.className = 'state';
      stateBadge.style.cssText = 'margin-left:auto;font-size:10px;';
      stateBadge.textContent = processStateMark(p.state);
      summary.appendChild(stateBadge);
      proj.appendChild(summary);

      const content = document.createElement('div');
      content.className = 'creo-accordion-content';

      // Architecture v4 (mem_1CaTpCQH8iLJ2PasRcPjHv): Project → Lane → Stand に統一。
      // 旧 vp-app local の Pane data model は撤去、 SP `/api/lanes` が SSOT。
      const lanes = (state && state.lanes_by_project && state.lanes_by_project[p.path]) || [];
      const isRunning = p.state === 'running';
      const activeAddr = (state && state.active_lane_address) || null;

      if (!isRunning) {
        // SP 未起動 — accordion を開いた瞬間 (= toggle expand=true) に Rust 側が auto-spawn する。
        // user は何もせずに待つだけで OK (mem: TheWorld が SP lifecycle を持つ Architecture v4)。
        const hint = document.createElement('div');
        hint.className = 'vp-empty-hint';
        hint.style.cssText = 'padding:6px 12px 6px 20px;font-size:11px;color:var(--color-text-tertiary);font-style:italic;';
        hint.textContent = p.expanded
          ? '⏳ SP starting…'
          : '💤 SP stopped — open to spawn';
        content.appendChild(hint);
      } else if (lanes.length === 0) {
        // SP は running だが Lane fetch 結果がまだ / 取得失敗
        const loading = document.createElement('div');
        loading.className = 'vp-empty-hint';
        loading.style.cssText = 'padding:6px 12px 6px 20px;font-size:11px;color:var(--color-text-tertiary);font-style:italic;';
        loading.textContent = '📡 loading lanes…';
        content.appendChild(loading);
      } else {
        for (const lane of lanes) {
          const addr = laneAddressKey(lane);
          const isActive = activeAddr && activeAddr === addr;

          // Lane row (📍 Session = Lead/Worker)
          const row = document.createElement('div');
          row.className = 'vp-lane-row' + (isActive ? ' active' : '');
          const icon = document.createElement('span');
          icon.className = 'icon';
          icon.textContent = processKindIcon('session');
          const label = document.createElement('span');
          label.className = 'label';
          label.textContent = laneLabel(lane);
          const stateMark = document.createElement('span');
          stateMark.className = 'state';
          stateMark.textContent = processStateMark(lane.state);
          row.appendChild(icon);
          row.appendChild(label);
          row.appendChild(stateMark);
          row.addEventListener('click', (e) => {
            e.stopPropagation();
            send({t: 'lane:select', path: p.path, address: addr});
          });
          content.appendChild(row);

          // Stand child row (🦾 Worker = Lane の中身、 HD/TH...)
          if (lane.stand) {
            const childRow = document.createElement('div');
            childRow.className = 'vp-stand-row';
            const childIcon = document.createElement('span');
            childIcon.className = 'icon';
            childIcon.textContent = processKindIcon('worker');
            const childLabel = document.createElement('span');
            childLabel.className = 'label';
            childLabel.textContent = standDisplayName(lane.stand) + ' ' + laneStandIcon(lane.stand);
            childRow.appendChild(childIcon);
            childRow.appendChild(childLabel);
            content.appendChild(childRow);
          }
        }

        // Phase 3 で実装予定: + Add Worker (POST /api/lanes)。 Phase 1 は read-only。
      }

      proj.appendChild(content);
      root.appendChild(proj);
    }
  }

  // Sprint 2 (Idea 1, Architecture v4): ProcessKind / ProcessState 表示用 helpers
  function processKindIcon(kind) {
    switch (kind) {
      case 'supervisor': return '👑';
      case 'runtime': return '⭐';
      case 'session': return '📍';
      case 'worker': return '🦾';
      default: return '·';
    }
  }
  function processStateMark(s) {
    switch (s) {
      case 'running': return '🟢';
      case 'spawning': return '🟡';
      case 'idle': return '🔵';
      case 'working': return '⚙';
      case 'pausing': return '⏸';
      case 'exiting': return '🟠';
      case 'dead': return '🔴';
      default: return '';
    }
  }
  // Sprint 2-2: Stand display name (Architecture v4 metaphor)
  function standDisplayName(stand) {
    switch (stand) {
      case 'heavens_door': return "Heaven's Door";
      case 'the_hand': return 'The Hand';
      case 'paisley_park': return 'Paisley Park';
      case 'gold_experience': return 'Gold Experience';
      case 'hermit_purple': return 'Hermit Purple';
      default: return stand || '';
    }
  }
  // Phase A4-3b-2: Lane 行表示用 helpers (Stand icon は Worker child row で reuse)
  function laneStandIcon(stand) {
    switch (stand) {
      case 'heavens_door': return '📖';
      case 'the_hand': return '✋';
      default: return '·';
    }
  }
  function laneLabel(lane) {
    if (!lane) return '';
    const kind = lane.kind || (lane.address && lane.address.kind);
    if (kind === 'lead') return 'Lead';
    if (kind === 'worker') return 'Worker: ' + (lane.name || (lane.address && lane.address.name) || '?');
    return kind || '';
  }
  // Lane address を Display 形 ("<project>/lead" / "<project>/worker/<name>") に変換。
  // Rust 側 `lane_address_key()` と完全一致させる (active selection の比較に使うため)。
  function laneAddressKey(lane) {
    if (!lane || !lane.address) return '';
    const a = lane.address;
    if (a.kind === 'worker') {
      return a.project + '/worker/' + (a.name || '<unnamed>');
    }
    return a.project + '/' + (a.kind || 'lead');
  }

  function applyState(s) {
    if (!domReady) { pendingState = s; return; }
    state = s;
    renderActivity(s.activity);
    renderProjects(s.processes);
  }

  // 起動初期エラー (TheWorld 未接続) 表示
  function applyError(msg) {
    if (!domReady) { pendingState = {projects: null, _error: msg, activity: {world_online:false}}; return; }
    renderActivity({world_online: false});
    const root = document.getElementById('projects');
    if (root) root.innerHTML = '<div class="error">' + (msg || 'TheWorld 未接続') + '</div>';
  }

  window.renderSidebarState = applyState;
  window.renderError = applyError;

  // uptime を 1 秒ごとに自更新 (state.activity.world_started_at から計算)
  setInterval(() => {
    if (state && state.activity) {
      const upt = document.getElementById('world-uptime');
      if (upt) upt.textContent = formatStartedAt(state.activity.world_started_at);
    }
  }, 1000);

  window.addEventListener('DOMContentLoaded', () => {
    domReady = true;
    if (pendingState !== null) {
      if (pendingState._error) {
        applyError(pendingState._error);
      } else {
        applyState(pendingState);
      }
      pendingState = null;
    }
    // VP-100 follow-up: 「+ Add」展開 → Select / Clone のサブアクション
    const addTrigger = document.getElementById('add-trigger');
    const addActions = document.getElementById('add-actions');
    function setAddExpanded(open) {
      if (!addTrigger || !addActions) return;
      addTrigger.classList.toggle('expanded', open);
      addActions.classList.toggle('expanded', open);
    }
    function toggleAdd() {
      setAddExpanded(!(addActions && addActions.classList.contains('expanded')));
    }
    function collapseAdd() { setAddExpanded(false); }
    if (addTrigger) addTrigger.addEventListener('click', toggleAdd);

    // Select Folder
    const selectBtn = document.getElementById('select-project-btn');
    if (selectBtn) selectBtn.addEventListener('click', () => {
      collapseAdd();
      send({t: 'process:add'});
    });

    // Clone Repository — sidebar 内 inline expand form で URL を受け取る
    const cloneBtn = document.getElementById('clone-project-btn');
    const cloneInline = document.getElementById('clone-inline');
    const cloneInput = document.getElementById('clone-url');
    const cloneCancel = document.getElementById('clone-cancel');
    const cloneConfirm = document.getElementById('clone-confirm');
    function openCloneInline() {
      if (!cloneInline) return;
      cloneInput.value = '';
      cloneInline.classList.add('expanded');
      setTimeout(() => cloneInput && cloneInput.focus(), 50);
    }
    function closeCloneInline() {
      if (!cloneInline) return;
      cloneInline.classList.remove('expanded');
    }
    function submitClone() {
      const url = (cloneInput && cloneInput.value || '').trim();
      if (!url) return;
      send({t: 'process:clone', url: url});
      closeCloneInline();
    }
    if (cloneBtn) cloneBtn.addEventListener('click', () => {
      collapseAdd();
      openCloneInline();
    });
    if (cloneCancel) cloneCancel.addEventListener('click', closeCloneInline);
    if (cloneConfirm) cloneConfirm.addEventListener('click', submitClone);
    if (cloneInput) {
      cloneInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') { e.preventDefault(); submitClone(); }
        else if (e.key === 'Escape') { e.preventDefault(); closeCloneInline(); }
      });
    }
    // 別の場所をクリックしたら add actions を畳む
    document.addEventListener('click', (e) => {
      if (!addTrigger || !addActions) return;
      if (!addActions.classList.contains('expanded')) return;
      const t = e.target;
      if (addTrigger.contains(t) || addActions.contains(t)) return;
      collapseAdd();
    });
  });
</script>
</body>
</html>"#
);

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

/// Settings + 既存プロジェクトから picker の初期ディレクトリを解決。
///
/// 優先順位:
/// 1. `Settings.default_project_root` が指定されていて存在する → それ
/// 2. **既存登録プロジェクトの親ディレクトリ** (= "vp のレポジトリホーム" 推定)
///    `sidebar_state.processes` の最初の project の parent dir。多くは
///    `~/repos` か `C:\Users\<user>\repos` 等の repos 親。
/// 3. `~/repos` が存在する → それ
/// 4. `~` (home) → それ
/// 5. それ以外 → `None`
fn resolve_default_project_root(
    settings: &Settings,
    sidebar_state: &SidebarState,
) -> Option<std::path::PathBuf> {
    // 1. Settings explicit
    if let Some(s) = &settings.default_project_root {
        let p = std::path::PathBuf::from(s);
        if p.exists() {
            return Some(p);
        }
        tracing::warn!(
            "default_project_root が設定されているが存在しない: {} → 推定にフォールバック",
            s
        );
    }
    // 2. 既存 project の parent dir = "vp レポジトリホーム" 推定
    for proj in &sidebar_state.processes {
        let path = std::path::PathBuf::from(&proj.path);
        if let Some(parent) = path.parent()
            && parent.exists()
        {
            tracing::debug!(
                "default picker dir 推定: {} (project '{}' の parent)",
                parent.display(),
                proj.name
            );
            return Some(parent.to_path_buf());
        }
    }
    // 3. ~/repos fallback
    let home = dirs::home_dir()?;
    let repos = home.join("repos");
    if repos.exists() {
        Some(repos)
    } else {
        Some(home)
    }
}

/// VP-100 follow-up: 「+ Add Project」クリック時の native folder picker + API 呼出。
///
/// rfd の picker は blocking なので別スレッドで実行。folder 選択後:
/// 1. `client.add_project(name, path)` を呼ぶ (TheWorld の `/api/world/projects` POST)
/// 2. 成功なら `client.list_projects()` で再取得 → `AppEvent::ProcessesLoaded`
///
/// User キャンセル / API 失敗時は何もしない (sidebar は変化しない)。
/// `initial_dir` が `Some` なら picker の初期表示ディレクトリに設定。
fn spawn_add_project_picker(
    proxy: EventLoopProxy<AppEvent>,
    initial_dir: Option<std::path::PathBuf>,
) {
    let _ = thread::Builder::new()
        .name("add-project-picker".into())
        .spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("プロジェクトフォルダを選択");
            if let Some(d) = initial_dir.as_ref() {
                dialog = dialog.set_directory(d);
            }
            let folder = match dialog.pick_folder() {
                Some(p) => p,
                None => {
                    tracing::debug!("process:add canceled by user");
                    return;
                }
            };
            let name = folder
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".to_string());
            let path = folder.to_string_lossy().into_owned();
            tracing::info!("process:add picker → name={} path={}", name, path);

            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("add-project tokio runtime 作成失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let client = TheWorldClient::default();
                if let Err(e) = client.add_project(&name, &path).await {
                    tracing::warn!("add_project API 失敗: {}", e);
                    return;
                }
                tracing::info!("add_project 成功 → projects 再 fetch");
                match client.list_projects().await {
                    Ok(projects) => {
                        let _ = proxy.send_event(AppEvent::ProcessesLoaded(projects));
                    }
                    Err(e) => {
                        tracing::warn!("add_project 後の list_projects 失敗: {}", e);
                    }
                }
            });
        });
}

/// VP-100 follow-up: 「+ Clone Repository」クリック時の git clone + API 呼出。
///
/// 1. `git clone <url> <default_root>/<repo_name>` を実行
/// 2. 成功なら `add_project` で TheWorld に register
/// 3. `list_projects` で再取得 → `AppEvent::ProcessesLoaded`
///
/// `default_root` が `None` の時は何もしない (default_project_root が解決できないケース)。
/// git バイナリが PATH に無い場合も spawn 失敗で終わる。
fn spawn_clone_project(
    proxy: EventLoopProxy<AppEvent>,
    url: String,
    default_root: Option<std::path::PathBuf>,
) {
    let Some(default_root) = default_root else {
        tracing::warn!("process:clone but default_project_root is unresolved (set in settings)");
        return;
    };
    let repo_name = derive_repo_name(&url);
    let _ = thread::Builder::new()
        .name("clone-project".into())
        .spawn(move || {
            let target = default_root.join(&repo_name);
            tracing::info!("git clone {} {}", url, target.display());
            let status = std::process::Command::new("git")
                .arg("clone")
                .arg(&url)
                .arg(&target)
                .status();
            let success = match status {
                Ok(s) if s.success() => true,
                Ok(s) => {
                    tracing::warn!("git clone failed: exit code {:?}", s.code());
                    false
                }
                Err(e) => {
                    tracing::warn!("git clone spawn 失敗 (git PATH 確認): {}", e);
                    false
                }
            };
            if !success {
                let _ = notify_rust::Notification::new()
                    .summary("Vantage Point")
                    .body(&format!("Clone 失敗: {}", url))
                    .show();
                return;
            }
            // Register
            let path_str = target.to_string_lossy().into_owned();
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("clone-project tokio runtime 失敗: {}", e);
                    return;
                }
            };
            rt.block_on(async move {
                let client = TheWorldClient::default();
                if let Err(e) = client.add_project(&repo_name, &path_str).await {
                    tracing::warn!("clone 後の add_project 失敗: {}", e);
                    return;
                }
                tracing::info!("clone + add_project 成功 → projects 再 fetch");
                match client.list_projects().await {
                    Ok(projects) => {
                        let _ = proxy.send_event(AppEvent::ProcessesLoaded(projects));
                    }
                    Err(e) => {
                        tracing::warn!("list_projects 失敗: {}", e);
                    }
                }
            });
        });
}

/// URL から repo 名を推定する (`/` or `:` の最後の segment、`.git` 末尾を除去)
///
/// 例:
/// - `https://github.com/user/repo.git` → `repo`
/// - `git@github.com:user/repo.git` → `repo`
/// - `https://gitlab.com/group/sub/repo` → `repo`
fn derive_repo_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("project")
        .trim_end_matches(".git");
    if last.is_empty() {
        "project".to_string()
    } else {
        last.to_string()
    }
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
/// これにより handler 側で `if let Some(port) = p.port { spawn_lanes_fetch(...) }` が動く経路完成。
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
                        // ProcessInfo に port + state を merge
                        for p in &mut processes {
                            if let Some(&port) = port_by_name.get(&p.name) {
                                p.port = Some(port);
                                p.state = crate::client::ProcessState::Running;
                            } else {
                                // running list 未掲載 = stopped (Architecture v4: ProcessState::Dead で代用、Sprint 後半で Stopped 追加検討)
                                p.state = crate::client::ProcessState::Dead;
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

/// Phase A4-3b: SP (33000+) の `/api/lanes` を別スレッドで fetch。
///
/// 成功/失敗を `AppEvent::LanesLoaded` / `LanesError` として main thread に通知。
/// ProjectsLoaded handler が各 project の SP に対してこの fn を呼び、
/// sidebar_state.lanes_by_project に保持する。
///
/// 関連 memory: mem_1CaSugEk1W2vr5TAdfDn5D (多 scope: Lane scope は SP per project)
fn spawn_lanes_fetch(proxy: EventLoopProxy<AppEvent>, process_path: String, sp_port: u16) {
    let _ = thread::Builder::new()
        .name(format!("lanes-fetch-{}", sp_port))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = proxy.send_event(AppEvent::LanesError {
                        process_path,
                        message: format!("tokio runtime: {}", e),
                    });
                    return;
                }
            };
            rt.block_on(async {
                let client = TheWorldClient::new(sp_port);
                match client.list_lanes().await {
                    Ok(lanes) => {
                        tracing::info!(
                            "LanesLoaded: project={} port={} ({} lanes)",
                            process_path,
                            sp_port,
                            lanes.len()
                        );
                        let _ = proxy.send_event(AppEvent::LanesLoaded {
                            process_path,
                            lanes,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(
                            "list_lanes failed: project={} port={}: {}",
                            process_path,
                            sp_port,
                            e
                        );
                        let _ = proxy.send_event(AppEvent::LanesError {
                            process_path,
                            message: e.to_string(),
                        });
                    }
                }
            });
        });
}

/// Phase 2.5 (per-Lane instance): main_view の JS API を呼ぶ helper 群。
/// xterm.js + WebSocket は **JS-side で per-Lane に管理** され、 Rust は thin trigger を出すだけ。
mod lane_js {
    use wry::WebView;

    /// JS string literal にする (基本 ASCII safe な path 想定だが、 念のため `'` `\\` を escape)
    fn js_str(s: &str) -> String {
        let escaped = s.replace('\\', "\\\\").replace('\'', "\\'");
        format!("'{}'", escaped)
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
                        // その流れで spawn_lanes_fetch が走って sidebar に Lane が出る。
                        // ここで明示的に再 fetch trigger する必要はない (polling が 5s で拾う)。
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
                            for p in &mut processes {
                                if let Some(&port) = port_by_name.get(&p.name) {
                                    p.port = Some(port);
                                    p.state = crate::client::ProcessState::Running;
                                } else {
                                    p.state = crate::client::ProcessState::Dead;
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

/// Architecture v4: sidebar の active Lane に応じて main area の表示 kind を切替。
///
/// 現状 (Phase 1): Lane が選択されていれば main area を `kind="terminal"` に切替、
/// なければ `kind=None` で empty 状態を出す。
/// Lane address ごとの terminal 接続切替 (Lane の WS terminal に bind) は Phase 2 で。
fn push_active_lane(main_view: &WebView, state: &SidebarState) {
    let info = match state.active_lane_address.as_deref() {
        Some(addr) => ActivePaneInfo {
            kind: Some("terminal"),
            pane_id: Some(addr),
            preview_url: None,
        },
        None => ActivePaneInfo {
            kind: None,
            pane_id: None,
            preview_url: None,
        },
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
    /// active Lane が変わったか (true なら push_active_lane を呼ぶ)
    active_changed: bool,
    /// SP auto-spawn が必要な project (= 「Current」 になった dead な project)。
    /// `(name, path)` を返し、 caller が `spawn_sp_start` を呼ぶ。
    /// dedup は caller の `sp_spawn_triggered: HashSet<String>` (path key) で行う。
    sp_spawn_request: Option<(String, String)>,
}

/// sidebar webview から IPC で受け取った JSON を解釈し、`SidebarState` を mutate。
fn handle_sidebar_ipc(msg: &str, state: &mut SidebarState) -> SidebarIpcOutcome {
    let mut out = SidebarIpcOutcome::default();
    let parsed: serde_json::Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("sidebar IPC JSON パース失敗: {}", e);
            return out;
        }
    };
    let t = parsed.get("t").and_then(|v| v.as_str()).unwrap_or("");
    let path = parsed.get("path").and_then(|v| v.as_str()).unwrap_or("");

    match t {
        "process:toggle" => {
            // VP-101 Phase A1.b: native <details> が IPC で `expanded` の新状態を渡してくる。
            // DOM は既に user click で toggle 済なので、Rust state を silently sync するだけ。
            // `out.changed` は立てない (rebuild すると flash する)。
            //
            // Architecture v4 auto-spawn: expand=true で state==dead の project は
            // 「user が current として designate した dead project」 として扱い、
            // SP auto-spawn を request する (mem_1CaTpCQH8iLJ2PasRcPjHv: SP lifecycle は TheWorld 責務)。
            if let Some(p) = state.processes.iter_mut().find(|p| p.path == path) {
                let new_state = parsed
                    .get("expanded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(!p.expanded);
                if p.expanded != new_state {
                    p.expanded = new_state;
                    tracing::debug!(
                        "process:toggle {} → expanded={} (silent sync)",
                        path,
                        p.expanded
                    );
                }
                if new_state && p.state.as_deref() == Some("dead") {
                    out.sp_spawn_request = Some((p.name.clone(), p.path.clone()));
                }
            }
        }
        "lane:select" => {
            // Architecture v4: Lane row click → `address` (Display 形 "<project>/lead") を受信
            let address = parsed.get("address").and_then(|v| v.as_str()).unwrap_or("");
            if address.is_empty() {
                tracing::warn!("lane:select with empty address: {}", msg);
                return out;
            }
            // 念のため: 該当 project の lanes_by_project に address が存在することを確認
            let lanes_exist = state
                .lanes_by_project
                .get(path)
                .map(|lanes| {
                    lanes
                        .iter()
                        .any(|l| lane_address_key(&l.address) == address)
                })
                .unwrap_or(false);
            if !lanes_exist {
                tracing::warn!(
                    "lane:select 対象 lane が見つからない: path={} address={}",
                    path,
                    address
                );
                return out;
            }
            if state.active_lane_address.as_deref() != Some(address) {
                state.active_lane_address = Some(address.to_string());
                tracing::info!("lane:select {} address={}", path, address);
                out.changed = true;
                out.active_changed = true;
            }
        }
        other => {
            tracing::debug!("sidebar IPC: 未知の type {:?}", other);
        }
    }
    out
}

/// Lane address (LaneAddressWire) を Display 形の文字列にする。
///
/// 形式: `"<project>/lead"` / `"<project>/worker/<name>"`
/// JS 側 `laneAddressKey()` と完全に一致させる必要がある (active 比較に使うため)。
fn lane_address_key(addr: &crate::client::LaneAddressWire) -> String {
    match (addr.kind.as_str(), addr.name.as_deref()) {
        ("worker", Some(n)) => format!("{}/worker/{}", addr.project, n),
        ("worker", None) => format!("{}/worker/<unnamed>", addr.project),
        _ => format!("{}/{}", addr.project, addr.kind),
    }
}

/// App のエントリポイント
pub fn run() -> anyhow::Result<()> {
    // VP-100 follow-up: KDL 1-line formatter で構造化ログ出力
    // (color disable + KdlFormatter で機械可読 / grep 可能な log を吐く)
    //
    // ## file writer に切替 (重要)
    //
    // Win GUI subsystem の vp-app では stderr handle が NUL 化される (CONIN$/CONOUT$ も無い)。
    // PowerShell の Start-Process -RedirectStandardOutput でも GUI subsystem に対しては
    // 確実に redirect が効かない。
    //
    // 解決: tracing-appender で **file に直接書き込む**。
    // Path: `%LOCALAPPDATA%\VantagePoint-dev\vp-app.kdl.log` (Win)
    //       `~/.local/share/vantage-point-dev/vp-app.kdl.log` (Linux/Mac fallback)
    //
    // mise run win の polling tail が同 file を見る。
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    // Phase A (2026-04-27, mem_1CaSiJkD9HATDY2srrv6D4):
    // macOS では `~/Library/Logs/Vantage/` に統一。
    // mise run logs / Console.app / TheWorld daemon log と同じ dir で一緒に tail できる。
    // Win/Linux は既存挙動を維持 (Phase B で揃える)。
    let log_dir = if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("Library/Logs/Vantage")
    } else {
        // Win: `%LOCALAPPDATA%\VantagePoint(-dev)\Logs\`
        let app_dir = if cfg!(debug_assertions) {
            "VantagePoint-dev"
        } else {
            "VantagePoint"
        };
        dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(app_dir)
            .join("Logs")
    };
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::never(&log_dir, "vp-app.kdl.log");
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "vp_app=info".into());
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .event_format(crate::log_format::KdlFormatter)
                .with_writer(file_appender),
        )
        .try_init();

    tracing::info!(
        log_dir = %log_dir.display(),
        "vp-app 起動 (Creo UI mint-dark)"
    );

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // VP-100 follow-up: 永続設定 + 1Password 風 開発者モード切替
    let mut settings = Settings::load();
    let initial_dev_mode = initial_developer_mode(&settings);
    tracing::info!("Settings: developer_mode = {} (initial)", initial_dev_mode);

    // メニューバー (View → Developer Mode / Open Developer Tools を含む) + トレイ
    let menu_handles = crate::menu::build_menu_bar(initial_dev_mode);
    let _menu = menu_handles.menu.clone();
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

    // Sidebar
    let sidebar_ipc_proxy = event_loop.create_proxy();
    let sidebar = WebViewBuilder::new()
        .with_html(SIDEBAR_HTML)
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

    // xterm.js が ready になるまで PTY 出力を buffer
    // (ConPTY は起動直後に DSR \x1b[6n を送ってきて xterm の応答を待つため、
    //  ready 前の bytes を欠落させると shell が永久に block する)
    let mut xterm_ready = false;
    let mut pending: Vec<u8> = Vec::new();
    // pending buffer の上限。xterm が永久に ready にならないシナリオでも OOM を回避する
    // (PH#2)。1MB を超えたら冒頭以外を捨てて overflow メッセージを残す。
    const PENDING_MAX: usize = 1_000_000;
    // VP-95: sidebar 全体 state (projects + widget + activity)
    let mut sidebar_state = SidebarState::default();
    // VP-100 γ-light: pane_id → slot rect。Phase 2 では蓄積するだけ、Phase 4+ で
    // native overlay の `set_position` 同期に使う。
    let mut slot_rects: std::collections::HashMap<String, SlotRect> =
        std::collections::HashMap::new();
    // SP auto-spawn: 1 セッションで同じ project を二重 trigger しないための guard。
    // path をキーにする (project_name は重複しうる、 path は正規化済 unique)。
    // TheWorld 側でも `Process already running` で弾かれるが、 無駄な POST を避ける。
    let mut sp_spawn_triggered: std::collections::HashSet<String> =
        std::collections::HashSet::new();
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
            Event::UserEvent(AppEvent::Output(bytes)) => {
                if !xterm_ready {
                    if pending.len() + bytes.len() > PENDING_MAX {
                        tracing::warn!(
                            "PTY pending overflow ({} + {} > {}), truncating",
                            pending.len(),
                            bytes.len(),
                            PENDING_MAX
                        );
                        pending.clear();
                        pending.extend_from_slice(
                            b"\r\n\x1b[33m[vp-app] PTY buffer overflow, truncated\x1b[0m\r\n",
                        );
                    }
                    pending.extend_from_slice(&bytes);
                    tracing::debug!(
                        "PTY output buffered ({} bytes, pending total={})",
                        bytes.len(),
                        pending.len()
                    );
                } else {
                    let script = terminal::build_output_script(&bytes);
                    if let Err(e) = main_view.evaluate_script(&script) {
                        tracing::warn!("main evaluate_script 失敗: {}", e);
                    }
                }
            }
            Event::UserEvent(AppEvent::XtermReady) => {
                // PH#1: 二重 ready 防御 — `setActivePane` 等で再起動した場合に
                // 二重 flush しないよう冪等化。
                if xterm_ready {
                    return;
                }
                xterm_ready = true;
                if !pending.is_empty() {
                    tracing::info!("xterm ready → flush {} 保留バイト", pending.len());
                    let script = terminal::build_output_script(&pending);
                    if let Err(e) = main_view.evaluate_script(&script) {
                        tracing::warn!("main flush 失敗: {}", e);
                    }
                    pending.clear();
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
                            // 新規 project: session 中の追加なら auto-expand
                            let mut s = ProcessPaneState::new(p.path.clone(), p.name.clone());
                            if !is_initial_load {
                                s.expanded = true;
                            }
                            s
                        };
                        pane_state.state = Some(state_str);
                        pane_state.port = port;
                        pane_state
                    })
                    .collect();
                // Phase A4-3b: 各 project の SP に対して /api/lanes を fetch
                // (memory mem_1CaSugEk1W2vr5TAdfDn5D: Lane scope は SP per project の所有)
                for (path, port) in &project_ports {
                    if let Some(sp_port) = port {
                        spawn_lanes_fetch(async_action_proxy.clone(), path.clone(), *sp_port);
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
                let auto_select = sidebar_state.active_lane_address.is_none()
                    && lanes
                        .first()
                        .map(|l| lane_address_key(&l.address))
                        .is_some();
                let first_addr = if auto_select {
                    lanes.first().map(|l| lane_address_key(&l.address))
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
                            .map(|l| lane_address_key(&l.address))
                            .collect();
                        prev.iter()
                            .map(|l| lane_address_key(&l.address))
                            .filter(|addr| !new_set.contains(addr))
                            .collect()
                    })
                    .unwrap_or_default();
                for addr in &removed_addrs {
                    tracing::info!("Lane removed (LanesLoaded diff): {}", addr);
                    lane_js::remove_lane(&main_view, addr);
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
                            let addr_str = lane_address_key(&lane.address);
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
                    push_active_lane(&main_view, &sidebar_state);
                    // Phase 2.5: per-Lane instance を main area に表示。
                    // ensureLane は上のループで呼んだので、 ここでは show のみ。
                    lane_js::show_lane(&main_view, Some(&addr));
                }
                push_sidebar_state(&sidebar, &sidebar_state);
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
            Event::UserEvent(AppEvent::ProcessesError(msg)) => {
                let js_msg = serde_json::to_string(&msg).unwrap_or_else(|_| "\"error\"".into());
                let script = format!("window.renderError({})", js_msg);
                if let Err(e) = sidebar.evaluate_script(&script) {
                    tracing::warn!("sidebar renderError 失敗: {}", e);
                }
            }
            Event::UserEvent(AppEvent::ActivityUpdate(snap)) => {
                sidebar_state.activity = snap;
                push_sidebar_state(&sidebar, &sidebar_state);
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
                            let default_root =
                                resolve_default_project_root(&settings, &sidebar_state);
                            spawn_clone_project(async_action_proxy.clone(), url, default_root);
                            return;
                        }
                        _ => {}
                    }
                }
                let outcome = handle_sidebar_ipc(&msg, &mut sidebar_state);
                if outcome.changed {
                    push_sidebar_state(&sidebar, &sidebar_state);
                }
                if outcome.active_changed {
                    push_active_lane(&main_view, &sidebar_state);
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
                if id == menu_ids.developer_mode {
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
                        main_view.open_devtools();
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
