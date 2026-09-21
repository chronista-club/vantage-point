/**
 * engine 別 settings panel の束ね役 — **agent 名 → panel の表**で引く。
 *
 * 原則（mako 2026-09-21）: 共通の器は作らない・if で engine を分けない・engine の code は
 * engine の file に閉じる。Claude / Codex / vpcode はそれぞれ自分の panel を持ち、ここは
 * 表を引いて描くだけ。表に無い engine（grok / opencode = model を engine 側で選ぶ）は
 * 実測 model の read-only 表示に落とす（押しても server に弾かれる行き止まりを作らない）。
 *
 * engine を足す = panel を 1 file 書いて表に 1 行。消す = file を消して行を消す。
 */
import { Show, type Component } from 'solid-js'
import { Dynamic } from 'solid-js/web'
import { ClaudeSettingsPanel } from './claude-settings-panel'
import { CodexSettingsPanel } from './codex-settings-panel'
import { VpcodeSettingsPanel } from './vpcode-settings-panel'
import { ReadonlyModel, type SettingsContext } from './engine-settings-shared'

export type { SettingsContext } from './engine-settings-shared'

export const ENGINE_SETTINGS_PANELS: Readonly<Record<string, Component<SettingsContext>>> = {
  claude: ClaudeSettingsPanel,
  codex: CodexSettingsPanel,
  vpcode: VpcodeSettingsPanel,
}

export function EngineSettingsPanel(props: SettingsContext) {
  const panel = () => {
    const agent = props.rosterEntry()?.agent ?? ''
    return Object.hasOwn(ENGINE_SETTINGS_PANELS, agent) ? ENGINE_SETTINGS_PANELS[agent] : undefined
  }
  return (
    <Show when={panel()} fallback={<ReadonlyModel model={props.lc.state.header?.model ?? ''} />}>
      {(p) => <Dynamic component={p()} {...props} />}
    </Show>
  )
}
