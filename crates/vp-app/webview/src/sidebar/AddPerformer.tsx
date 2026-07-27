/**
 * Performer Lane 作成フォーム。
 *
 * v1.0 柱 2 PR-3。 開閉 state は RepoAccordion が持ち、 Repo summary 右上の
 * 「+」アイコンボタンで toggle する。 name + optional branch + **engine(agent) dropdown**
 * → 作成で `lane:add_performer` IPC を送る。
 *
 * agent dropdown（doc 37）: mount 時に `agents:fetch` を撃ち、 Daemon `agents_list`
 * （SSOT = `EngineKind::ALL` + shell）の結果を `window.handleAgentsResult` で受けて populate
 * する。 選択値は IPC の `agent` に載る（未選択 = repo-side default = claude）。
 * fetch 前 / 失敗時は dropdown を出さず、 従来どおり default engine で作成できる（fail-open）。
 */
import { createSignal, onCleanup, onMount, For, Show } from 'solid-js'
import { sendIpc } from './ipc'

/** Daemon `agents_list` の 1 entry（`crate::client::AgentInfo` の wire shape）。 */
type AgentInfo = { name: string; description: string }

/** `window.handleAgentsResult` が受ける payload（app.rs AgentsResult 由来）。 */
type AgentsResultPayload = {
  repo_path?: string
  agents?: AgentInfo[]
  error?: string | null
}

export function AddPerformer(props: { repoPath: string; onClose: () => void }) {
  const [name, setName] = createSignal('')
  const [branch, setBranch] = createSignal('')
  const [agents, setAgents] = createSignal<AgentInfo[]>([])
  const [agent, setAgent] = createSignal<string>('')

  // mount 時に当該 repo の利用可能 agent を fetch し、 結果 callback を差し込む。
  // handleAgentsResult は global singleton だが、 Add Performer form は同時に 1 つしか開かない
  // （repo accordion ごとの toggle）ので、 mount で奪い cleanup で stub へ戻す。
  onMount(() => {
    const w = window as unknown as {
      handleAgentsResult?: (msg: unknown) => void
    }
    const prev = w.handleAgentsResult
    w.handleAgentsResult = (msg: unknown) => {
      const p = (msg ?? {}) as AgentsResultPayload
      // 別 repo の遅延応答が現フォームの dropdown を汚さないよう repo_path で照合。
      if (p.repo_path && p.repo_path !== props.repoPath) return
      const list = Array.isArray(p.agents) ? p.agents : []
      setAgents(list)
      // 既定選択 = 先頭（= list_agents の並び: conversation が先頭）。未選択のままでも repo default に倒れる。
      if (list.length > 0 && !agent()) setAgent(list[0]!.name)
    }
    onCleanup(() => {
      w.handleAgentsResult = prev
    })
    sendIpc({ t: 'agents:fetch', path: props.repoPath })
  })

  const submit = () => {
    const n = name().trim()
    if (!n) return
    const b = branch().trim()
    const s = agent().trim()
    sendIpc({
      t: 'lane:add_performer',
      path: props.repoPath,
      name: n,
      branch: b || undefined,
      // 未 fetch / 未選択は undefined = repo-side default（conversation）に倒す。
      agent: s || undefined,
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
      {/* engine(agent) 選択。 fetch 済みで 2 件以上ある時だけ出す（1 件 = 選択の余地なし）。 */}
      <Show when={agents().length > 1}>
        <select
          class="vp-add-performer-input vp-add-performer-agent"
          value={agent()}
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
      <div class="vp-add-performer-actions">
        <button onClick={props.onClose}>キャンセル</button>
        <button class="primary" onClick={submit}>
          作成
        </button>
      </div>
    </div>
  )
}
