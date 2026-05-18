/**
 * sidebar の shell layout component。
 *
 * v1.0 柱 2。 3 段 layout (header / scrollable list / World widget) の骨格と、
 * Project を「稼働中 / 一時停止中」 の 2 セクションに分けた表示を担う。
 *
 * - PR-1: shell layout + Solid store の最小可視化。
 * - PR-2: 稼働中 / 一時停止中 の 2 セクション分割 (本 component の現状)。
 *   実描画の残り — Project accordion・Lane ツリー (glyph/status/awaiting dot/
 *   mailbox icon/worker git meta)・World widget 本体 — は後続で旧 SIDEBAR_HTML
 *   から移植する。
 */
import { For, Show, createMemo } from 'solid-js'
import type { ProcessPaneState } from '../generated/ProcessPaneState'
import { sidebar } from './store'
import { isRunningProcess } from './classify'

/** 1 セクション (稼働中 or 一時停止中) を見出し + Project 行で描画する。 */
function ProcSection(props: { label: string; procs: ProcessPaneState[] }) {
  return (
    <Show when={props.procs.length > 0}>
      <section class="vp-sidebar-section">
        <div class="vp-sidebar-section-header">
          <span class="vp-sidebar-section-label">{props.label}</span>
          <span class="vp-sidebar-section-count">{props.procs.length}</span>
        </div>
        <For each={props.procs}>
          {(proc) => (
            <div class="vp-sidebar-proc">
              <span class="vp-sidebar-proc-name">{proc.name}</span>
              <Show when={proc.state}>
                <span class="vp-sidebar-proc-state">{proc.state}</span>
              </Show>
            </div>
          )}
        </For>
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
 * component CSS の本格利用は後続 PR。
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
.vp-sidebar-section + .vp-sidebar-section{margin-top:var(--spacing-xs,4px);}
.vp-sidebar-section-header{display:flex;align-items:center;gap:6px;
  padding:var(--spacing-sm,8px) var(--spacing-sm,8px) var(--spacing-xs,4px);
  font-size:10px;color:var(--color-text-tertiary);font-weight:500;user-select:none;}
.vp-sidebar-section-count{margin-left:auto;font-variant-numeric:tabular-nums;}
.vp-sidebar-proc{display:flex;align-items:center;gap:6px;
  padding:5px var(--spacing-sm,8px);}
.vp-sidebar-proc-name{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-sidebar-proc-state{font-size:10px;color:var(--color-text-tertiary);
  text-transform:uppercase;letter-spacing:0.04em;}
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
