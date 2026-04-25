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
    include_str!("../assets/xterm.min.css"),
    r#"
html,body{margin:0;padding:0;height:100%;width:100%;background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:system-ui,-apple-system,"Segoe UI","Cascadia Code",monospace;}
body{overflow:hidden;}
#host{position:relative;width:100%;height:100%;}
.pane{position:absolute;inset:0;display:none;}
.pane.active{display:block;}
.pane.terminal{padding:0;}
.pane.terminal #t{padding:12px;height:100%;width:100%;box-sizing:border-box;}
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
    <div id="t"></div>
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
<script>
(function() {
  // Creo tokens から xterm.js theme を構築 (runtime で var() 解決)
  const css = getComputedStyle(document.documentElement);
  const v = (name, fallback) => (css.getPropertyValue(name).trim() || fallback);
  const theme = {
    background: v('--color-surface-bg-base', '#0F1128'),
    foreground: v('--color-text-primary', '#EDEEF4'),
    cursor: v('--color-brand-primary', '#7D6BC2'),
    cursorAccent: v('--color-surface-bg-base', '#0F1128'),
    selectionBackground: v('--color-brand-primary-subtle', '#2C2843')
  };
  const term = new Terminal({
    fontFamily: '"Cascadia Code", "Cascadia Mono", "SF Mono", Menlo, Consolas, monospace',
    fontSize: 13,
    lineHeight: 1.15,
    letterSpacing: 0,
    theme: theme,
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
  term.open(document.getElementById('t'));
  // 初回は terminal が hidden の可能性があるので、active 化時にも fit する。
  fitAddon.fit();

  function sendResize() {
    window.ipc.postMessage(JSON.stringify({t:'resize', cols: term.cols, rows: term.rows}));
  }

  window.addEventListener('resize', () => {
    if (document.getElementById('pane-terminal').classList.contains('active')) {
      fitAddon.fit();
      sendResize();
    }
  });

  term.onData(d => {
    window.ipc.postMessage(JSON.stringify({t:'in', d: d}));
  });

  // Rust から呼ばれる関数
  window.onPtyData = function(b64) {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    term.write(bytes);
  };

  // ========= VP-100 Phase 2: Pane 切替 API =========
  // Rust → JS で active pane を切替。kind が null の場合は empty 状態を表示。
  // payload: {kind: "agent"|"canvas"|"preview"|"shell"|null, pane_id, preview_url}
  const KIND_TO_PANE = {
    agent: 'pane-terminal',
    shell: 'pane-terminal',
    canvas: 'pane-canvas',
    preview: 'pane-preview',
    empty: 'pane-empty',
  };
  // 現在 active な pane の info (slot:rect 送出時の pane_id 補完用)
  let activePaneInfo = null;
  function setActiveImpl(info) {
    activePaneInfo = info || null;
    const kind = info && info.kind ? info.kind : 'empty';
    const targetId = KIND_TO_PANE[kind] || 'pane-empty';
    document.querySelectorAll('.pane').forEach(el => {
      const isActive = (el.id === targetId);
      el.classList.toggle('active', isActive);
      // 動的に data-pane-id を設定 (γ-light: native overlay が pane_id で照合する想定)
      if (isActive && info && info.pane_id) {
        el.setAttribute('data-pane-id', info.pane_id);
      } else if (isActive) {
        el.removeAttribute('data-pane-id');
      }
    });
    if (kind === 'preview') {
      const frame = document.getElementById('preview-frame');
      const url = (info && info.preview_url) || 'about:blank';
      if (frame && frame.getAttribute('src') !== url) {
        frame.setAttribute('src', url);
      }
    }
    if (kind === 'agent' || kind === 'shell') {
      // hidden 中はサイズ計算が 0 になり xterm が壊れるので、active 化直後に fit + resize 通知
      try {
        fitAddon.fit();
        sendResize();
        focusTerm();
      } catch (_) {}
    }
    // active 切替直後に slot rect を一発送る (ResizeObserver 起動前 fail-safe)
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
  // ResizeObserver が active pane container の rect 変化を捕捉。
  // Phase 2 時点では Rust は受け取って store するだけ (Phase 4+ で native overlay 同期に使用)。
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
  // ResizeObserver は host (= main area の root) に張る。中の pane も同サイズでリサイズされる。
  if (typeof ResizeObserver !== 'undefined') {
    const ro = new ResizeObserver(() => sendSlotRect());
    ro.observe(document.getElementById('host'));
  }

  // 初期化完了を Rust に通知 (resize 情報も同時に)
  window.ipc.postMessage(JSON.stringify({t:'ready'}));
  sendResize();

  // DevTools console から term を手動検査できるよう露出
  window.__vpTerm = term;

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
      window.ipc.postMessage(JSON.stringify({t:'debug', msg: 'OSC 52 copy: ' + text.length + ' chars'}));
    } catch (e) {
      window.ipc.postMessage(JSON.stringify({t:'debug', msg: 'OSC 52 decode error: ' + e.message}));
    }
    return true;
  });

  // focus 制御
  const container = document.getElementById('t');
  const focusTerm = () => {
    try { term.focus(); } catch (_) {}
  };
  container.addEventListener('mousedown', focusTerm);
  container.addEventListener('click', focusTerm);
  window.addEventListener('focus', focusTerm);
  setTimeout(focusTerm, 100);
  setTimeout(focusTerm, 500);

  // ----- Copy / Paste -----
  const dbg = (msg) => window.ipc.postMessage(JSON.stringify({t:'debug', msg: msg}));

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

  window.addEventListener('keydown', (e) => {
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

  // Copy-on-select
  container.addEventListener('mouseup', () => {
    setTimeout(() => {
      const sel = term.getSelection();
      if (sel && sel.length > 0) doCopy();
    }, 0);
  });

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
