//! Main area — 単一 wry WebView 内で複数 Pane kind の content を切替表示
//!
//! ## VP-94 Phase 2 / VP-100
//!
//! Phase 1 (VP-95) では sidebar accordion + Pane state を Rust 側に整備した。
//! Phase 2 では main area を **canvas + terminal の 2 WebView から、単一 WebView** に
//! 統合する (β 戦略)。
//!
//! 単一 WebView 内に各 PaneKind の content を全部 mount しておき、
//! `window.setActivePane({kind, paneId, previewUrl})` で表示切替する。
//! 非表示 pane は `display:none` で隠すだけなので、xterm.js + PTY 接続は維持される。
//!
//! ## レイアウト
//! ```text
//! ┌──────────────────────────────────────────┐
//! │ pane-host (relative container)            │
//! │ ┌──────────────────────────────────────┐ │
//! │ │ pane-terminal (xterm.js, agent/shell) │ │
//! │ │ ────────────────────────────────────  │ │
//! │ │ pane-canvas   (Canvas placeholder)    │ │
//! │ │ ────────────────────────────────────  │ │
//! │ │ pane-preview  (iframe)                │ │
//! │ │ ────────────────────────────────────  │ │
//! │ │ pane-empty    (no selection)          │ │
//! │ └──────────────────────────────────────┘ │
//! └──────────────────────────────────────────┘
//! ```
//!
//! 同時に表示されるのは 1 つの pane のみ (Phase 2)。
//! 複数 pane の同時表示 (split / overlay / tab) は Phase 3 で。
//!
//! ## IPC contract
//! - **Rust → main**: `window.setActivePane({kind, paneId, previewUrl})`
//! - **main → Rust**: 既存の terminal IPC (`{t:'in'/'resize'/'ready'/'copy'/'debug'}`) のみ
//!
//! ## PTY 接続
//! Phase 2 時点では xterm.js 1 instance が PTY 1 つに接続。複数 agent/shell pane を
//! 作っても全部同じ PTY を共有する。pane ごとの PTY 分離は Phase 3 で。

use serde::{Deserialize, Serialize};

/// Rust から main area JS に渡す active pane の payload
#[derive(Debug, Clone, Serialize)]
pub struct ActivePaneInfo<'a> {
    /// Pane kind ("agent" | "canvas" | "preview" | "shell" | null)
    /// null = 何も active でない (空状態を表示)
    pub kind: Option<&'a str>,
    pub pane_id: Option<&'a str>,
    /// Preview kind の URL (preview kind 以外では None)
    pub preview_url: Option<&'a str>,
}

/// `window.setActivePane(info)` を呼ぶ JS スニペットを生成
pub fn build_set_active_pane_script(info: &ActivePaneInfo<'_>) -> String {
    let json = serde_json::to_string(info).unwrap_or_else(|_| "null".into());
    format!("window.setActivePane({})", json)
}

/// Pane slot の矩形 (CSS pixel、main area 左上原点)
///
/// VP-100 γ-light: HTML grid の slot 矩形を JS の ResizeObserver から
/// IPC で Rust に push する。Phase 2 時点では store するだけ、Phase 4+ で
/// native overlay が追加された時にこの値で `tao::Window::set_position` を
/// 同期する。詳細は memory:vp_app_native_overlay_resize_ghost.md。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SlotRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Main area の HTML (xterm.js + canvas placeholder + preview iframe + empty state)
///
/// 旧 `terminal::TERMINAL_HTML` を発展させたもの。xterm.js 周りの copy/paste / OSC 52 /
/// Creo tokens 統一は維持。kind 切替を window.setActivePane で行う。
pub const MAIN_AREA_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="en" data-theme="mint-dark">
<head>
<meta charset="utf-8">
<title>vp-app main</title>
<style>
"#,
    include_str!("../assets/creo-tokens.css"),
    r#"
</style>
<style>
"#,
    include_str!("../assets/creo-components.css"),
    r#"
</style>
<style>
"#,
    include_str!("../assets/xterm.min.css"),
    r#"
html,body{margin:0;padding:0;height:100%;width:100%;background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:var(--typography-family-sans);}
body{overflow:hidden;}
#host{position:relative;width:100%;height:100%;}
.pane{position:absolute;inset:0;display:none;}
.pane.active{display:block;}
.pane.terminal{padding:0;}
.pane.terminal #term-pool{position:relative;width:100%;height:100%;}
/* Phase 3: pane ごとに独立した xterm.js Terminal が動的に追加される。
   active pane のみ display:block、他は display:none で hidden。 */
.term-instance{position:absolute;inset:0;padding:12px;box-sizing:border-box;display:none;}
.term-instance.active{display:block;}
.pane.canvas{display:none;place-items:center;}
.pane.canvas.active{display:grid;}
.pane.canvas main{text-align:center;}
.pane.canvas h1{font-weight:500;font-size:1.6rem;margin:0 0 .25rem;color:var(--color-text-primary);}
.pane.canvas p{color:var(--color-text-tertiary);margin:0;font-size:.9rem;}
.pane.canvas .brand{color:var(--color-brand-primary);}
.pane.preview iframe{width:100%;height:100%;border:0;background:#fff;}
.pane.empty{display:none;place-items:center;}
.pane.empty.active{display:grid;}
.pane.empty main{text-align:center;color:var(--color-text-tertiary);}
.pane.empty h1{font-weight:400;font-size:1.1rem;margin:0;}
.pane.empty p{margin:.25rem 0 0;font-size:.85rem;}
/* xterm 内 scrollbar を Creo tokens で統一 */
.xterm-viewport::-webkit-scrollbar{width:8px;}
.xterm-viewport::-webkit-scrollbar-track{background:transparent;}
.xterm-viewport::-webkit-scrollbar-thumb{background:var(--color-surface-border);border-radius:4px;}
.xterm-viewport::-webkit-scrollbar-thumb:hover{background:var(--color-brand-primary-subtle);}
</style>
</head>
<body>
<div id="host">
  <!-- 各 .pane は data-kind を持つ。data-pane-id は active pane 切替時に Rust が動的に設定。
       VP-100 γ-light: ResizeObserver が slot rect を IPC で送る (Phase 4+ で native overlay 同期に使う)。 -->
  <div class="pane terminal" id="pane-terminal" data-kind="agent">
    <!-- Phase 3: pane ごとの xterm.js Terminal は <div class="term-instance"> として
         JS から動的に追加される。term-pool は単なる positioning 親。 -->
    <div id="term-pool"></div>
  </div>
  <div class="pane canvas" id="pane-canvas" data-kind="canvas">
    <main>
      <h1>Canvas pane</h1>
      <p>Phase 2 — <span class="brand">Creo UI mint-dark</span> を全ペイン統一で適用</p>
    </main>
  </div>
  <div class="pane preview" id="pane-preview" data-kind="preview">
    <iframe id="preview-frame" src="about:blank" sandbox="allow-same-origin allow-scripts"></iframe>
  </div>
  <div class="pane empty active" id="pane-empty" data-kind="empty">
    <main>
      <h1>No pane selected</h1>
      <p>sidebar から pane を選択してください</p>
    </main>
  </div>
</div>
<!-- VP-101 Phase A2: creo-ui-editor-host (SolidJS) の mount 先。
     Ctrl+Shift+E で activate される floating overlay (font / theme / token を runtime 編集)。 -->
<div id="editor-root"></div>
<script>
"#,
    include_str!("../assets/xterm.min.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-fit.min.js"),
    r#"
</script>
<!-- VP-101 Phase A2: creo-ui-editor-host bundle (SolidJS + EditorLayer + tokens auto-discover).
     Ctrl+Shift+E で activate、font / theme / spacing 等を runtime 編集。
     Build: cd crates/vp-app/web-bundle && bun install && bun run build。 -->
<script>
"#,
    include_str!("../assets/editor-host.bundle.js"),
    r#"
</script>
<script>
(function() {
  // Creo tokens から xterm.js theme を構築 (runtime で var() 解決)
  const css = getComputedStyle(document.documentElement);
  const v = (name, fallback) => (css.getPropertyValue(name).trim() || fallback);
  const monoFamily = (css.getPropertyValue('--typography-family-mono') || '').trim()
    || '"JetBrainsMono Nerd Font", "Cascadia Code", "SF Mono", Menlo, Consolas, monospace';
  function buildTheme() {
    return {
      background: v('--color-surface-bg-base', '#0F1128'),
      foreground: v('--color-text-primary', '#EDEEF4'),
      cursor: v('--color-brand-primary', '#7D6BC2'),
      cursorAccent: v('--color-surface-bg-base', '#0F1128'),
      selectionBackground: v('--color-brand-primary-subtle', '#2C2843')
    };
  }

  // ========= Phase 3: per-pane xterm.js Terminal Map =========
  // paneId → {term, fitAddon, container}
  const terms = new Map();
  const termPool = document.getElementById('term-pool');

  const dbg = (msg) => window.ipc.postMessage(JSON.stringify({t:'debug', msg: msg}));

  function attachCopyPaste(container, term) {
    const doCopy = () => {
      const sel = term.getSelection();
      if (!sel) return false;
      navigator.clipboard.writeText(sel).catch(() => {
        window.ipc.postMessage(JSON.stringify({t:'copy', d: sel}));
      });
      return true;
    };
    const doPaste = () => {
      navigator.clipboard.readText()
        .then((text) => { if (text) term.paste(text); })
        .catch((err) => dbg('paste failed: ' + err));
    };

    // Ctrl+Shift+C は container 単位で capture (term の上でだけ拾うように)
    container.addEventListener('keydown', (e) => {
      if (e.ctrlKey && e.shiftKey && (e.key === 'C' || e.key === 'c')) {
        e.preventDefault();
        e.stopPropagation();
        doCopy();
      }
    }, true);

    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown') return true;
      const key = (e.key || '').toLowerCase();
      if ((e.ctrlKey && e.key === 'Insert' && !e.shiftKey) || (e.metaKey && key === 'c')) {
        if (doCopy()) return false;
      }
      if ((e.shiftKey && e.key === 'Insert' && !e.ctrlKey) ||
          (e.ctrlKey && e.shiftKey && key === 'v') ||
          (e.metaKey && key === 'v')) {
        doPaste();
        return false;
      }
      if (e.ctrlKey && !e.shiftKey && !e.metaKey && key === 'c') {
        if (term.hasSelection()) {
          doCopy();
          term.clearSelection();
          return false;
        }
      }
      return true;
    });

    container.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      doPaste();
    });

    container.addEventListener('mouseup', () => {
      setTimeout(() => {
        const sel = term.getSelection();
        if (sel && sel.length > 0) doCopy();
      }, 0);
    });
  }

  function ensureTerm(paneId) {
    if (terms.has(paneId)) return terms.get(paneId);

    const container = document.createElement('div');
    container.className = 'term-instance';
    container.dataset.paneId = paneId;
    termPool.appendChild(container);

    const term = new Terminal({
      fontFamily: monoFamily,
      fontSize: 13,
      lineHeight: 1.15,
      letterSpacing: 0,
      theme: buildTheme(),
      allowProposedApi: true,
      convertEol: true,
      scrollback: 5000,
      cursorBlink: true,
      cursorStyle: 'bar',
      cursorWidth: 2,
      smoothScrollDuration: 80,
      fontLigatures: true
    });
    const fitAddon = new FitAddon.FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);

    term.onData(d => {
      window.ipc.postMessage(JSON.stringify({t:'in', paneId: paneId, d: d}));
    });

    // OSC 52 (clipboard) intercept (Phase 1 から継続)
    term.parser.registerOscHandler(52, (data) => {
      const idx = data.indexOf(';');
      if (idx < 0) return true;
      const pd = data.slice(idx + 1);
      if (pd === '?' || pd.length === 0) return true;
      try {
        const binary = atob(pd);
        const bytes = new Uint8Array(binary.length);
        for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
        const text = new TextDecoder('utf-8').decode(bytes);
        window.ipc.postMessage(JSON.stringify({t:'copy', d: text}));
      } catch (e) {
        dbg('OSC 52 decode error: ' + e.message);
      }
      return true;
    });

    attachCopyPaste(container, term);

    const focusTerm = () => { try { term.focus(); } catch (_) {} };
    container.addEventListener('mousedown', focusTerm);
    container.addEventListener('click', focusTerm);

    const entry = { term, fitAddon, container };
    terms.set(paneId, entry);

    // ready 通知 (Rust が pre-ready buffer を flush してくる)
    window.ipc.postMessage(JSON.stringify({t:'ready', paneId: paneId}));

    return entry;
  }

  // Rust → JS で per-pane PTY 出力
  window.onPtyData = function(paneId, b64) {
    const e = terms.get(paneId);
    if (!e) {
      // ready 前の race: Rust が xterm_ready をセットする前に flush しに来た or
      // pane が JS 側で生成される前 → console warn のみ (Rust 側 buffer で吸収済のはず)
      return;
    }
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    e.term.write(bytes);
  };

  // ========= Phase 3: Pane 切替 API =========
  // Rust → JS で active pane を切替。kind が null の場合は empty 状態を表示。
  const KIND_TO_PANE = {
    agent: 'pane-terminal',
    shell: 'pane-terminal',
    canvas: 'pane-canvas',
    preview: 'pane-preview',
    empty: 'pane-empty',
  };
  let activePaneInfo = null;

  function setActiveImpl(info) {
    activePaneInfo = info || null;
    const kind = info && info.kind ? info.kind : 'empty';
    const targetId = KIND_TO_PANE[kind] || 'pane-empty';
    document.querySelectorAll('.pane').forEach(el => {
      const isActive = (el.id === targetId);
      el.classList.toggle('active', isActive);
      if (isActive && info && info.pane_id) {
        el.setAttribute('data-pane-id', info.pane_id);
      } else if (isActive) {
        el.removeAttribute('data-pane-id');
      }
    });

    // Phase 3: per-pane xterm container 切替
    if (kind === 'agent' || kind === 'shell') {
      const paneId = info && info.pane_id;
      if (paneId) ensureTerm(paneId);
      for (const [id, e] of terms) {
        e.container.classList.toggle('active', id === paneId);
      }
      if (paneId) {
        const e = terms.get(paneId);
        try {
          e.fitAddon.fit();
          window.ipc.postMessage(JSON.stringify({
            t: 'resize',
            paneId: paneId,
            cols: e.term.cols,
            rows: e.term.rows
          }));
          e.term.focus();
        } catch (_) {}
      }
    } else {
      // 非 terminal kind: 全 term container を hide
      for (const [, e] of terms) {
        e.container.classList.remove('active');
      }
    }

    if (kind === 'preview') {
      const frame = document.getElementById('preview-frame');
      const url = (info && info.preview_url) || 'about:blank';
      if (frame && frame.getAttribute('src') !== url) {
        frame.setAttribute('src', url);
      }
    }
    sendSlotRect();
  }

  // DOM 未 ready の前に呼ばれた場合は buffer
  let pendingPane = null;
  let domReady = false;
  window.setActivePane = function(info) {
    if (!domReady) { pendingPane = info; return; }
    setActiveImpl(info);
  };

  // ========= VP-100 γ-light: slot rect を Rust に push =========
  function sendSlotRect() {
    const target = document.querySelector('.pane.active');
    if (!target) return;
    const r = target.getBoundingClientRect();
    window.ipc.postMessage(JSON.stringify({
      t: 'slot:rect',
      pane_id: activePaneInfo ? (activePaneInfo.pane_id || null) : null,
      kind: target.getAttribute('data-kind') || 'empty',
      rect: { x: r.left, y: r.top, w: r.width, h: r.height },
    }));
  }
  let rafScheduled = false;
  function scheduleSendSlotRect() {
    if (rafScheduled) return;
    rafScheduled = true;
    requestAnimationFrame(() => {
      rafScheduled = false;
      sendSlotRect();
    });
  }
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => scheduleSendSlotRect());
    ro.observe(document.getElementById('host'));
  }

  // window resize で active term の fit + resize 通知
  window.addEventListener('resize', () => {
    if (!activePaneInfo) return;
    const k = activePaneInfo.kind;
    if (k !== 'agent' && k !== 'shell') return;
    const paneId = activePaneInfo.pane_id;
    if (!paneId) return;
    const e = terms.get(paneId);
    if (!e) return;
    try {
      e.fitAddon.fit();
      window.ipc.postMessage(JSON.stringify({
        t: 'resize',
        paneId: paneId,
        cols: e.term.cols,
        rows: e.term.rows
      }));
    } catch (_) {}
  });

  // DevTools console から active term を inspect する getter
  window.__vpTerm = () => {
    const id = activePaneInfo ? activePaneInfo.pane_id : null;
    return id ? terms.get(id) : null;
  };

  // DOM ready 後に pending pane を flush
  window.addEventListener('DOMContentLoaded', () => {
    domReady = true;
    if (pendingPane !== null) {
      setActiveImpl(pendingPane);
      pendingPane = null;
    }
  });
})();
</script>
</body>
</html>"#
);
