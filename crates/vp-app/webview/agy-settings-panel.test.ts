// agy の settings panel — engine が出した configOptions から描く軸と、送る settings の形（doc 88 §6）。
import { describe, expect, it } from 'vitest'
import { agyAxes, agySettingsFor } from './agy-settings-panel'
import type { AcpConfigView } from './src/generated/AcpConfigView'

/** 実測（agy 1.5.9）の configOptions を roster の形にしたもの（候補は一部）。 */
const view = (extra: Partial<AcpConfigView> = {}): AcpConfigView => ({
  options: [
    { id: 'mode', name: 'Mode', current_value: 'default',
      choices: [{ value: 'default', name: 'Default' }, { value: 'plan', name: 'Plan', description: 'Plan carefully before executing a task.' }] },
    { id: 'model', name: 'Model', current_value: 'gemini-4-argon',
      choices: [{ value: 'gemini-4-argon', name: 'Gemini 4 Argon' }, { value: 'claude-opus-5-5', name: 'Claude Opus 5.5' }] },
    { id: 'thought_level', name: 'Thinking Level', current_value: 'medium',
      choices: [{ value: 'low', name: 'Low' }, { value: 'medium', name: 'Medium' }] },
    { id: 'telemetry', name: 'Telemetry', current_value: 'on', choices: [{ value: 'on', name: 'On' }] },
  ],
  ...extra,
})

describe('agyAxes', () => {
  it('Model → Thinking Level → Mode の順に並べ、VP が意図として残せない軸は出さない', () => {
    expect(agyAxes(view()).map((o) => o.id)).toEqual(['model', 'thought_level', 'mode'])
  })
  it('候補に無い現在値は候補に足して見せる（engine の真実を隠さない）', () => {
    const v = view()
    v.options[2].current_value = 'xhigh'
    const level = agyAxes(v).find((o) => o.id === 'thought_level')
    expect(level?.choices.map((c) => c.value)).toEqual(['low', 'medium', 'xhigh'])
  })
  it('設定面が無い（engine 未起動 / 旧 server）なら空', () => {
    expect(agyAxes(null)).toEqual([])
    expect(agyAxes(undefined)).toEqual([])
  })
})

describe('agySettingsFor', () => {
  it('切り替えた 1 軸だけを {agy: {...}} で送る（他の軸は server が registry の意図を保つ）', () => {
    expect(agySettingsFor('thought_level', 'low')).toEqual({ agy: { thought_level: 'low' } })
    expect(agySettingsFor('model', 'claude-opus-5-5')).toEqual({ agy: { model: 'claude-opus-5-5' } })
    expect(agySettingsFor('mode', 'plan')).toEqual({ agy: { mode: 'plan' } })
  })
})
