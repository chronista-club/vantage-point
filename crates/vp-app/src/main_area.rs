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
    /// Pane kind ("terminal" | "preview" | "paisley_park" | "gold_experience" | "hermit_purple" | "empty" | null)
    /// null = 何も active でない (空状態を表示)。
    /// VP-142 cleanup (PR-ε-4): legacy "canvas" kind 削除 (PR-ε-3 で PP body が Smart Canvas surface 物理化)
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
    include_str!("../assets/xterm.css"),
    r#"
html,body{margin:0;padding:0;height:100%;width:100%;background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:var(--typography-family-sans);}
body{overflow:hidden;}
#host{position:relative;width:100%;height:100%;}
/* VP-140 (PR-ε-1): 3D Frame Layout Engine 化。 旧 `.pane{display:none} + .pane.active{display:block}`
   による visibility gating を廃止、 left/top/width/height/opacity を JS (Frame Engine renderer.ts) で
   制御する形に inversion。 Pane は分割で生まれるのではなく、 frame 全体に対する transform を持つ
   独立 portable object として配置される (LSCM 公理 A1 と同型構造)。
   `.active` class は legacy 互換 (sendSlotRect / showLane gate) のため renderer が primary pane に
   付け替える、 表示制御自体は opacity が司る。 transition 速度は creo-ui-editor-host の token として
   `--frame-transition-ms` で runtime 編集可能 (Ctrl+Shift+E)。 */
:root{
  --frame-transition-ms:220ms;
  --frame-transition-easing:cubic-bezier(.2,.8,.2,1);
  /* VP-143: Echoes terminal (xterm.js) の Live Token 群。 creo-ui-editor-host (Ctrl+Shift+E)
     で runtime 調整可能。 JS 側 createLaneInstance が値を読んで `new Terminal({...})` を構築、
     MutationObserver が documentElement style 変更を捕捉して全 terminal に setter +
     fitAddon.fit() + WS resize 通知で伝播 → 既存 lane terminal も即時反映。
     default は旧 hardcoded 値と同じなので既存挙動への regression なし。 */
  --terminal-font-size:16;
  --terminal-line-height:1.27;
  --terminal-letter-spacing:0;
  --terminal-font-family:'VPMono35', 'VPMono', 'JetBrainsMono Nerd Font', 'Cascadia Code', 'SF Mono', Menlo, Consolas, monospace;
  --terminal-cursor-style:bar; /* "bar" / "block" / "underline" */
}
.pane{
  position:absolute;
  left:0;top:0;width:100%;height:100%;
  opacity:0;
  pointer-events:none;
  transition:
    top var(--frame-transition-ms) var(--frame-transition-easing),
    left var(--frame-transition-ms) var(--frame-transition-easing),
    width var(--frame-transition-ms) var(--frame-transition-easing),
    height var(--frame-transition-ms) var(--frame-transition-easing),
    opacity calc(var(--frame-transition-ms) * 0.82) ease;
  will-change:top,left,width,height,opacity;
}
.pane.active{pointer-events:auto;}
.pane.terminal{padding:0;}
/* Phase 2.5: per-Lane instance container. lane-host が pane-terminal 全領域を埋め、
   各 .lane-pane が absolute で重なる。 active のみ display:block。 */
#lane-host{position:absolute;inset:0;}
.lane-pane{position:absolute;inset:0;display:none;}
.lane-pane.active{display:block;}
.lane-pane .lane-term{padding:0;height:100%;width:100%;box-sizing:border-box;}
/* どの Lane も無い時の placeholder (active class で表示制御、 default は表示) */
#lane-empty{position:absolute;inset:0;display:none;place-items:center;color:var(--color-text-tertiary);text-align:center;}
#lane-empty.active{display:grid;}
#lane-empty .lane-empty-icon{width:44px;height:44px;display:block;margin:0 auto .75rem;opacity:.55;}
#lane-empty h1{font-weight:400;font-size:1.1rem;margin:0;}
#lane-empty p{margin:.25rem 0 0;font-size:.85rem;}
/* VP-141 (PR-ε-2): Pane header chrome — pane に「ヘッダ + body」 構造を持たせる共通 chrome。
   icon + Stand 名 + breadcrumb + actions (Clear 等) を提供。 terminal pane (Echoes、 xterm.js
   full-bleed) は header なしで除外。 .pane-header と .pane-body は両方 position:absolute なので
   .pane.stand/empty の display:grid context から opt-out される (centering は body 側の
   `.center` modifier で個別制御)。 */
.pane-header{
  position:absolute;
  top:0;left:0;right:0;height:28px;
  display:flex;
  align-items:center;
  gap:8px;
  padding:0 10px;
  font-size:12px;
  background:var(--color-surface-bg-raised);
  border-bottom:1px solid var(--color-border-subtle);
  user-select:none;
  -webkit-app-region:drag;
  z-index:1;
}
.pane-header .pane-title{
  flex:1;
  display:flex;
  align-items:center;
  gap:6px;
  color:var(--color-text-primary);
  min-width:0;
}
.pane-header .pane-icon{flex-shrink:0;font-size:14px;}
.pane-header .pane-name{font-weight:500;}
.pane-header .pane-breadcrumb{color:var(--color-text-tertiary);font-size:11px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.pane-header .pane-actions{
  display:flex;
  gap:4px;
  -webkit-app-region:no-drag;
}
.pane-header .pane-action-btn{
  cursor:pointer;
  padding:2px 8px;
  font-size:11px;
  background:transparent;
  border:1px solid var(--color-border-subtle);
  border-radius:4px;
  color:var(--color-text-secondary);
  font-family:inherit;
}
.pane-header .pane-action-btn:hover{background:var(--color-surface-bg-elevated);color:var(--color-text-primary);}
.pane-body{
  position:absolute;
  top:28px;left:0;right:0;bottom:0;
  overflow:auto;
}
.pane-body.center{display:grid;place-items:center;}
.pane-body iframe{width:100%;height:100%;border:0;background:#fff;}
/* PP markdown render 領域 (PR-ε-3 で mcp__show 経由 markdown が流れ込む rendering target) */
.pp-content{padding:16px 20px;color:var(--color-text-primary);font-size:13px;line-height:1.6;}
.pp-content h1{font-size:1.6rem;font-weight:500;margin:0 0 .5rem;color:var(--color-text-primary);}
.pp-content h2{font-size:1.3rem;font-weight:500;margin:1.2rem 0 .5rem;}
.pp-content h3{font-size:1.1rem;font-weight:500;margin:1rem 0 .4rem;}
.pp-content p{margin:.5rem 0;color:var(--color-text-secondary);}
.pp-content code{background:var(--color-surface-bg-raised);padding:1px 5px;border-radius:3px;font-family:var(--typography-family-mono);font-size:.9em;}
.pp-content pre{background:var(--color-surface-bg-raised);padding:12px;border-radius:6px;overflow-x:auto;}
.pp-content pre code{background:transparent;padding:0;}
.pp-content a{color:var(--color-brand-primary);}
.pp-content ul,.pp-content ol{padding-left:1.5em;margin:.5rem 0;}
.pp-content blockquote{border-left:3px solid var(--color-brand-primary-subtle);margin:.5rem 0;padding:0 1em;color:var(--color-text-tertiary);}
.pp-content table{border-collapse:collapse;margin:.5rem 0;}
.pp-content th,.pp-content td{border:1px solid var(--color-border-subtle);padding:4px 8px;}
.pp-content hr{border:0;border-top:1px solid var(--color-border-subtle);margin:1rem 0;}
.pp-placeholder{color:var(--color-text-tertiary);font-style:italic;}
/* VP-140: display:none/active gate 廃止、 always display:grid。 visibility は opacity (Frame Engine) が司る. */
/* VP-142 cleanup: .pane.canvas rules 削除 (pane-canvas HTML element 削除に伴い)。
   PP body が Smart Canvas surface を物理化したため pane-canvas は vestigial。 */
.pane.preview iframe{width:100%;height:100%;border:0;background:#fff;}
/* Phase 5-A: Project-scope Stand placeholder panes (PP/GE/HP) */
.pane.stand{display:grid;place-items:center;}
.pane.stand main{text-align:center;max-width:520px;padding:0 24px;}
.pane.stand h1{font-weight:500;font-size:1.6rem;margin:0 0 .5rem;color:var(--color-text-primary);}
.pane.stand p{color:var(--color-text-tertiary);margin:.25rem 0;font-size:.95rem;}
.pane.stand .sub{font-size:.85rem;color:var(--color-text-tertiary);opacity:.85;margin-top:1rem;line-height:1.6;}
.pane.stand .brand{color:var(--color-brand-primary);}
.pane.empty{display:grid;place-items:center;}
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
  <!-- 各 .pane の attribute 規約 (VP-141 で 2 attribute に分離):
       - data-kind="..."    : 静的 (HTML hardcode、 「terminal」「paisley_park」 等の kind classification)
       - data-frame-id="..." : 静的 (HTML hardcode、 Frame Engine の Scene lookup key、 「echoes」「pp」 等)
       - data-pane-id="..." : 動的 (active pane 切替時に main_area inline JS `setActiveImpl` が Lane address
                              等で setAttribute、 VP-100 γ-light native overlay sync 用 / Phase 4+ 同期 target)
       Frame Engine と legacy native overlay sync の attribute を分離しているのは、 Lane click で
       legacy 側 setAttribute が Frame Engine の static attribute を hijack して Scene lookup undefined
       → HIDDEN_TRANSFORM 適用 → pane が見えなくなる回帰を防ぐため (VP-141 fix)。
       VP-100 γ-light: ResizeObserver が slot rect を IPC で送る (Phase 4+ で native overlay 同期に使う)。 -->
  <!-- Phase 2.5 (per-Lane instance): pane-terminal 内に lane-host を置き、
       Lane ごとに xterm.js + WebSocket instance を mount。 active な 1 つだけ display:block。 -->
  <!-- VP-140 fail-safe: pane-terminal は Frame Engine が apply される前から visible にしておく。
       inline opacity:1 を CSS .pane{opacity:0} default より優先させ、 Frame Engine 不在 / 起動失敗時も
       少なくとも Echoes terminal は見える状態を保つ (= echoes が default visible 約束)。
       Frame Engine 起動後は inline style.opacity を engine が上書きする (lead-focus:1 / pp-focus:0)。 -->
  <div class="pane terminal" id="pane-terminal" data-kind="terminal" data-frame-id="echoes" style="opacity:1;pointer-events:auto;">
    <div id="lane-host"></div>
    <!-- empty placeholder: どの Lane も無い時に出す -->
    <div id="lane-empty" class="lane-empty active">
      <main>
        <!-- Lane を象徴する Codicon "git-merge" (user 指定、vscode-codicons より)。
             Lane (特に Wing) は git branch ベースの隔離環境なので branch graph
             アイコンが概念に合う。main_view は vp-asset:// 未登録で Nerd Font を
             load できないため、font glyph ではなく自己完結 inline SVG を使う。
             currentColor で text-tertiary に追従。 -->
        <svg class="lane-empty-icon" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M11.5 5.99998C10.9265 6.00006 10.3704 6.19736 9.92505 6.55877C9.47971 6.92018 9.17217 7.42373 9.05402 7.98498C7.17202 7.85998 5.46602 6.96298 5.08102 5.93098C5.67998 5.78724 6.20478 5.42744 6.55479 4.92058C6.9048 4.41373 7.05538 3.7955 6.97763 3.18446C6.89989 2.57343 6.5993 2.0126 6.13352 1.60954C5.66774 1.20648 5.06956 0.989562 4.45369 1.00039C3.83782 1.01121 3.24763 1.24902 2.7963 1.6682C2.34497 2.08738 2.06428 2.65842 2.00806 3.27181C1.95184 3.8852 2.12404 4.49776 2.49165 4.992C2.85925 5.48624 3.39638 5.82737 4.00002 5.94998V10.05C3.393 10.1739 2.85361 10.5188 2.48642 11.0178C2.11923 11.5168 1.95041 12.1343 2.01268 12.7507C2.07495 13.3671 2.36387 13.9385 2.82344 14.3539C3.28301 14.7694 3.88048 14.9995 4.50002 14.9995C5.11956 14.9995 5.71703 14.7694 6.1766 14.3539C6.63616 13.9385 6.92509 13.3671 6.98736 12.7507C7.04963 12.1343 6.88081 11.5168 6.51362 11.0178C6.14643 10.5188 5.60704 10.1739 5.00002 10.05V7.46598C6.15462 8.38805 7.57188 8.92022 9.04802 8.98598C9.1401 9.4506 9.36227 9.8795 9.68867 10.2227C10.0151 10.566 10.4323 10.8094 10.8917 10.9248C11.3511 11.0401 11.8338 11.0225 12.2836 10.8741C12.7334 10.7257 13.1318 10.4526 13.4324 10.0865C13.733 9.72047 13.9234 9.27655 13.9815 8.80647C14.0395 8.33639 13.9629 7.85948 13.7604 7.43128C13.5579 7.00308 13.238 6.64122 12.8378 6.38782C12.4376 6.13442 11.9737 5.99992 11.5 5.99998ZM3.00002 3.49998C3.00002 3.20331 3.08799 2.9133 3.25282 2.66662C3.41764 2.41995 3.65191 2.22769 3.92599 2.11416C4.20008 2.00063 4.50168 1.97092 4.79265 2.0288C5.08363 2.08668 5.3509 2.22954 5.56068 2.43932C5.77046 2.6491 5.91332 2.91637 5.9712 3.20734C6.02908 3.49831 5.99937 3.79991 5.88584 4.074C5.77231 4.34809 5.58005 4.58236 5.33337 4.74718C5.0867 4.912 4.79669 4.99998 4.50002 4.99998C4.10219 4.99998 3.72066 4.84194 3.43936 4.56064C3.15805 4.27933 3.00002 3.8978 3.00002 3.49998ZM6.00002 12.5C6.00002 12.7966 5.91205 13.0867 5.74722 13.3333C5.5824 13.58 5.34813 13.7723 5.07404 13.8858C4.79996 13.9993 4.49836 14.029 4.20738 13.9712C3.91641 13.9133 3.64914 13.7704 3.43936 13.5606C3.22958 13.3509 3.08672 13.0836 3.02884 12.7926C2.97096 12.5016 3.00067 12.2 3.1142 11.926C3.22773 11.6519 3.41999 11.4176 3.66666 11.2528C3.91334 11.088 4.20335 11 4.50002 11C4.89784 11 5.27938 11.158 5.56068 11.4393C5.84198 11.7206 6.00002 12.1022 6.00002 12.5ZM11.5 9.99998C11.2033 9.99998 10.9133 9.91201 10.6667 9.74718C10.42 9.58236 10.2277 9.34809 10.1142 9.074C10.0007 8.79991 9.97096 8.49831 10.0288 8.20734C10.0867 7.91637 10.2296 7.6491 10.4394 7.43932C10.6491 7.22954 10.9164 7.08668 11.2074 7.0288C11.4984 6.97092 11.8 7.00063 12.074 7.11416C12.3481 7.22769 12.5824 7.41995 12.7472 7.66662C12.912 7.9133 13 8.20331 13 8.49998C13 8.8978 12.842 9.27933 12.5607 9.56064C12.2794 9.84194 11.8978 9.99998 11.5 9.99998Z"/></svg>
        <h1>Lane が選択されていません</h1>
        <p>左のサイドバーから Lane を選んでください</p>
      </main>
    </div>
  </div>
  <!-- VP-142 cleanup (PR-ε-4): legacy `pane-canvas` placeholder を削除済。
       VP-42 era の「汎用 Canvas surface」 placeholder だったが、 PR-ε-3 で PP body
       (`pane-paisley-park` 内 `<div id="pp-content">`) が Smart Canvas surface を物理化
       したため vestigial。 doc 13 §10 Q-3 (= Smart Canvas 配置) も PR-ε-3 で確定済。 -->

  <div class="pane preview" id="pane-preview" data-kind="preview" data-frame-id="preview">
    <div class="pane-header">
      <div class="pane-title">
        <span class="pane-icon">🔍</span>
        <span class="pane-name">Preview</span>
        <span class="pane-breadcrumb" id="preview-breadcrumb">about:blank</span>
      </div>
    </div>
    <div class="pane-body">
      <iframe id="preview-frame" src="about:blank" sandbox="allow-same-origin allow-scripts"></iframe>
    </div>
  </div>
  <!-- Phase 5-A: Project-scope Stand placeholder panes (PP/GE/HP)。
       click action は Phase 3-B で導入した sidebar の vp-project-stand-row から発火、
       将来 (Phase 6+) で Canvas 実描画 / Ruby eval / MIDI 制御を bind する予定。 -->
  <div class="pane stand" id="pane-paisley-park" data-kind="paisley_park" data-frame-id="pp">
    <div class="pane-header">
      <div class="pane-title">
        <span class="pane-icon">🧭</span>
        <span class="pane-name">Paisley Park</span>
        <span class="pane-breadcrumb" id="pp-breadcrumb">Information Router</span>
      </div>
      <div class="pane-actions">
        <button class="pane-action-btn" data-action="clear" data-target="pp" title="Clear PP body content">Clear</button>
      </div>
    </div>
    <div class="pane-body">
      <!-- VP-141: PR-ε-3 で mcp__show 経由 markdown が流れ込む rendering target。
           initial state は placeholder、 window.vpPP.renderPP(content) で innerHTML が差し替わる。 -->
      <div class="pp-content" id="pp-content">
        <p class="pp-placeholder">Information Router — markdown / HTML / 画像 を表示する surface (PR-ε-3 で mcp__show 経路から content が流れ込む)</p>
      </div>
    </div>
  </div>
  <div class="pane stand" id="pane-gold-experience" data-kind="gold_experience" data-frame-id="ge">
    <div class="pane-header">
      <div class="pane-title">
        <span class="pane-icon">🌿</span>
        <span class="pane-name">Gold Experience</span>
        <span class="pane-breadcrumb">Code Runner</span>
      </div>
    </div>
    <div class="pane-body center">
      <main>
        <p>動的生命注入エンジン</p>
        <p class="sub">Phase 6+ で <span class="brand">Ruby eval / process_runner</span> 結合、 inline result preview を実装予定</p>
      </main>
    </div>
  </div>
  <div class="pane stand" id="pane-hermit-purple" data-kind="hermit_purple" data-frame-id="hp">
    <div class="pane-header">
      <div class="pane-title">
        <span class="pane-icon">🍇</span>
        <span class="pane-name">Hermit Purple</span>
        <span class="pane-breadcrumb">External Control</span>
      </div>
    </div>
    <div class="pane-body center">
      <main>
        <p>MIDI / MCP / tmux</p>
        <p class="sub">Phase 6+ で <span class="brand">MIDI lpd8 / MCP server / tmux session</span> 接続パネルを実装予定</p>
      </main>
    </div>
  </div>
  <!-- VP-140: 旧 active class を削除 (Frame Engine の empty Scene が opacity 制御するため)。 -->
  <div class="pane empty" id="pane-empty" data-kind="empty" data-frame-id="empty">
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
    include_str!("../assets/xterm.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-fit.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-webgl.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-unicode11.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-unicode-graphemes.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-image.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-progress.js"),
    r#"
</script>
<script>
"#,
    include_str!("../assets/addon-web-links.js"),
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
// VP-140 inline diagnostic: bundle 失敗時でも script tag 自体は別なので、 こちらが先行 OR 並行 で動く。
// window.vpBundleStatus に bundle 到達 stage を残す (DevTools console から runtime 検査用)。
window.vpBundleStatus = window.vpBundleStatus || { booted: false, importsResolved: false, vpFrameSet: false };
window.vpBundleProbe = function() {
  return {
    bundleStatus: window.vpBundleStatus,
    vpFrameDefined: typeof window.vpFrame !== 'undefined',
    setActivePaneDefined: typeof window.setActivePane === 'function',
    ensureLaneDefined: typeof window.ensureLane === 'function',
    showLaneDefined: typeof window.showLane === 'function',
    laneInstancesSize: window.__vpLanes ? window.__vpLanes.size : 'no __vpLanes',
    paneCount: document.querySelectorAll('[data-frame-id]').length,
    paneTerminalOpacity: getComputedStyle(document.querySelector('#pane-terminal')).opacity,
  };
};
console.info('[vp-inline] vpBundleProbe registered (call window.vpBundleProbe() in console)');
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
    || `'VPMono35', 'VPMono', 'JetBrainsMono Nerd Font', 'Cascadia Code', 'SF Mono', Menlo, Consolas, monospace`;

  // ========= VP-143 Live Token 群 (terminal): default 値 + reader / validator =========
  // CSS variable から読取、 不正値や未設定時は fallback (= 旧 hardcoded 値) に縮退。
  // documentElement style の MutationObserver でも同 logic を再利用するため関数化。
  const TERMINAL_FONT_SIZE_FALLBACK = 16;
  const TERMINAL_LINE_HEIGHT_FALLBACK = 1.15;
  const TERMINAL_LETTER_SPACING_FALLBACK = 0;
  const TERMINAL_CURSOR_STYLE_FALLBACK = 'bar';
  const TERMINAL_CURSOR_STYLES = new Set(['bar', 'block', 'underline']);
  function readTerminalTokens() {
    const cs = getComputedStyle(document.documentElement);
    const fontSize = parseFloat(cs.getPropertyValue('--terminal-font-size'));
    const lineHeight = parseFloat(cs.getPropertyValue('--terminal-line-height'));
    const letterSpacing = parseFloat(cs.getPropertyValue('--terminal-letter-spacing'));
    const fontFamilyRaw = (cs.getPropertyValue('--terminal-font-family') || '').trim();
    const cursorRaw = (cs.getPropertyValue('--terminal-cursor-style') || '').trim().toLowerCase();
    return {
      fontSize: Number.isFinite(fontSize) && fontSize > 0 ? fontSize : TERMINAL_FONT_SIZE_FALLBACK,
      lineHeight: Number.isFinite(lineHeight) && lineHeight > 0 ? lineHeight : TERMINAL_LINE_HEIGHT_FALLBACK,
      letterSpacing: Number.isFinite(letterSpacing) ? letterSpacing : TERMINAL_LETTER_SPACING_FALLBACK,
      fontFamily: fontFamilyRaw || monoFamily,
      cursorStyle: TERMINAL_CURSOR_STYLES.has(cursorRaw) ? cursorRaw : TERMINAL_CURSOR_STYLE_FALLBACK,
    };
  }

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

    // VP-162 baseline (2026-05-10、 user request):
    //   drift / ghost char 調査のため、 Terminal options + 全 addon を **完全 minimum** に reset。
    //   この状態で「きちんと描画されるか」 を確認 → 1 つずつ active 化して drift 再現する要素を特定。
    //   過去 PR #247 / Phase 5-D / mem_1CaVpvsBKR3ckieRXo1nwr の累積 fix が multi-factor で
    //   原因特定不能になったため、 systematic isolation で再 derive する experimental approach。
    const tokens = readTerminalTokens();
    const term = new Terminal({
      fontFamily: tokens.fontFamily,
      fontSize: tokens.fontSize,
      lineHeight: tokens.lineHeight,    // V4+ visual axis (default 1.27、 Live Token で 1.0-1.5 調整可)
      cursorStyle: tokens.cursorStyle,  // V4+ visual axis (default 'bar'、 Live Token で bar/block/underline 切替可)
      scrollSensitivity: 5,             // trackpad で適度 (5 確定 2026-05-11)、 mouse wheel は xterm.js 内部 limitation で 1 行扱い、 page scroll は Shift+PgUp/PgDn で代替
      smoothScrollDuration: 0,          // discrete jump、 PR #247 ghost char 抑制 (= smooth 125ms + 高速 scroll で cell update skip → fragment 残骸)、 V4+ で再証明 (2026-05-11)
      scrollback: 5000,                 // history buffer (drift 無罪確認 2026-05-11、 default 1000 でも drift 再現 → scrollback は origin ではない)
      allowProposedApi: true,           // Unicode11Addon 等の proposed API 利用許可 (V8.2 復帰 2026-05-11、 V6 baseline reset で削除した silent regression を fix)
      theme: theme,
    });

    // === FitAddon = 描画 prerequisite (baseline で確定 2026-05-10) ===
    // xterm.js は term.open(tdiv) 時に container 現 size から cols/rows 計算、
    // .lane-term は flex layout で size 確定が fitAddon.fit() 後 → これ無しだと
    // canvas/DOM が 0 描画 = 「コンソール表示されない」 状態。 baseline 必須。
    // line 624 ほか 6 箇所で fitAddon.fit() を参照、 ResizeObserver / Live Token 反映でも依存。
    const fitAddon = new FitAddon.FitAddon();
    term.loadAddon(fitAddon);

    // === Unicode11Addon + UnicodeGraphemesAddon = Unicode 15 grapheme cluster + width 補正 ===
    //  ⚠️ load 順序が critical: WebglAddon load の **前** に widthProvider を確定する必要がある。
    //  理由: WebGL renderer は loadAddon 時に widthProvider を読んで glyph atlas を事前構築、
    //  後から activeVersion を切替えても atlas は古い width のまま (= upstream design limitation)。
    //  ⚠️ allowProposedApi: true (Terminal options) と pair 必須。 V6 baseline reset で proposed API
    //  gate を閉じた時、 2 addon とも silent fail で drift + 1 cell 幅の同時症状発生 (= V7/V8.1/V8.2
    //  経由で path 探索後、 V8.3 で `allowProposedApi: true` 復帰、 V9 で graphemes も再 enable)。
    //
    //  Unicode11Addon: width table 補正 (Unicode 11、 emoji / CJK 拡張 / box-drawing の cell width)
    //  UnicodeGraphemesAddon: grapheme cluster 認識 (Unicode 15、 ZWJ + skin tone + variation selector)
    //  activeVersion は graphemes が '15-graphemes' で u11 の '11' を上書き、 widthProvider を確定。
    try {
      const u11 = new Unicode11Addon.Unicode11Addon();
      term.loadAddon(u11);
      term.unicode.activeVersion = '11';
    } catch (e) {
      console.warn('[xterm:' + address + '] Unicode11Addon load failed:', e);
    }
    try {
      const ug = new UnicodeGraphemesAddon.UnicodeGraphemesAddon();
      term.loadAddon(ug);
      term.unicode.activeVersion = '15-graphemes';
    } catch (e) {
      console.warn('[xterm:' + address + '] UnicodeGraphemesAddon load failed:', e);
      // Fallback: Unicode 11 (= width table のみ、 grapheme 不対応)
      try { term.unicode.activeVersion = '11'; } catch (_) {}
    }

    // === WebglAddon = baseline 高速 renderer (V4 確定 2026-05-10、 user 方針) ===
    // user 方針「基本 WebGL 描画」 を反映、 V3 (= DOM renderer) で drift 不再現を観察した上で
    // V4 で WebGL active 化。 過去 Phase 5-D (2026-05-02) で「WebGL 復活が正解」 と判断、 frame
    // 毎 canvas 全描画で cell recycling 起因 ghost char が原理的に発生しない property が再評価対象。
    // GPU context loss (Mac で別 app 切替時) は onContextLoss で dispose → DOM fallback。
    let webglAddon = null;
    let webglCleanup = null;
    const VP_USE_WEBGL = true;
    if (VP_USE_WEBGL) {
      try {
        webglAddon = new WebglAddon.WebglAddon();
        term.loadAddon(webglAddon);
        webglAddon.onContextLoss(() => {
          console.warn('[xterm:' + address + '] WebGL context loss — DOM fallback');
          webglAddon.dispose();
        });
        // glyph atlas 破損の自動復旧。 GPU context loss を伴わない silent な atlas
        // corruption (= 文字化けが mr app まで治らない症状) を、 app が foreground
        // に戻った時に clearTextureAtlas で atlas を作り直して wipe する。 corruption
        // の trigger (GPU 切替 / sleep-wake / メモリ圧) は app の background 化と
        // 相関するため、 visible 復帰時の再構築が実効的。 真の context loss は上の
        // onContextLoss で dispose → DOM fallback されるため別経路。
        const onVisible = () => {
          if (document.visibilityState !== 'visible') return;
          try { webglAddon.clearTextureAtlas(); } catch (_) {}
        };
        document.addEventListener('visibilitychange', onVisible);
        webglCleanup = () => document.removeEventListener('visibilitychange', onVisible);
      } catch (e) {
        console.warn('[xterm:' + address + '] WebGL unavailable:', e);
      }
    }

    // === DISABLED for baseline (drift 再現性確認後、 1 つずつ active 化) ===
    //
    // Terminal options (drift 観察後 必要なものを再追加):
    //   letterSpacing: tokens.letterSpacing,     // Live Token (default 0)、 xterm.js default = 0 で同じ
    //   cursorBlink: false,                      // PR #247 frame budget save、 xterm.js default = false で同じ
    //   cursorWidth: 2,                          // bar cursor 太め、 default = 1 (= thin)
    //   fontLigatures: false,                    // mem_1CaVpvsBKR3ckieRXo1nwr cell tracking 抑制、 xterm.js default = false で同じ
    //
    //   (allowProposedApi: true は V8.2 で Terminal options 本体に復帰、 Unicode11Addon と pair 必須)
    //
    // === ProgressAddon = OSC 9;4 ConEmu progress event capture (V4+ enhancer) ===
    //  shell tool / build script (cargo, bun, npm) や Claude CLI が emit する progress 状態
    //  (state: 0=remove/1=normal/2=error/3=indeterminate/4=warning、 value: 0-100) を event 化。
    //  creo-ui の `.creo-progress[data-variant][data-indeterminate]` に state mapping が完全一致 →
    //  CSS 既存資産で即視覚化可。 MVP: console.log で event を確認、 後続 PR で sidebar wire。
    try {
      const progressAddon = new ProgressAddon.ProgressAddon();
      term.loadAddon(progressAddon);
      progressAddon.onChange((p) => {
        console.log('[osc9;4:' + address + '] state=' + p.state + ' value=' + p.value);
      });
    } catch (e) {
      console.warn('[xterm:' + address + '] ProgressAddon load failed:', e);
    }

    // === WebLinksAddon = URL auto-link + cmd+click handler (V4+ enhancer) ===
    //  default handler は window.open ─ wry/tao の WebView では tao の navigation handler が
    //  intercept する想定。 WebView 内遷移なら custom handler で IPC 経由 Mac native open に置換。
    //  MVP: default handler、 dogfood で挙動を観察してから wire 方針を決める。
    try {
      const webLinksAddon = new WebLinksAddon.WebLinksAddon();
      term.loadAddon(webLinksAddon);
    } catch (e) {
      console.warn('[xterm:' + address + '] WebLinksAddon load failed:', e);
    }

    // Addons (final state、 VP-162 V9 baseline 候補 2026-05-11、 dogfood 検証中):
    //   ✅ FitAddon              — baseline 必須 (描画 prerequisite、 load 順序 1st)
    //   ✅ Unicode11Addon        — width table 補正 (load 順序 2nd、 activeVersion '11')
    //   ✅ UnicodeGraphemesAddon — grapheme cluster 認識 (load 順序 3rd、 activeVersion '15-graphemes')
    //   ✅ WebglAddon            — 高速 GPU 描画 (load 順序 4th、 Unicode 15 widthProvider で atlas 構築)
    //   ✅ ProgressAddon         — V4+ enhancer
    //   ✅ WebLinksAddon         — V4+ enhancer
    //   ❌ ImageAddon            — VP-162 で不要判定 (2026-05-11、 = archaeology trace below)
    //
    // === drift 真犯人 (= archaeology trace、 2026-05-11) ===
    //   convertEol: true — drift origin 確定:
    //     V4+++ で 4 件一括復帰した時 drift 再演、 1 件ずつ revert isolation で convertEol が真犯人と判明。
    //     仮説では「単純な \n → \r\n 変換、 cell rendering 不介入」 だったが、 実際は xterm.js の
    //     write buffer + cell index 計算と相互作用で drift 起こす (= 推測、 真因は upstream issue 候補)。
    //     modern shell (zsh / bash) は \r\n standard で disable で問題なし、 legacy shell の
    //     \n only output 対策が必要な時のみ revisit。
    //
    //   allowProposedApi: false (= V6 baseline reset の silent regression) — V7-V8.2 で発見遅延:
    //     V6 で「Terminal options 1 件ずつ active 化」 で削除した allowProposedApi が、 Unicode11Addon
    //     + UnicodeGraphemesAddon (= 両方 proposed API) を silent fail させていた。 V6 → V7 (graphemes
    //     disable) → V8.1 (Unicode11 復帰 + activeVersion '11') → V8.2 (load 順序修正) でも emoji 1 cell
    //     幅と drift が残存、 V8.3 で `allowProposedApi: true` 復帰して両症状を解消。 V7 で UnicodeGraphemes
    //     を「wrap drift 主犯」 と判定したが、 真犯人は proposed API gate の方だった (= V9 で再 enable
    //     して drift 不再現を検証)。 教訓: **proposed addon を使う時は allowProposedApi: true と必ず pair、
    //     baseline reset で削除しない**。
    //
    // === 不要判定 (= archaeology trace、 2026-05-11) ===
    //   ImageAddon — VP-162 で不要判定:
    //     VP architecture は「Canvas (= PP) で視る、 TUI で操る」、 image 表示は PP body markdown
    //     pipeline (doc 13 PR-ε で landed) が主路線、 terminal 内 inline (sixel/iTerm IIP/kitty
    //     graphics) は副次的。 claude も markdown ![...](data:...) で emit が主、 protocol-level
    //     emit は rare。 復帰 cost 低い (= try/catch 1 block) ので必要時のみ revisit。

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
    //  注意 (review F-233-1): `9;hello world` のような pure 数字始まり plain notify の case は
    //  subcode="9" 扱い になる ambiguity がある。 これは dogfood log のみへの影響で、
    //  cc は OSC 9 を emit していない (PR #221 / #233 dogfood で観測ゼロ) ため実害なし。
    //  別 emitter が乗ってきた段階で iTerm2 既知 subcode (1 / 2 / 9 / 50 / 51 等) の whitelist
    //  に絞るか、 観察 log にフラグ立てるかを再検討する。
    //  もう一つの corner case: payload = "9" (semicolon なし、 単一文字) の場合は
    //  semi < 0 経路で `{ subcode: null, rest: "9" }` を返す。 plain msg "9" として扱われ、
    //  実害ゼロ (whitelist 化したら自然解消)。
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

    // ===== window title (OSC 0 / 2) capture =====
    // xterm.js は OSC 0 (icon + title) と OSC 2 (title) を内部で parse して onTitleChange event を fire する。
    // dogfood 仮説: cc が `/rename` 後に session name を window title として emit していれば、
    //  この listener で `osc-handler-debug-logging` 等の renamed value が拾える。
    //  もし fire しなければ session JSONL file watch (~/.claude/projects/<encoded-cwd>/...) の fallback path 検討。
    try {
      term.onTitleChange((title) => {
        console.log('[term-title] lane=' + address + ' title=' + JSON.stringify(title));
        dbg('[term-title:' + address + '] ' + JSON.stringify(title));
      });
    } catch (e) {
      console.warn('[xterm:' + address + '] onTitleChange listener registration failed:', e);
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
        // active (可視) な lane のみ fit + resize 通知。 hidden lane (display:none)
        //  で fit すると container clientWidth=0 → 極小 dims になり、 sendResize で
        //  tmux session を 9×3 に潰す (tmux window-size latest)。 sidebar は全 project
        //  分の lane を ensureLane → WS 接続するので、 ガード無しだと表示してない
        //  project の Stand まで縮む。 headless lane は resize を送らず spawn 時 size
        //  (tmux new-session -x120 -y48) を保つ。 ResizeObserver / showLane と同じ active ガード。
        if (container.classList.contains('active')) {
          try { fitAddon.fit(); } catch (_) {}
          sendResize();
        }
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

    return { term, fitAddon, conn, container, ro, webglAddon, webglCleanup };
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
      if (info.webglCleanup) { try { info.webglCleanup(); } catch (_) {} }
      if (info.webglAddon) { try { info.webglAddon.dispose(); } catch (_) {} }
      info.term.dispose();
      info.container.remove();
    } catch (e) {
      console.error('removeLane error:', e);
    }
    laneInstances.delete(address);
    dbg('[lane:' + address + '] removed');
  };

  // ========= VP-143: terminal Live Token 群の runtime 反映 (creo-ui-editor-host 連携) =========
  // creo-ui-editor-host (Ctrl+Shift+E で activate) が token slider/input 等で document.documentElement
  // の inline style を setProperty('--terminal-{font-size,line-height,letter-spacing,font-family,cursor-style}', ...)
  // で書き換えると、 MutationObserver が style 属性変更を検知して 5 token を全 xterm instance に伝播:
  //   - term.options setter で値反映 (xterm.js は init-time 受取 API だが setter も同等の runtime API)
  //   - fitAddon.fit() で grid 再計算 (font size / line height 変更で cell 寸法が変わる)
  //   - WS resize 通知で PTY 側にも cols/rows 伝達 (= SIGWINCH 相当)
  // → user は editor で値変更すると即時に全 lane terminal が追従。 5 token のうち diff があるものだけ
  // 反映する (= 不要な fitAddon.fit を避ける)。 cursorStyle は grid 寸法に影響しないので fit 不要だが、
  // 残り 4 token のいずれかが変わったら fit 必要 ─ 簡素化のため diff があれば fit する pattern で良い。
  let lastTokens = readTerminalTokens();
  const tokenObserver = new MutationObserver(() => {
    const current = readTerminalTokens();
    const fontSizeChanged = current.fontSize !== lastTokens.fontSize;
    const lineHeightChanged = current.lineHeight !== lastTokens.lineHeight;
    const letterSpacingChanged = current.letterSpacing !== lastTokens.letterSpacing;
    const fontFamilyChanged = current.fontFamily !== lastTokens.fontFamily;
    const cursorStyleChanged = current.cursorStyle !== lastTokens.cursorStyle;
    const anyChanged =
      fontSizeChanged || lineHeightChanged || letterSpacingChanged || fontFamilyChanged || cursorStyleChanged;
    if (!anyChanged) return;
    lastTokens = current;
    // grid 寸法に影響する 4 token のいずれか変更があれば fit 必要、 cursorStyle のみは fit 不要
    const needsFit = fontSizeChanged || lineHeightChanged || letterSpacingChanged || fontFamilyChanged;
    for (const [, info] of laneInstances) {
      try {
        if (fontSizeChanged) info.term.options.fontSize = current.fontSize;
        if (lineHeightChanged) info.term.options.lineHeight = current.lineHeight;
        if (letterSpacingChanged) info.term.options.letterSpacing = current.letterSpacing;
        if (fontFamilyChanged) info.term.options.fontFamily = current.fontFamily;
        if (cursorStyleChanged) info.term.options.cursorStyle = current.cursorStyle;
        if (needsFit) {
          info.fitAddon.fit();
          if (info.conn && info.conn.ws && info.conn.ws.readyState === WebSocket.OPEN) {
            info.conn.ws.send(JSON.stringify({type:'resize', cols: info.term.cols, rows: info.term.rows}));
          }
        }
      } catch (_) { /* noop on individual lane failure */ }
    }
  });
  tokenObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['style'] });

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
  // payload: {kind: "terminal"|"preview"|"paisley_park"|"gold_experience"|"hermit_purple"|null, pane_id, preview_url}
  // Phase 5-A: Project-scope Stand (PP/GE/HP) を click 可能 pane として追加。
  // VP-142 cleanup: legacy "canvas" kind 削除 (pane-canvas placeholder 廃止に伴い)。
  const KIND_TO_PANE = {
    terminal: 'pane-terminal',
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
      // 動的に data-pane-id を設定 (γ-light: native overlay が pane_id で照合する想定)。
      // 注: Frame Engine の static `data-frame-id` (= "echoes" / "pp" 等の Scene lookup key) とは
      // 別 attribute。 同名にすると Lane click でこの動的書き換えが Frame Engine の attribute を
      // hijack して Scene lookup undefined → HIDDEN_TRANSFORM で pane が見えなくなる (VP-141 fix)。
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

  // VP-140: lane catch-up 要求 — 起動 race で WebView HTML load 完了前に Rust 側 ensureLane が
  // silent drop された場合の救済。 ここは inline IIFE 内 (DOMContentLoaded 直後と等価のタイミング)
  // で実行されるので、 JS 側 window.ensureLane が既に定義済 = Rust 側 evaluate_script が成功する。
  // Rust 側は AppEvent::LanesEnsureAll を受けて全 project の lanes_by_project を walk + ensureLane 再発行。
  // idempotent (laneInstances.has なら no-op) なので、 既に ensured 済の lane は影響なし。
  window.ipc.postMessage(JSON.stringify({t:'lanes:ensure-all'}));

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
  // VP-142 (PR-ε-3): flush は `window.setActivePane(pendingPane)` 経由で行う (= bridge を通す)。
  // setActiveImpl 直叩きだと entry.tsx で wrap した setActivePane bridge を bypass し、
  // setWantedLane (show-subscriber) や applyScene (Frame Engine) が fire しないため、 auto-select Lane の
  // show-subscriber が永続的に未接続のままになる回帰を起こす。 domReady=true になっているので window.setActivePane
  // 内の buffering 分岐は再 hit しない (= 無限再帰なし)。
  window.addEventListener('DOMContentLoaded', () => {
    domReady = true;
    if (pendingPane !== null) {
      const flush = pendingPane;
      pendingPane = null;
      window.setActivePane(flush);
    }
  });
})();
</script>
</body>
</html>"#
);
