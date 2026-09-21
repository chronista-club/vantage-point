/**
 * Codex の settings panel — runtime（mode / permission）+ model + effort。
 *
 * 候補は host が app-server の `model/list` から引いて `codex_config` event で届ける
 * （effort は model ごとに違う）。選択は `{codex: {model, effort}}` を request_id 付きで送り、
 * 結果は `codex_config` event（request_id 一致）で受ける — Codex は host の runtime を変えるので
 * 「保存中…」の ack が要る（Claude / vpcode は respawn の session_init で視覚確認するため不要）。
 */
import { For, Show } from 'solid-js'
import { nextRequestId } from './console'
import { CodexRuntimePanel } from './codex-runtime'
import { changeCodexSelection } from './codex-selection-control'
import { sendCodexInput } from './codex-input'
import { postSettings, type SettingsContext } from './engine-settings-shared'

export function CodexSettingsPanel(props: SettingsContext) {
  const state = () => props.lc.state
  const codexModel = () => state().codexConfig?.selection?.model ?? state().codexConfig?.model ?? ''
  const codexEffort = () => state().codexConfig?.selection?.effort ?? state().codexConfig?.effort ?? ''
  const codexModels = () => state().codexConfig?.models ?? []
  const busy = () => state().streaming || state().replaying || !!state().submission || !!state().pending || !!state().codexSettingsRequest
    || !!state().codexInput || !!state().codexQueue?.turn_id || !!state().codexQueue?.items.length
    || (!!state().codexQueue && !state().codexQueue?.ready)
  const input = (action: Record<string, unknown>) => sendCodexInput(props.lc, props.lane, props.session, action)
  const setSelection = (model: string, effort: string) => {
    if (busy()) return
    const requestId = nextRequestId('codex-settings')
    props.lc.set('codexSettingsRequest', requestId)
    props.lc.set('codexSettingsError', null)
    if (!postSettings(props, { codex: { model, effort } }, requestId)) {
      props.lc.set('codexSettingsRequest', null)
      return
    }
    setTimeout(() => {
      if (props.lc.state.codexSettingsRequest === requestId) {
        props.lc.set('codexSettingsRequest', null)
        props.lc.set('codexSettingsError', '設定変更の結果を確認できませんでした。表示を確認して再試行してください。')
      }
    }, 30_000)
  }
  return (
    <>
      <CodexRuntimePanel runtime={state().codexConfig?.runtime} busy={busy() || !codexModel()}
        connected={state().codexQueue?.ready === true}
        permissionChoices={state().codexConfig?.permission_choices}
        requestPermissions={() => input({ kind: 'permission_options' })}
        changePermissions={(choice, confirmed) => input({ kind: 'permissions', choice, confirmed })}
        changeMode={mode => input({ kind: 'mode', mode })} />
      <Show when={codexModels().length > 0} fallback={<span class="conversation-model-readonly">{state().codexConfig?.error ?? 'モデル候補を取得中…'}</span>}>
        <select class="conversation-model-select" aria-label="Codex model" title="次の Chat 送信に使うモデル" disabled={busy()}
          onChange={(e) => changeCodexSelection(e.currentTarget, codexModel(), value => { const model = codexModels().find(m => m.model === value); if (model) setSelection(model.model, model.default_effort) })}>
          <Show when={!codexModels().some(m => m.model === codexModel())}>
            <option value={codexModel()} selected disabled>{codexModel() || 'モデルを選択'}</option>
          </Show>
          <For each={codexModels()}>{m => <option value={m.model} selected={m.model === codexModel()}>{m.label}</option>}</For>
        </select>
        <select class="conversation-model-select" aria-label="Codex effort" title="次の Chat 送信の reasoning effort" disabled={busy() || !codexModels().some(m => m.model === codexModel())}
          onChange={(e) => changeCodexSelection(e.currentTarget, codexEffort(), value => setSelection(codexModel(), value))}>
          <Show when={!codexModels().find(m => m.model === codexModel())?.efforts.includes(codexEffort())}>
            <option value={codexEffort()} selected disabled>{codexEffort() || 'Codex 既定'}</option>
          </Show>
          <For each={codexModels().find(m => m.model === codexModel())?.efforts ?? []}>{effort => <option value={effort} selected={effort === codexEffort()}>{effort}</option>}</For>
        </select>
      </Show>
      <Show when={state().codexSettingsRequest}><span class="conversation-model-readonly">保存中…</span></Show>
      <Show when={state().codexSettingsError || (codexModels().length > 0 && state().codexConfig?.error)}>
        <span role="status" class="conversation-model-readonly">{state().codexSettingsError || state().codexConfig?.error}</span>
      </Show>
    </>
  )
}
