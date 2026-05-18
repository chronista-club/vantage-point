/**
 * Project (= Runtime Process) 1 件を accordion で描画する component。
 *
 * v1.0 柱 2。 旧 SIDEBAR_HTML の `renderProjectAccordion` を SolidJS に port。
 * native `<details>` で expand/collapse、 開閉時に `process:toggle` IPC を送って
 * Rust 側 state に永続化する。 展開時の内容は SP state に応じた hint、 または Lane 行。
 *
 * PR-3: active project (= 現在 active な Lane を含む project) の summary 右上に
 * 「+」アイコンを出し、 click で Add Wing フォームを開閉する。
 */
import { For, Show, createSignal } from 'solid-js'
import { CreoIcon } from 'creoui-icons-web'
import type { ProcessPaneState } from '../generated/ProcessPaneState'
import { sidebar } from './store'
import { sendIpc } from './ipc'
import { laneAddressKey } from './lane'
import { LaneRow } from './LaneRow'
import { AddWing } from './AddWing'

/**
 * SP の state に応じた hint 文字列。 `null` を返したら Lane 行を描画する。
 * 旧 SIDEBAR_HTML のロジックを踏襲 — SP 未起動/過渡/error は spinner で永久ロード
 * 表示にならないよう、 state 別に明示的な hint を返す。
 */
function hintFor(proc: ProcessPaneState, laneCount: number): string | null {
  const s = proc.state
  if (!s || s === 'stopped') {
    return proc.expanded ? '⏳ SP starting…' : '💤 SP stopped — open to spawn'
  }
  if (s === 'starting') return '⏳ SP starting…'
  if (s === 'stopping') return '⏳ SP stopping…'
  if (s === 'error') return '⚠️ SP error — restart で復帰'
  if (laneCount === 0) return '📡 loading lanes…'
  return null
}

export function ProjectAccordion(props: { proc: ProcessPaneState }) {
  const lanes = () => sidebar.lanes_by_project[props.proc.path] ?? []
  const hint = () => hintFor(props.proc, lanes().length)
  // active project = 現在 active な Lane を含む project。 Add Wing の「+」はこの時だけ出す。
  const isActiveProject = () => {
    const a = sidebar.active_lane_address
    return a != null && lanes().some((l) => laneAddressKey(l) === a)
  }

  const [addWingOpen, setAddWingOpen] = createSignal(false)

  // native toggle → process:toggle IPC。 store 由来の open 反映で発火した場合は
  // 値が一致するので IPC を送らない (echo loop 防止)。
  const onToggle = (e: Event & { currentTarget: HTMLDetailsElement }) => {
    const open = e.currentTarget.open
    if (open !== props.proc.expanded) {
      sendIpc({ t: 'process:toggle', path: props.proc.path, expanded: open })
    }
  }

  return (
    <details class="vp-proj" open={props.proc.expanded} onToggle={onToggle}>
      <summary class="vp-proj-summary">
        <CreoIcon name={props.proc.expanded ? 'ph:folder-open' : 'ph:folder'} size={14} />
        <span class="vp-proj-name">{props.proc.name}</span>
        <Show when={isActiveProject()}>
          <button
            class="vp-proj-addwing"
            classList={{ open: addWingOpen() }}
            title="Add Wing"
            onClick={(e) => {
              // summary click は <details> を toggle するので止める。
              e.preventDefault()
              e.stopPropagation()
              setAddWingOpen((v) => !v)
            }}
          >
            <CreoIcon name="ph:plus" size={12} />
          </button>
        </Show>
      </summary>
      <div class="vp-proj-content">
        <Show
          when={hint()}
          fallback={
            <>
              <For each={lanes()}>
                {(lane) => <LaneRow lane={lane} projectPath={props.proc.path} />}
              </For>
              <Show when={addWingOpen()}>
                <AddWing
                  projectPath={props.proc.path}
                  onClose={() => setAddWingOpen(false)}
                />
              </Show>
            </>
          }
        >
          <div class="vp-proj-hint">{hint()}</div>
        </Show>
      </div>
    </details>
  )
}
