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
import { ProjectAccordion } from './ProjectAccordion'

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

      <footer class="vp-sidebar-world">
        <span
          class="vp-sidebar-world-dot"
          classList={{ offline: !sidebar.activity.world_online }}
        />
        <span>World {sidebar.activity.world_online ? 'online' : 'offline'}</span>
        <span class="vp-sidebar-world-count">
          {sidebar.activity.running_process_count} / {sidebar.activity.project_count}
        </span>
      </footer>
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
  padding:5px var(--spacing-sm,8px) 5px 14px;font-size:12px;
  transition:background .1s ease;}
.vp-lane-row + .vp-lane-row{border-top:1px solid
  color-mix(in oklch, var(--color-surface-border,#1f2233), transparent 60%);}
.vp-lane-row.active{background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);font-weight:500;
  box-shadow:inset -2px 0 0 0 var(--color-brand-primary);}
.vp-lane-row.inactive{color:color-mix(in oklch, var(--color-text-secondary),
  transparent 45%);font-style:italic;}
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

/* World widget (PR-1 placeholder、 本体描画は後続 increment) */
.vp-sidebar-world{flex:0 0 auto;display:flex;align-items:center;gap:6px;
  padding:var(--spacing-xs,4px) var(--spacing-sm,8px);font-size:11px;
  color:var(--color-text-secondary);
  border-top:1px solid var(--color-surface-border,#1f2233);}
.vp-sidebar-world-dot{width:6px;height:6px;border-radius:50%;
  background:var(--color-status-success,#3fb950);}
.vp-sidebar-world-dot.offline{background:var(--color-status-error,#d4444c);}
.vp-sidebar-world-count{margin-left:auto;font-variant-numeric:tabular-nums;
  color:var(--color-text-tertiary);}
`
