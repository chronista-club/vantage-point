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
use crate::pane::{ActiveStand, ActivitySnapshot, ProcessPaneState, SidebarState};
use crate::session_state::SessionState;
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
///
/// - `{"t":"process:toggle","path":"...","expanded":true|false}`
/// - `{"t":"lane:select","path":"...","address":"<project>/lead"}`
/// - `{"t":"process:add"}` / `{"t":"process:clone","url":"..."}`
///
/// Phase 5-C: sidebar HTML を `vp-asset://app/sidebar.html` で配信するための extra entry。
/// font 群は `web_assets::FONT_ASSETS` 側で揃ってるので、 ここは sidebar 固有の HTML 1 個のみ。
/// `web_assets::serve()` が FONT_ASSETS と chain して両方 lookup する。
const SIDEBAR_ASSETS: &[(&str, &[u8], &str)] = &[(
    "app/sidebar.html",
    SIDEBAR_HTML.as_bytes(),
    "text/html; charset=utf-8",
)];

const SIDEBAR_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="ja" data-theme="contrast-dark">
<head><meta charset="utf-8"><style>"#,
    include_str!("../assets/creo-tokens.css"),
    r#"</style><style>"#,
    include_str!("../assets/creo-components.css"),
    r#"</style><style>"#,
    include_str!("../assets/nerd-font.css"),
    r#"</style><style>
  /* Phase 5-C: body の font-family を VPMono (= PlemolJP Console NF、 web_assets で bundle)
     に統一。 .nf-icon と同 family なので Latin / 日本語 / icon を一貫した字形で描画。 */
  html,body{margin:0;height:100%;background:var(--color-surface-bg-subtle);color:var(--color-text-primary);font-family:'VPMono',monospace;font-size:12px;line-height:1.4;overflow:hidden;}
  body{display:flex;flex-direction:column;height:100%;}

  /* Projects accordion area (flex 1、 scroll) */
  .processes-section{flex:1;overflow-y:auto;padding:var(--spacing-xs) 0;}

  /* Phase 5-C: Currents / Stopped section header (running vs dead で project list を分割表示) */
  .vp-proc-section-header{padding:var(--spacing-sm) var(--spacing-sm) var(--spacing-xs) var(--spacing-sm);font-size:10px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:0.08em;font-weight:500;user-select:none;}
  .vp-proc-section-header:first-child{padding-top:var(--spacing-xs);}
  /* Phase 5-C polish: Dormant project は視覚的に弱化 (active への視線誘導)。 hover で復帰。 */
  .vp-proc-dormant{opacity:0.65;transition:opacity .15s ease;}
  .vp-proc-dormant:hover{opacity:1;}

  /* Currents 並び替え DnD (HTML5 native) ─ drag 中の対象を半透明 + cursor で示す。
     hover 中の挿入位置は DOM 移動で表現 (placeholder 線は出さず、 シンプルに動く先を直接見せる)。 */
  .vp-currents-list{display:flex;flex-direction:column;}
  .vp-currents-list .creo-accordion{cursor:grab;}
  .vp-currents-list .creo-accordion:active{cursor:grabbing;}
  .vp-currents-list .creo-accordion.dragging{opacity:0.4;}
  /* drag 中は子 (lane row 等) の pointer event を抑止 ─ summary だけ click/drag 認識させる */
  .vp-currents-list .creo-accordion.dragging *{pointer-events:none;}

  /* Phase 5-D: Dormant section 全体を accordion 化 (default 閉じ、 localStorage 永続)。
     vp-proc-section-header の見た目を summary に踏襲。 chevron は ▶ → 90deg 回転。 */
  .vp-dormant-section{}
  .vp-dormant-section>summary{list-style:none;padding:var(--spacing-sm) var(--spacing-sm) var(--spacing-xs) var(--spacing-sm);cursor:pointer;display:flex;align-items:center;gap:6px;font-size:10px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:0.08em;font-weight:500;user-select:none;transition:color .12s ease;}
  .vp-dormant-section>summary::-webkit-details-marker{display:none;}
  .vp-dormant-section>summary:hover{color:var(--color-text-secondary);}
  .vp-dormant-section>summary .chevron{font-family:'VPMono',monospace;font-size:8px;width:8px;display:inline-block;transition:transform .15s ease;color:var(--color-text-tertiary);}
  .vp-dormant-section[open]>summary .chevron{transform:rotate(90deg);}
  .vp-dormant-section .count{font-size:10px;color:var(--color-text-tertiary);font-weight:400;text-transform:none;letter-spacing:0;margin-left:auto;font-variant-numeric:tabular-nums;}

  /* World widget (sidebar 最下部 fixed、 accordion 1-line collapsed)
     Phase 5-C: 旧 widget-slot を最下部に移動、 details/summary で展開可能に */
  .world-widget{flex:0 0 auto;border-top:1px solid var(--color-surface-border,#1f2233);background:var(--color-surface-bg-base);}
  .world-widget>summary{list-style:none;padding:var(--spacing-xs) var(--spacing-sm);cursor:pointer;display:flex;align-items:center;gap:var(--spacing-xs);font-size:11px;color:var(--color-text-secondary);user-select:none;}
  .world-widget>summary::-webkit-details-marker{display:none;}
  .world-widget>summary:hover{background:var(--color-surface-bg-emphasis);}
  .world-widget .world-status{font-size:10px;color:var(--color-status-success,#3fb950);width:12px;text-align:center;}
  .world-widget .world-status.offline{color:var(--color-status-error,#d4444c);}
  .world-widget .world-line{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-variant-numeric:tabular-nums;}
  .world-widget .world-detail{padding:var(--spacing-xs) var(--spacing-sm);border-top:1px dashed var(--color-surface-border,#1f2233);}
  .world-widget .world-stat{display:flex;justify-content:space-between;font-size:11px;padding:1px 0;color:var(--color-text-secondary);}
  .world-widget .world-stat .label{color:var(--color-text-tertiary);}
  .world-widget .world-stat .value{font-weight:500;color:var(--color-text-primary);font-variant-numeric:tabular-nums;}

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
  .vp-clone-inline.expanded{max-height:240px;opacity:1;margin-top:-6px;pointer-events:auto;}
  .vp-clone-inline label{font-size:10px;color:var(--color-text-tertiary);text-transform:uppercase;letter-spacing:0.06em;}
  .vp-clone-inline input{width:100%;padding:6px 8px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:inherit;font-size:12px;box-sizing:border-box;}
  .vp-clone-inline .path-row{display:flex;align-items:center;gap:6px;}
  .vp-clone-inline .path-display{flex:1;min-width:0;padding:5px 8px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:var(--color-surface-bg-base);color:var(--color-text-secondary);font-size:11px;font-family:inherit;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
  .vp-clone-inline .path-display.is-default{color:var(--color-text-tertiary);font-style:italic;}
  .vp-clone-inline .path-icon-btn{flex:0 0 auto;padding:4px 8px;font-size:12px;}
  .vp-clone-inline input:focus{outline:none;border-color:var(--color-brand-primary);}
  .vp-clone-inline .actions{display:flex;justify-content:flex-end;gap:6px;}
  .vp-clone-inline button{padding:4px 10px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:transparent;color:var(--color-text-secondary);cursor:pointer;font-size:11px;font-family:inherit;transition:background .12s ease,color .12s ease;}
  .vp-clone-inline button:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);}
  .vp-clone-inline button.primary{background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);border-color:var(--color-brand-primary-subtle);}
  .vp-clone-inline button.primary:hover{background:var(--color-brand-primary);color:var(--color-surface-bg-base);}

  /* creo-accordion を sidebar 用に override (default の bordered card 風 → flush) */
  .processes-section .creo-accordion{margin:0 6px 2px;background:transparent;border:none;border-radius:var(--radius-sm,6px);overflow:visible;}
  /* Phase 3-D: project title は 13 → 12px に統一 (sidebar base と揃える)、 weight 500 で emphasis */
  .processes-section .creo-accordion-summary{padding:6px 8px;min-height:auto;font-size:12px;border-radius:var(--radius-sm,6px);}
  .processes-section .creo-accordion-summary:hover{background:var(--color-surface-bg-emphasis);}
  .processes-section .creo-accordion-summary::before{font-size:9px;color:var(--color-text-tertiary);width:10px;}
  .processes-section .creo-accordion-title{font-weight:500;font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
  .processes-section .creo-accordion-content{padding:2px 0 4px 18px;}
  .processes-section .creo-accordion-content > * + * {margin-top:0;}

  /* Architecture v4: Lane row (Project → Lane → Stand 階層の中段) */
  .vp-lane-row{display:flex;align-items:center;gap:6px;padding:5px 8px 5px 14px;border-radius:var(--radius-sm,6px);cursor:pointer;transition:background .1s ease;font-size:12px;}
  .vp-lane-row:hover{background:var(--color-surface-bg-emphasis);}
  .vp-lane-row.active{background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);font-weight:500;}
  /* Phase 5-C polish: var(--typography-family-icon) は specificity で .nf-icon を上書きするため
     direct 'VPMono' 宣言に固定。 width:18px は Lane row レイアウト固有なので保持。 */
  .vp-lane-row .icon{width:18px;text-align:center;font-size:12px;font-family:'VPMono',monospace;}
  .vp-lane-row .label{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
  .vp-lane-row .state{font-size:9px;}
  /* Phase 5-D: Worker row の git 状態 subtitle (= branch / ahead-behind / dirty / merged) */
  .vp-lane-row .worker-meta{flex:1;font-size:10px;color:var(--color-text-tertiary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;margin-left:6px;font-style:italic;}
  .vp-lane-row .worker-meta .dirty{color:var(--color-status-warning,#d49b3f);font-weight:500;}
  .vp-lane-row .worker-meta .ahead{color:var(--color-status-info,#3fb9d4);}
  .vp-lane-row .worker-meta .behind{color:var(--color-status-warning,#d49b3f);}
  .vp-lane-row .worker-meta .merged{color:var(--color-status-success,#3fb950);}
  /* Phase 4-A: Worker row × button (delete) — hover 時のみ表示で row UI を雑然とさせない */
  .vp-lane-row .vp-lane-delete{font-size:14px;color:var(--color-text-tertiary);padding:0 4px;border-radius:3px;opacity:0;transition:opacity .12s ease,color .12s ease,background .12s ease;cursor:pointer;}
  .vp-lane-row:hover .vp-lane-delete{opacity:0.8;}
  .vp-lane-row .vp-lane-delete:hover{color:#fff;background:var(--color-status-error,#d4444c);opacity:1;}

  /* Phase 5-C minimal: Project Stands (PP/GE/HP) を sidebar からオミット。
     stand-row / project-stands / lanes-header は全削除。 Lane 階層は単一 section
     (= LANES のみ) なので header 自体不要。 Lane 行末尾に Stand glyph を inline 統合。 */

  /* SP 未起動 / Lane loading 等の hint 表示 */
  .vp-empty-hint{padding:6px 12px 6px 14px;font-size:11px;color:var(--color-text-tertiary);font-style:italic;}

  /* Phase 3-A: + Add Worker button + inline form */
  .vp-add-worker{display:flex;align-items:center;gap:6px;padding:5px 8px 5px 14px;border-radius:var(--radius-sm,6px);cursor:pointer;color:var(--color-text-tertiary);font-size:11px;font-style:italic;transition:background .12s ease,color .12s ease;}
  .vp-add-worker:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-secondary);}
  .vp-add-worker .icon{width:18px;text-align:center;}
  .vp-add-worker-form{margin:0 8px 6px 14px;display:flex;flex-direction:column;gap:6px;max-height:0;opacity:0;overflow:hidden;transition:max-height .22s ease,opacity .22s ease,margin-top .22s ease;margin-top:0;pointer-events:none;}
  .vp-add-worker-form.expanded{max-height:160px;opacity:1;margin-top:-2px;pointer-events:auto;}
  .vp-add-worker-form .creo-input{width:100%;padding:5px 8px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:inherit;font-size:11px;box-sizing:border-box;}
  .vp-add-worker-form .creo-input:focus{outline:none;border-color:var(--color-brand-primary);}
  .vp-add-worker-actions{display:flex;justify-content:flex-end;gap:6px;}
  .vp-add-worker-form button{padding:3px 10px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:transparent;color:var(--color-text-secondary);cursor:pointer;font-size:10px;font-family:inherit;transition:background .12s ease,color .12s ease;}
  .vp-add-worker-form button:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);}
  .vp-add-worker-form button[data-variant="primary"]{background:var(--color-brand-primary-subtle);color:var(--color-brand-primary);border-color:var(--color-brand-primary-subtle);}
  .vp-add-worker-form button[data-variant="primary"]:hover{background:var(--color-brand-primary);color:var(--color-surface-bg-base);}

  .empty,.loading,.error{padding:var(--spacing-sm);color:var(--color-text-tertiary);font-style:italic;font-size:12px;}

  /* Phase 5-C minimal: Nerd Font 単色 icon + state color (emoji 全廃)
     icon は --typography-family-icon (Nerd Font Mono symbols)、 色は CSS class で割当。
     .nf-icon CSS rule 自体は web_assets::NERD_FONT_CSS で先頭 <style> に注入済み。 */
  .vp-state-running{color:var(--color-status-success,#3fb950);}
  .vp-state-dead{color:var(--color-status-error,#d4444c);}
  .vp-state-spawning{color:var(--color-status-warning,#d49b3f);}
  .vp-state-idle{color:var(--color-text-tertiary);}
  .vp-state-working{color:var(--color-brand-primary);}
  .vp-state-pausing{color:var(--color-text-tertiary);}
  .vp-state-exiting{color:var(--color-status-warning,#d49b3f);}

  /* Phase 5-C: Lead restart button (project row hover で表示) */
  .vp-project-restart{font-family:'VPMono',monospace;font-size:11px;color:var(--color-text-tertiary);padding:0 var(--spacing-xs);border-radius:3px;opacity:0;transition:opacity .12s ease,color .12s ease,background .12s ease;cursor:pointer;}
  .processes-section .creo-accordion-summary:hover .vp-project-restart{opacity:0.7;}
  .vp-project-restart:hover{color:var(--color-brand-primary);background:var(--color-brand-primary-subtle);opacity:1;}
  /* Lane Lead Stand restart icon (Lane row hover で表示、 click → confirm dialog) */
  .vp-lane-restart{font-family:'VPMono',monospace;font-size:11px;color:var(--color-text-tertiary);padding:0 var(--spacing-xs);border-radius:3px;opacity:0;transition:opacity .12s ease,color .12s ease,background .12s ease;cursor:pointer;margin-left:4px;}
  .vp-lane-row:hover .vp-lane-restart{opacity:0.6;}
  .vp-lane-restart:hover{color:var(--color-brand-primary);background:var(--color-brand-primary-subtle);opacity:1;}
  /* Restart confirm dialog (HTML5 <dialog> + ::backdrop) */
  .vp-restart-dialog{padding:0;border:1px solid var(--color-surface-border,#1f2233);border-radius:var(--radius-md,8px);background:var(--color-surface-bg-base);color:var(--color-text-primary);font-family:inherit;font-size:13px;min-width:340px;max-width:480px;box-shadow:0 8px 32px rgba(0,0,0,.5);}
  .vp-restart-dialog::backdrop{background:rgba(0,0,0,.5);backdrop-filter:blur(2px);}
  .vp-restart-dialog .body{padding:16px 20px 12px 20px;}
  .vp-restart-dialog .title{font-weight:600;margin:0 0 8px 0;font-size:14px;}
  .vp-restart-dialog .detail{margin:0;color:var(--color-text-secondary);font-size:12px;line-height:1.5;}
  .vp-restart-dialog .target{margin:10px 0 0 0;font-family:'VPMono',monospace;font-size:11px;color:var(--color-text-tertiary);padding:6px 8px;background:var(--color-surface-bg-emphasis);border-radius:var(--radius-sm,4px);}
  .vp-restart-dialog .actions{display:flex;justify-content:flex-end;gap:8px;padding:8px 20px 16px 20px;}
  .vp-restart-dialog button{padding:5px 14px;border-radius:var(--radius-sm,6px);border:1px solid var(--color-surface-border,#1f2233);background:transparent;color:var(--color-text-secondary);font-family:inherit;font-size:12px;cursor:pointer;transition:background .12s ease,color .12s ease,border-color .12s ease;}
  .vp-restart-dialog button:hover{background:var(--color-surface-bg-emphasis);color:var(--color-text-primary);}
  .vp-restart-dialog button[data-variant="danger"]{background:var(--color-warning-subtle,rgba(255,107,107,.12));color:var(--color-warning,#ff6b6b);border-color:var(--color-warning-subtle,rgba(255,107,107,.32));}
  .vp-restart-dialog button[data-variant="danger"]:hover{background:var(--color-warning,#ff6b6b);color:var(--color-surface-bg-base);}
</style></head>
<body>
  <!-- Phase 5-C minimal: section header "Projects" は冗長 (sidebar = projects と自明) → 削除。
       widget-slot を world-widget として最下部に移動、 accordion 化。 -->
  <div class="processes-section">
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
    <label>Clone destination</label>
    <div class="path-row">
      <span id="clone-path-display" class="path-display is-default" title="default 設定 (~/repos など)">(default)</span>
      <button type="button" id="clone-path-browse" class="path-icon-btn" title="Browse folder...">📁</button>
      <button type="button" id="clone-path-clear" class="path-icon-btn" title="Use default">×</button>
    </div>
    <div class="actions">
      <button type="button" id="clone-cancel">Cancel</button>
      <button type="button" class="primary" id="clone-confirm">Clone</button>
    </div>
  </div>
  <!-- Phase 5-C minimal: World widget = sidebar 最下部 fixed accordion。
       collapsed = 1 行 ("● online vN — Pp/Rr"), expanded = 詳細 stat list。 -->
  <details class="world-widget" id="world-details">
    <summary>
      <span class="world-status offline nf-icon" id="world-status"></span>
      <span class="world-line" id="world-line">offline</span>
    </summary>
    <div class="world-detail">
      <div class="world-stat"><span class="label">Version</span><span class="value" id="world-version">—</span></div>
      <div class="world-stat"><span class="label">Started</span><span class="value" id="world-uptime">—</span></div>
      <div class="world-stat"><span class="label">Projects</span><span class="value" id="proj-count">0</span></div>
      <div class="world-stat"><span class="label">Processes</span><span class="value" id="proc-count">0</span></div>
    </div>
  </details>
  <!-- Lane Lead Stand restart 確認 dialog (global single instance、 表示は showRestartDialog から) -->
  <dialog id="vp-restart-dialog" class="vp-restart-dialog">
    <div class="body">
      <p class="title">Restart Lead Stand?</p>
      <p class="detail">この Lane の child process (claude 等) を kill して同じ stand 設定で再 spawn します。 進行中の対話は失われ、 WebSocket は auto-reconnect で新 PtySlot に attach し直します。</p>
      <p class="target" id="restart-dialog-target"></p>
    </div>
    <div class="actions">
      <button type="button" data-action="cancel">Cancel</button>
      <button type="button" data-action="ok" data-variant="danger">Restart</button>
    </div>
  </dialog>
<script>"#,
    include_str!("../assets/nerd-font-loader.js"),
    r#"
  // Rust から push される sidebar state を保持
  let state = null;
  let pendingState = null;
  let domReady = false;

  // Phase 5-D fix: ephemeral UI state を re-render を跨いで保持。
  //  full DOM rebuild (`root.innerHTML = ''`) で form の expanded class が消える問題を回避。
  //  Set 内に project path があれば `<vp-add-worker-form>` を expanded として再構成。
  const addWorkerOpen = new Set();

  // ipc 送信 wrapper (window.ipc は wry が提供)
  function send(msg) {
    if (window.ipc && window.ipc.postMessage) {
      window.ipc.postMessage(JSON.stringify(msg));
    }
  }

  // Lane Lead Stand restart 確認 dialog (HTML5 <dialog>、 global single instance)。
  //  Lane row の restart icon click → showRestartDialog(path, address) で modal 表示、
  //  OK で `lane:restart` IPC、 Cancel / Esc で dismiss。 destructive action なので
  //  確認 1 step を必須化 (cf. Worker delete は dogfood speed 優先で confirm 無し)。
  let restartTarget = null;
  function showRestartDialog(path, address) {
    restartTarget = {path: path, address: address};
    const tgt = document.getElementById('restart-dialog-target');
    if (tgt) tgt.textContent = address;
    const dlg = document.getElementById('vp-restart-dialog');
    if (dlg && dlg.showModal) dlg.showModal();
  }
  document.addEventListener('DOMContentLoaded', () => {
    const dlg = document.getElementById('vp-restart-dialog');
    if (!dlg) return;
    const cancelBtn = dlg.querySelector('button[data-action="cancel"]');
    const okBtn = dlg.querySelector('button[data-action="ok"]');
    if (cancelBtn) cancelBtn.addEventListener('click', () => { dlg.close(); restartTarget = null; });
    if (okBtn) okBtn.addEventListener('click', () => {
      if (restartTarget) send({t: 'lane:restart', path: restartTarget.path, address: restartTarget.address});
      dlg.close();
      restartTarget = null;
    });
    dlg.addEventListener('cancel', () => { restartTarget = null; }); // Esc キー
  });


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
    // Phase 5-C minimal: collapsed 1 行 + expanded 詳細の 2 view。
    // collapsed: 状態 dot + "v<ver> — P<n> R<m>" の compact 表現。
    const status = document.getElementById('world-status');
    const line = document.getElementById('world-line');
    const ver = document.getElementById('world-version');
    const upt = document.getElementById('world-uptime');
    const pc = document.getElementById('proj-count');
    const rc = document.getElementById('proc-count');
    if (!status || !line || !ver || !upt || !pc || !rc) return;
    const online = !!(activity && activity.world_online);
    status.classList.toggle('offline', !online);
    const projCount = (activity && activity.project_count) || 0;
    const runCount = (activity && activity.running_process_count) || 0;
    if (online) {
      const v = (activity && activity.world_version) || '?';
      line.textContent = 'TheWorld v' + v + ' — P' + projCount + ' R' + runCount;
    } else {
      line.textContent = 'TheWorld offline';
    }
    ver.textContent = (activity && activity.world_version) || '—';
    upt.textContent = formatStartedAt(activity && activity.world_started_at);
    pc.textContent = String(projCount);
    rc.textContent = String(runCount);
  }

  // Phase 5-C: Currents / Dormant でプロジェクトを 2 段に分割。
  //   ルールは反転定義 (white-list より意図が明確):
  //     Dormant = state が `dead` または unset (= 一度も起動していない / 死亡後 reset 済)
  //     Currents = 上記以外すべて (spawning / running / idle / working / pausing / exiting)
  //   触ると state が遷移し (例: dead → spawning)、 自然に Currents 側に上がる UX。
  //   各セクションは空ならヘッダごと描画しない (累計 0 のラベル違和感を避ける)。
  function isProcessAlive(state) {
    return !!state && state !== 'dead';
  }
  function renderProjects(projects) {
    const root = document.getElementById('projects');
    if (!root) return;
    root.innerHTML = '';
    if (!projects || projects.length === 0) {
      root.innerHTML = '<div class="empty">(no projects)</div>';
      return;
    }
    const currents = [];
    const stopped = [];
    for (const p of projects) {
      if (isProcessAlive(p.state)) currents.push(p);
      else stopped.push(p);
    }
    if (currents.length > 0) {
      // Currents 表示順: state.currents_order があればその順で並べる (vp-app 再起動越え)。
      // order に含まれない project は order の末尾に TheWorld 由来順で追加。
      const order = (state && state.currents_order) || null;
      if (order && order.length > 0) {
        const indexOf = new Map(order.map((p, i) => [p, i]));
        currents.sort((a, b) => {
          const ai = indexOf.has(a.path) ? indexOf.get(a.path) : Number.MAX_SAFE_INTEGER;
          const bi = indexOf.has(b.path) ? indexOf.get(b.path) : Number.MAX_SAFE_INTEGER;
          return ai - bi;
        });
      }

      const h = document.createElement('div');
      h.className = 'vp-proc-section-header';
      h.textContent = 'Currents';
      root.appendChild(h);

      // DnD container — drop position を data-drop-after で示すため container 単位で hover/drop 受ける
      const cur = document.createElement('div');
      cur.className = 'vp-currents-list';
      cur.dataset.section = 'currents';
      for (const p of currents) renderProjectAccordion(p, cur, false);
      attachCurrentsDnd(cur);
      root.appendChild(cur);
    }
    if (stopped.length > 0) {
      // Phase 5-D: Dormant 全体を <details> で囲んで折りたたみ可能に。 default 閉じ、
      //  localStorage で開閉状態を永続化 (sidebar 内で完結、 IPC 不要の軽量永続化)。
      let dormantOpen = false;
      try { dormantOpen = localStorage.getItem('vp.sidebar.dormant.open') === '1'; } catch (_) {}
      const sec = document.createElement('details');
      sec.className = 'vp-dormant-section';
      if (dormantOpen) sec.setAttribute('open', '');
      sec.addEventListener('toggle', () => {
        try { localStorage.setItem('vp.sidebar.dormant.open', sec.open ? '1' : '0'); } catch (_) {}
      });
      const sum = document.createElement('summary');
      const chev = document.createElement('span');
      chev.className = 'chevron';
      chev.textContent = '▶';
      const lbl = document.createElement('span');
      lbl.className = 'label';
      // "Currents" (流水) の対比として "Dormant" (休眠) — active ⇄ dormant の王道対比。
      lbl.textContent = 'Dormant';
      const cnt = document.createElement('span');
      cnt.className = 'count';
      cnt.textContent = '(' + stopped.length + ')';
      sum.appendChild(chev);
      sum.appendChild(lbl);
      sum.appendChild(cnt);
      sec.appendChild(sum);
      for (const p of stopped) renderProjectAccordion(p, sec, true);
      root.appendChild(sec);
    }
  }

  function renderProjectAccordion(p, root, dormant) {
      // creo-accordion: native <details> ベース。expand/collapse + chevron + ARIA は creo-ui 側 CSS。
      // Phase 5-C polish: Dormant 行は `vp-proc-dormant` で opacity 0.65 に視覚弱化。
      const proj = document.createElement('details');
      proj.className = 'creo-accordion' + (dormant ? ' vp-proc-dormant' : '');
      if (p.expanded) proj.setAttribute('open', '');
      // Currents (= !dormant) のみ drag 可。 Dormant は順序操作対象外 (state による分類)。
      if (!dormant) {
        proj.draggable = true;
        proj.dataset.path = p.path;
      }
      // 'toggle' イベントで Rust に永続化 IPC を送る (native toggle は即時、IPC は state 同期用)
      proj.addEventListener('toggle', () => {
        send({t: 'process:toggle', path: p.path, expanded: proj.open});
      });

      const summary = document.createElement('summary');
      summary.className = 'creo-accordion-summary';
      // Phase 5-C minimal: kind icon (⭐) は冗長 (accordion title だけで project と分かる) → 削除。
      // state は Nerd Font 単色 glyph で右端に。
      const title = document.createElement('span');
      title.className = 'creo-accordion-title';
      title.textContent = p.name;
      summary.appendChild(title);
      // Phase 5-C polish: restart button (hover で出現、 click で SP restart)。
      //  = nf-fa-refresh、 lane WS error 時に user が即復帰できる入口。
      const restartBtn = document.createElement('span');
      restartBtn.className = 'vp-project-restart nf-icon';
      restartBtn.textContent = '';
      restartBtn.style.cssText = 'margin-left:auto;';
      restartBtn.title = 'Restart SP (stop + start)';
      restartBtn.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        send({t: 'process:restart', path: p.path});
      });
      summary.appendChild(restartBtn);
      const stateBadge = document.createElement('span');
      stateBadge.className = 'state';
      stateBadge.style.cssText = 'margin-left:6px;';
      stateBadge.innerHTML = stateGlyphHTML(p.state);
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
        // Phase 5-C minimal: Project Stands (PP/GE/HP) は sidebar からオミット。
        // section header (PROJECT STANDS / LANES) も冗長になるため削除。
        // Lane 行を直接列挙する flat 構造に統一。
        for (const lane of lanes) {
          const addr = laneAddressKey(lane);
          const isActive = activeAddr && activeAddr === addr;

          // Phase 5-C minimal: Lane row 単一行に集約。
          // 構成: [stand glyph (Nerd Font 単色)] [label] ... [state circle] [× (worker only)]
          // 旧 separate Stand child row は削除、 Stand 識別は 行頭 glyph で表現。
          const row = document.createElement('div');
          row.className = 'vp-lane-row' + (isActive ? ' active' : '');
          const isWorker = (lane.kind === 'worker') ||
            (lane.address && lane.address.kind === 'worker');
          const standGlyph = STAND_GLYPH[lane.stand] || '';
          const standIcon = document.createElement('span');
          standIcon.className = 'icon nf-icon';
          standIcon.textContent = standGlyph;
          standIcon.title = standDisplayName(lane.stand) || lane.stand || '';
          const label = document.createElement('span');
          label.className = 'label';
          label.textContent = laneLabel(lane);
          const stateMark = document.createElement('span');
          stateMark.className = 'state';
          stateMark.innerHTML = stateGlyphHTML(lane.state);
          row.appendChild(standIcon);
          row.appendChild(label);
          // Phase 5-D: Worker のみ git 状態 subtitle (branch · ahead/behind · dirty/merged)
          if (isWorker && lane.worker_status) {
            const ws = lane.worker_status;
            const meta = document.createElement('span');
            meta.className = 'worker-meta';
            // branch は textContent で escape (user 入力由来 → XSS 対策)、 数値系は HTML safe
            let hasContent = false;
            if (ws.branch) {
              const br = document.createElement('span');
              br.textContent = ws.branch;
              meta.appendChild(br);
              hasContent = true;
            }
            const ahead = ws.ahead | 0;
            const behind = ws.behind | 0;
            if (ahead > 0 || behind > 0) {
              if (hasContent) meta.appendChild(document.createTextNode(' '));
              if (ahead > 0) {
                const a = document.createElement('span');
                a.className = 'ahead';
                a.textContent = '↑' + ahead;
                meta.appendChild(a);
              }
              if (behind > 0) {
                const b = document.createElement('span');
                b.className = 'behind';
                b.textContent = '↓' + behind;
                meta.appendChild(b);
              }
              hasContent = true;
            }
            if ((ws.dirty_count | 0) > 0) {
              if (hasContent) meta.appendChild(document.createTextNode(' '));
              const d = document.createElement('span');
              d.className = 'dirty';
              d.textContent = ws.dirty_count + 'M';
              meta.appendChild(d);
              hasContent = true;
            }
            if (ws.is_merged) {
              if (hasContent) meta.appendChild(document.createTextNode(' '));
              const m = document.createElement('span');
              m.className = 'merged';
              m.textContent = 'merged';
              meta.appendChild(m);
              hasContent = true;
            }
            if (hasContent) row.appendChild(meta);
          }
          row.appendChild(stateMark);
          // Lane Lead Stand restart icon (Lead/Worker 共通、 hover で出現、 click → confirm dialog)
          //  destructive action (claude 等の child 強制 kill + respawn) なので
          //  showRestartDialog で OK/Cancel 1 step 挟む。
          const restartLaneBtn = document.createElement('span');
          restartLaneBtn.className = 'vp-lane-restart nf-icon';
          restartLaneBtn.textContent = ''; // nf-fa-refresh (vp-project-restart と同じ glyph)
          restartLaneBtn.title = 'Restart Lead Stand (kill + respawn、 confirm dialog 表示)';
          restartLaneBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            showRestartDialog(p.path, addr);
          });
          row.appendChild(restartLaneBtn);
          // Phase 4-A: Worker のみ × button (即 delete、 confirm dialog なし — dogfooding speed 優先)
          if (isWorker) {
            const delBtn = document.createElement('span');
            delBtn.className = 'vp-lane-delete';
            delBtn.textContent = '×';
            delBtn.title = 'Delete worker (PtySlot kill + lane remove)';
            delBtn.addEventListener('click', (e) => {
              e.stopPropagation();
              send({t: 'lane:delete', path: p.path, address: addr});
            });
            row.appendChild(delBtn);
          }
          row.addEventListener('click', (e) => {
            e.stopPropagation();
            send({t: 'lane:select', path: p.path, address: addr});
          });
          content.appendChild(row);
        }

        // Phase 3-A: + Add Worker button + inline form (POST /api/lanes + ccws clone 連動)
        const addWorker = document.createElement('div');
        addWorker.className = 'vp-add-worker';
        // Phase 5-C minimal: ラベルは "+" のみ (hover で title tooltip)
        addWorker.title = 'Add Worker (ccws clone)';
        addWorker.innerHTML =
          '<span class="icon">+</span>' +
          '<span class="label">Add Worker</span>';
        const addForm = document.createElement('div');
        addForm.className = 'vp-add-worker-form';
        addForm.innerHTML =
          '<input type="text" class="creo-input" placeholder="worker name (例: feat-api)" data-field="name">' +
          '<input type="text" class="creo-input" placeholder="branch (例: mako/feat-api)" data-field="branch">' +
          '<div class="vp-add-worker-actions">' +
            '<button class="creo-btn" data-variant="secondary" data-size="sm" data-action="cancel">Cancel</button>' +
            '<button class="creo-btn" data-variant="primary" data-size="sm" data-action="submit">Create</button>' +
          '</div>';
        // Phase 5-D fix: form 開閉状態を addWorkerOpen Set に永続化。
        //  re-render で DOM 再生成されても、 Set に path があれば expanded を維持。
        if (addWorkerOpen.has(p.path)) addForm.classList.add('expanded');
        addWorker.addEventListener('click', () => {
          addWorkerOpen.add(p.path);
          addForm.classList.add('expanded');
          const nameInput = addForm.querySelector('input[data-field="name"]');
          if (nameInput) setTimeout(() => nameInput.focus(), 50);
        });
        const cancelBtn = addForm.querySelector('button[data-action="cancel"]');
        const submitBtn = addForm.querySelector('button[data-action="submit"]');
        const closeForm = () => {
          addWorkerOpen.delete(p.path);
          addForm.classList.remove('expanded');
        };
        const submit = () => {
          const nameInput = addForm.querySelector('input[data-field="name"]');
          const branchInput = addForm.querySelector('input[data-field="branch"]');
          const nameVal = (nameInput && nameInput.value || '').trim();
          const branchVal = (branchInput && branchInput.value || '').trim();
          if (!nameVal) return;
          send({t: 'lane:add_worker', path: p.path, name: nameVal, branch: branchVal || null});
          closeForm();
          if (nameInput) nameInput.value = '';
          if (branchInput) branchInput.value = '';
        };
        if (cancelBtn) cancelBtn.addEventListener('click', closeForm);
        if (submitBtn) submitBtn.addEventListener('click', submit);
        addForm.querySelectorAll('input').forEach(inp => {
          inp.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') { e.preventDefault(); submit(); }
            else if (e.key === 'Escape') { e.preventDefault(); closeForm(); }
          });
        });
        content.appendChild(addWorker);
        content.appendChild(addForm);
      }

      proj.appendChild(content);
      root.appendChild(proj);
  }

  // Currents 並び替え DnD ─ HTML5 native drag-and-drop。
  //   dragstart: drag 元 details に .dragging class
  //   dragover:  drop 候補位置を計算 (mouseY と各 child の中間点で判定)
  //   drop:      新 order を data-path から拾って IPC `process:reorder` 送信
  // Currents container 単位で 1 度 attach、 内部の details が draggable=true。
  function attachCurrentsDnd(container) {
    let dragging = null;

    container.addEventListener('dragstart', (e) => {
      const tgt = e.target.closest('details.creo-accordion');
      if (!tgt || tgt.parentElement !== container) return;
      dragging = tgt;
      tgt.classList.add('dragging');
      // setData で Firefox 対応 (一部 browser は dataTransfer 空だと drag 開始しない)
      try { e.dataTransfer.setData('text/plain', tgt.dataset.path || ''); } catch (_) {}
      e.dataTransfer.effectAllowed = 'move';
    });

    container.addEventListener('dragend', () => {
      if (dragging) dragging.classList.remove('dragging');
      dragging = null;
    });

    container.addEventListener('dragover', (e) => {
      if (!dragging) return;
      e.preventDefault();           // drop を許可するために必要
      e.dataTransfer.dropEffect = 'move';
      // mouse Y 位置で挿入先を決定 — children のうち最も近い「上半分」 の child の前に置く
      const after = getDropTarget(container, e.clientY, dragging);
      if (after === null) {
        container.appendChild(dragging);
      } else if (after !== dragging.nextSibling) {
        container.insertBefore(dragging, after);
      }
    });

    container.addEventListener('drop', (e) => {
      e.preventDefault();
      if (!dragging) return;
      // 新 order を DOM から取得 → IPC 送信
      const order = Array.from(container.querySelectorAll(':scope > details.creo-accordion'))
        .map(el => el.dataset.path)
        .filter(Boolean);
      send({t: 'process:reorder', order});
    });
  }

  // dragover 時の挿入先を決定する helper。 mouse Y より下にある最初の child の上半分なら
  // その child の前に挿入、 それ以外は末尾。 dragging 中の自身は除外。
  function getDropTarget(container, y, dragging) {
    const others = Array.from(container.querySelectorAll(':scope > details.creo-accordion'))
      .filter(el => el !== dragging);
    for (const el of others) {
      const rect = el.getBoundingClientRect();
      if (y < rect.top + rect.height / 2) return el;
    }
    return null;
  }

  // Phase 5-C minimal: Nerd Font 単色 glyph 集約。 redundant な kindIcon (⭐📍🦾) は撤去、
  // Stand identity (PP/GE/HP/HD/TH) と state circle のみ残す。 全 emoji → Nerd Font に置換、
  // 色は CSS class (vp-state-running 等) で割当 (絵文字の色味バラつきゼロ)。
  const STAND_GLYPH = {
    paisley_park: '',   // nf-fa-compass
    gold_experience: '', // nf-fa-leaf
    hermit_purple: '',  // nf-fa-plug
    heavens_door: '',   // nf-fa-book
    the_hand: '',       // nf-fa-terminal
  };
  const STATE_CLASS = {
    running: 'vp-state-running',
    spawning: 'vp-state-spawning',
    idle: 'vp-state-idle',
    working: 'vp-state-working',
    pausing: 'vp-state-pausing',
    exiting: 'vp-state-exiting',
    dead: 'vp-state-dead',
  };
  //  = nf-fa-circle (filled)、 色は CSS class で。 unknown state は空 ('')。
  function stateGlyphHTML(s) {
    const cls = STATE_CLASS[s];
    if (!cls) return '';
    return '<span class="nf-icon ' + cls + '"></span>';
  }
  // 旧 processKindIcon は撤去 (kind 別 emoji 不要、 row 位置 + label が情報主体)。
  // 旧 processStateMark は stateGlyphHTML に置換。
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
  // Phase 5-C minimal: 旧 laneStandIcon (絵文字 📖/✋) は STAND_GLYPH (Nerd Font 単色) に置換、 削除。
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

    // Clone Repository — sidebar 内 inline expand form で URL + clone 先 (任意) を受け取る
    const cloneBtn = document.getElementById('clone-project-btn');
    const cloneInline = document.getElementById('clone-inline');
    const cloneInput = document.getElementById('clone-url');
    const cloneCancel = document.getElementById('clone-cancel');
    const cloneConfirm = document.getElementById('clone-confirm');
    const clonePathDisplay = document.getElementById('clone-path-display');
    const clonePathBrowse = document.getElementById('clone-path-browse');
    const clonePathClear = document.getElementById('clone-path-clear');
    // null = default (Settings の default_project_root + repo 名 で auto)
    let clonePathOverride = null;
    function updateClonePathDisplay() {
      if (!clonePathDisplay) return;
      if (clonePathOverride) {
        clonePathDisplay.textContent = clonePathOverride;
        clonePathDisplay.classList.remove('is-default');
        clonePathDisplay.title = clonePathOverride;
      } else {
        clonePathDisplay.textContent = '(default)';
        clonePathDisplay.classList.add('is-default');
        clonePathDisplay.title = 'default 設定 (~/repos など)';
      }
    }
    function openCloneInline() {
      if (!cloneInline) return;
      cloneInput.value = '';
      clonePathOverride = null;
      updateClonePathDisplay();
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
      // Phase 2.x-a: #210 の target_dir picker 機能を Phase 1 の process: prefix に統合
      const msg = {t: 'process:clone', url: url};
      if (clonePathOverride) msg.target_dir = clonePathOverride;
      send(msg);
      closeCloneInline();
    }
    if (cloneBtn) cloneBtn.addEventListener('click', () => {
      collapseAdd();
      openCloneInline();
    });
    if (cloneCancel) cloneCancel.addEventListener('click', closeCloneInline);
    if (cloneConfirm) cloneConfirm.addEventListener('click', submitClone);
    if (clonePathBrowse) clonePathBrowse.addEventListener('click', () => {
      send({t: 'project:clone:pickFolder'});
    });
    if (clonePathClear) clonePathClear.addEventListener('click', () => {
      clonePathOverride = null;
      updateClonePathDisplay();
    });
    // Rust 側の picker から呼ばれる setter (キャンセル時は path = null)
    window.setClonePath = (path) => {
      clonePathOverride = (typeof path === 'string' && path.length > 0) ? path : null;
      updateClonePathDisplay();
    };
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
/// 1. `git clone <url> <target>` を実行 (target は override 優先、無ければ
///    `<default_root>/<repo_name>`)
/// 2. 成功なら `add_project` で TheWorld に register
/// 3. `list_projects` で再取得 → `AppEvent::ProcessesLoaded`
///
/// `target_override` が `Some` ならそれを target とする (user が picker で選択した
/// folder)。`None` なら `default_root` + repo 名で auto 決定。後者で `default_root`
/// も `None` の場合は何もしない (default_project_root が解決できないケース)。
/// git バイナリが PATH に無い場合も spawn 失敗で終わる。
fn spawn_clone_project(
    proxy: EventLoopProxy<AppEvent>,
    url: String,
    default_root: Option<std::path::PathBuf>,
    target_override: Option<std::path::PathBuf>,
) {
    // Phase 2.x-a: #210 の target_override を取り込み + Phase 1 の `process:` prefix を維持。
    // priority: 1) explicit target_override (picker 選択 path)、 2) default_root + repo 名
    let target = if let Some(t) = target_override {
        t
    } else if let Some(root) = default_root {
        root.join(derive_repo_name(&url))
    } else {
        tracing::warn!("process:clone but default_project_root is unresolved (set in settings)");
        return;
    };
    let _ = thread::Builder::new()
        .name("clone-project".into())
        .spawn(move || {
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
            // Register — project 名は target folder の末尾セグメントから (override 時は
            // user が選んだ folder 名、default 時は repo 名と同一になる)
            let project_name = target
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| derive_repo_name(&url));
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
                if let Err(e) = client.add_project(&project_name, &path_str).await {
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

/// Clone 先 folder picker。選択結果 (キャンセル時は None) を `AppEvent::ClonePathPicked`
/// で main thread に送り、sidebar JS の `window.setClonePath()` に push する。
///
/// `initial_dir` が `Some` なら picker の初期表示ディレクトリに設定。
fn spawn_clone_path_picker(
    proxy: EventLoopProxy<AppEvent>,
    initial_dir: Option<std::path::PathBuf>,
) {
    let _ = thread::Builder::new()
        .name("clone-path-picker".into())
        .spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Clone 先フォルダを選択");
            if let Some(d) = initial_dir.as_ref() {
                dialog = dialog.set_directory(d);
            }
            let folder = dialog.pick_folder();
            let payload = folder.map(|p| p.to_string_lossy().into_owned());
            let _ = proxy.send_event(AppEvent::ClonePathPicked(payload));
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

    /// JS string literal にする (Phase review fix #3 と同設計: serde_json::to_string で
    /// 全 UTF-8 + null byte + surrogate を JSON spec で escape、 JS の valid string literal に)。
    /// Lane address は通常 ASCII safe (`<project>/lead`) だが、 一貫性と future-proof のため統一。
    fn js_str(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
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

/// Architecture v4: sidebar の active selection に応じて main area の表示 kind を切替。
///
/// Phase 5-A 拡張: Lane と Stand が **mutually exclusive** な active 軸として扱われる。
/// 優先順位:
///   1. `active_stand` Some → kind = "paisley_park" / "gold_experience" / "hermit_purple"
///   2. `active_lane_address` Some → kind = "terminal"、 pane_id = Lane address
///   3. 両方 None → kind=None で empty placeholder
///
/// Lane address ごとの terminal 接続は per-Lane xterm.js (Phase 2.5) が JS-side で管理。
fn push_active_view(main_view: &WebView, state: &SidebarState) {
    let info = if let Some(stand) = state.active_stand.as_ref() {
        ActivePaneInfo {
            kind: Some(stand.kind.as_str()),
            pane_id: None,
            preview_url: None,
        }
    } else if let Some(addr) = state.active_lane_address.as_deref() {
        ActivePaneInfo {
            kind: Some("terminal"),
            pane_id: Some(addr),
            preview_url: None,
        }
    } else {
        ActivePaneInfo {
            kind: None,
            pane_id: None,
            preview_url: None,
        }
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
    /// active Lane が変わったか (true なら push_active_view を呼ぶ)
    active_changed: bool,
    /// SP auto-spawn が必要な project (= 「Current」 になった dead な project)。
    /// `(name, path)` を返し、 caller が `spawn_sp_start` を呼ぶ。
    /// dedup は caller の `sp_spawn_triggered: HashSet<String>` (path key) で行う。
    sp_spawn_request: Option<(String, String)>,
    /// Phase 3-A: Worker Lane 作成要求 `(project_path, name, branch)`。
    /// caller が project の SP port を解決して `client.create_worker_lane` を呼ぶ。
    add_worker_request: Option<(String, String, Option<String>)>,
    /// Phase 4-A: Worker Lane 削除要求 `(project_path, address)`。
    /// caller が SP port を解決して `client.delete_lane` を呼ぶ。
    delete_lane_request: Option<(String, String)>,
    /// Lane Lead Stand restart 要求 `(project_path, address)`。
    /// caller が SP port を解決して `client.restart_lane` を呼ぶ。
    restart_lane_request: Option<(String, String)>,
    /// Phase 5-C: Process restart 要求 `(project_name)`。
    /// caller が TheWorld の `/api/world/processes/{name}/restart` を呼ぶ。
    restart_process_request: Option<String>,
    /// Phase 5-D fix: SP auto-spawn dedup HashSet から path を release する要求。
    /// 「accordion を閉じる」 = 「ユーザが retry を望んでいる」 と解釈、 失敗ループの
    /// dedup deadlock を抜けられるようにする。 caller は `sp_spawn_triggered.remove(path)` を呼ぶ。
    sp_spawn_release: Option<String>,
}

/// sidebar webview から IPC で受け取った JSON を解釈し、`SidebarState` を mutate。
fn handle_sidebar_ipc(
    msg: &str,
    state: &mut SidebarState,
    session: &mut SessionState,
) -> SidebarIpcOutcome {
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
                    // session 永続化: vp-app 再起動時に accordion 状態を復元
                    session.set_project_expanded(path.to_string(), new_state);
                    session.save();
                }
                if new_state && p.state.as_deref() == Some("dead") {
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
        "lane:delete" => {
            // Phase 4-A: Worker Lane 削除要求。 caller (event loop) で SP port を解決して
            // client.delete_lane を呼ぶ。 active Lane を消した場合は active_lane_address を unset。
            let address = parsed.get("address").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() && !address.is_empty() {
                out.delete_lane_request = Some((path.to_string(), address.to_string()));
                // active だった Lane が消えるなら preemptively clear (UI 反映を待たず)
                if state.active_lane_address.as_deref() == Some(address) {
                    state.active_lane_address = None;
                    out.changed = true;
                    out.active_changed = true;
                }
            }
        }
        "lane:restart" => {
            // sidebar の restart icon → confirm dialog OK の連鎖。 caller が SP port を
            // 解決して `client.restart_lane` を呼ぶ。 active Lane を restart した場合は
            // WS が onclose → reconnect で新 PtySlot に attach し直す (PR #218)。
            let address = parsed.get("address").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() && !address.is_empty() {
                out.restart_lane_request = Some((path.to_string(), address.to_string()));
            }
        }
        "lane:add_worker" => {
            // Phase 3-A: sidebar から Worker Lane 作成要求。 caller (event loop) で
            // 該当 project の SP port を解決して client.create_worker_lane を呼ぶ。
            let name = parsed
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let branch = parsed
                .get("branch")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            if !path.is_empty() && !name.is_empty() {
                out.add_worker_request = Some((path.to_string(), name, branch));
            }
        }
        "stand:select" => {
            // Phase 5-A: Project-scope Stand row click → main area に対応 pane を表示
            // (Lane と mutually exclusive、 active_lane_address は preemptively clear)
            let kind = parsed.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if path.is_empty() || kind.is_empty() {
                tracing::warn!("stand:select with empty path/kind: {}", msg);
                return out;
            }
            let new_stand = ActiveStand {
                project_path: path.to_string(),
                kind: kind.to_string(),
            };
            // 既に同じ Stand が active なら no-op
            if state.active_stand.as_ref() == Some(&new_stand) {
                return out;
            }
            tracing::info!("stand:select project={} kind={}", path, kind);
            state.active_stand = Some(new_stand);
            // Lane を排他で clear (= main area の active 軸を Stand に切替)
            if state.active_lane_address.is_some() {
                state.active_lane_address = None;
            }
            out.changed = true;
            out.active_changed = true;
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
                // session 永続化: vp-app 再起動時に直前 active Lane を復元
                session.active_lane_address = Some(address.to_string());
                session.save();
            }
            // Phase 5-A: Lane と Stand は排他なので active_stand を clear
            if state.active_stand.is_some() {
                state.active_stand = None;
                out.changed = true;
                out.active_changed = true;
            }
        }
        "process:reorder" => {
            // Currents セクションを drag-and-drop で並び替えた時の通知。
            // payload: `{"t":"process:reorder","order":["/path/a","/path/b",...]}`。
            // session_state に保存し、 次回起動時 + 現在の sidebar push に反映。
            let Some(arr) = parsed.get("order").and_then(|v| v.as_array()) else {
                tracing::warn!("process:reorder: order array が無い: {}", msg);
                return out;
            };
            let order: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            tracing::info!("process:reorder: {} entries", order.len());
            session.currents_order = Some(order.clone());
            session.save();
            // SidebarState にも反映 (次回 push で JS 側 sort に使う)
            state.currents_order = Some(order);
            // changed フラグは立てない (DOM 順は user 操作で既に変わっている、
            // re-push で flash するのを避ける)。 次回 push 時に新 order が乗る。
        }
        "process:restart" => {
            // Phase 5-C: project name (from p.path → leaf name) を抽出して async restart に投げる。
            // path は normalized full path、 SP の API は project name で識別する。
            if path.is_empty() {
                tracing::warn!("process:restart with empty path: {}", msg);
                return out;
            }
            let project_name = std::path::Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path)
                .to_string();
            tracing::info!("process:restart {} (project_name={})", path, project_name);
            out.restart_process_request = Some(project_name);
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
    // Phase 5-C: log filter の noise 抑制 (2026-04-28 観測: 23MB log の 70% が hyper_util::pool、
    //   25% が vp_app::terminal の PTY I/O event だった)。 vp_app の他モジュールは info で残し、
    //   noise 源を warn まで上げる。 必要なら RUST_LOG 環境変数で override 可。
    //
    // Phase 5-D fix: ユーザ shell の `RUST_LOG=vantage_point=debug` 等が `try_from_default_env` で
    //   default を完全 override してしまい、 hyper_util の debug log が大量に残っていた。
    //   読み込み後に `add_directive` で noise 源を強制 warn 上書きする (same-target は replace)。
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(
                "vp_app=info,vp_app::terminal=warn,vantage_point=info",
            )
        })
        .add_directive("hyper_util=warn".parse().expect("static directive"))
        .add_directive("hyper=warn".parse().expect("static directive"))
        .add_directive("reqwest=warn".parse().expect("static directive"))
        .add_directive("h2=warn".parse().expect("static directive"))
        .add_directive("rustls=warn".parse().expect("static directive"));
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
        // Phase 5-C: vp-asset:// custom protocol で bundled font (FONT_ASSETS) + sidebar.html を配信。
        // serve() に SIDEBAR_ASSETS を渡すと FONT_ASSETS と chain して両方 lookup される。
        // HTML 自体も同 scheme から読むことで page origin = vp-asset:// に統一、 font fetch も同一 origin。
        .with_custom_protocol("vp-asset".to_string(), |id, request| {
            crate::web_assets::serve(id, request, SIDEBAR_ASSETS)
        })
        .with_url("vp-asset://app/sidebar.html")
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

    // Phase 2.x-d: 旧 single-PTY 経路 (`xterm_ready` / `pending` / `PENDING_MAX`) は撤去。
    // per-Lane instance + browser-native WebSocket では各 Lane の xterm.js が独立に
    // WS から bytes を受けるので、 Rust 側で buffer / flush 同期する必要が無い。
    // VP-95: sidebar 全体 state (projects + widget + activity)
    let mut sidebar_state = SidebarState::default();
    // session 永続化: 起動を跨いで復元する UI state (expanded / active_lane / currents_order)
    let mut session_state = SessionState::load();
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
            // Phase 2.x-d: AppEvent::Output / XtermReady は撤去済 (per-Lane browser native WS へ移行)。
            // 関連の `xterm_ready` / `pending` / `PENDING_MAX` も一括削除。
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
                    if let Err(e) = main_view.evaluate_script(&script) {
                        tracing::warn!("paste deliver script failed: {}", e);
                    }
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
                            // 新規 project の expanded 解決:
                            //   1. session_state に saved 値があれば最優先 (vp-app 再起動の復元)
                            //   2. 上記 None かつ session 中の追加 (= 初回 fetch ではない) なら auto-expand
                            //   3. 初回 fetch の新規は閉じた状態
                            let mut s = ProcessPaneState::new(p.path.clone(), p.name.clone());
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
                // Phase A4-3b: 各 project の SP に対して /api/lanes を fetch
                // (memory mem_1CaSugEk1W2vr5TAdfDn5D: Lane scope は SP per project の所有)
                for (path, port) in &project_ports {
                    if let Some(sp_port) = port {
                        spawn_lanes_fetch(async_action_proxy.clone(), path.clone(), *sp_port);
                    }
                }
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
                            .any(|l| &lane_address_key(&l.address) == *saved)
                    })
                    .cloned();
                let auto_select = !is_secondary
                    && sidebar_state.active_lane_address.is_none()
                    && session_match.is_none()
                    && lanes
                        .first()
                        .map(|l| lane_address_key(&l.address))
                        .is_some();
                let first_addr = if let Some(saved) = session_match {
                    // session 復元: 1 度限り、 復元済 marker として pending を消費
                    pending_session_active_lane = None;
                    tracing::info!("session 復元: active_lane = {}", saved);
                    Some(saved)
                } else if auto_select {
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
                    push_active_view(&main_view, &sidebar_state);
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
            Event::UserEvent(AppEvent::ClonePathPicked(path)) => {
                // user キャンセル時 (None) は JS 状態を変更しない (= 既存 override を保持)
                if let Some(p) = path {
                    let js_arg = serde_json::to_string(&p).unwrap_or_else(|_| "null".into());
                    let script =
                        format!("window.setClonePath && window.setClonePath({})", js_arg);
                    if let Err(e) = sidebar.evaluate_script(&script) {
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
                if outcome.changed {
                    push_sidebar_state(&sidebar, &sidebar_state);
                }
                if outcome.active_changed {
                    push_active_view(&main_view, &sidebar_state);
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
                // Phase 5-C: Process restart 要求 (sidebar の 🔄 button から)
                // Phase 5-D fix: bare `tokio::spawn` は wry main thread (= tokio runtime context 無)
                //   から呼ぶと panic 即死。 他の async handler と同じく
                //   `thread::Builder::spawn + Builder::new_current_thread + rt.block_on` にする。
                if let Some(project_name) = outcome.restart_process_request {
                    let proxy = async_action_proxy.clone();
                    let project_name_clone = project_name.clone();
                    thread::Builder::new()
                        .name(format!("restart-{}", project_name))
                        .spawn(move || {
                            let rt = match tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                            {
                                Ok(rt) => rt,
                                Err(e) => {
                                    tracing::warn!("restart_process tokio runtime: {}", e);
                                    return;
                                }
                            };
                            rt.block_on(async move {
                                // TheWorld port は固定 32000 (vantage_point::cli::WORLD_PORT と同期)
                                let client = crate::client::TheWorldClient::new(32000);
                                match client.restart_process(&project_name_clone).await {
                                    Ok(()) => {
                                        tracing::info!(
                                            "restart_process OK: {}",
                                            project_name_clone
                                        );
                                        // 完了 → projects 再 fetch → sidebar state badge 更新
                                        if let Ok(projects) = client.list_projects().await {
                                            let _ = proxy
                                                .send_event(AppEvent::ProcessesLoaded(projects));
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "restart_process failed for {}: {}",
                                            project_name_clone,
                                            e
                                        );
                                    }
                                }
                            });
                        })
                        .ok();
                }
                // Phase 4-A: Worker Lane 削除要求 (sidebar の × button から)
                if let Some((project_path, address)) = outcome.delete_lane_request {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| p.path == project_path)
                        .and_then(|p| p.port);
                    if let Some(port) = sp_port {
                        // JS-side からも先 removeLane を呼ぶ (= xterm + WS 即時 dispose、
                        // server side は polling で sidebar から消える前にこちらが先)
                        lane_js::remove_lane(&main_view, &address);
                        let proxy = async_action_proxy.clone();
                        let path_clone = project_path.clone();
                        let addr_clone = address.clone();
                        thread::Builder::new()
                            .name(format!("delete-lane-{}", address))
                            .spawn(move || {
                                let rt =
                                    match tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                    {
                                        Ok(rt) => rt,
                                        Err(e) => {
                                            tracing::warn!("delete-lane runtime: {}", e);
                                            return;
                                        }
                                    };
                                rt.block_on(async {
                                    let client = TheWorldClient::new(port);
                                    match client.delete_lane(&addr_clone).await {
                                        Ok(()) => {
                                            tracing::info!(
                                                "Lane deleted: project={} address={}",
                                                path_clone,
                                                addr_clone
                                            );
                                            spawn_lanes_fetch(
                                                proxy.clone(),
                                                path_clone,
                                                port,
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "delete_lane failed: project={} address={}: {}",
                                                path_clone,
                                                addr_clone,
                                                e
                                            );
                                        }
                                    }
                                });
                            })
                            .ok();
                    } else {
                        tracing::warn!(
                            "lane:delete: SP port unknown for path={} (skip)",
                            project_path
                        );
                    }
                }
                // Lane Lead Stand restart 要求 (sidebar の restart icon → confirm dialog から)
                if let Some((project_path, address)) = outcome.restart_lane_request {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| p.path == project_path)
                        .and_then(|p| p.port);
                    if let Some(port) = sp_port {
                        let proxy = async_action_proxy.clone();
                        let path_clone = project_path.clone();
                        let addr_clone = address.clone();
                        thread::Builder::new()
                            .name(format!("restart-lane-{}", address))
                            .spawn(move || {
                                let rt = match tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                {
                                    Ok(rt) => rt,
                                    Err(e) => {
                                        tracing::warn!("restart-lane runtime: {}", e);
                                        return;
                                    }
                                };
                                rt.block_on(async {
                                    let client = TheWorldClient::new(port);
                                    match client.restart_lane(&addr_clone).await {
                                        Ok(()) => {
                                            tracing::info!(
                                                "Lane restarted: project={} address={}",
                                                path_clone,
                                                addr_clone
                                            );
                                            // 再 fetch で新 pid / state を sidebar に反映。
                                            // WS は PR #218 の auto-reconnect で透過的に新 PtySlot に attach し直す。
                                            spawn_lanes_fetch(proxy.clone(), path_clone, port);
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "restart_lane failed: project={} address={}: {}",
                                                path_clone,
                                                addr_clone,
                                                e
                                            );
                                        }
                                    }
                                });
                            })
                            .ok();
                    } else {
                        tracing::warn!(
                            "lane:restart: SP port unknown for path={} (skip)",
                            project_path
                        );
                    }
                }
                // Phase 3-A: Worker Lane 作成要求 (sidebar の + Add Worker から)
                if let Some((project_path, name, branch)) = outcome.add_worker_request {
                    let sp_port = sidebar_state
                        .processes
                        .iter()
                        .find(|p| p.path == project_path)
                        .and_then(|p| p.port);
                    if let Some(port) = sp_port {
                        let proxy = async_action_proxy.clone();
                        let name_clone = name.clone();
                        let branch_clone = branch.clone();
                        let path_clone = project_path.clone();
                        thread::Builder::new()
                            .name(format!("create-worker-{}", name))
                            .spawn(move || {
                                let rt =
                                    match tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                    {
                                        Ok(rt) => rt,
                                        Err(e) => {
                                            tracing::warn!(
                                                "create-worker tokio runtime: {}",
                                                e
                                            );
                                            return;
                                        }
                                    };
                                rt.block_on(async {
                                    let client = TheWorldClient::new(port);
                                    match client
                                        .create_worker_lane(
                                            &name_clone,
                                            branch_clone.as_deref(),
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            tracing::info!(
                                                "Worker Lane created: project={} name={} branch={:?}",
                                                path_clone,
                                                name_clone,
                                                branch_clone
                                            );
                                            // 即座に lanes を re-fetch して sidebar 反映
                                            spawn_lanes_fetch(
                                                proxy.clone(),
                                                path_clone,
                                                port,
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "create_worker_lane failed: project={} name={}: {}",
                                                path_clone,
                                                name_clone,
                                                e
                                            );
                                        }
                                    }
                                });
                            })
                            .ok();
                    } else {
                        tracing::warn!(
                            "lane:add_worker: SP port unknown for path={} (skip)",
                            project_path
                        );
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
                if id == menu_ids.new_window {
                    // Cmd+N: 新規 vp-app process を spawn = 新しい MainWindow が独立 process で立つ。
                    // 同 EventLoop に重ねるのではなく fork-style で別 process 化することで、
                    // state 干渉ゼロ + crash isolation + multi-instance 並行開発が可能に。
                    // TheWorld daemon (port 32000) は process 横断 shared なので projects 一覧は同期。
                    match std::env::current_exe() {
                        Ok(exe) => {
                            match std::process::Command::new(&exe)
                                // 子 process は auto-select を skip ── 元 vp-app と active_lane
                                // が衝突して両方の terminal WS が壊れるのを防ぐ。
                                // 起動後 user が手動で lane 選択するまで main_area は empty。
                                .env("VP_APP_SECONDARY", "1")
                                .spawn()
                            {
                                Ok(child) => {
                                    tracing::info!(
                                        "Cmd+N: spawned new vp-app process (pid={})",
                                        child.id()
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Cmd+N: failed to spawn new process at {}: {}",
                                        exe.display(),
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Cmd+N: current_exe() failed: {}", e);
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

#[cfg(test)]
mod sidebar_html_tests {
    //! Phase 5-C: SIDEBAR_HTML の組み立て構造検証。
    //! Bundle font / serve handler のテストは `web_assets` module 側に分離。
    use super::*;

    /// HTML サイズが WKWebView の loadHTMLString 安全範囲 (< 200KB) に収まる
    #[test]
    fn html_size_under_wkwebview_limit() {
        let size = SIDEBAR_HTML.len();
        assert!(
            size < 200_000,
            "SIDEBAR_HTML size {} bytes exceeds WKWebView safe range — \
             check that no font binary got embedded in HTML",
            size
        );
    }

    /// HTML 内に旧 placeholder / data URL / @font-face declaration が残っていない
    #[test]
    fn no_legacy_artifacts() {
        assert!(
            !SIDEBAR_HTML.contains("__VP_SYMBOLS_FONT_B64__"),
            "legacy Base64 placeholder still in HTML"
        );
        assert!(
            !SIDEBAR_HTML.contains("data:font/ttf;base64,"),
            "legacy data URL still in HTML"
        );
        assert!(
            !SIDEBAR_HTML.contains("@font-face {"),
            "legacy CSS @font-face declaration still present (Plan A: JS FontFace API only)"
        );
    }

    /// NERD_FONT_LOADER_JS 主要要素が SIDEBAR_HTML に embed されている
    #[test]
    fn nerd_font_loader_embedded() {
        assert!(
            SIDEBAR_HTML.contains("new FontFace("),
            "FontFace constructor not found"
        );
        assert!(
            SIDEBAR_HTML.contains("vp-asset://font/"),
            "vp-asset:// font URL pattern not found"
        );
        assert!(
            SIDEBAR_HTML.contains("document.fonts.add"),
            "document.fonts.add not found"
        );
        assert!(
            SIDEBAR_HTML.contains("VPMono"),
            "VPMono family name not found"
        );
    }

    /// .nf-icon CSS rule が VPMono direct 宣言を使ってる (var() indirection 経由ではない)
    #[test]
    fn nf_icon_uses_vpmono_direct() {
        assert!(
            SIDEBAR_HTML.contains(".nf-icon{font-family:'VPMono'"),
            ".nf-icon CSS rule does not use 'VPMono' direct font-family"
        );
    }

    /// SIDEBAR_ASSETS で sidebar.html が `web_assets::lookup_asset` 経由で取れる
    #[test]
    fn sidebar_html_servable_via_vp_asset() {
        let r = crate::web_assets::lookup_asset("vp-asset://app/sidebar.html", SIDEBAR_ASSETS);
        assert!(r.is_some(), "sidebar.html not lookupable via vp-asset://");
        let (bytes, ct) = r.unwrap();
        assert_eq!(ct, "text/html; charset=utf-8");
        assert_eq!(bytes, SIDEBAR_HTML.as_bytes());
    }
}
