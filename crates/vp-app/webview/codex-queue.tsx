import { For, Show } from 'solid-js'
import type { CodexQueueView } from './src/generated/CodexQueueView'

export function CodexQueuePanel(props: {
  queue: CodexQueueView
  busy: boolean
  edits: Record<string, string>
  edit(id: string, text: string | undefined): void
  act(action: Record<string, unknown>, text?: string): boolean
}) {
  const disabled = () => props.busy || !props.queue.ready
  const move = (index: number, direction: number) => {
    const ids = props.queue.items.map(item => item.id)
    const to = index + direction
    if (to < 0 || to >= ids.length) return
    ;[ids[index], ids[to]] = [ids[to], ids[index]]
    props.act({ kind: 'reorder', ids })
  }
  return <section class="codex-queue" aria-label="次に実行する入力">
    <Show when={props.queue.items.length > 0}>
      <div class="codex-queue-heading">
        <span>次に実行する · {props.queue.items.length}</span>
        <Show when={!props.queue.turn_id}>
          <button data-queue-start disabled={disabled()} onClick={() => props.act({kind:'start',id:props.queue.items[0].id})}>再開</button>
        </Show>
        <Show when={!props.queue.ready}><span role="status">一覧を更新中</span></Show>
      </div>
      <For each={props.queue.items}>{(item, index) => <div class="codex-queue-row" data-queued-id={item.id}>
        <Show when={props.edits[item.id] !== undefined} fallback={<div class="codex-queue-text">{item.text}</div>}>
          <textarea aria-label="待機入力を編集" value={props.edits[item.id]} disabled={disabled()}
            onInput={e => props.edit(item.id,e.currentTarget.value)} />
        </Show>
        <div class="codex-queue-actions">
          <Show when={props.edits[item.id] !== undefined} fallback={
            <button data-queue-edit disabled={disabled() || !item.editable} onClick={() => props.edit(item.id,item.text)}>編集</button>
          }>
            <button data-queue-save disabled={disabled() || !props.edits[item.id]?.trim()}
              onClick={() => props.act({kind:'update',id:item.id,text:props.edits[item.id]},props.edits[item.id])}>保存</button>
            <button disabled={props.busy} onClick={() => props.edit(item.id,undefined)}>編集を閉じる</button>
          </Show>
          <button data-queue-up aria-label="前へ移動" disabled={disabled() || index()===0} onClick={() => move(index(),-1)}>↑</button>
          <button data-queue-down aria-label="後へ移動" disabled={disabled() || index()===props.queue.items.length-1} onClick={() => move(index(),1)}>↓</button>
          <button data-queue-delete disabled={disabled()} onClick={() => props.act({kind:'delete',id:item.id})}>取り消す</button>
        </div>
      </div>}</For>
    </Show>
    <Show when={props.queue.error}>
      <div role="status">{props.queue.error}</div>
      <button data-queue-refresh disabled={props.busy} onClick={() => props.act({kind:'refresh'})}>一覧を再取得</button>
    </Show>
  </section>
}
