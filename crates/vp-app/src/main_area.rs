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
    /// Pane kind ("terminal" | "preview" | "paisley_park" | "gold_experience" | "bastet" | "empty" | null)
    /// null = 何も active でない (空状態を表示)。
    /// VP-142 cleanup (PR-ε-4): legacy "canvas" kind 削除 (PR-ε-3 で PP body が Smart Canvas surface 物理化)
    pub kind: Option<&'a str>,
    pub pane_id: Option<&'a str>,
    /// Preview kind の URL (preview kind 以外では None)
    pub preview_url: Option<&'a str>,
    /// この Lane が Act II (root act="chat"、sessions 由来 — doc 53 R1) か。terminal kind でのみ意味を持つ。
    ///
    /// chat lane は engine-less (pid=None) が正常形で **xterm instance を持たない**ため、
    /// JS の `showLane` が「xterm が無い = 表示すべき内容が無い」と誤判定して
    /// `#lane-empty` placeholder を ChatView の上に被せてしまう。 本 flag で
    /// 「xterm は無いが ChatView が内容を持つ」を伝え、 placeholder を抑止する。
    pub chat: bool,
    /// Echoes 共通ヘッダ (操縦席) の cwd chip 用: この lane の cwd (絶対 path)。
    /// header は `~` 短縮 + 中略で表示し、click で full path を clipboard copy する。
    /// terminal kind でのみ意味を持つ。None = lane 不明 / 非 lane pane (chip 非表示)。
    ///
    /// cwd は address (pane_id) から導出できない唯一の lane 情報なので、setActivePane に
    /// 相乗りさせて運ぶ (新しい配信チャネルは増やさない — 既存 lane 状態配信経路)。
    pub cwd: Option<&'a str>,
    /// Echoes 共通ヘッダの branch chip 用: performer lane の git branch
    /// (`performer_status.branch` 由来、「安価に取れる場合のみ」)。
    /// conductor / 取得不能時は None (chip 非表示)。
    pub branch: Option<&'a str>,
    /// Echoes 共通ヘッダの lane 名 chip 用: `LaneInfo.name`（表示名）。
    /// 現状 server 側は常に None のため JS 側は addr 由来の短縮名に fallback するが、
    /// 将来 name が populate された時にヘッダだけ古い表示に取り残されないよう
    /// cwd / branch と同じ経路で供給しておく（JS 側 entry.tsx は受け取り済み）。
    pub lane_name: Option<&'a str>,
    /// Echoes 共通ヘッダの session chip 用: active engine の session id
    /// （`LaneInfo.engine_session_id` 由来）。Act I は EchoesEvent が流れないため
    /// event 経路では供給されず、この setActivePane 相乗りが唯一の供給路になる
    /// （Act II では event 由来の真値が上書きする — EchoesHeader 側で OR merge）。
    pub session_id: Option<&'a str>,
    /// Echoes 共通ヘッダの chip prefix 用: **root session の stand**（= slot に載る engine 種別、
    /// "echoes" / "codex" / "grok" 等）。session chip の engine 別 prefix 導出に使う。doc 39 P4-C:
    /// 供給値は `LaneInfo.engine_stand`（root の engine）優先で、無ければ lane 固定 `stand` に
    /// fallback（push_active_view が解決済み — cross-engine root で chip prefix が正しく点く）。
    pub stand: Option<&'a str>,
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

/// 統合 HTML から外部 script 化した SolidJS bundle (doc 48 Phase 1)。
/// `MAIN_VIEW_ASSETS` (app.rs) が `vp-asset://app/*.bundle.js` として baked 配信し、
/// `VP_WEBVIEW_DEV=<assets dir>` 設定時は `web_assets::serve` の disk-read が優先される
/// (= cargo build なしの bundle 差替え = HMR)。vendor 静的 JS/CSS は inline のまま
/// (dev loop で変わるのは bundle だけ)。
pub const EDITOR_HOST_BUNDLE_JS: &str = include_str!("../assets/editor-host.bundle.js");
pub const SIDEBAR_BUNDLE_JS: &str = include_str!("../assets/sidebar.bundle.js");

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
    include_str!("../assets/vp-tokens.css"),
    r#"
</style>
<style>
"#,
    include_str!("../assets/creo-components.css"),
    r#"
</style>
<style>
"#,
    // xterm.css は生成物 — build.mjs が node_modules/@xterm/xterm から複写する
    // (*.bundle.js と同じ扱い、build.rs が存在を guard)。bundle 側から head へ注入せず
    // ここに焼くのは、直後に続く app 側の上書き規則 (.xterm-viewport::-webkit-scrollbar 等) が
    // **同じ <style> の後ろ**に来る cascade 順を保つため。
    include_str!("../assets/xterm.css"),
    r#"
html,body{margin:0;padding:0;height:100%;width:100%;background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:var(--vp-font-sans),var(--typography-family-sans);font-weight:300;}
body{overflow:hidden;}
/* WebView 統合 (step 3a): sidebar + main を 1 DOM に CSS flex で同居。
   app-shell が [sidebar-root | host] を横並び、editor-root は別 (floating overlay)。 */
#app-shell{display:flex;width:100%;height:100%;}
#sidebar-root{width:280px;flex:none;height:100%;overflow:hidden;}
#host{position:relative;flex:1;height:100%;min-width:0;}
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
  /* Pane の名札（上段）token。「上 = この pane が何であるか（居る間 変わらない素性）」を
     載せる帯の見た目を、実装 2 本（静的 .pane-header と SolidJS の #echoes-header）で共有する。
     doc 29/30 の Edge Ring（上 = global / 下 = local）を pane スケールへ再適用した縦軸に基づく。
     以前は 28px（.pane-header）と 30px（#echoes-header）で高さが割れており、隣り合うと段差が
     見えていた（2026-07-23 の実機比較）。値の SSOT はここ 1 箇所。 */
  --vp-nameplate-h:28px;
  --vp-nameplate-pad-x:10px;
  --vp-nameplate-font-size:12px;
  --vp-nameplate-bg:var(--color-surface-surface);
  --vp-nameplate-border:1px solid var(--color-surface-border-subtle);
  /* VP-143: Echoes terminal (xterm.js) の Live Token 群。 creo-ui-editor-host (Ctrl+Shift+E)
     で runtime 調整可能。 JS 側 createLaneInstance が値を読んで `new Terminal({...})` を構築、
     MutationObserver が documentElement style 変更を捕捉して全 terminal に setter +
     fitAddon.fit() + WS resize 通知で伝播 → 既存 lane terminal も即時反映。
     default は旧 hardcoded 値と同じなので既存挙動への regression なし。 */
  --terminal-font-size:16;
  --terminal-line-height:1.27;
  --terminal-letter-spacing:0;
  /* font zero-start (2026-07-11): Echoes terminal は principal mono ('UDEV Gothic NF'、
     Nerd Font glyph 込み) のみ。 bundle はせず local (OS install 済) font を名前参照、
     未 install 環境は末尾 monospace に縮退するのでどの OS でも描画は壊れない。 */
  --terminal-font-family:'UDEV Gothic NF', monospace;
  --terminal-cursor-style:bar; /* "bar" / "block" / "underline" */
}
.pane{
  position:absolute;
  left:0;top:0;width:100%;height:100%;
  opacity:0;
  pointer-events:none;
  /* wheel 吸い込み根治（2026-07-24）: opacity:0 でも iframe が compositor の scroll
     hit-test に残るため、投影前の boot 窓も含めて hit-test から外す（app-panes.ts の
     投影が visible 時に inline visibility:visible で上書きする）。 */
  visibility:hidden;
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
/* Echoes 共通ヘッダ (操縦席、mem `vp-pane-common-header`): Act I(xterm)/Act II(chat) を跨いで
   載り続ける lane-local な情報 + 操作の strip。DOM の器だけを World A が用意し、中身は
   editor-host bundle の EchoesHeader component が #echoes-header に mount する
   (chat session host と同じ mount 点パターン)。
   高さ 0 が default = header 不在時は xterm/chat が全高 (既存挙動、regression なし)。
   header が内容を持つ時だけ World B が #pane-terminal に .echoes-header-active を付け、
   strip を開いて session host 群 (#term-session-<n> / .chat-session-host) と lane-empty を
   その分だけ押し下げる
   (= xterm 表示領域を header 分だけ譲る。押し下げ後の container 縮小を ResizeObserver が
   捕捉して fitAddon.fit() が再計算する — 「xterm を圧迫しない」検証点)。 */
#pane-terminal{--echoes-header-h:0px;}
#pane-terminal.echoes-header-active{--echoes-header-h:var(--vp-nameplate-h);}
#echoes-header{position:absolute;top:0;left:0;right:0;height:var(--echoes-header-h);
  overflow:hidden;z-index:2;}
/* doc 49 LE-P4 PR2: lane 内 tiling は creo-ui-layout の lane scope が担い、JS
   (lane-panes.ts) が resolved rect を inline style (left/top/width/height %) で書く。
   子 Pane (.term-session-host / .chat-session-host) は中身を変えず位置づけだけ absolute。
   inset:0 は JS が走る前の既定 — inline の width/height が入れば over-constraint
   解決 (LTR) で right/bottom が無視され、inline の rect が勝つ。 */
/* 下端の帯（#pane-tabs）は退役（doc 51 §1 A1）— pane chip は tiling 既定で存在理由が
   消え、+ New / Act 切替は EchoesHeader（lane の名札）へ移設した。 */
#lane-panes{position:absolute;top:var(--echoes-header-h);left:0;right:0;
  bottom:0;background:var(--color-border,#2a3040);}
/* outline は隣接 Pane との区切り線 (旧 flex gap:1px の後継 — layout に影響しない描画のみの線)。 */
#lane-panes > *{position:absolute;inset:0;background:var(--color-bg,#0f1115);
  outline:1px solid var(--color-border,#2a3040);outline-offset:-1px;}
/* 要件 3: フォーカスが**視認できる**。内側 ring なので幅を食わず、区切り線とも干渉しない。 */
#lane-panes > .pane-focused{box-shadow:inset 0 0 0 1px var(--sb-conn-auto,#22E0FF);}
/* Phase 2.5: per-Lane instance container。各 .lane-pane が absolute で重なり active のみ表示。 */
.lane-pane{position:absolute;inset:0;display:none;}
.lane-pane.active{display:block;}
/* doc 50 §4.6 A6 ②: term pane にも名札が載る。名札の DOM と中身は World B（SolidJS の
   SessionPlate）が host に差し込むが、**xterm を下げる責務は World A 側**に置く —
   `.lane-pane` は World A の持ち物なので、その位置決めも World A が持つ（World B は
   host に `.has-term-plate` を付けるだけ = DOM 所有の境界を跨がない、doc 33 §8）。
   名札は絶対配置で上端に載せ、xterm はその分だけ top を下げる。 */
.has-term-plate > .lane-pane{top:var(--vp-nameplate-h);}
.term-plate{position:absolute;top:0;left:0;right:0;height:var(--vp-nameplate-h);
  z-index:1;overflow:hidden;}
/* doc 33 §9: 切替 progress overlay。pane 全面 (header 下) を覆い、resume 確定まで表示 (= switch lock)。
   header は switch 中も lane identity を見せ続けたいので overlay の上に残す (top を header 分下げる)。 */
#console-switching{position:absolute;top:var(--echoes-header-h);left:0;right:0;bottom:0;display:none;z-index:20;
  align-items:center;justify-content:center;
  background:color-mix(in srgb, var(--color-bg,#0f1115) 82%, transparent);backdrop-filter:blur(2px);}
#console-switching.active{display:flex;}
.console-switching-card{display:flex;flex-direction:column;align-items:center;gap:14px;
  color:var(--color-text-secondary,#a8b0c0);font-size:13px;
  font-family:var(--vp-font-sans),var(--typography-family-sans);}
.console-switching-spinner{width:26px;height:26px;border-radius:50%;
  border:2.5px solid var(--color-border,#2a3040);border-top-color:var(--color-accent,#3b82f6);
  animation:console-spin .7s linear infinite;}
@keyframes console-spin{to{transform:rotate(360deg);}}
@media (prefers-reduced-motion:reduce){.console-switching-spinner{animation-duration:1.6s;}}
.lane-pane .lane-term{padding:0;height:100%;width:100%;box-sizing:border-box;}
/* どの Lane も無い時の placeholder (active class で表示制御、 default は表示)。
   header 分 (var) 押し下げ — header 不在時は 0 なので従来通り全面。 */
#lane-empty{position:absolute;top:var(--echoes-header-h);left:0;right:0;bottom:0;display:none;place-items:center;color:var(--color-text-tertiary);text-align:center;}
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
  top:0;left:0;right:0;height:var(--vp-nameplate-h);
  display:flex;
  align-items:center;
  gap:8px;
  padding:0 var(--vp-nameplate-pad-x);
  font-size:var(--vp-nameplate-font-size);
  background:var(--vp-nameplate-bg);
  border-bottom:var(--vp-nameplate-border);
  user-select:none;
  -webkit-app-region:drag;
  z-index:1;
  /* 名札は 1 行きり。溢れは折り返さず省略する（PP の "Paisley Park" が 2 行に割れて
     隣の pane と高さが揃わなくなっていた実機バグ、2026-07-23）。 */
  white-space:nowrap;
  overflow:hidden;
}
.pane-header .pane-title{
  flex:1;
  display:flex;
  align-items:center;
  gap:6px;
  color:var(--color-text-primary);
  min-width:0;
  overflow:hidden;
}
/* glyph は Phosphor (iconify-icon)。sidebar は既に CreoIcon で統一済で、額縁だけ絵文字が
   残っていたのを揃えた (2026-07-23)。iconify-icon の既定サイズは 1em なので、寸法は
   font-size で決まる = 周囲の文字と自然に揃う。 */
iconify-icon{display:inline-flex;align-items:center;flex-shrink:0;vertical-align:-0.125em;}
.pane-header .pane-icon{flex-shrink:0;font-size:14px;display:inline-flex;align-items:center;}
/* 名前は最優先で残し、breadcrumb（従属情報）から先に削る。 */
.pane-header .pane-name{font-weight:500;flex-shrink:0;}
.pane-header .pane-breadcrumb{color:var(--color-text-tertiary);font-size:11px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.pane-header .pane-actions{
  display:flex;
  gap:4px;
  -webkit-app-region:no-drag;
  /* doc 50 §2: ツール（Clear / ×）は名札に常設しない — 縦軸に乗らない操作は hover で
     召喚する（mako 決定 2026-07-23 の暫定形。恒久の home は右 edge 構想 = 棚上げ中）。
     focus-within はキーボード到達性の保険（Tab で入れば見える）。 */
  opacity:0;
  transition:opacity .12s ease;
}
.pane-header:hover .pane-actions,
.pane-header:focus-within .pane-actions{opacity:1;}
.pane-header .pane-action-btn{
  cursor:pointer;
  padding:2px 8px;
  font-size:11px;
  background:transparent;
  border:1px solid var(--color-surface-border-subtle);
  border-radius:4px;
  color:var(--color-text-secondary);
  font-family:inherit;
}
.pane-header .pane-action-btn:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);}
.pane-body{
  position:absolute;
  top:var(--vp-nameplate-h);left:0;right:0;bottom:0;
  overflow:auto;
}
.pane-body.center{display:grid;place-items:center;}
.pane-body iframe{width:100%;height:100%;border:0;background:#fff;}
/* board (PP) pane — doc 52 §10 wave 0: lane tiling の 1 枚。#lane-panes > * の absolute inset を
   受けた上で、中身を plate / content / history-strip の縦並びにする。solid surface で載せて
   背後の xterm が透けないようにする（旧 pp-overlay の可読性対策の後継）。 */
#lane-board{display:flex;flex-direction:column;background:var(--color-surface-bg-base);
  font-family:var(--vp-font-sans),var(--typography-family-sans);font-weight:300;}
/* board の名札 — 台の中で「これは Paisley Park の board」と読める最小 chrome + Clear。 */
.board-plate{flex:0 0 auto;display:flex;align-items:center;justify-content:space-between;
  gap:8px;padding:4px 10px;border-bottom:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-subtle);}
.board-plate-name{display:inline-flex;align-items:center;gap:6px;font-size:12px;
  color:var(--color-text-secondary);letter-spacing:.02em;}
/* 鮮度（doc 52 §5 計器盤）: cursor item の最終更新時刻。name の右に寄せ、控えめに。 */
.board-freshness{margin-left:auto;margin-right:10px;font-size:11px;
  color:var(--color-text-tertiary,#8a8fa3);font-variant-numeric:tabular-nums;
  font-family:var(--typography-family-mono);white-space:nowrap;}
.board-clear-btn{border:1px solid var(--color-surface-border,#1f2233);background:transparent;
  color:var(--color-text-tertiary);font-size:11px;padding:1px 8px;border-radius:4px;cursor:pointer;
  font-family:inherit;transition:color .1s ease,border-color .1s ease,background .1s ease;}
.board-clear-btn:hover{color:var(--color-text-primary);background:var(--color-surface-bg-emphasis);}
/* Bastet 🧲 pane: device 一覧の行。名前と IN/OUT バッジが素の連結で「Roto-ControlIN · OUT」に
   見えていた（2026-07-23 実機）— gap + バッジの弱色化で読めるように。 */
.bastet-devices{display:flex;flex-direction:column;gap:2px;padding:10px 16px;}
.bastet-device{display:flex;align-items:baseline;gap:10px;}
.bastet-device-io{color:var(--color-text-tertiary,#8a8fa3);font-size:.78em;letter-spacing:.06em;}
.bastet-empty{color:var(--color-text-tertiary,#8a8fa3);padding:10px 16px;margin:0;}
/* PP markdown render 領域 (PR-ε-3 で mcp__show 経由 markdown が流れ込む rendering target)。
   font zero-start (2026-07-11): 旧 Mizolet/みぞれ 直指定を principal token に置換 (2 書体統一)。 */
/* ink stage（doc 52 §3）: #pp-content を充填 + overlay / palette の位置決め基準。 */
#ink-stage{position:relative;flex:1 1 auto;min-height:0;display:flex;flex-direction:column;}
.pp-content{flex:1 1 auto;min-height:0;overflow:auto;
  padding:16px 20px;color:var(--color-text-primary);font-size:13px;line-height:1.6;
  font-family:var(--vp-font-sans),var(--typography-family-sans);font-weight:300;}
/* ink 赤: shottr と同じ 1 色固定（意図的に単色 — 描くのは「場所と関係を指す指」）。 */
#ink-stage{--vp-ink-color:#ff5b4d;}
/* 透明レイヤー: 道具未選択（.ink-off）時は pointer-events:none で下の item を素通し
   （text 選択 / Clear ボタンが生きる）。道具選択で auto に切り替え描画を捕まえる。
   overlay は div（HTML box 全体を捕まえる）。空白部分を透過させないため svg には委ねない。 */
#ink-overlay{position:absolute;inset:0;pointer-events:none;touch-action:none;z-index:4;
  user-select:none;-webkit-user-select:none;}
#ink-overlay:not(.ink-off){pointer-events:auto;cursor:crosshair;}
/* 描画キャンバス: div いっぱいの svg。pointer は div が捕まえるので自身は none。 */
#ink-canvas{position:absolute;inset:0;width:100%;height:100%;pointer-events:none;
  overflow:visible;}
#ink-canvas .ink-note{font:600 14px var(--vp-font-sans),var(--typography-family-sans);
  fill:var(--vp-ink-color);paint-order:stroke;stroke:rgba(0,0,0,.55);stroke-width:3px;
  stroke-linejoin:round;}
/* text 注釈の入力ボックス（配置は ink.ts が left/top を絶対指定）。 */
#ink-text{position:absolute;display:none;transform:translate(-2px,-50%);z-index:6;
  background:rgba(12,14,22,.92);color:var(--vp-ink-color);border:1px dashed var(--vp-ink-color);
  border-radius:4px;font:600 14px var(--vp-font-sans),var(--typography-family-sans);
  padding:2px 6px;outline:none;min-width:120px;}
/* 道具パレット: stage 下端中央に浮かす。撮影時は ink.ts が visibility:hidden にする。 */
#ink-palette{position:absolute;left:50%;bottom:12px;transform:translateX(-50%);z-index:8;
  display:flex;align-items:center;gap:3px;padding:5px;
  background:var(--color-surface-bg-subtle);border:1px solid var(--color-surface-border,#1f2233);
  border-radius:10px;box-shadow:0 6px 22px rgba(0,0,0,.4);}
#ink-palette button{background:none;border:1px solid transparent;border-radius:7px;
  width:36px;height:32px;color:var(--color-text-tertiary);cursor:pointer;
  display:grid;place-items:center;padding:0;font-family:inherit;transition:color .1s,border-color .1s,background .1s;}
#ink-palette button:hover{color:var(--color-text-primary);background:var(--color-surface-bg-emphasis);}
#ink-palette button.ink-active{color:var(--vp-ink-color);border-color:var(--vp-ink-color);
  background:color-mix(in srgb,var(--vp-ink-color) 12%,transparent);}
#ink-palette button:disabled{opacity:.35;cursor:default;}
#ink-palette .ink-sep{width:1px;height:20px;background:var(--color-surface-border,#1f2233);margin:0 3px;}
#ink-palette svg{display:block;pointer-events:none;}
#ink-send{width:auto!important;padding:0 14px!important;font-weight:600;font-size:13px;
  color:var(--color-surface-bg-base)!important;background:var(--vp-ink-color)!important;
  border-color:var(--vp-ink-color)!important;}
#ink-send:hover:not(:disabled){filter:brightness(1.08);}
#ink-send:disabled{opacity:.4;background:var(--vp-ink-color)!important;}
/* 送信結果の一時トースト（成功/失敗）。ink.ts が textContent + .show を付ける。 */
#ink-toast{position:absolute;left:50%;bottom:54px;transform:translateX(-50%);z-index:9;
  max-width:80%;padding:5px 12px;border-radius:6px;font-size:12px;pointer-events:none;
  background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);
  border:1px solid var(--color-surface-border,#1f2233);opacity:0;transition:opacity .15s;}
#ink-toast.show{opacity:1;}
#ink-toast.ink-error{color:var(--vp-ink-color);border-color:var(--vp-ink-color);}
.pp-content h1{font-size:1.6rem;font-weight:500;margin:0 0 .5rem;color:var(--color-text-primary);}
.pp-content h2{font-size:1.3rem;font-weight:500;margin:1.2rem 0 .5rem;}
.pp-content h3{font-size:1.1rem;font-weight:500;margin:1rem 0 .4rem;}
.pp-content p{margin:.5rem 0;color:var(--color-text-secondary);}
.pp-content code{background:var(--color-surface-surface);padding:1px 5px;border-radius:3px;font-family:var(--typography-family-mono);font-size:.9em;}
.pp-content pre{background:var(--color-surface-surface);padding:12px;border-radius:6px;overflow-x:auto;}
.pp-content pre code{background:transparent;padding:0;}
.pp-content a{color:var(--color-brand-primary);}
.pp-content ul,.pp-content ol{padding-left:1.5em;margin:.5rem 0;}
.pp-content blockquote{border-left:3px solid var(--color-brand-primary-subtle);margin:.5rem 0;padding:0 1em;color:var(--color-text-tertiary);}
.pp-content table{border-collapse:collapse;margin:.5rem 0;}
.pp-content th,.pp-content td{border:1px solid var(--color-surface-border-subtle);padding:4px 8px;}
.pp-content hr{border:0;border-top:1px solid var(--color-surface-border-subtle);margin:1rem 0;}
.pp-placeholder{color:var(--color-text-tertiary);font-style:italic;}
/* content_type=html: sandbox iframe を PP pane いっぱいに広げる。
   renderPP が container に .pp-content-html を付与し full-bleed に切り替える。 */
.pp-content.pp-content-html{padding:0;height:100%;}
.pp-html-frame{width:100%;height:100%;border:0;display:block;background:#fff;}
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
   関連: mem_1CaSmvKgsX2AQxRYFYgNM3 (Conductor pane shell), creo-ui contrast-dark theme. */
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
<div id="app-shell">
<!-- WebView 統合 (step 3a): sidebar bundle (SolidJS) の mount 先。bundle は外部 script
     (sidebar.bundle.js、doc 48 Phase 1 で inline → 外部化) が mount する。 -->
<div id="sidebar-root"></div>
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
  <!-- Phase 2.5 (per-Lane instance) → doc 50 §4.6 A6: pane-terminal 内の #lane-panes に
       (lane, session) ごとの xterm.js instance を mount。 active な instance だけ display:block。 -->
  <!-- VP-140 fail-safe: pane-terminal は Frame Engine が apply される前から visible にしておく。
       inline opacity:1 を CSS .pane{opacity:0} default より優先させ、 Frame Engine 不在 / 起動失敗時も
       少なくとも Echoes terminal は見える状態を保つ (= echoes が default visible 約束)。
       Frame Engine 起動後は inline style.opacity を engine が上書きする (conductor-focus:1 / pp-focus:0)。 -->
  <div class="pane terminal" id="pane-terminal" data-kind="terminal" data-frame-id="echoes" style="opacity:1;pointer-events:auto;visibility:visible;">
    <!-- Echoes 共通ヘッダ (操縦席) の mount 点。器だけ World A が置き、editor-host bundle の
         EchoesHeader が中身を render する。lane 切替で内容だけ差し替わる (帰属は lane の Echoes、
         Act I/II を跨いで同一 header が載り続ける)。default 高さ 0、内容がある時だけ開く。 -->
    <div id="echoes-header"></div>
    <!-- doc 46 P1 → doc 49 LE-P4 PR2 → doc 50 P1: lane の表示領域 = N 枚の Pane を並べる
         tiling の器。配置は creo-ui-layout の lane scope (lane-panes.ts) が resolved rect を
         inline で書く。子は **session ごとの host 群**を動的に生やす:
         `#term-session-<n>`（Act I xterm、World A 所有・中身に触れない）と
         `#chat-session-<n>`（World B の lane-panes が生やす）。どちらも 1 session = 1 Pane。
         旧 #console-chat-host 固定 1 枚 / 静的 #lane-host（root 専用の term host）は退役 —
         host の身元を role でなく session に紐づけた（doc 50 §4.6 A6、`ensureTermHost`）。
         下端の帯 (#pane-tabs) は doc 51 §1 A1 で退役 — 表示は既定 tiling、
         + New / Act 切替は EchoesHeader (lane の名札) へ移設。 -->
    <div id="lane-panes">
      <!-- board (PP) pane — doc 52 §10 wave 0: app 層の #pane-paisley-park から lane tiling へ
           引っ越した「貼る台」。**lane に 1 枚の静的 host**（board は lane-scoped、
           表示 lane は常に 1 つ = xterm と同じ性質）。roster に載るのは board 非空のときだけで、
           位置決めは lane-panes.ts が担う。中身の #pp-content / #pp-history-strip は移設のみで
           id 不変 = pp.ts / HistoryStrip / board-handler の render 先は変わらない。 -->
      <div id="lane-board">
        <div class="board-plate">
          <span class="board-plate-name"><iconify-icon icon="ph:compass"></iconify-icon> Paisley Park</span>
          <!-- 鮮度: cursor item の updatedAt を board-handler が「更新 HH:MM:SS」で書く（doc 52 §5 計器盤）。
               出力元は SP の updatedAt 一箇所（content 手書きに依存しない）。 -->
          <span id="board-freshness" class="board-freshness"></span>
          <button class="board-clear-btn" data-action="clear" data-target="pp" title="board を空にする">Clear</button>
        </div>
        <!-- ink（対話面、doc 52 §3）: #pp-content の上に透明レイヤーを重ねて描く。renderPP は
             #pp-content の innerHTML を差し替えるので overlay は sibling（stage 直下）に置く。
             stage = 描画対象 = snapshot 矩形。overlay / palette / text input の挙動は ink.ts。 -->
        <div id="ink-stage">
          <div class="pp-content" id="pp-content"></div>
          <!-- overlay は **div**（HTML box 全体で pointer を捕まえる）。中の svg で描く。
               svg root を直に armed にすると、SVG の pointer-events 既定 visiblePainted の
               ため空白部分が透過し pointerdown が下の文字に落ちて text 選択に吸われる。 -->
          <div id="ink-overlay" class="ink-off" aria-label="対話面 描画レイヤー">
            <svg id="ink-canvas">
              <defs>
                <marker id="ink-arrowhead" markerWidth="9" markerHeight="8" refX="7" refY="4" orient="auto">
                  <path d="M0,0 L8,4 L0,8 z" fill="var(--vp-ink-color)"></path>
                </marker>
              </defs>
            </svg>
          </div>
          <input id="ink-text" type="text" placeholder="文字を入力 → Enter" aria-label="ink text 注釈の入力" />
          <div id="ink-palette" role="toolbar" aria-label="対話面 描画道具"></div>
          <div id="ink-toast" role="status"></div>
        </div>
        <div class="pp-history-strip" id="pp-history-strip"></div>
      </div>
    </div>
    <!-- doc 33 §9: Act I⇄II 切替中の progress overlay (World B)。toggle 押下で .active、
         resume 確定 (session_init) / mode 適用で clear。切替を resume 確定まで見せる + lock。 -->
    <div id="console-switching">
      <div class="console-switching-card">
        <div class="console-switching-spinner"></div>
        <div class="console-switching-msg">セッションを引き継ぎ中…</div>
      </div>
    </div>
    <!-- empty placeholder: どの Lane も無い時に出す -->
    <div id="lane-empty" class="lane-empty active">
      <main>
        <!-- Lane を象徴する Codicon "git-merge" (user 指定、vscode-codicons より)。
             Lane (特に Performer) は git branch ベースの隔離環境なので branch graph
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
        <span class="pane-icon"><iconify-icon icon="ph:magnifying-glass"></iconify-icon></span>
        <span class="pane-name">Preview</span>
        <span class="pane-breadcrumb" id="preview-breadcrumb">about:blank</span>
      </div>
    </div>
    <div class="pane-body">
      <iframe id="preview-frame" src="about:blank" sandbox="allow-same-origin allow-scripts"></iframe>
    </div>
  </div>
  <!-- doc 52 §10 wave 0: Paisley Park は app 層の pane を退役し、lane tiling の board pane
       （#lane-board、上方 #lane-panes 内）へ引っ越した。GE / Bastet / Preview は app pane のまま。 -->
  <div class="pane stand" id="pane-gold-experience" data-kind="gold_experience" data-frame-id="ge">
    <div class="pane-header">
      <div class="pane-title">
        <span class="pane-icon"><iconify-icon icon="ph:plant"></iconify-icon></span>
        <span class="pane-name">Gold Experience</span>
        <span class="pane-breadcrumb">Code Runner</span>
      </div>
      <div class="pane-actions">
        <button class="pane-action-btn" data-action="close-pane" title="閉じる — 元の配置に戻る"><iconify-icon icon="ph:x"></iconify-icon></button>
      </div>
    </div>
    <div class="pane-body center">
      <main>
        <p>動的生命注入エンジン</p>
        <p class="sub">Phase 6+ で <span class="brand">Ruby eval / process_runner</span> 結合、 inline result preview を実装予定</p>
      </main>
    </div>
  </div>
  <div class="pane stand" id="pane-bastet" data-kind="bastet" data-frame-id="bs">
    <div class="pane-header">
      <div class="pane-title">
        <span class="pane-icon"><iconify-icon icon="ph:magnet"></iconify-icon></span>
        <span class="pane-name">Bastet</span>
        <span class="pane-breadcrumb">Device Registry</span>
      </div>
      <div class="pane-actions">
        <button class="pane-action-btn" data-action="close-pane" title="閉じる — 元の配置に戻る"><iconify-icon icon="ph:x"></iconify-icon></button>
      </div>
    </div>
    <div class="pane-body">
      <div class="bastet-devices" id="bastet-devices">
        <p class="bastet-empty">No devices connected</p>
      </div>
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
</div><!-- /#app-shell (WebView 統合 step 3a) -->
<!-- VP-101 Phase A2: creo-ui-editor-host (SolidJS) の mount 先。
     Ctrl+Shift+E で activate される floating overlay (font / theme / token を runtime 編集)。 -->
<div id="editor-root"></div>
<!-- VP-101 Phase A2: creo-ui-editor-host bundle (SolidJS + EditorLayer + tokens auto-discover).
     Ctrl+Shift+E で activate、font / theme / spacing 等を runtime 編集。
     Build: cd crates/vp-app/webview && bun install && bun run build。
     doc 48 Phase 1: inline をやめ外部 script 化。page は custom protocol で load される
     (origin = vp-asset://app) ため相対 src が vp-asset://app/*.bundle.js に解決される。
     旧「with_html は null origin で inline 一択」の制約は with_url 化で失効済。
     classic script (defer/async なし) は文書順 blocking 実行なので inline 時と実行順は不変。 -->
<script src="editor-host.bundle.js"></script>
<!-- WebView 統合 (step 3a): sidebar bundle (SolidJS)。#sidebar-root に mount。 -->
<script src="sidebar.bundle.js"></script>
<!-- 旧 World A（inline xterm JS 976 行）はここにあった。doc 53 §6.5 の畳み込みで
     webview bundle へ移設（`webview/term.ts` = xterm 配線 / `webview/active-pane.ts` =
     pane 切替 + slot rect）。install は `entry.tsx` の module body 末尾で、実行順は
     inline 時と同じ（bundle は classic blocking script なので文書順で走る）。
     Rust → JS の制御面は単一受け口 `window.vpDispatch` の envelope（SSOT = schema/vp-push.kdl、
     型は codegen が両側に出す）。受け側は webview/dispatch.ts。

     xterm.js + addon は npm 依存として term.ts が直 import する（#920 で vendored asset 8 本
     ≈938KB の include_str! を撤去、window global 経由の橋渡しも本 PR で不要になった）。
     CSS だけは cascade 順を保つため上の <style> に焼いたまま（build.mjs が node_modules から複写）。 -->
</body>
</html>"#
);

#[cfg(test)]
mod tests {
    use super::*;

    /// doc 33: chat lane (Act II) は `chat: true` で JS に伝わる。
    ///
    /// これが落ちると `showLane` が「xterm instance 無し = 内容無し」と誤判定し、
    /// `#lane-empty` placeholder が ChatView を覆って「Lane が選択されていません」で
    /// 固着する（Act II が選べない体感バグ）。
    #[test]
    fn active_pane_script_carries_chat_flag_for_act2_lane() {
        let script = build_set_active_pane_script(&ActivePaneInfo {
            kind: Some("terminal"),
            pane_id: Some("vp/root"),
            preview_url: None,
            chat: true,
            cwd: None,
            branch: None,
            lane_name: None,
            session_id: None,
            stand: None,
        });
        assert!(script.contains("\"chat\":true"), "script={script}");
        assert!(script.contains("\"pane_id\":\"vp/root\""));
    }

    /// Act I (tui) lane と非 terminal kind は `chat: false`（従来の xterm 判定に従う）。
    #[test]
    fn active_pane_script_chat_false_for_tui_and_stand() {
        let tui = build_set_active_pane_script(&ActivePaneInfo {
            kind: Some("terminal"),
            pane_id: Some("vp/performer/x"),
            preview_url: None,
            chat: false,
            cwd: Some("/Users/mako/repos/vp/.vp/lanes/x"),
            branch: Some("mako/x"),
            lane_name: Some("x"),
            session_id: Some("0196-abcd-ef01"),
            stand: Some("echoes"),
        });
        assert!(tui.contains("\"chat\":false"), "script={tui}");
        // cwd / branch chip の供給が setActivePane 経由で JS に届くこと（header の情報源）。
        assert!(
            tui.contains("\"cwd\":\"/Users/mako/repos/vp/.vp/lanes/x\""),
            "script={tui}"
        );
        assert!(tui.contains("\"branch\":\"mako/x\""), "script={tui}");
        // Act I の session chip 供給路（engine_session_id 相乗り + engine 種別）。
        // Act I は EchoesEvent が流れないため、この経路が欠けると chip が出ない
        //（bug mem_1Cd3icsvKiGsQ8TtX8t1FR の再発防止）。
        assert!(
            tui.contains("\"session_id\":\"0196-abcd-ef01\""),
            "script={tui}"
        );
        assert!(tui.contains("\"stand\":\"echoes\""), "script={tui}");

        let stand = build_set_active_pane_script(&ActivePaneInfo {
            kind: Some("bastet"),
            pane_id: None,
            preview_url: None,
            chat: false,
            cwd: None,
            branch: None,
            lane_name: None,
            session_id: None,
            stand: None,
        });
        assert!(stand.contains("\"chat\":false"), "script={stand}");
        // 非 lane pane は cwd/branch を持たない（chip 非表示）。
        assert!(stand.contains("\"cwd\":null"), "script={stand}");
    }

    // ⚠️ 旧「HTML 文字列に対する assert」4 本（`embedded_show_lane_takes_is_chat_arg` /
    // `embedded_terminal_api_is_session_keyed` / `term_host_is_keyed_by_session_and_never_static` /
    // `xterm_globals_are_supplied_by_the_bundle`）は World A 畳み込みで撤去した。
    // 対象の JS が HTML から消えたので assert が空振りするだけになったため。
    //
    // これらは doc 53 §6.5.1 が言う「**境界に型が無い**ので検証を HTML 文字列に対する assert で
    // 代替している」状態そのものだった。
    //
    // Rust → JS の名前呼びも **制御面は型で塞いだ**（`schema/vp-push.kdl` を SSOT に codegen が
    // 両側へ enum / union を出し、受け口は `window.vpDispatch` 1 本）。引数の数が食い違えば
    // TS のコンパイルが落ちる。残るは高頻度 stream の `vpTerminal.handleOutput` だけで、
    // これは buffer 方針を別に決める必要があるため移行は別 PR。
}
