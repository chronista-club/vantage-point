/**
 * vpcode の settings panel — model picker のみ（effort / permission の概念は無い）。
 *
 * 候補は server が vpcode の endpoint から動的に引く catalog（vpcode_catalog）。vpcode に
 * engine 既定は無いので "Default" は無く、常に model を送る。
 */
import { Show } from 'solid-js'
import type { PickerChoice } from './console'
import { ModelSelect, ReadonlyModel, postSettings, type SettingsContext } from './engine-settings-shared'

export function VpcodeSettingsPanel(props: SettingsContext) {
  const state = () => props.lc.state
  const observedModel = (): string => state().header?.model ?? ''
  const modelChoices = (): ReadonlyArray<PickerChoice> => props.rosterEntry()?.model_choices ?? []
  // catalog は endpoint から動的に引くので、LM Studio 不在なら空 → 実測 model の read-only に落とす
  return (
    <Show when={modelChoices().length > 0} fallback={<ReadonlyModel model={observedModel()} />}>
      <ModelSelect
        choices={modelChoices()}
        current={observedModel()}
        disabled={state().streaming}
        title="model（この session に適用 — 会話は transcript で継続したまま入れ替わる）"
        onChange={(value) => {
          if (value) postSettings(props, { vpcode: { model: value } })
        }}
      />
    </Show>
  )
}
