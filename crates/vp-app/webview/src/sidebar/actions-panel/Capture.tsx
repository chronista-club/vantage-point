import { For, createEffect, createSignal, untrack } from 'solid-js'
import { actions, commitActions, newActionId } from './store'
import { orderBetween } from './model'
import type { ActionAtlas } from '../../generated/ActionAtlas'

// App-wide draft. Project selection and sidebar mounting never choose its destination.
const [draft, setDraft] = createSignal('')
const [selectedAtlas, setSelectedAtlas] = createSignal('')

const drafts = new Map<string, {text: string; atlas: string}>()
let activeScope = ''

export function Capture(props: { atlases: ActionAtlas[]; scope: string }) {
  createEffect(() => {
    const scope = props.scope
    untrack(() => {
      if (scope === activeScope) return
      drafts.set(activeScope, {text: draft(), atlas: selectedAtlas()})
      activeScope = scope
      const previous = drafts.get(scope)
      setDraft(previous?.text ?? '')
      setSelectedAtlas(previous?.atlas ?? '')
    })
  })
  const destination = () => props.atlases.find(a => a.id === selectedAtlas() && a.writable)
  const canSave = () => !!draft().trim() && !!destination()
  const save = () => {
    if (!canSave()) return
    const id = newActionId()
    const last = [...actions()].sort((a,b) => a.order.localeCompare(b.order)).at(-1)
    commitActions([...actions(), {
      id, client_id: id, text: draft(), atlas_id: destination()!.id,
      kind: null, bucket: 'ideas', order: orderBetween(last?.order ?? null, null),
    }])
    // The submitted row remains visible until the daemon returns its native receipt.
    setDraft('')
  }
  return <div class="vp-act-capture">
    <textarea aria-label="メモ" placeholder="思いついたことをメモ" value={draft()}
      onInput={event => setDraft(event.currentTarget.value)} rows={2} />
    <div class="vp-act-capture-controls">
      <select aria-label="保存先 Atlas" value={selectedAtlas()}
        onChange={event => setSelectedAtlas(event.currentTarget.value)}>
        <option value="">Atlas を選択</option>
        <For each={props.atlases.filter(a => a.writable)}>{atlas =>
          <option value={atlas.id}>{atlas.path || atlas.name}</option>
        }</For>
      </select>
      <button type="button" aria-label="メモを保存" disabled={!canSave()} onClick={save}>保存</button>
    </div>
  </div>
}
