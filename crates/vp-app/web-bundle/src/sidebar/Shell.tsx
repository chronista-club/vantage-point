/**
 * sidebar の shell layout component。
 *
 * v1.0 柱 2。 3 段 layout (header / scrollable list / World widget) の骨格と、
 * Project を「稼働中 / 一時停止中」 の 2 セクションに分け、 各 Project を accordion +
 * Lane ツリーで描画する。
 *
 * - PR-1: shell layout + Solid store の最小可視化。
 * - PR-2: 稼働中 / 一時停止中 の 2 セクション分割 + Project accordion + Lane ツリー
 *   (stand icon / status / awaiting dot / mailbox icon / worker git meta)。
 *   操作 (click 選択・context menu・restart/delete・Add Worker form・DnD) は PR-3。
 *   World widget 本体は後続 increment。
 */
import { For, Show, createMemo } from 'solid-js'
import type { ProcessPaneState } from '../generated/ProcessPaneState'
import { sidebar } from './store'
import { isRunningProcess } from './classify'
import { ContextMenu } from './ContextMenu'
import { ProjectAccordion } from './ProjectAccordion'
import { WorldWidget } from './WorldWidget'

/** 1 セクション (稼働中 or 一時停止中) を見出し + Project accordion で描画する。 */
function ProcSection(props: { label: string; procs: ProcessPaneState[] }) {
  return (
    <Show when={props.procs.length > 0}>
      <section class="vp-sidebar-section">
        <div class="vp-sidebar-section-header">
          <span class="vp-sidebar-section-label">{props.label}</span>
          <span class="vp-sidebar-section-count">{props.procs.length}</span>
        </div>
        <For each={props.procs}>{(proc) => <ProjectAccordion proc={proc} />}</For>
      </section>
    </Show>
  )
}

export function Shell() {
  // processes を稼働中 / 一時停止中 に分割。 store の processes が変われば再計算される。
  const running = createMemo(() => sidebar.processes.filter(isRunningProcess))
  const paused = createMemo(() => sidebar.processes.filter((p) => !isRunningProcess(p)))

  return (
    <div class="vp-sidebar-shell">
      <header class="vp-sidebar-header">Vantage Point</header>

      <div class="vp-sidebar-list">
        <Show
          when={sidebar.processes.length > 0}
          fallback={<div class="vp-sidebar-empty">プロジェクトなし</div>}
        >
          <ProcSection label="稼働中" procs={running()} />
          <ProcSection label="一時停止中" procs={paused()} />
        </Show>
      </div>

      <WorldWidget />

      {/* 右クリック context menu (Lane 行 / project ヘッダ 共通、 singleton、 VP-204 PR-1)。 */}
      <ContextMenu />
    </div>
  )
}

/**
 * shell layout の CSS。 creoui token (`var(--color-*)` / `var(--spacing-*)`) は
 * SIDEBAR_HTML_V2 が `creo-tokens.css` を inline 済なので、 ここは layout のみ定義する。
 */
export const SHELL_CSS = `
html,body{margin:0;height:100%;background:var(--color-surface-bg-subtle);
  color:var(--color-text-primary);font-family:'VPMono',monospace;font-size:12px;
  line-height:1.4;overflow:hidden;}
.vp-sidebar-shell{display:flex;flex-direction:column;height:100%;}
.vp-sidebar-header{flex:0 0 auto;padding:var(--spacing-sm,8px);font-size:11px;
  font-weight:500;color:var(--color-text-secondary);
  border-bottom:1px solid var(--color-surface-border,#1f2233);user-select:none;}
.vp-sidebar-list{flex:1;overflow-y:auto;padding:var(--spacing-xs,4px) 0;}
.vp-sidebar-empty{padding:var(--spacing-sm,8px);color:var(--color-text-tertiary);
  font-size:11px;}

/* 稼働中 / 一時停止中 セクション */
.vp-sidebar-section + .vp-sidebar-section{margin-top:var(--spacing-xs,4px);}
.vp-sidebar-section-header{display:flex;align-items:center;gap:6px;
  padding:var(--spacing-sm,8px) var(--spacing-sm,8px) var(--spacing-xs,4px);
  font-size:10px;color:var(--color-text-tertiary);font-weight:500;user-select:none;}
.vp-sidebar-section-count{margin-left:auto;font-variant-numeric:tabular-nums;}

/* Project accordion */
.vp-proj{margin:0;}
.vp-proj + .vp-proj{border-top:1px solid var(--color-surface-border,#1f2233);}
.vp-proj-summary{list-style:none;display:flex;align-items:center;gap:6px;
  padding:6px var(--spacing-sm,8px);cursor:pointer;font-size:13px;user-select:none;
  transition:background .1s ease;}
.vp-proj-summary::-webkit-details-marker{display:none;}
.vp-proj-summary:hover{background:var(--color-surface-bg-emphasis);}
.vp-proj-name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-proj-hint{padding:6px 12px 6px 20px;font-size:11px;
  color:var(--color-text-tertiary);font-style:italic;}

/* Lane 行 */
.vp-lane-row{display:flex;flex-wrap:wrap;align-items:center;gap:6px;
  padding:5px var(--spacing-sm,8px) 5px 14px;font-size:12px;cursor:pointer;
  transition:background .1s ease;}
.vp-lane-row:hover{background:var(--color-surface-bg-emphasis);}
.vp-lane-row + .vp-lane-row{border-top:1px solid
  color-mix(in oklch, var(--color-surface-border,#1f2233), transparent 60%);}
.vp-lane-row.active{background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);font-weight:500;
  box-shadow:inset -2px 0 0 0 var(--color-brand-primary);}
.vp-lane-row.inactive{color:color-mix(in oklch, var(--color-text-secondary),
  transparent 45%);font-style:italic;cursor:default;}
.vp-lane-icon{display:inline-flex;width:18px;justify-content:center;}
.vp-lane-row.inactive .vp-lane-icon{opacity:0.55;}
.vp-lane-msg{display:inline-flex;color:var(--color-text-tertiary);opacity:0.55;}
.vp-lane-msg.unread{color:var(--color-brand-primary);opacity:1;}
.vp-lane-label{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-lane-meta{flex:1;display:flex;gap:5px;font-size:10px;font-style:italic;
  color:var(--color-text-tertiary);white-space:nowrap;overflow:hidden;
  text-overflow:ellipsis;margin-left:6px;}
.vp-lane-meta .ahead{color:var(--color-status-info,#3fb9d4);}
.vp-lane-meta .behind{color:var(--color-status-warning,#d49b3f);}
.vp-lane-meta .dirty{color:var(--color-status-warning,#d49b3f);font-weight:500;}
.vp-lane-meta .merged{color:var(--color-status-success,#3fb950);}
.vp-lane-awaiting{margin-left:auto;width:6px;height:6px;border-radius:50%;
  background:var(--color-status-warning,#d49b3f);flex:0 0 auto;}
.vp-lane-line2{flex-basis:100%;padding-left:24px;font-size:10px;
  color:var(--color-text-tertiary);overflow:hidden;text-overflow:ellipsis;
  white-space:nowrap;}
.vp-lane-line2.empty{opacity:0.5;}
.vp-lane-row.active .vp-lane-line2{color:var(--color-brand-primary);opacity:0.7;}

/* Add Wing — active project summary 右上の「+」 + 展開フォーム */
.vp-proj-addwing{margin-left:auto;display:inline-flex;align-items:center;padding:2px;
  border:none;background:transparent;color:var(--color-text-tertiary);cursor:pointer;
  border-radius:3px;flex:0 0 auto;transition:background .12s ease,color .12s ease;}
.vp-proj-addwing:hover,.vp-proj-addwing.open{
  background:var(--color-surface-bg-emphasis);color:var(--color-brand-primary);}
.vp-add-wing-form{display:flex;flex-direction:column;gap:5px;
  padding:4px var(--spacing-sm,8px) 6px 14px;}
.vp-add-wing-input{padding:5px 8px;border:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-base);color:var(--color-text-primary);
  border-radius:var(--radius-sm,6px);font-family:inherit;font-size:11px;
  box-sizing:border-box;}
.vp-add-wing-input:focus{outline:none;border-color:var(--color-brand-primary);}
.vp-add-wing-actions{display:flex;justify-content:flex-end;gap:6px;}
.vp-add-wing-actions button{padding:3px 10px;
  border:1px solid var(--color-surface-border,#1f2233);background:transparent;
  color:var(--color-text-secondary);border-radius:var(--radius-sm,6px);cursor:pointer;
  font-size:10px;font-family:inherit;transition:background .12s ease,color .12s ease;}
.vp-add-wing-actions button:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.vp-add-wing-actions button.primary{background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);border-color:var(--color-brand-primary-subtle);}

/* World widget (sidebar 最下部、 collapsed 1 行 + expanded 詳細の accordion) */
.vp-world{flex:0 0 auto;border-top:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-base);}
.vp-world-summary{list-style:none;display:flex;align-items:center;gap:6px;
  padding:var(--spacing-xs,4px) var(--spacing-sm,8px);cursor:pointer;
  font-size:11px;color:var(--color-text-secondary);user-select:none;}
.vp-world-summary::-webkit-details-marker{display:none;}
.vp-world-summary:hover{background:var(--color-surface-bg-emphasis);}
.vp-world-dot{width:6px;height:6px;border-radius:50%;flex:0 0 auto;
  background:var(--color-status-success,#3fb950);}
.vp-world-dot.offline{background:var(--color-status-error,#d4444c);}
.vp-world-line{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;
  font-variant-numeric:tabular-nums;}
.vp-world-detail{padding:var(--spacing-xs,4px) var(--spacing-sm,8px);
  border-top:1px dashed var(--color-surface-border,#1f2233);}
.vp-world-stat{display:flex;justify-content:space-between;font-size:11px;
  padding:1px 0;}
.vp-world-stat .k{color:var(--color-text-tertiary);}
.vp-world-stat .v{color:var(--color-text-primary);font-weight:500;
  font-variant-numeric:tabular-nums;}

/* Lane 行 右クリック context menu (VP-204 PR-1、 singleton popup) */
.vp-ctx-backdrop{position:fixed;inset:0;z-index:9998;}
.vp-ctx-menu{position:fixed;z-index:9999;min-width:180px;
  background:var(--color-surface-bg-base);
  border:1px solid var(--color-surface-border,#1f2233);
  border-radius:var(--radius-md,6px);box-shadow:0 8px 24px rgba(0,0,0,.4);
  padding:4px 0;font-size:12px;user-select:none;}
.vp-ctx-header{padding:4px 14px 6px;font-size:10px;
  color:var(--color-text-tertiary);
  border-bottom:1px solid var(--color-surface-border,#1f2233);
  margin-bottom:4px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-ctx-item{padding:6px 14px;cursor:pointer;display:flex;align-items:center;
  gap:8px;color:var(--color-text-secondary);
  transition:background .1s ease,color .1s ease;}
.vp-ctx-item:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.vp-ctx-item.danger:hover{background:var(--color-status-error,#d4444c);color:#fff;}
.vp-ctx-item.danger.confirming{background:var(--color-status-error,#d4444c);
  color:#fff;}
`
