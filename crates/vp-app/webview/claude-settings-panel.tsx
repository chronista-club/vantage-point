/**
 * Claude の settings panel — model picker + effort picker + permission mode picker。
 *
 * model / effort の切替は `{claude: {model, effort}}` を `conversation:set_settings` で送る
 * （spec: セッション進行中でも切替可能）。片方を変えるときも他方は roster の intent から
 * 引き継ぐ（settings は丸ごと置換なので、送らない = 既定に戻る）。
 *
 * repo が engine を --resume + 新 flag で入れ替える = 会話コンテキスト継続で設定交換。
 * model の適用は新 engine の session_init が header.model を更新することで視覚確認できる
 * （picker は実測値に追従）。effort は VP が engine から実測を受け取っていないので、picker の
 * 現在値は registry の intent。streaming 中は disable — engine drop が進行中 turn を切るのを
 * UI で抑止する。
 */
import { For, Show } from 'solid-js'
import { produce } from 'solid-js/store'
import type { ConversationSession, PickerChoice } from './console'
import type { ClaudeSettings } from './src/generated/ClaudeSettings'
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

/** roster の intent（registry の `settings.claude`）。未設定 = 既定（何も注入しない）。 */
function claudeIntent(entry: ConversationSession | undefined): ClaudeSettings {
  const settings = entry?.settings
  return settings && 'claude' in settings ? settings.claude : {}
}

export function ClaudeSettingsPanel(props: SettingsContext) {
  const state = () => props.lc.state
  const observedModel = (): string => state().header?.model ?? ''
  const intent = (): ClaudeSettings => claudeIntent(props.rosterEntry())
  const modelChoices = (): ReadonlyArray<PickerChoice> => props.rosterEntry()?.model_choices ?? []
  const effortChoices = (): ReadonlyArray<PickerChoice> => props.rosterEntry()?.effort_choices ?? []
  const permissionChoices = (): ReadonlyArray<PickerChoice> =>
    props.rosterEntry()?.permission_choices ?? []
  const currentPermMode = (): string => state().permissionMode ?? 'bypassPermissions'
  /** "" = Default → その field を省く（engine 既定 = flag を注入しない）。 */
  const send = (next: { model?: string; effort?: string }) => {
    const claude: ClaudeSettings = {}
    if (next.model) claude.model = next.model
    if (next.effort) claude.effort = next.effort
    postSettings(props, { claude })
  }
  return (
    <>
      {/* catalog 空 = VP からの切替なし（server 能力表明）。空なら描かない。 */}
      <Show when={modelChoices().length > 0}>
        <ModelSelect
          choices={modelChoices()}
          current={observedModel()}
          disabled={state().streaming}
          title="model（この session に適用 — 会話は resume で継続したまま入れ替わる）"
          onChange={(value) => send({ model: value, effort: intent().effort })}
        />
      </Show>
      {/* effort: VP は engine から実測を受け取っていないので「現在値」は registry の intent。 */}
      <Show when={effortChoices().length > 0}>
        <select
          class="conversation-model-select"
          aria-label="Claude effort"
          disabled={state().streaming}
          title="effort（この session に適用 — model と同じく resume で継続したまま入れ替わる）"
          onChange={(e) => send({ model: intent().model, effort: e.currentTarget.value })}
        >
          <For each={effortChoices()}>
            {(c) => (
              <option value={c.value} selected={(intent().effort ?? '') === c.value}>
                {c.label}
              </option>
            )}
          </For>
        </select>
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
