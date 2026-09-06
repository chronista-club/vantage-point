/**
 * Sub Lane 作成フォーム。
 *
 * v1.0 柱 2 PR-3。 開閉 state は RepoAccordion が持ち、 Repo summary 右上の
 * 「+」アイコンボタンで toggle する。 name + optional branch + **engine(agent) dropdown**
 * → 作成で `lane:add_sub` IPC を送る。
 *
 * agent dropdown（doc 37）: mount 時に `agents:fetch` を撃ち、 Daemon `agents_list`
 * （SSOT = `EngineKind::ALL` + shell）の結果を repo ごとの購読で受けて populate
 * する。 選択値は IPC の `agent` に載る（未選択 = repo-side default = claude）。
 * fetch 前 / 失敗時は dropdown を出さず、 従来どおり default engine で作成できる（fail-open）。
 */
import { createEffect, createSignal, onCleanup, onMount, For, Show } from 'solid-js'
import { sendIpc } from './ipc'
import { createLaneCreation, subscribeAgents } from './feedback'

/** Daemon `agents_list` の 1 entry（`crate::client::AgentInfo` の wire shape）。 */
type AgentInfo = { name: string; description: string }

export function AddSub(props: { repoPath: string; onClose: () => void; onPendingChange: (pending: boolean) => void }) {
  const [name, setName] = createSignal('')
  const [branch, setBranch] = createSignal('')
  const [agents, setAgents] = createSignal<AgentInfo[]>([])
  const [agent, setAgent] = createSignal<string>('')

  const creation = createLaneCreation(props.repoPath, (message) => {
    if (!window.ipc) throw new Error('接続がありません。しばらくしてから再試行してください。')
    sendIpc(message)
  }, props.onClose)
  createEffect(() => props.onPendingChange(creation.pending()))
  onCleanup(() => { creation.dispose(); props.onPendingChange(false) })
  onMount(() => {
    const unsubscribe = subscribeAgents((p) => {
      if (p.repo_path !== props.repoPath) return
      const list = (p.agents ?? []).filter((a): a is AgentInfo =>
        typeof a === 'object' && a !== null && 'name' in a && typeof a.name === 'string' &&
        'description' in a && typeof a.description === 'string')
      setAgents(list)
      if (list.length > 0 && !agent()) setAgent(list[0]!.name)
    })
    onCleanup(unsubscribe)
    sendIpc({ t: 'agents:fetch', path: props.repoPath })
  })

  const submit = () => {
    const n = name().trim()
    if (!n) return
    const b = branch().trim()
    const s = agent().trim()
    creation.submit({
      name: n,
      branch: b || undefined,
      // 未 fetch / 未選択は undefined = repo-side default（conversation）に倒す。
      agent: s || undefined,
    })
  }

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Enter') submit()
    else if (e.key === 'Escape' && !creation.pending()) props.onClose()
  }

  return (
    <div class="vp-add-sub-form">
      <input
        class="vp-add-sub-input"
        disabled={creation.pending()}
        placeholder="sub name"
        ref={(el) => queueMicrotask(() => el.focus())}
        onInput={(e) => setName(e.currentTarget.value)}
        onKeyDown={onKey}
      />
      <input
        class="vp-add-sub-input"
        disabled={creation.pending()}
        placeholder="branch (optional)"
        onInput={(e) => setBranch(e.currentTarget.value)}
        onKeyDown={onKey}
      />
      {/* engine(agent) 選択。 fetch 済みで 2 件以上ある時だけ出す（1 件 = 選択の余地なし）。 */}
      <Show when={agents().length > 1}>
        <select
          class="vp-add-sub-input vp-add-sub-agent"
          value={agent()}
          disabled={creation.pending()}
          onChange={(e) => setAgent(e.currentTarget.value)}
          onKeyDown={onKey}
        >
          <For each={agents()}>
            {(s) => (
              <option value={s.name} title={s.description}>
                {s.name}
              </option>
            )}
          </For>
        </select>
      </Show>
      <Show when={creation.error()}><div role="alert" class="vp-operation-error">{creation.error()}</div></Show>
      <div class="vp-add-sub-actions">
        <button disabled={creation.pending()} onClick={props.onClose}>キャンセル</button>
        <button class="primary" disabled={creation.pending() || !name().trim()} onClick={submit}>
          {creation.pending() ? '作成中…' : '作成'}
        </button>
      </div>
    </div>
  )
}
