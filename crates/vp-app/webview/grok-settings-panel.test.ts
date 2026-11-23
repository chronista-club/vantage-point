// Task mem_1ChKFNBY7VQYSwFseyxUs9
import { describe, expect, it } from 'vitest'
import { grokAxes, grokSettingsFor } from './grok-settings-panel'
import type { AcpConfigView } from './src/generated/AcpConfigView'

const view: AcpConfigView = { options: [
  { id: 'reasoning_effort', name: 'Reasoning', current_value: 'xhigh', choices: [{value:'high',name:'High'}] },
  { id: 'model', name: 'Model', current_value: 'future-model', choices: [{value:'future-model',name:'Future'}] },
  { id: 'mode', name: 'Mode', current_value: 'auto', choices: [] },
] }

describe('Grok settings panel', () => {
  it('engine の model → reasoning_effort を描き、未知の現在値も隠さない', () => {
    const axes = grokAxes(view)
    expect(axes.map(o => o.id)).toEqual(['model','reasoning_effort'])
    expect(axes[0].choices[0]).toEqual({ value:'future-model',name:'Future' })
    expect(axes[1].choices.map(c => c.value)).toEqual(['high','xhigh'])
    expect(view.options[0].choices).toHaveLength(1)
    expect(grokAxes(null)).toEqual([])
  })
  it('他の軸の古い値を持ち越さず、切り替える軸だけを送る', () => {
    expect(grokSettingsFor('model','new-model')).toEqual({grok:{model:'new-model'}})
    expect(grokSettingsFor('reasoning_effort','medium')).toEqual({grok:{reasoning_effort:'medium'}})
  })
})
