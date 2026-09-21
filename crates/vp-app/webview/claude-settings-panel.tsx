/**
 * Claude の settings panel — model picker + permission mode picker。
 *
 * model 切替は `{claude: {model}}` を `conversation:set_settings` で送る（spec: セッション
 * 進行中でも切替可能）。repo が engine を --resume + 新 --model で入れ替える = 会話コンテキスト
 * 継続でモデル交換。適用の視覚確認は新 engine の session_init が header.model を更新することで
 * 得る（picker は実測値に追従）。streaming 中は disable — engine drop が進行中 turn を切るのを
 * UI で抑止する。
 */
import { For, Show } from 'solid-js'
import { produce } from 'solid-js/store'
import type { PickerChoice } from './console'
import { ModelSelect, postIpc, postSettings, type SettingsContext } from './engine-settings-shared'

/**
 * doc 35 PR3: permission mode（tool 承認の opt-in）。spawn 既定は bypassPermissions（素通し）。
 * "default" に切替えると Write/Bash 等が承認要求（PermissionRequest）経由になる。
 * doc 35 PR3/PR4: permission mode は per-session（engine の真値 = session_init.permission_mode）。
 * optimistic: 当該 session に即反映。engine は set_permission_mode を適用し、respawn 時は
 * session_init.permission_mode が真値（通常 bypassPermissions）で上書きする。
 *
 * panel 外（plan 承認で default へ戻す経路）からも呼ぶので export。
 */
export function setPermissionMode(ctx: Pick<SettingsContext, 'lane' | 'session' | 'lc'>, mode: string): void {
  ctx.lc.set(produce((s) => (s.permissionMode = mode)))
  postIpc({ t: 'conversation:set_permission_mode', lane: ctx.lane, session: ctx.session, mode })
}

export function ClaudeSettingsPanel(props: SettingsContext) {
  const state = () => props.lc.state
  const observedModel = (): string => state().header?.model ?? ''
  const modelChoices = (): ReadonlyArray<PickerChoice> => props.rosterEntry()?.model_choices ?? []
  const permissionChoices = (): ReadonlyArray<PickerChoice> =>
    props.rosterEntry()?.permission_choices ?? []
  const currentPermMode = (): string => state().permissionMode ?? 'bypassPermissions'
  return (
    <>
      {/* catalog 空 = VP からの切替なし（server 能力表明）。空なら描かない。 */}
      <Show when={modelChoices().length > 0}>
        <ModelSelect
          choices={modelChoices()}
          current={observedModel()}
          disabled={state().streaming}
          title="model（この session に適用 — 会話は resume で継続したまま入れ替わる）"
          onChange={(value) =>
            // "" = Default → model を省く（engine 既定 = --model を注入しない）
            postSettings(props, { claude: value ? { model: value } : {} })
          }
        />
      </Show>
      {/* permission picker: 表記は TUI と同一の英語 4 mode。空 = 対話承認の概念なし → 出さない。 */}
      <Show when={permissionChoices().length > 0}>
        <select
          class="conversation-model-select"
          title="permission mode（この session に適用。表記は TUI と同一）"
          onChange={(e) => setPermissionMode(props, e.currentTarget.value)}
        >
          <For each={permissionChoices()}>
            {(c) => (
              <option value={c.value} selected={currentPermMode() === c.value}>
                {c.label}
              </option>
            )}
          </For>
        </select>
      </Show>
    </>
  )
}
