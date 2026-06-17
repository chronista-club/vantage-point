/**
 * Performer Lane 作成フォーム。
 *
 * v1.0 柱 2 PR-3。 開閉 state は ProjectAccordion が持ち、 Project summary 右上の
 * 「+」アイコンボタンで toggle する。 本 component は **フォーム本体のみ** — name +
 * optional branch input → 作成で `lane:add_performer` IPC を送る。
 *
 * follow-up: stand dropdown (`stands:fetch` 連動) と inline error 表示
 * (`handleAddPerformerResult` の wire) は後続。 現状 stand は SP-side default。
 */
import { createSignal } from 'solid-js'
import { sendIpc } from './ipc'

export function AddPerformer(props: { projectPath: string; onClose: () => void }) {
  const [name, setName] = createSignal('')
  const [branch, setBranch] = createSignal('')

  const submit = () => {
    const n = name().trim()
    if (!n) return
    const b = branch().trim()
    sendIpc({ t: 'lane:add_performer', path: props.projectPath, name: n, branch: b || undefined })
    props.onClose()
  }

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Enter') submit()
    else if (e.key === 'Escape') props.onClose()
  }

  return (
    <div class="vp-add-performer-form">
      <input
        class="vp-add-performer-input"
        placeholder="performer name"
        ref={(el) => queueMicrotask(() => el.focus())}
        onInput={(e) => setName(e.currentTarget.value)}
        onKeyDown={onKey}
      />
      <input
        class="vp-add-performer-input"
        placeholder="branch (optional)"
        onInput={(e) => setBranch(e.currentTarget.value)}
        onKeyDown={onKey}
      />
      <div class="vp-add-performer-actions">
        <button onClick={props.onClose}>キャンセル</button>
        <button class="primary" onClick={submit}>
          作成
        </button>
      </div>
    </div>
  )
}
