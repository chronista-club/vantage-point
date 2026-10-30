import { describe, expect, it } from 'vitest'
import { linkOptionsFrom } from './link-config'

describe('linkOptionsFrom（doc 86: Rust が置いた __VP_LINK__ → LinkOptions）', () => {
  it('url / surface / app をそのまま LinkOptions にする', () => {
    expect(
      linkOptionsFrom({ url: 'ws://127.0.0.1:32100/link', surface: 'vp-app#0', app: 'Vantage Point' }),
    ).toEqual({ url: 'ws://127.0.0.1:32100/link', surface: 'vp-app#0', app: 'Vantage Point' })
  })

  it('url だけでも成立する（surface / app は editor-host の既定に任せる）', () => {
    expect(linkOptionsFrom({ url: 'ws://127.0.0.1:32000/link' })).toEqual({ url: 'ws://127.0.0.1:32000/link' })
    expect(linkOptionsFrom({ url: 'ws://127.0.0.1:32000/link', surface: '', app: 7 })).toEqual({
      url: 'ws://127.0.0.1:32000/link',
    })
  })

  it('値が無い / 形が違うときは undefined = Link を張らない', () => {
    expect(linkOptionsFrom(undefined)).toBeUndefined()
    expect(linkOptionsFrom(null)).toBeUndefined()
    expect(linkOptionsFrom('ws://127.0.0.1:32000/link')).toBeUndefined()
    expect(linkOptionsFrom({})).toBeUndefined()
    expect(linkOptionsFrom({ url: 'http://127.0.0.1:32000/link' })).toBeUndefined()
    expect(linkOptionsFrom({ url: 42 })).toBeUndefined()
  })
})
