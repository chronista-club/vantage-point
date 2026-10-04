import { expect, it } from 'vitest'
import { readPaneStowState } from './pane-stow-state'
it('壊れた値と未知の版を無視し、保存値から独立した layout を返す', () => {
 const good = {version:1,layout:{structure:{columns:[{panes:['lane-code']}]},attention:{'lane-code':0}},shares:{'lane-code':3}}
 for (const bad of [null, {}, {...good,version:2}, {...good,shares:{'lane-code':-1}}, {...good,layout:{...good.layout,attention:{'lane-code':NaN}}}, {...good,shares:{missing:2}}]) expect(readPaneStowState(bad)).toBeNull()
 const result=readPaneStowState(good)!
 expect(result).toEqual(good)
 good.layout.attention['lane-code']=3
 expect(result.layout.attention['lane-code']).toBe(0)
})
