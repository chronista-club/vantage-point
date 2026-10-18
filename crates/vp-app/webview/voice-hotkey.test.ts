// mem_1CgANNcfXDjfnXyBQtSCsZ
import { describe, expect, it } from 'vitest'
import { createRightOptionTap } from './voice-hotkey'

describe('right Option single tap', () => {
  it('emits only on release, accepts consecutive taps and ignores repeat edges', () => {
    const tap = createRightOptionTap()
    expect(tap.keydown({code:'AltRight'})).toBeUndefined()
    tap.keydown({code:'AltRight',repeat:true})
    expect(tap.keyup({code:'AltRight'})).toBe(true)
    expect(tap.keyup({code:'AltRight'})).toBe(false)
    tap.keydown({code:'AltRight'})
    expect(tap.keyup({code:'AltRight'})).toBe(true)
  })

  it('rejects other keys before or during Option, including the left Option', () => {
    const tap = createRightOptionTap()
    tap.keydown({code:'AltLeft'})
    expect(tap.keyup({code:'AltLeft'})).toBe(false)
    tap.keydown({code:'AltRight'});tap.keydown({code:'ArrowLeft'})
    tap.keyup({code:'ArrowLeft'})
    expect(tap.keyup({code:'AltRight'})).toBe(false)
    tap.keydown({code:'KeyA'});tap.keydown({code:'AltRight'});tap.keyup({code:'KeyA'})
    expect(tap.keyup({code:'AltRight'})).toBe(false)
    for (const modifier of ['shiftKey','ctrlKey','metaKey']) {
      tap.keydown({code:'AltRight',[modifier]:true})
      expect(tap.keyup({code:'AltRight'})).toBe(false)
    }
  })

  it('rejects composition and resets an interrupted gesture', () => {
    const tap = createRightOptionTap()
    tap.keydown({code:'AltRight',isComposing:true})
    expect(tap.keyup({code:'AltRight'})).toBe(false)
    tap.keydown({code:'AltRight'})
    expect(tap.keyup({code:'AltRight',isComposing:true})).toBe(false)
    tap.keydown({code:'AltRight'});tap.reset()
    expect(tap.keyup({code:'AltRight'})).toBe(false)
    tap.keydown({code:'AltRight',repeat:true})
    expect(tap.keyup({code:'AltRight'})).toBe(false)
    tap.keydown({code:'AltRight'})
    expect(tap.keyup({code:'AltRight'})).toBe(true)
  })
})
