/**
 * Codex の settings panel — runtime（mode / permission）+ model + effort。
 * 表示は共有 `CodexSettingsControls`（design 74）。権限欄は VP が所有する native の権限メニューを差し込む。
 *
 * 候補は host が app-server の `model/list` から引いて `codex_config` event で届ける
 * （effort は model ごとに違う）。選択は `{codex: {model, effort}}` を request_id 付きで送り、
 * 結果は `codex_config` event（request_id 一致）で受ける — Codex は host の runtime を変えるので
 * 「保存中…」の ack が要る（Claude / vpcode は respawn の session_init で視覚確認するため不要）。
 */
import { CodexSettingsControls, codexModelOf } from '@chronista-club/agent-chat/codex-settings'
import { nextRequestId } from './console'
import { CodexPermissionMenu } from './codex-permissions'
import { sendCodexInput } from './codex-input'
import { postSettings, type SettingsContext } from './engine-settings-shared'

export function CodexSettingsPanel(props: SettingsContext) {
  const state = () => props.lc.state
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
    <CodexSettingsControls config={state().codexConfig} busy={busy()}
      connected={state().codexQueue?.ready === true}
      saving={!!state().codexSettingsRequest} error={state().codexSettingsError}
      onSelection={setSelection} onMode={mode => input({ kind: 'mode', mode })}
      permission={<CodexPermissionMenu runtime={state().codexConfig?.runtime}
        choices={state().codexConfig?.permission_choices}
        busy={busy() || !codexModelOf(state().codexConfig)} connected={state().codexQueue?.ready === true}
        requestOptions={() => input({ kind: 'permission_options' })}
        change={(choice, confirmed) => input({ kind: 'permissions', choice, confirmed })} />} />
  )
}
