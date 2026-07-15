/**
 * Performer Lane 作成フォーム。
 *
 * v1.0 柱 2 PR-3。 開閉 state は ProjectAccordion が持ち、 Project summary 右上の
 * 「+」アイコンボタンで toggle する。 name + optional branch + **engine(stand) dropdown**
 * → 作成で `lane:add_performer` IPC を送る。
 *
 * stand dropdown（doc 37）: mount 時に `stands:fetch` を撃ち、 World `stands_list`
 * （SSOT = `EngineKind::ALL` + shell）の結果を `window.handleStandsResult` で受けて populate
 * する。 選択値は IPC の `stand` に載る（未選択 = SP-side default = echoes）。
 * fetch 前 / 失敗時は dropdown を出さず、 従来どおり default engine で作成できる（fail-open）。
 */
import { createSignal, onCleanup, onMount, For, Show } from 'solid-js'
import { sendIpc } from './ipc'

/** World `stands_list` の 1 entry（`crate::client::StandInfo` の wire shape）。 */
type StandInfo = { name: string; description: string }

/** `window.handleStandsResult` が受ける payload（app.rs StandsResult 由来）。 */
type StandsResultPayload = {
  project_path?: string
  stands?: StandInfo[]
  error?: string | null
}

export function AddPerformer(props: { projectPath: string; onClose: () => void }) {
  const [name, setName] = createSignal('')
  const [branch, setBranch] = createSignal('')
  const [stands, setStands] = createSignal<StandInfo[]>([])
  const [stand, setStand] = createSignal<string>('')

  // mount 時に当該 project の利用可能 stand を fetch し、 結果 callback を差し込む。
  // handleStandsResult は global singleton だが、 Add Performer form は同時に 1 つしか開かない
  // （project accordion ごとの toggle）ので、 mount で奪い cleanup で stub へ戻す。
  onMount(() => {
    const w = window as unknown as {
      handleStandsResult?: (msg: unknown) => void
    }
    const prev = w.handleStandsResult
    w.handleStandsResult = (msg: unknown) => {
      const p = (msg ?? {}) as StandsResultPayload
      // 別 project の遅延応答が現フォームの dropdown を汚さないよう project_path で照合。
      if (p.project_path && p.project_path !== props.projectPath) return
      const list = Array.isArray(p.stands) ? p.stands : []
      setStands(list)
      // 既定選択 = 先頭（= list_stands の並び: echoes が先頭）。未選択のままでも SP default に倒れる。
      if (list.length > 0 && !stand()) setStand(list[0]!.name)
    }
    onCleanup(() => {
      w.handleStandsResult = prev
    })
    sendIpc({ t: 'stands:fetch', path: props.projectPath })
  })

  const submit = () => {
    const n = name().trim()
    if (!n) return
    const b = branch().trim()
    const s = stand().trim()
    sendIpc({
      t: 'lane:add_performer',
      path: props.projectPath,
      name: n,
      branch: b || undefined,
      // 未 fetch / 未選択は undefined = SP-side default（echoes）に倒す。
      stand: s || undefined,
    })
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
      {/* engine(stand) 選択。 fetch 済みで 2 件以上ある時だけ出す（1 件 = 選択の余地なし）。 */}
      <Show when={stands().length > 1}>
        <select
          class="vp-add-performer-input vp-add-performer-stand"
          value={stand()}
          onChange={(e) => setStand(e.currentTarget.value)}
          onKeyDown={onKey}
        >
          <For each={stands()}>
            {(s) => (
              <option value={s.name} title={s.description}>
                {s.name}
              </option>
            )}
          </For>
        </select>
      </Show>
      <div class="vp-add-performer-actions">
        <button onClick={props.onClose}>キャンセル</button>
        <button class="primary" onClick={submit}>
          作成
        </button>
      </div>
    </div>
  )
}
