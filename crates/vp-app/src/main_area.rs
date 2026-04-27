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
<html lang="en" data-theme="contrast-dark">
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
/* Phase 2.5 bug fix: position:relative を付けると inset:0 が効かなくなり pane-terminal が 0x0 に潰れる。
   .pane が既に position:absolute なので containing block 機能は十分、 padding 0 だけ追加で OK。 */
.pane.terminal{padding:0;}
/* Phase 2.5: per-Lane instance container. lane-host が pane-terminal 全領域を埋め、
   各 .lane-pane が absolute で重なる。 active のみ display:block。 */
#lane-host{position:absolute;inset:0;}
.lane-pane{position:absolute;inset:0;display:none;}
.lane-pane.active{display:block;}
.lane-pane .lane-term{padding:12px;height:100%;width:100%;box-sizing:border-box;}
/* どの Lane も無い時の placeholder (active class で表示制御、 default は表示) */
#lane-empty{position:absolute;inset:0;display:none;place-items:center;color:var(--color-text-tertiary);text-align:center;}
#lane-empty.active{display:grid;}
#lane-empty h1{font-weight:400;font-size:1.1rem;margin:0;}
#lane-empty p{margin:.25rem 0 0;font-size:.85rem;}
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
/* contrast-dark の terminal ANSI 16 色 — creo-ui に red/green/yellow/blue/cyan が無いので
   いつもの色空間メソッド (OKLCH) で hue rotation して role に合った色を synthesize。
   chroma は brand と同等 (~0.16)、L=0.65 (normal) / 0.78 (bright) で
   背景 (L=0.16) との contrast を WCAG AA 以上確保。
   関連: mem_1CaSmvKgsX2AQxRYFYgNM3 (Lead pane shell), creo-ui contrast-dark theme. */
:root[data-theme="contrast-dark"]{
  --terminal-ansi-black:oklch(0.20 0.02 280);
  --terminal-ansi-red:oklch(0.65 0.18 25);
  --terminal-ansi-green:oklch(0.70 0.15 145);
  --terminal-ansi-yellow:oklch(0.78 0.13 90);
  --terminal-ansi-blue:oklch(0.65 0.16 255);
  --terminal-ansi-magenta:oklch(0.70 0.18 320);
  --terminal-ansi-cyan:oklch(0.72 0.13 195);
  --terminal-ansi-white:var(--color-text-secondary);
  --terminal-ansi-bright-black:var(--color-text-tertiary);
  --terminal-ansi-bright-red:oklch(0.78 0.20 25);
  --terminal-ansi-bright-green:oklch(0.82 0.18 145);
  --terminal-ansi-bright-yellow:oklch(0.88 0.15 90);
  --terminal-ansi-bright-blue:oklch(0.78 0.18 255);
  --terminal-ansi-bright-magenta:oklch(0.82 0.20 320);
  --terminal-ansi-bright-cyan:oklch(0.85 0.15 195);
  --terminal-ansi-bright-white:var(--color-text-primary);
}
</style>
</head>
<body>
<div id="host">
  <!-- 各 .pane は data-kind を持つ。data-pane-id は active pane 切替時に Rust が動的に設定。
       VP-100 γ-light: ResizeObserver が slot rect を IPC で送る (Phase 4+ で native overlay 同期に使う)。 -->
  <!-- Phase 2.5 (per-Lane instance): pane-terminal 内に lane-host を置き、
       Lane ごとに xterm.js + WebSocket instance を mount。 active な 1 つだけ display:block。 -->
  <div class="pane terminal" id="pane-terminal" data-kind="terminal">
    <div id="lane-host"></div>
    <!-- empty placeholder: どの Lane も無い時に出す -->
    <div id="lane-empty" class="lane-empty active">
      <main>
        <h1>No Lane selected</h1>
        <p>sidebar から Lane を選択してください (or accordion を開いて auto-spawn)</p>
      </main>
    </div>
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
<script>
"#,
    include_str!("../assets/addon-webgl.min.js"),
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
  // Creo tokens から xterm.js theme を構築 (全 Lane instance で共有)。
  // OKLCH 値は xterm.js の内部 color parser が直接解釈できないので、
  // hidden probe で `color: var(...)` を browser に解決させて
  // `getComputedStyle().color` から rgb(R,G,B) を取得 → hex に降ろす。
  const probe = document.createElement('span');
  probe.style.position = 'absolute';
  probe.style.visibility = 'hidden';
  document.body.appendChild(probe);

  const resolveHex = (varName, fallback) => {
    probe.style.color = `var(${varName}, ${fallback})`;
    const rgb = getComputedStyle(probe).color;
    const m = rgb.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
    if (!m) return fallback;
    return '#' + [m[1], m[2], m[3]]
      .map(n => Number(n).toString(16).padStart(2, '0'))
      .join('');
  };

  const css = getComputedStyle(document.documentElement);
  const theme = {
    background: resolveHex('--color-surface-bg-base', '#0F1128'),
    foreground: resolveHex('--color-text-primary', '#EDEEF4'),
    cursor: resolveHex('--color-brand-primary', '#7D6BC2'),
    cursorAccent: resolveHex('--color-surface-bg-base', '#0F1128'),
    selectionBackground: resolveHex('--color-brand-primary-subtle', '#2C2843'),
    black: resolveHex('--terminal-ansi-black', '#1E1E2E'),
    red: resolveHex('--terminal-ansi-red', '#F38BA8'),
    green: resolveHex('--terminal-ansi-green', '#A6E3A1'),
    yellow: resolveHex('--terminal-ansi-yellow', '#F9E2AF'),
    blue: resolveHex('--terminal-ansi-blue', '#89B4FA'),
    magenta: resolveHex('--terminal-ansi-magenta', '#F5C2E7'),
    cyan: resolveHex('--terminal-ansi-cyan', '#94E2D5'),
    white: resolveHex('--terminal-ansi-white', '#BAC2DE'),
    brightBlack: resolveHex('--terminal-ansi-bright-black', '#585B70'),
    brightRed: resolveHex('--terminal-ansi-bright-red', '#F38BA8'),
    brightGreen: resolveHex('--terminal-ansi-bright-green', '#A6E3A1'),
    brightYellow: resolveHex('--terminal-ansi-bright-yellow', '#F9E2AF'),
    brightBlue: resolveHex('--terminal-ansi-bright-blue', '#89B4FA'),
    brightMagenta: resolveHex('--terminal-ansi-bright-magenta', '#F5C2E7'),
    brightCyan: resolveHex('--terminal-ansi-bright-cyan', '#94E2D5'),
    brightWhite: resolveHex('--terminal-ansi-bright-white', '#FFFFFF')
  };
  probe.remove();
  const monoFamily = (css.getPropertyValue('--typography-family-mono') || '').trim()
    || '"JetBrainsMono Nerd Font", "Cascadia Code", "SF Mono", Menlo, Consolas, monospace';

  // ========= Phase 2.5: per-Lane instance registry =========
  // Lane address → {term, fitAddon, ws, container, ro, webglAddon}
  // Architecture v4: Lane = Session Process なので 1 Lane に 1 xterm.js + 1 WebSocket。
  // memory cost > switch reliability の trade-off で per-instance を選択 (user 決定)。
  const laneInstances = new Map();

  function dbg(msg) {
    try { window.ipc.postMessage(JSON.stringify({t:'debug', msg: msg})); } catch (_) {}
  }

  function createLaneInstance(address, port) {
    const host = document.getElementById('lane-host');
    if (!host) {
      console.error('createLaneInstance: lane-host not found');
      return null;
    }
    // container は Lane あたり 1 つ、 absolute で pane-terminal 全領域を埋める
    const container = document.createElement('div');
    container.className = 'lane-pane';
    container.dataset.laneAddr = address;
    const tdiv = document.createElement('div');
    tdiv.className = 'lane-term';
    container.appendChild(tdiv);
    host.appendChild(container);

    const term = new Terminal({
      fontFamily: monoFamily,
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

    // WebGL renderer (per-instance、 個別に context 持つ)
    let webglAddon = null;
    try {
      webglAddon = new WebglAddon.WebglAddon();
      term.loadAddon(webglAddon);
      webglAddon.onContextLoss(() => {
        console.warn('[xterm:' + address + '] WebGL context loss — DOM fallback');
        webglAddon.dispose();
      });
    } catch (e) {
      console.warn('[xterm:' + address + '] WebGL unavailable:', e);
    }

    term.open(tdiv);
    // hidden 状態で fit すると 0 cols になるので、 showLane の active 化後にも fit を呼ぶ
    try { fitAddon.fit(); } catch (_) {}

    // ===== WebSocket: SP に直接接続 (Phase 2.5: Rust 側 mpsc 中継を撤去) =====
    // URL: ws://127.0.0.1:<sp_port>/ws/terminal?lane=<address>&cols=&rows=
    const initCols = term.cols || 80;
    const initRows = term.rows || 24;
    const wsUrl = 'ws://127.0.0.1:' + port + '/ws/terminal?lane='
      + encodeURIComponent(address)
      + '&cols=' + initCols + '&rows=' + initRows;
    const ws = new WebSocket(wsUrl);
    ws.binaryType = 'arraybuffer';

    function sendResize() {
      if (ws.readyState !== WebSocket.OPEN) return;
      try {
        ws.send(JSON.stringify({type:'resize', cols: term.cols, rows: term.rows}));
      } catch (_) {}
    }

    ws.onopen = () => {
      dbg('[lane:' + address + '] ws open');
      try { fitAddon.fit(); } catch (_) {}
      sendResize();
    };
    ws.onmessage = (ev) => {
      if (ev.data instanceof ArrayBuffer) {
        term.write(new Uint8Array(ev.data));
      } else if (typeof ev.data === 'string') {
        // server からの error 等 (Text frame)
        term.write('\r\n\x1b[33m[lane:' + address + '] ' + ev.data + '\x1b[0m\r\n');
      }
    };
    ws.onclose = (ev) => {
      dbg('[lane:' + address + '] ws close code=' + ev.code);
      // 静かに切断 (Lane が dead になった、 user が remove した、 等)
    };
    ws.onerror = () => {
      term.write('\r\n\x1b[31m[lane:' + address + '] WebSocket error\x1b[0m\r\n');
    };

    // input → WS (Rust 中継せず直接送信)
    term.onData((d) => {
      if (ws.readyState !== WebSocket.OPEN) return;
      try {
        ws.send(new TextEncoder().encode(d));
      } catch (e) {
        dbg('[lane:' + address + '] input send error: ' + e);
      }
    });

    // OSC 52 (clipboard) intercept — Lane ごとに独立
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
      } catch (_) {}
      return true;
    });

    // Copy/Paste (per-Lane scope)
    function doCopy() {
      const sel = term.getSelection();
      if (!sel) return false;
      navigator.clipboard.writeText(sel).catch(() => {
        window.ipc.postMessage(JSON.stringify({t:'copy', d: sel}));
      });
      return true;
    }
    function doPaste() {
      navigator.clipboard.readText()
        .then((text) => { if (text) term.paste(text); })
        .catch((err) => dbg('[lane:' + address + '] paste failed: ' + err));
    }
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
    container.addEventListener('click', () => { try { term.focus(); } catch (_) {} });

    // ResizeObserver (per-container): active な間だけ fit + resize 通知
    const ro = new ResizeObserver(() => {
      if (!container.classList.contains('active')) return;
      try { fitAddon.fit(); sendResize(); } catch (_) {}
    });
    ro.observe(container);

    return { term, fitAddon, ws, container, ro, webglAddon };
  }

  window.ensureLane = function(address, port) {
    if (laneInstances.has(address)) return;
    const inst = createLaneInstance(address, port);
    if (inst) {
      laneInstances.set(address, inst);
      dbg('[lane:' + address + '] ensured');
    }
  };

  window.showLane = function(address) {
    // empty placeholder は非表示に
    const empty = document.getElementById('lane-empty');
    if (empty) empty.classList.toggle('active', !address || !laneInstances.has(address));
    for (const [addr, info] of laneInstances) {
      info.container.classList.toggle('active', addr === address);
    }
    const active = laneInstances.get(address);
    if (active) {
      // active 化直後の hidden→visible 遷移で fit / focus
      setTimeout(() => {
        try {
          active.fitAddon.fit();
          if (active.ws.readyState === WebSocket.OPEN) {
            active.ws.send(JSON.stringify({type:'resize', cols: active.term.cols, rows: active.term.rows}));
          }
          active.term.focus();
        } catch (_) {}
      }, 0);
    }
  };

  window.removeLane = function(address) {
    const info = laneInstances.get(address);
    if (!info) return;
    try {
      info.ws.close();
      info.ro.disconnect();
      if (info.webglAddon) { try { info.webglAddon.dispose(); } catch (_) {} }
      info.term.dispose();
      info.container.remove();
    } catch (e) {
      console.error('removeLane error:', e);
    }
    laneInstances.delete(address);
    dbg('[lane:' + address + '] removed');
  };

  // 互換: legacy callers (terminal.rs::build_output_script) が onPtyData を呼ぶケースを安全に noop。
  // Phase 2.5 では Rust 側 PTY 経路は廃止されているが、 startup の placeholder PTY が
  // 残っているケースで誤って呼ばれても落ちないように。
  window.onPtyData = function(_b64) {
    // no-op: Lane WebSocket が直接 term.write するので Rust 経路の出力は無視
  };

  window.addEventListener('resize', () => {
    // active な Lane だけ fit + resize 通知
    for (const [, info] of laneInstances) {
      if (info.container.classList.contains('active')) {
        try {
          info.fitAddon.fit();
          if (info.ws.readyState === WebSocket.OPEN) {
            info.ws.send(JSON.stringify({type:'resize', cols: info.term.cols, rows: info.term.rows}));
          }
        } catch (_) {}
        break;
      }
    }
  });

  // ========= Architecture v4: Lane 切替 API =========
  // Rust → JS で active Lane を切替。kind が null の場合は empty 状態を表示。
  // payload: {kind: "terminal"|"canvas"|"preview"|null, pane_id (= Lane address), preview_url}
  // 旧 "agent"/"shell" は terminal 系として "terminal" に統合 (Lane SSOT 化に伴う)。
  const KIND_TO_PANE = {
    terminal: 'pane-terminal',
    // 互換: 旧 callsite が "agent"/"shell" を渡しても動く
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
    if (kind === 'terminal' || kind === 'agent' || kind === 'shell') {
      // Phase 2.5: per-Lane instance を切替 (= showLane(address))。 pane_id は Lane address。
      // showLane が空なら lane-empty placeholder を出す。
      try {
        window.showLane(info && info.pane_id);
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
  // PH#4: rAF debounce — window resize 中の高頻度発火で event queue が詰まらないよう、
  // 1 frame に最大 1 回 sendSlotRect を呼ぶように制限。
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

  // 初期化完了を Rust に通知 (Phase 2.5: legacy `sendResize()` は撤去、 Lane 個別の WS が resize 通知する)
  window.ipc.postMessage(JSON.stringify({t:'ready'}));

  // DevTools console から laneInstances を手動検査できるよう露出
  window.__vpLanes = laneInstances;

  // 全体 Ctrl+Shift+C のフォールバック (active Lane の selection を copy)
  // Lane 個別の handler では取り切れないケース (focus が container 外にある等) の保険。
  window.addEventListener('keydown', (e) => {
    if (e.ctrlKey && e.shiftKey && (e.key === 'C' || e.key === 'c')) {
      // active な Lane を探して selection 取得
      for (const [, info] of laneInstances) {
        if (info.container.classList.contains('active')) {
          const sel = info.term.getSelection();
          if (sel) {
            e.preventDefault();
            e.stopPropagation();
            navigator.clipboard.writeText(sel).catch(() => {
              window.ipc.postMessage(JSON.stringify({t:'copy', d: sel}));
            });
          }
          break;
        }
      }
    }
  }, true);

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
