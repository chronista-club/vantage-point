/**
 * Wing Lane 作成フォーム (Project accordion 内、 Lane ツリーの下に配置)。
 *
 * v1.0 柱 2 PR-3。 旧 SIDEBAR_HTML の `+ Add Worker` inline form を SolidJS に port。
 * 「+ Add Wing」 trigger → 展開で name + optional branch input → 作成で `lane:add_wing`
 * IPC を送る。 作成成功時は Rust が lane を再 fetch するので新 Wing が sidebar に出る。
 *
 * follow-up: stand dropdown (`stands:fetch` 連動) と inline error 表示
 * (`handleAddWorkerResult` の wire) は後続で追加する。 現状 stand は SP-side default。
 */
import { Show, createSignal } from 'solid-js'
import { sendIpc } from './ipc'

export function AddWing(props: { projectPath: string }) {
  const [expanded, setExpanded] = createSignal(false)
  const [name, setName] = createSignal('')
  const [branch, setBranch] = createSignal('')

  const reset = () => {
    setName('')
    setBranch('')
    setExpanded(false)
  }

  const submit = () => {
    const n = name().trim()
    if (!n) return
    const b = branch().trim()
    sendIpc({ t: 'lane:add_wing', path: props.projectPath, name: n, branch: b || undefined })
    reset()
  }

  return (
    <div class="vp-add-wing">
      <Show
        when={expanded()}
        fallback={
          <button class="vp-add-wing-trigger" onClick={() => setExpanded(true)}>
            + Add Wing
          </button>
        }
      >
        <div class="vp-add-wing-form">
          <input
            class="vp-add-wing-input"
            placeholder="wing name"
            value={name()}
            // eslint-disable-next-line solid/reactivity
            ref={(el) => queueMicrotask(() => el.focus())}
            onInput={(e) => setName(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') submit()
              else if (e.key === 'Escape') reset()
            }}
          />
          <input
            class="vp-add-wing-input"
            placeholder="branch (optional)"
            value={branch()}
            onInput={(e) => setBranch(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') submit()
              else if (e.key === 'Escape') reset()
            }}
          />
          <div class="vp-add-wing-actions">
            <button onClick={reset}>キャンセル</button>
            <button class="primary" onClick={submit}>
              作成
            </button>
          </div>
        </div>
      </Show>
    </div>
  )
}
