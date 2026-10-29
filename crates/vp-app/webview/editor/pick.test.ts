// @vitest-environment happy-dom
// pick.ts は editor-host（module 読み込み時に window を触る）を import するので、この file だけ DOM 環境で動かす
import { afterEach, describe, expect, it } from 'vitest'
import { findSelectable, selectableIdOf, type PickResolver } from './pick'

function dom(html: string): Document {
  document.body.innerHTML = html
  return document
}
afterEach(() => { document.body.innerHTML = '' })

/** `.creo-<id>` class から component id、knob は `data-knobs` に書いた id を返す stub */
const resolver: PickResolver = {
  componentIdOf: el => Array.from(el.classList).find(c => c.startsWith('creo-'))?.slice('creo-'.length) ?? null,
  match: el => ((el as HTMLElement).dataset.knobs ?? '').split(',').filter(Boolean).map(id => ({ id }) as never),
  register: knobs => knobs.map(k => `${k.id}`),
}

describe('findSelectable（editor-host selection.ts の写し）', () => {
  it('root が無ければ何も選ばない（fail-closed）', () => {
    const doc = dom('<main><button class="creo-btn">x</button></main>')
    expect(findSelectable(doc.querySelector('button'), null, resolver)).toBeNull()
  })

  it('root の外は選ばない', () => {
    const doc = dom('<main><button class="creo-btn">x</button></main><aside><button class="creo-btn">y</button></aside>')
    expect(findSelectable(doc.querySelector('aside button'), doc.querySelector('main'), resolver)).toBeNull()
  })

  it('明示 bind（data-editor-fields）が逆引きより優先される', () => {
    const doc = dom('<main><div class="creo-card" data-knobs="card.pad" data-editor-fields="a, b"><span id="t">x</span></div></main>')
    const found = findSelectable(doc.getElementById('t'), doc.body, resolver)
    expect(found?.explicitIds).toEqual(['a', 'b'])
    expect(found?.componentId).toBe('card')
    expect(found?.element.className).toBe('creo-card')
  })

  it('ノブを持つ最も内側の component を選ぶ', () => {
    const doc = dom('<main><div class="creo-card" data-knobs="card.pad"><button class="creo-btn" data-knobs="btn.pad.x"><span id="t">x</span></button></div></main>')
    const found = findSelectable(doc.getElementById('t'), doc.body, resolver)
    expect(found?.componentId).toBe('btn')
    expect(found?.knobs.map(k => k.id)).toEqual(['btn.pad.x'])
  })

  it('ノブの無い component は、外側にノブ持ちが居なければ fallback として選ばれる', () => {
    const doc = dom('<main><div class="creo-plain"><span id="t">x</span></div></main>')
    const found = findSelectable(doc.getElementById('t'), doc.body, resolver)
    expect(found?.componentId).toBe('plain')
    expect(found?.knobs).toEqual([])
    const outer = dom('<main><div class="creo-card" data-knobs="card.pad"><div class="creo-plain"><span id="t">x</span></div></div></main>')
    expect(findSelectable(outer.getElementById('t'), outer.body, resolver)?.componentId).toBe('card')
  })

  it('component でも bind でもない要素だけなら null', () => {
    const doc = dom('<main><p><span id="t">x</span></p></main>')
    expect(findSelectable(doc.getElementById('t'), doc.body, resolver)).toBeNull()
  })

  it('表示名は data-editor-selectable-id > component 名 > #id > tag の順', () => {
    const doc = dom('<main><div id="box" data-editor-fields="a"><i>x</i></div></main>')
    const found = findSelectable(doc.querySelector('i'), doc.body, resolver)!
    expect(selectableIdOf(found)).toBe('#box')
    found.element.dataset.editorSelectableId = 'The Box'
    expect(selectableIdOf(found)).toBe('The Box')
    const bare = findSelectable(dom('<main><section data-editor-fields="z"><b>x</b></section></main>').querySelector('b'), document.body, resolver)!
    expect(selectableIdOf(bare)).toBe('z')
  })
})
