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
use crate::pane::{ActivitySnapshot, PaneKind, ProjectPaneState, SidebarState};
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
///   - `{"t":"project:toggle","path":"..."}`
///   - `{"t":"pane:select","path":"...","paneId":"..."}`
///   - `{"t":"pane:add","path":"...","kind":"agent|canvas|preview|shell"}`
const SIDEBAR_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="ja" data-theme="mint-dark">
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
  .projects-section{flex:1;overflow-y:auto;padding:6px 0;}
  .projects-section .section-header{padding:10px 16px 6px;font-size:10px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:.08em;display:flex;justify-content:space-between;align-items:center;}

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

  /* Clone modal overlay */
  .modal-overlay{position:fixed;inset:0;background:rgba(0,0,0,0.5);display:none;align-items:center;justify-content:center;z-index:100;}
  .modal-overlay.active{display:flex;}
  .modal{background:var(--color-surface-bg-base);border:1px solid var(--color-surface-border,#1f2233);border-radius:8px;padding:16px;min-width:240px;max-width:90%;}
  .modal h2{font-size:13px;margin:0 0 8px;color:var(--color-text-primary);font-weight:500;}
  .modal input{width:100%;padding:6px 8px;border-radius:4px;border:1px solid var(--color-surface-border,#1f2233);background:var(--color-surface-bg-subtle);color:var(--color-text-primary);font-family:inherit;font-size:12px;box-sizing:border-box;}
  .modal input:focus{outline:none;border-color:var(--color-brand-primary);}
  .modal .actions{display:flex;justify-content:flex-end;gap:6px;margin-top:10px;}
  .modal button{padding:5px 12px;border-radius:4px;border:1px solid var(--color-surface-border,#1f2233);background:transparent;color:var(--color-text-secondary);cursor:pointer;font-size:11px;transition:background .12s ease,color .12s ease;}
  .modal button:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);}
  .modal button.primary{background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);border-color:var(--color-brand-primary-subtle);}
  .modal button.primary:hover{background:var(--color-brand-primary);color:var(--color-surface-bg-base);}

  .project{margin:0 6px 2px;}
  .project-header{display:flex;align-items:center;gap:6px;padding:6px 8px;border-radius:var(--radius-sm,6px);cursor:pointer;transition:background .1s ease;user-select:none;}
  .project-header:hover{background:var(--color-surface-bg-emphasis);}
  .project-header .chevron{font-size:9px;color:var(--color-text-tertiary);width:10px;display:inline-block;transition:transform .12s ease;}
  .project-header.expanded .chevron{transform:rotate(90deg);}
  .project-header .name{flex:1;font-weight:500;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
  .project-header .path{font-size:10px;color:var(--color-text-tertiary);}

  .pane-list{display:none;padding:2px 0 4px 18px;}
  .project.expanded .pane-list{display:block;}

  .pane-row{display:flex;align-items:center;gap:6px;padding:5px 8px;border-radius:var(--radius-sm,6px);cursor:pointer;transition:background .1s ease;font-size:12px;}
  .pane-row:hover{background:var(--color-surface-bg-emphasis);}
  .pane-row.active{background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);}
  .pane-row .icon{width:16px;text-align:center;font-size:13px;}
  .pane-row .label{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}

  .pane-add{display:flex;align-items:center;gap:6px;padding:5px 8px;border-radius:var(--radius-sm,6px);cursor:pointer;color:var(--color-text-tertiary);font-size:11px;font-style:italic;}
  .pane-add:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-secondary);}
  .pane-add .icon{width:16px;text-align:center;}

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
  <!-- Clone modal (重畳 overlay、Cancel / Clone) -->
  <div class="modal-overlay" id="clone-modal">
    <div class="modal">
      <h2>Clone Repository</h2>
      <input type="text" id="clone-url" placeholder="https://github.com/user/repo.git" />
      <div class="actions">
        <button id="clone-cancel">Cancel</button>
        <button id="clone-confirm" class="primary">Clone</button>
      </div>
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
      const proj = document.createElement('div');
      proj.className = 'project' + (p.expanded ? ' expanded' : '');

      const head = document.createElement('div');
      head.className = 'project-header' + (p.expanded ? ' expanded' : '');
      const chev = document.createElement('span');
      chev.className = 'chevron';
      chev.textContent = '▶';
      const name = document.createElement('span');
      name.className = 'name';
      name.textContent = p.name;
      head.appendChild(chev);
      head.appendChild(name);
      head.addEventListener('click', () => send({t: 'project:toggle', path: p.path}));
      proj.appendChild(head);

      const list = document.createElement('div');
      list.className = 'pane-list';
      for (const pane of p.panes || []) {
        const row = document.createElement('div');
        row.className = 'pane-row' + (p.active_pane_id === pane.id ? ' active' : '');
        const icon = document.createElement('span');
        icon.className = 'icon';
        icon.textContent = paneIcon(pane.kind);
        const label = document.createElement('span');
        label.className = 'label';
        label.textContent = pane.title || defaultLabel(pane.kind);
        row.appendChild(icon);
        row.appendChild(label);
        row.addEventListener('click', (e) => {
          e.stopPropagation();
          send({t: 'pane:select', path: p.path, paneId: pane.id});
        });
        list.appendChild(row);
      }
      // "+" Add pane (P2/P3 で wire up、今は kind picker なし MVP として agent を追加)
      const add = document.createElement('div');
      add.className = 'pane-add';
      const addIcon = document.createElement('span');
      addIcon.className = 'icon';
      addIcon.textContent = '+';
      const addLabel = document.createElement('span');
      addLabel.textContent = 'Add pane';
      add.appendChild(addIcon);
      add.appendChild(addLabel);
      add.addEventListener('click', (e) => {
        e.stopPropagation();
        // P1 MVP: kind 選択 prompt は P3 で。今は agent を追加して動作確認用
        send({t: 'pane:add', path: p.path, kind: 'agent'});
      });
      list.appendChild(add);

      proj.appendChild(list);
      root.appendChild(proj);
    }
  }

  function paneIcon(kind) {
    switch (kind) {
      case 'agent': return '📖';
      case 'canvas': return '🧭';
      case 'preview': return '📄';
      case 'shell': return '⚙';
      default: return '·';
    }
  }
  function defaultLabel(kind) {
    switch (kind) {
      case 'agent': return 'Lead Agent';
      case 'canvas': return 'Canvas';
      case 'preview': return 'Preview';
      case 'shell': return 'Shell';
      default: return kind || '';
    }
  }

  function applyState(s) {
    if (!domReady) { pendingState = s; return; }
    state = s;
    renderActivity(s.activity);
    renderProjects(s.projects);
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
      send({t: 'project:add'});
    });

    // Clone Repository — modal で URL を受け取る
    const cloneBtn = document.getElementById('clone-project-btn');
    const cloneModal = document.getElementById('clone-modal');
    const cloneInput = document.getElementById('clone-url');
    const cloneCancel = document.getElementById('clone-cancel');
    const cloneConfirm = document.getElementById('clone-confirm');
    function openCloneModal() {
      if (!cloneModal) return;
      cloneInput.value = '';
      cloneModal.classList.add('active');
      setTimeout(() => cloneInput && cloneInput.focus(), 50);
    }
    function closeCloneModal() {
      if (cloneModal) cloneModal.classList.remove('active');
    }
    function submitClone() {
      const url = (cloneInput && cloneInput.value || '').trim();
      if (!url) return;
      send({t: 'project:clone', url: url});
      closeCloneModal();
    }
    if (cloneBtn) cloneBtn.addEventListener('click', () => {
      collapseAdd();
      openCloneModal();
    });
    if (cloneCancel) cloneCancel.addEventListener('click', closeCloneModal);
    if (cloneConfirm) cloneConfirm.addEventListener('click', submitClone);
    if (cloneInput) {
      cloneInput.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') { e.preventDefault(); submitClone(); }
        else if (e.key === 'Escape') { e.preventDefault(); closeCloneModal(); }
      });
    }
    if (cloneModal) {
      cloneModal.addEventListener('click', (e) => {
        if (e.target === cloneModal) closeCloneModal();
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

/// Settings → 実 path 解決。
///
/// 優先順位:
/// 1. `Settings.default_project_root` が指定されていて存在する → それ
/// 2. `~/repos` が存在する → それ
/// 3. `~` (home) → それ
/// 4. それ以外 → `None`
fn resolve_default_project_root(settings: &Settings) -> Option<std::path::PathBuf> {
    if let Some(s) = &settings.default_project_root {
        let p = std::path::PathBuf::from(s);
        if p.exists() {
            return Some(p);
        }
        tracing::warn!(
            "default_project_root が設定されているが存在しない: {} → home にフォールバック",
            s
        );
    }
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
/// 2. 成功なら `client.list_projects()` で再取得 → `AppEvent::ProjectsLoaded`
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
                    tracing::debug!("project:add canceled by user");
                    return;
                }
            };
            let name = folder
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".to_string());
            let path = folder.to_string_lossy().into_owned();
            tracing::info!("project:add picker → name={} path={}", name, path);

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
                        let _ = proxy.send_event(AppEvent::ProjectsLoaded(projects));
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
/// 3. `list_projects` で再取得 → `AppEvent::ProjectsLoaded`
///
/// `default_root` が `None` の時は何もしない (default_project_root が解決できないケース)。
/// git バイナリが PATH に無い場合も spawn 失敗で終わる。
fn spawn_clone_project(
    proxy: EventLoopProxy<AppEvent>,
    url: String,
    default_root: Option<std::path::PathBuf>,
) {
    let Some(default_root) = default_root else {
        tracing::warn!("project:clone but default_project_root is unresolved (set in settings)");
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
                        let _ = proxy.send_event(AppEvent::ProjectsLoaded(projects));
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

/// 起動時に TheWorld `/api/world/projects` を別スレッドで fetch。
/// 成功/失敗を `AppEvent::ProjectsLoaded` / `ProjectsError` として main thread に通知。
fn spawn_projects_fetch(proxy: EventLoopProxy<AppEvent>) {
    let _ = thread::Builder::new()
        .name("projects-fetch".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = proxy.send_event(AppEvent::ProjectsError(format!(
                        "tokio runtime 作成失敗: {}",
                        e
                    )));
                    return;
                }
            };
            rt.block_on(async {
                let client = TheWorldClient::default();
                match client.list_projects().await {
                    Ok(projects) => {
                        tracing::info!("TheWorld projects: {} 件", projects.len());
                        let _ = proxy.send_event(AppEvent::ProjectsLoaded(projects));
                    }
                    Err(e) => {
                        tracing::warn!("TheWorld fetch 失敗 (daemon 未起動?): {}", e);
                        let _ = proxy.send_event(AppEvent::ProjectsError(e.to_string()));
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
/// 再 fetch し `AppEvent::ProjectsLoaded` を再送する。これにより sidebar
/// projects accordion が永遠に空のまま、という UX バグを防ぐ。
/// 起動初回 (`prev_online == None`) では `spawn_projects_fetch` 側が担当するので
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
                loop {
                    tick.tick().await;
                    let snap = collect_activity(&client).await;
                    let became_online = matches!(prev_online, Some(false)) && snap.world_online;
                    prev_online = Some(snap.world_online);
                    if proxy.send_event(AppEvent::ActivityUpdate(snap)).is_err() {
                        tracing::debug!("EventLoop 終了、activity poller も終了");
                        break;
                    }
                    // daemon 復帰検知 → projects 再 fetch を kick
                    if became_online {
                        match client.list_projects().await {
                            Ok(projects) => {
                                tracing::info!(
                                    "daemon online 復帰検知 → projects 再 fetch ({} 件)",
                                    projects.len()
                                );
                                if proxy
                                    .send_event(AppEvent::ProjectsLoaded(projects))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("daemon online but list_projects failed: {}", e);
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

/// VP-100 Phase 2: sidebar の active pane 情報を main area に push。
///
/// SidebarState から「現在 focus している project の active pane」を抜き出して、
/// `window.setActivePane({kind, pane_id, preview_url})` を呼ぶ JS を main area に
/// evaluate_script する。active pane が無ければ kind=None で empty 状態に切替。
///
/// 「focus している project」の決定ロジック (Phase 2 暫定): 直近で active_pane_id を
/// 設定した project (= sidebar IPC で pane:select / pane:add した project)。
/// Phase 3 で project-level focus state を導入したら整理する。
fn push_active_pane(main_view: &WebView, state: &SidebarState, focused_path: Option<&str>) {
    let active = focused_path
        .and_then(|fp| state.projects.iter().find(|p| p.path == fp))
        .or_else(|| {
            // fallback: active_pane_id を持つ最初の project
            state.projects.iter().find(|p| p.active_pane_id.is_some())
        })
        .and_then(|p| {
            p.active_pane_id.as_ref().and_then(|aid| {
                p.panes
                    .iter()
                    .find(|pn| &pn.id == aid)
                    .map(|pn| (pn, p.path.as_str()))
            })
        });

    let info = match active {
        Some((pane, _path)) => ActivePaneInfo {
            kind: Some(match pane.kind {
                PaneKind::Agent => "agent",
                PaneKind::Canvas => "canvas",
                PaneKind::Preview => "preview",
                PaneKind::Shell => "shell",
            }),
            pane_id: Some(&pane.id),
            preview_url: pane.preview_url.as_deref(),
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
    /// active pane を変更した project の path (main area に push する手がかり)
    /// pane:select / pane:add で更新される
    active_changed_path: Option<String>,
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
        "project:toggle" => {
            if let Some(p) = state.projects.iter_mut().find(|p| p.path == path) {
                p.expanded = !p.expanded;
                tracing::debug!("project:toggle {} → expanded={}", path, p.expanded);
                out.changed = true;
            }
        }
        "pane:select" => {
            let pane_id = parsed.get("paneId").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(p) = state.projects.iter_mut().find(|p| p.path == path)
                && p.panes.iter().any(|pn| pn.id == pane_id)
            {
                p.active_pane_id = Some(pane_id.to_string());
                tracing::info!("pane:select {} pane={}", path, pane_id);
                out.changed = true;
                out.active_changed_path = Some(path.to_string());
            }
        }
        "pane:add" => {
            let kind_str = parsed
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let kind = match kind_str {
                "agent" => PaneKind::Agent,
                "canvas" => PaneKind::Canvas,
                "preview" => PaneKind::Preview,
                "shell" => PaneKind::Shell,
                _ => PaneKind::Agent,
            };
            if let Some(p) = state.projects.iter_mut().find(|p| p.path == path) {
                let pane = crate::pane::Pane::with_default_label(kind);
                let id = pane.id.clone();
                p.panes.push(pane);
                p.active_pane_id = Some(id);
                tracing::info!("pane:add {} kind={:?}", path, kind);
                out.changed = true;
                out.active_changed_path = Some(path.to_string());
            }
        }
        other => {
            tracing::debug!("sidebar IPC: 未知の type {:?}", other);
        }
    }
    out
}

/// App のエントリポイント
pub fn run() -> anyhow::Result<()> {
    // VP-100 follow-up: KDL 1-line formatter で構造化ログ出力
    // (color disable + KdlFormatter で機械可読 / grep 可能な log を吐く)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vp_app=info".into()),
        )
        .with_ansi(false)
        .event_format(crate::log_format::KdlFormatter)
        .init();

    tracing::info!("vp-app 起動 (Creo UI mint-dark)");

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
    let pty = match std::env::var("VP_TERMINAL_MODE").as_deref() {
        Ok("local") => {
            tracing::info!("terminal mode = local (portable-pty, explicit opt-out)");
            terminal::TerminalHandle::Local(terminal::spawn_shell(None, 80, 24, proxy)?)
        }
        _ => {
            let world_url =
                std::env::var("VP_WORLD_URL").unwrap_or_else(|_| "http://127.0.0.1:32000".into());
            match crate::daemon_launcher::ensure_daemon_ready(&world_url) {
                Ok(()) => {
                    tracing::info!("terminal mode = daemon (WS to {})", world_url);
                    terminal::TerminalHandle::Daemon(crate::ws_terminal::connect_daemon_terminal(
                        &world_url, 80, 24, proxy,
                    )?)
                }
                Err(e) => {
                    tracing::warn!(
                        "daemon auto-launch 失敗、local portable-pty に fallback: {}",
                        e
                    );
                    terminal::TerminalHandle::Local(terminal::spawn_shell(None, 80, 24, proxy)?)
                }
            }
        }
    };

    // TheWorld から project list を非同期 fetch (起動初回)
    spawn_projects_fetch(event_loop.create_proxy());
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
    let pty_for_ipc = pty.clone();
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
            terminal::handle_ipc_message(req.body(), &pty_for_ipc, &ipc_proxy);
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
            Event::UserEvent(AppEvent::ProjectsLoaded(projects)) => {
                // 既存 SidebarState とマージ:
                //  - 同じ path があれば既存 state を維持 (expanded / panes / active 保持)
                //  - 新規は ProjectPaneState::new (Lead Agent 1 つ + 折畳)
                //  - サーバから消えた project は除外
                let prev: std::collections::HashMap<String, ProjectPaneState> = sidebar_state
                    .projects
                    .drain(..)
                    .map(|p| (p.path.clone(), p))
                    .collect();
                sidebar_state.projects = projects
                    .into_iter()
                    .map(|p| {
                        prev.get(&p.path).cloned().unwrap_or_else(|| {
                            ProjectPaneState::new(p.path.clone(), p.name.clone())
                        })
                    })
                    .collect();
                push_sidebar_state(&sidebar, &sidebar_state);
            }
            Event::UserEvent(AppEvent::ProjectsError(msg)) => {
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
                        Some("project:add") => {
                            let initial_dir = resolve_default_project_root(&settings);
                            spawn_add_project_picker(async_action_proxy.clone(), initial_dir);
                            return;
                        }
                        Some("project:clone") => {
                            let url = parsed
                                .get("url")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if url.is_empty() {
                                tracing::warn!("project:clone with empty url");
                                return;
                            }
                            let default_root = resolve_default_project_root(&settings);
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
                if outcome.active_changed_path.is_some() {
                    push_active_pane(
                        &main_view,
                        &sidebar_state,
                        outcome.active_changed_path.as_deref(),
                    );
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
