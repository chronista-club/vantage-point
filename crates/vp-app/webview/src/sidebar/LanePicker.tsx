/**
 * Lane / project switcher picker overlay (= 規約 v0.4 §C.3 `s` directive)。
 *
 * `Cmd hold s` で発火、 sidebar 全面を覆う overlay に **全 lane + project header の flat list**
 * を表示する。 fuzzy 検索 + ↑↓ で navigate、 Enter で:
 * - lane を選択 → `lane:select` IPC で main area を当該 Lane に切替
 * - project header を選択 → `process:toggle` IPC で accordion を expand
 *
 * FileExplorer.tsx の構造を base に reduce: tree expand なし、 単純 flat list + fuzzy。
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from 'solid-js'
import { sidebar } from './store'
import { sendIpc } from './ipc'
import { laneAddressKey, laneLabel } from './lane'

interface PickerEntry {
  kind: 'lane' | 'project'
  /** lane なら `LaneAddressWire.key()` (例: `vantage-point/conductor`)、 project なら path */
  id: string
  /** 表示 label (`project / Lane label` 等) */
  label: string
  /** sendIpc 用 project path */
  path: string
  /** lane の時のみ存在 (= sendIpc address) */
  address?: string
}

declare global {
  interface Window {
    vpLanePicker?: { open(): void }
  }
}

// module-scope singleton state
const [visible, setVisible] = createSignal(false)
const [query, setQuery] = createSignal('')
const [entries, setEntries] = createSignal<PickerEntry[]>([])
const [selectedIndex, setSelectedIndex] = createSignal(0)

function buildEntries(): PickerEntry[] {
  const out: PickerEntry[] = []
  const map = sidebar.lanes_by_project ?? {}
  for (const proc of sidebar.processes) {
    const projectName = proc.path.split('/').pop() ?? proc.path
    out.push({
      kind: 'project',
      id: proc.path,
      label: `${projectName}`,
      path: proc.path,
    })
    const lanes = map[proc.path] ?? []
    for (const lane of lanes) {
      const addr = laneAddressKey(lane)
      out.push({
        kind: 'lane',
        id: addr,
        label: `${projectName} / ${laneLabel(lane)}`,
        path: proc.path,
        address: addr,
      })
    }
  }
  return out
}

function open(): void {
  setEntries(buildEntries())
  setQuery('')
  setSelectedIndex(0)
  setVisible(true)
}

function dismiss(): void {
  setVisible(false)
}

/**
 * Fuzzy matcher (= FileExplorer.tsx と同じ簡易 score)。 query の各文字を順序保持で部分マッチ、
 * basename hit / 連続マッチ / 境界 hit にボーナス。 全文字マッチしなければ `null`。
 */
function fuzzyScore(q: string, target: string): number | null {
  if (!q) return 0
  const ql = q.toLowerCase()
  const tl = target.toLowerCase()
  const basenameStart = tl.lastIndexOf('/') + 1
  let qi = 0
  let score = 0
  let prev = -2
  for (let i = 0; i < tl.length && qi < ql.length; i++) {
    if (tl[i] === ql[qi]) {
      score += 10
      if (i === prev + 1) score += 8
      if (i >= basenameStart) score += 5
      const before = i === 0 ? '/' : tl[i - 1]
      if (before === '/' || before === '-' || before === '_' || before === '.') score += 3
      prev = i
      qi++
    }
  }
  return qi === ql.length ? score : null
}

function filterEntries(all: PickerEntry[], q: string): PickerEntry[] {
  if (!q) return all
  const scored: { entry: PickerEntry; score: number }[] = []
  for (const entry of all) {
    const s = fuzzyScore(q, entry.label)
    if (s !== null) scored.push({ entry, score: s })
  }
  scored.sort((a, b) => b.score - a.score)
  return scored.map(({ entry }) => entry)
}

export function LanePicker() {
  const view = createMemo(() => filterEntries(entries(), query()))

  let inputRef: HTMLInputElement | undefined
  let bodyRef: HTMLDivElement | undefined

  onMount(() => {
    window.vpLanePicker = { open }
  })
  onCleanup(() => {
    if (window.vpLanePicker?.open === open) window.vpLanePicker = undefined
  })

  // visible になったら input に focus
  createEffect(() => {
    if (!visible()) return
    setTimeout(() => inputRef?.focus(), 0)
  })

  // selectedIndex 変化に追従して selected row を viewport に
  createEffect(() => {
    selectedIndex()
    if (!visible() || !bodyRef) return
    const sel = bodyRef.querySelector('.vp-lp-row.selected') as HTMLElement | null
    sel?.scrollIntoView({ block: 'nearest' })
  })

  const activate = (entry: PickerEntry) => {
    if (entry.kind === 'lane' && entry.address) {
      sendIpc({ t: 'lane:select', path: entry.path, address: entry.address })
    } else {
      // project header → accordion expand (process:toggle で SP auto-spawn も連動)
      sendIpc({ t: 'process:toggle', path: entry.path, expanded: true })
    }
    dismiss()
  }

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault()
      dismiss()
      return
    }
    const items = view()
    if (items.length === 0) return
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setSelectedIndex((idx) => Math.min(idx + 1, items.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setSelectedIndex((idx) => Math.max(idx - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      const item = items[selectedIndex()]
      if (item) activate(item)
    }
  }

  return (
    <Show when={visible()}>
      <div class="vp-lane-picker" onKeyDown={onKeyDown}>
        <div class="vp-lp-backdrop" onClick={dismiss} />
        <div class="vp-lp-panel" onClick={(e) => e.stopPropagation()}>
          <div class="vp-lp-header">
            <input
              ref={inputRef}
              class="vp-lp-search"
              type="text"
              placeholder="Lane / project を検索..."
              value={query()}
              onInput={(e) => {
                setQuery(e.currentTarget.value)
                setSelectedIndex(0)
              }}
            />
            <button
              class="vp-lp-close"
              onClick={dismiss}
              title="閉じる (Esc)"
              type="button"
            >
              ✕
            </button>
          </div>
          <div class="vp-lp-body" ref={bodyRef}>
            <Show when={view().length === 0}>
              <div class="vp-lp-empty">{query() ? 'マッチなし' : 'lane なし'}</div>
            </Show>
            <For each={view()}>
              {(entry, idx) => (
                <div
                  class="vp-lp-row"
                  classList={{
                    selected: idx() === selectedIndex(),
                    lane: entry.kind === 'lane',
                    project: entry.kind === 'project',
                  }}
                  onClick={() => {
                    setSelectedIndex(idx())
                    activate(entry)
                  }}
                >
                  <span class="vp-lp-icon">{entry.kind === 'project' ? '📁' : '◉'}</span>
                  <span class="vp-lp-label">{entry.label}</span>
                </div>
              )}
            </For>
          </div>
          <div class="vp-lp-footer">↑↓ 移動 · Enter 開く · Esc 閉じる</div>
        </div>
      </div>
    </Show>
  )
}

/** Shell.tsx の SHELL_CSS に concat する CSS。 FileExplorer の class prefix と分離 (= vp-lp-)。 */
export const LANE_PICKER_CSS = `
.vp-lane-picker{position:absolute;inset:0;z-index:10000;display:flex;}
.vp-lp-backdrop{position:absolute;inset:0;background:rgba(0,0,0,.45);}
.vp-lp-panel{position:relative;display:flex;flex-direction:column;width:100%;
  background:var(--color-surface-bg-base);
  border-right:1px solid var(--color-surface-border,#1f2233);}
.vp-lp-header{flex:0 0 auto;display:flex;gap:6px;padding:8px;
  border-bottom:1px solid var(--color-surface-border,#1f2233);}
.vp-lp-search{flex:1;padding:5px 8px;
  border:1px solid var(--color-surface-border,#1f2233);
  background:var(--color-surface-bg-subtle);color:var(--color-text-primary);
  border-radius:var(--radius-sm,6px);font-family:inherit;font-size:var(--sb-text-hint,12px);}
.vp-lp-search:focus{outline:none;border-color:var(--color-brand-primary);}
.vp-lp-close{border:none;background:transparent;color:var(--color-text-tertiary);
  font-size:14px;cursor:pointer;padding:0 6px;border-radius:3px;}
.vp-lp-close:hover{background:var(--color-surface-bg-emphasis);
  color:var(--color-text-primary);}
.vp-lp-body{flex:1;overflow-y:auto;padding:4px 0;
  font-family:'VPMono',monospace;font-size:var(--sb-text-meta,11px);}
.vp-lp-empty{padding:12px;text-align:center;color:var(--color-text-tertiary);}
.vp-lp-row{display:flex;align-items:center;gap:6px;padding:5px 12px;cursor:pointer;
  white-space:nowrap;overflow:hidden;text-overflow:ellipsis;}
.vp-lp-row:hover{background:var(--color-surface-bg-emphasis);}
.vp-lp-row.selected{background:var(--color-brand-primary-subtle);
  color:var(--color-brand-primary);}
.vp-lp-row.project{font-weight:500;color:var(--color-text-secondary);}
.vp-lp-icon{display:inline-block;width:14px;text-align:center;flex:0 0 auto;
  font-size:var(--sb-text-micro,10px);color:var(--color-text-tertiary);}
.vp-lp-row.selected .vp-lp-icon{color:var(--color-brand-primary);}
.vp-lp-label{overflow:hidden;text-overflow:ellipsis;}
.vp-lp-footer{flex:0 0 auto;padding:4px 8px;font-size:var(--sb-text-micro,10px);text-align:center;
  color:var(--color-text-tertiary);
  border-top:1px solid var(--color-surface-border,#1f2233);}

/* PR 445 \`d\` directive — 2-click delete confirm hint bar (sidebar 下端、 absolute) */
.vp-delete-hint{position:absolute;bottom:0;left:0;right:0;z-index:9000;
  display:flex;align-items:center;gap:8px;padding:6px 10px;
  background:var(--color-status-warning-subtle,#3a2c14);
  color:var(--color-status-warning,#d49b3f);
  border-top:1px solid var(--color-status-warning,#d49b3f);
  font-size:var(--sb-text-meta,11px);}
.vp-delete-hint-icon{flex:0 0 auto;}
.vp-delete-hint-label{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}

/* PR 447 \`l\` directive — lane number switcher mode hint bar (sidebar 下端、 \`d\` hint と排他想定) */
.vp-lane-select-hint{position:absolute;bottom:0;left:0;right:0;z-index:9100;
  display:flex;align-items:center;gap:8px;padding:6px 10px;
  background:var(--color-brand-primary-subtle,#1c2436);
  color:var(--color-brand-primary,#3fb9d4);
  border-top:1px solid var(--color-brand-primary,#3fb9d4);
  font-size:var(--sb-text-meta,11px);font-variant-numeric:tabular-nums;}
.vp-lane-select-hint-icon{flex:0 0 auto;}
.vp-lane-select-hint-label{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}
.vp-lane-select-hint-help{flex:0 0 auto;opacity:0.7;font-size:var(--sb-text-micro,10px);}
`
