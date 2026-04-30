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
/* Phase 5-A: Project-scope Stand placeholder panes (PP/GE/HP) */
.pane.stand{display:none;place-items:center;}
.pane.stand.active{display:grid;}
.pane.stand main{text-align:center;max-width:520px;padding:0 24px;}
.pane.stand h1{font-weight:500;font-size:1.6rem;margin:0 0 .5rem;color:var(--color-text-primary);}
.pane.stand p{color:var(--color-text-tertiary);margin:.25rem 0;font-size:.95rem;}
.pane.stand .sub{font-size:.85rem;color:var(--color-text-tertiary);opacity:.85;margin-top:1rem;line-height:1.6;}
.pane.stand .brand{color:var(--color-brand-primary);}
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
  <!-- Phase 5-A: Project-scope Stand placeholder panes (PP/GE/HP)。
       click action は Phase 3-B で導入した sidebar の vp-project-stand-row から発火、
       将来 (Phase 6+) で Canvas 実描画 / Ruby eval / MIDI 制御を bind する予定。 -->
  <div class="pane stand" id="pane-paisley-park" data-kind="paisley_park">
    <main>
      <h1>🧭 Paisley Park</h1>
      <p>Information Navigator — Canvas / Markdown / HTML / 画像</p>
      <p class="sub">Phase 6+ で <span class="brand">/api/show 結合</span>、 file watch 連動、 layered Canvas を実装予定</p>
    </main>
  </div>
  <div class="pane stand" id="pane-gold-experience" data-kind="gold_experience">
    <main>
      <h1>🌿 Gold Experience</h1>
      <p>Code Runner — 動的生命注入エンジン</p>
      <p class="sub">Phase 6+ で <span class="brand">Ruby eval / process_runner</span> 結合、 inline result preview を実装予定</p>
    </main>
  </div>
  <div class="pane stand" id="pane-hermit-purple" data-kind="hermit_purple">
    <main>
      <h1>🍇 Hermit Purple</h1>
      <p>External Control — MIDI / MCP / tmux</p>
      <p class="sub">Phase 6+ で <span class="brand">MIDI lpd8 / MCP server / tmux session</span> 接続パネルを実装予定</p>
    </main>
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

  // 右クリック context menu (macOS の text actions / AutoFill / Services 等) を全面 suppress。
  //  per-Lane terminal container は別 listener で paste 動作に差替え済 (e.preventDefault + doPaste)、
  //  capture phase の document listener は preventDefault のみ呼ぶので container listener の paste も生きる。
  //  対象外: preview iframe (cross-context、 iframe 内に独立 listener が必要)。
  document.addEventListener('contextmenu', (e) => { e.preventDefault(); }, { capture: true });

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
    // Phase 5-D 実験 (ghost char 調査): WebGL の dirty cell tracking で文字幅再計算後に古い cells が
    //  clear されない疑惑検証中。 一時的に DOM renderer fallback で再現するか確認。
    //  → 再現しなければ WebGL 起因確定、 dispose 戦略 or canvas 移行を検討。
    //  → 再現するなら xterm.js core or wcwidth 起因、 別調査。
    const VP_USE_WEBGL = false; // TEMPORARY for ghost char repro test
    let webglAddon = null;
    if (VP_USE_WEBGL) {
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
    }

    term.open(tdiv);
    // 実験: terminal textarea の autocomplete を **on** に。 browser の autofill が typed commands を
    //  保存して提案する挙動を観察する。 dogfood で「過去 command の suggestion が出るか / UI の overlay が
    //  邪魔にならないか / cross-lane suggestion 混在しないか」 を実測。 問題あれば off に戻す。
    try { term.textarea && term.textarea.setAttribute('autocomplete', 'on'); } catch (_) {}
    // hidden 状態で fit すると 0 cols になるので、 showLane の active 化後にも fit を呼ぶ
    try { fitAddon.fit(); } catch (_) {}

    // ===== OSC notification capture (Slice 1: capture-only、 UI は後続 PR) =====
    // 3 codes 全部 cover ─ cc は terminal 検知して emit する code を切り替える可能性あり、
    // defensive にすべて hook して dogfood 中に何が来るかを catalog 化する。
    //
    // - OSC 9  (iTerm2 / Windows Terminal style):
    //     ESC ] 9 ; <message> BEL                ─ body only、 metadata 無し
    //     ESC ] 9 ; <subcode> ; <args> BEL       ─ iTerm2 拡張 (9;2=notification 等)、 cwd reporting にも overload
    // - OSC 99 (kitty notification protocol):
    //     ESC ] 99 ; <metadata> ; <payload> ESC \\
    //   metadata は colon-separated key=value (i=ID:d=0|1:p=title|body|close|...:a=focus|report:u=0|1|2 等)
    //   multi-chunk: 同 i=ID で `d=0` (cont) / `d=1` (final) を使い分け、 final で commit。
    // - OSC 777 (rxvt-unicode、 Ghostty / foot 等が踏襲):
    //     ESC ] 777 ; notify ; <TITLE> ; <BODY> BEL
    //
    // observed (2026-04-29 dogfood): cc は vp-app に対して OSC 99 multi-chunk を emit している。
    //   例: i=211:d=0:p=title;Claude Code → i=211:p=body;Claude is waiting for your input → i=211:d=1:a=focus;
    //
    // Phase S1 では capture が動くか確認するだけ ─ console.log + Rust tracing (`[xterm debug]` ログ) に流す。
    // S2 で id-based accumulator + `d=1` で commit + IPC push、 S3 で sidebar tint UI。
    //
    // ----- structured parse helpers (dogfood 観察用、 S2 accumulator の前哨) -----
    // raw payload は `[osc99:lane] i=211:d=0:p=title;Claude Code` の形式で、 colon が key delimiter、
    //  semicolon が value 開始 ─ 人間が毎回頭で parse するのは認知負荷が高いので、
    //  key=value 対を space-spread した一行サマリも併せて吐く:
    //    `[osc99-keys:lane] {i=211 d=0 p=title} value="Claude Code"`
    //
    // dogfood で観察したい open question:
    //   * cc が `t=` (semantic type tag) や `u=` (urgency 0/1/2) を emit するか
    //   * permission prompt 時の `p=body` 文字列 (input 待ちと distinguish できるか)
    //   * `p=close` / `p=icon` / `p=buttons` 等の non-title/body type が flow するか
    //   * OSC 9 / 777 が cc 以外の emitter から来るか
    function parseOsc99(payload) {
      const semi = payload.indexOf(';');
      const metaStr = semi >= 0 ? payload.substring(0, semi) : payload;
      const value = semi >= 0 ? payload.substring(semi + 1) : '';
      const m = {};
      for (const kv of metaStr.split(':')) {
        if (!kv) continue;
        const eq = kv.indexOf('=');
        if (eq > 0) m[kv.slice(0, eq)] = kv.slice(eq + 1);
        else m[kv] = '';
      }
      return { m, value };
    }
    function fmtOsc99Keys(m) {
      return Object.entries(m)
        .map(([k, v]) => v === '' ? k : k + '=' + v)
        .join(' ');
    }
    // OSC 9 = `9;<msg>` (無印 iTerm2 notify) or iTerm2 拡張 `9;<subcode>;<args>` (subcode 9=cwd reporting 等) の混在。
    //  先頭 segment が pure 数字なら subcode 形式と判定する。
    function parseOsc9(payload) {
      const semi = payload.indexOf(';');
      if (semi < 0) return { subcode: null, rest: payload };
      const head = payload.substring(0, semi);
      if (/^\d+$/.test(head)) {
        return { subcode: head, rest: payload.substring(semi + 1) };
      }
      return { subcode: null, rest: payload };
    }
    // OSC 777 = `notify;<title>;<body>` (urxvt / foot 流) — title/body を semicolon 区切りで取り出す。
    function parseOsc777(payload) {
      const parts = payload.split(';');
      if (parts[0] === 'notify' && parts.length >= 2) {
        return { title: parts[1] || '', body: parts.slice(2).join(';') };
      }
      return { title: null, body: payload };
    }

    try {
      term.parser.registerOscHandler(9, (data) => {
        const payload = String(data || '');
        console.log('[OSC 9] lane=' + address + ' payload=' + JSON.stringify(payload));
        dbg('[osc9:' + address + '] ' + payload);
        try {
          const p = parseOsc9(payload);
          if (p.subcode != null) {
            dbg('[osc9-keys:' + address + '] subcode=' + p.subcode + ' rest=' + JSON.stringify(p.rest));
          } else {
            dbg('[osc9-keys:' + address + '] (plain) msg=' + JSON.stringify(p.rest));
          }
        } catch (_) {}
        return true;
      });
      term.parser.registerOscHandler(99, (data) => {
        const payload = String(data || '');
        console.log('[OSC 99] lane=' + address + ' payload=' + JSON.stringify(payload));
        dbg('[osc99:' + address + '] ' + payload);
        try {
          const p = parseOsc99(payload);
          dbg('[osc99-keys:' + address + '] {' + fmtOsc99Keys(p.m) + '} value=' + JSON.stringify(p.value));
        } catch (_) {}
        // Phase 5-D Sprint C P2.1: final-chunk + focus action のみ「user attention 要求」 と判定。
        //  metadata は最初の ; までの key=value list。 d=1 (final) かつ a=focus を含む chunk が trigger。
        //  Rust 側で unread count を加算 → sidebar に push back → badge 表示。
        const semi = payload.indexOf(';');
        const meta = semi >= 0 ? payload.substring(0, semi) : payload;
        if (/\bd=1\b/.test(meta) && /\ba=focus\b/.test(meta)) {
          try {
            window.ipc.postMessage(JSON.stringify({ t: 'osc:notification', lane: address, code: 99 }));
          } catch (_) {}
        }
        return true;
      });
      term.parser.registerOscHandler(777, (data) => {
        const payload = String(data || '');
        console.log('[OSC 777] lane=' + address + ' payload=' + JSON.stringify(payload));
        dbg('[osc777:' + address + '] ' + payload);
        try {
          const p = parseOsc777(payload);
          if (p.title !== null) {
            dbg('[osc777-keys:' + address + '] title=' + JSON.stringify(p.title) + ' body=' + JSON.stringify(p.body));
          } else {
            dbg('[osc777-keys:' + address + '] (non-notify form) raw=' + JSON.stringify(p.body));
          }
        } catch (_) {}
        return true;
      });
    } catch (e) {
      console.warn('[xterm:' + address + '] OSC handler registration failed:', e);
    }

    // ===== WebSocket: SP に直接接続 (Phase 2.5: Rust 側 mpsc 中継を撤去) =====
    // URL: ws://127.0.0.1:<sp_port>/ws/terminal?lane=<address>&cols=&rows=
    //
    // Auto-reconnect (2026-04-28 PR #218): SP 再起動 / 一時的 network 断で WS が close した時、
    // 指数バックオフ (500ms → 16s) で最大 10 回 retry。 user が removeLane() を呼ぶまでは
    // disposed=false を保ち、 onclose を fail signal として扱う。 Phase 5-D で TUI→Process
    // 経路に同 pattern (mem_1CYqH6rR7U6RBTxjyDHnfH) を実装済、 vp-app per-Lane WS にも横展開。
    const RETRY_BACKOFF_MS = [500, 1000, 2000, 4000, 8000, 16000, 16000, 16000, 16000, 16000];
    const MAX_RETRIES = RETRY_BACKOFF_MS.length;
    const conn = { ws: null, disposed: false, retryCount: 0, retryTimer: null };
    // Input keystroke buffer (FIFO、 max 1000 chunk)。 reconnect 中の数百 ms ~ 数秒の窓で
    //  user が typing した keystroke を保持して、 onopen で flush する。
    //  「ASCII fast typing 後ろのキーストロークが消失」 (dogfood 観測) への対策 ─ 旧 code は
    //  readyState !== OPEN で silent drop していたが、 reconnect 中に typing した分が消える。
    //  上限 1000 chunk: 1 chunk ≈ 1-数 byte なので最大 ~10KB、 stuck 時の memory 暴走を防ぐ。
    const inputBuffer = [];
    const INPUT_BUFFER_MAX = 1000;

    function sendResize() {
      if (!conn.ws || conn.ws.readyState !== WebSocket.OPEN) return;
      try {
        conn.ws.send(JSON.stringify({type:'resize', cols: term.cols, rows: term.rows}));
      } catch (_) {}
    }

    function connectWs() {
      if (conn.disposed) return;
      const initCols = term.cols || 80;
      const initRows = term.rows || 24;
      const wsUrl = 'ws://127.0.0.1:' + port + '/ws/terminal?lane='
        + encodeURIComponent(address)
        + '&cols=' + initCols + '&rows=' + initRows;
      const ws = new WebSocket(wsUrl);
      ws.binaryType = 'arraybuffer';
      conn.ws = ws;

      ws.onopen = () => {
        dbg('[lane:' + address + '] ws open');
        if (conn.retryCount > 0) {
          // reconnect: server は always full scrollback を replay する設計 (PR #218 で
          //  WS auto-reconnect 導入後、 reconnect ごとに重複 scrollback が来る)。
          //  既存 rendered state に scrollback を上書きすると、 同 ANSI sequence
          //  (cursor positioning / erase / scroll 等) が二度処理されて render state が drift、
          //  結果として ghost characters (mem_1CaVpvsBKR3ckieRXo1nwr) が出る。
          //  対策: term.reset() で xterm.js を clean canvas に戻し、 直後の scrollback replay で
          //  ground truth state を再構築する。 失う物は xterm.js own scrollback (history) のみ、
          //  server 側 scrollback (256KB) は保持されるので次回 full attach で復活。
          term.reset();
          term.write('\x1b[32m[lane:' + address + '] reconnected\x1b[0m\r\n');
        }
        conn.retryCount = 0;
        try { fitAddon.fit(); } catch (_) {}
        sendResize();
      };
      // 別 listener で input buffer flush ─ ws.onopen (property-based) と並走できる
      // (addEventListener は property assignment を override しない)。 PR #224 等で
      // onopen 本体が変更されてもこちらは独立、 conflict 回避。
      ws.addEventListener('open', () => {
        if (inputBuffer.length === 0) return;
        const flushed = inputBuffer.length;
        while (inputBuffer.length > 0 && conn.ws && conn.ws.readyState === WebSocket.OPEN) {
          const d = inputBuffer.shift();
          try {
            conn.ws.send(new TextEncoder().encode(d));
          } catch (_) {
            // 送信失敗 = WS が closing/closed 状態。 残りは drop (次 reconnect で再現難しい)
            inputBuffer.length = 0;
            break;
          }
        }
        dbg('[lane:' + address + '] input buffer flushed (' + flushed + ' chunks)');
      });
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
        if (conn.disposed) return;
        if (conn.retryCount >= MAX_RETRIES) {
          term.write('\r\n\x1b[31m[lane:' + address + '] reconnect failed after '
            + MAX_RETRIES + ' attempts, give up\x1b[0m\r\n');
          return;
        }
        const wait = RETRY_BACKOFF_MS[conn.retryCount];
        conn.retryCount++;
        term.write('\r\n\x1b[33m[lane:' + address + '] disconnected (code=' + ev.code
          + '), reconnecting in ' + wait + 'ms (' + conn.retryCount + '/' + MAX_RETRIES
          + ')...\x1b[0m\r\n');
        conn.retryTimer = setTimeout(connectWs, wait);
      };
      ws.onerror = () => {
        // onerror 直後に onclose が必ず fire する (W3C spec) ので retry はそこで処理。
        // ここでは log のみ ─ 「WebSocket error」 の冗長 noise を避ける。
        dbg('[lane:' + address + '] ws error (will close)');
      };
    }

    connectWs(); // initial connect

    // input → WS (Rust 中継せず直接送信)。
    //  reconnect 中 (readyState !== OPEN) は inputBuffer に積んで onopen で flush する ─
    //  silent drop を避ける (dogfood で 「fast typing 後ろが消失」 と観測されてた問題)。
    term.onData((d) => {
      if (!conn.ws || conn.ws.readyState !== WebSocket.OPEN) {
        inputBuffer.push(d);
        if (inputBuffer.length > INPUT_BUFFER_MAX) {
          inputBuffer.shift();
          dbg('[lane:' + address + '] input buffer overflow, oldest dropped');
        }
        return;
      }
      try {
        conn.ws.send(new TextEncoder().encode(d));
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
      // Phase 4-paste-fix: navigator.clipboard.readText() は webview の permission policy で
      // silent fail することがあるので、 **常に IPC fallback を併用**。 Rust 側 arboard が
      // OS clipboard を読んで `window.deliverPaste(text)` で戻してくる経路。
      try {
        navigator.clipboard.readText()
          .then((text) => { if (text) term.paste(text); })
          .catch(() => {
            window.ipc.postMessage(JSON.stringify({t:'paste:request'}));
          });
      } catch (_) {
        // navigator.clipboard 自体が undefined のケース (古い WebKit 等)
        window.ipc.postMessage(JSON.stringify({t:'paste:request'}));
      }
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

    return { term, fitAddon, conn, container, ro, webglAddon };
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
          if (active.conn.ws && active.conn.ws.readyState === WebSocket.OPEN) {
            active.conn.ws.send(JSON.stringify({type:'resize', cols: active.term.cols, rows: active.term.rows}));
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
      // 意図的 dispose ─ retry loop を止めて、 onclose の reconnect スケジュールを抑止
      info.conn.disposed = true;
      if (info.conn.retryTimer) {
        clearTimeout(info.conn.retryTimer);
        info.conn.retryTimer = null;
      }
      if (info.conn.ws) info.conn.ws.close();
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

  // Phase 2.x-d: 旧 onPtyData shim も terminal::build_output_script と一緒に撤去済。
  // Lane WebSocket が直接 term.write するので Rust 経路の出力は存在しない。

  // Phase 4-paste-fix: Rust 側 arboard で読み取った OS clipboard 内容を active Lane の xterm に inject。
  // `terminal.rs::handle_ipc_message` の `paste:request` → `AppEvent::PasteText` → `app.rs` event loop
  // で `main_view.evaluate_script("window.deliverPaste(text)")` の最終受け取り口。
  window.deliverPaste = function(text) {
    if (!text) return;
    for (const [, info] of laneInstances) {
      if (info.container.classList.contains('active')) {
        try {
          info.term.paste(text);
        } catch (e) {
          console.error('deliverPaste error:', e);
        }
        return;
      }
    }
    // active Lane が無い場合は noop
  };

  window.addEventListener('resize', () => {
    // active な Lane だけ fit + resize 通知
    for (const [, info] of laneInstances) {
      if (info.container.classList.contains('active')) {
        try {
          info.fitAddon.fit();
          if (info.conn.ws && info.conn.ws.readyState === WebSocket.OPEN) {
            info.conn.ws.send(JSON.stringify({type:'resize', cols: info.term.cols, rows: info.term.rows}));
          }
        } catch (_) {}
        break;
      }
    }
  });

  // ========= Architecture v4: Lane / Stand 切替 API =========
  // Rust → JS で active Lane / Stand を切替。kind が null の場合は empty 状態を表示。
  // payload: {kind: "terminal"|"canvas"|"preview"|"paisley_park"|"gold_experience"|"hermit_purple"|null, pane_id, preview_url}
  // Phase 5-A: Project-scope Stand (PP/GE/HP) を click 可能 pane として追加。
  const KIND_TO_PANE = {
    terminal: 'pane-terminal',
    canvas: 'pane-canvas',
    preview: 'pane-preview',
    paisley_park: 'pane-paisley-park',
    gold_experience: 'pane-gold-experience',
    hermit_purple: 'pane-hermit-purple',
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
    if (kind === 'terminal') {
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
