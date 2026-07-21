/**
 * PaneLayout の状態遷移テスト（doc 46 P1）。
 *
 * DOM binder（`PaneShell`）ではなく **純粋な状態機械**を対象にする — この repo の
 * vitest は node 環境で document を持たないため、テストできる形に層を切るのが規約
 * （chatview.tsx の「document 非依存 = vitest 対象」と同じ線引き）。
 */
import { describe, expect, it } from 'vitest'
import { PaneLayout, newPaneChoices } from './pane-shell'

const TERM = 'lane-host'
const CHAT = 'console-chat-host'

function setup(): PaneLayout {
  const l = new PaneLayout()
  l.dock({ id: TERM, label: 'Console' })
  l.dock({ id: CHAT, label: 'Chat' })
  return l
}

describe('PaneLayout', () => {
  it('既定では全 Pane が並ぶ（要件 1）', () => {
    const l = setup()
    expect(l.dockedIds()).toEqual([TERM, CHAT])
    expect(l.minimizedPanes()).toHaveLength(0)
  })

  it('最初に dock した Pane が focus を持つ（要件 3）', () => {
    expect(setup().focused()).toBe(TERM)
  })

  it('1 クリックで縮小・復元を往復する（要件 2）', () => {
    const l = setup()

    l.toggle(CHAT)
    expect(l.isMinimized(CHAT)).toBe(true)
    expect(l.dockedIds()).toEqual([TERM])
    expect(l.minimizedPanes().map((p) => p.id)).toEqual([CHAT])

    l.toggle(CHAT)
    expect(l.isMinimized(CHAT)).toBe(false)
    expect(l.minimizedPanes()).toHaveLength(0)
  })

  it('復元は元の位置に戻る（末尾送りにしない）', () => {
    const l = setup()
    l.minimize(TERM)
    l.restore(TERM)
    // minimize で配列から抜くと必ず末尾に戻り「畳んだら場所が変わった」になる
    expect(l.dockedIds()).toEqual([TERM, CHAT])
  })

  it('最後の 1 枚は畳めない（空の画面から戻れなくなるのを防ぐ）', () => {
    const l = setup()
    expect(l.minimize(CHAT)).toBe(true)
    expect(l.minimize(TERM)).toBe(false)
    expect(l.dockedIds()).toEqual([TERM])
  })

  it('未登録 id は畳めない', () => {
    const l = setup()
    expect(l.minimize('ghost')).toBe(false)
    expect(l.dockedIds()).toEqual([TERM, CHAT])
  })

  it('focus を持つ Pane を畳んだら focus が残った Pane へ移る', () => {
    const l = setup()
    l.focus(CHAT)
    expect(l.focused()).toBe(CHAT)

    l.minimize(CHAT)
    expect(l.focused()).toBe(TERM)
  })

  it('minimized な Pane に focus すると復元される', () => {
    const l = setup()
    l.minimize(CHAT)
    l.focus(CHAT)
    expect(l.isMinimized(CHAT)).toBe(false)
    expect(l.focused()).toBe(CHAT)
  })

  it('focus 移動は docked を巡回し、端で巻き戻る（要件 3）', () => {
    const l = setup()
    expect(l.focused()).toBe(TERM)

    l.moveFocus(1)
    expect(l.focused()).toBe(CHAT)
    // 端で止めると 2 枚構成で「右へ」が無反応になるので巻き戻す
    l.moveFocus(1)
    expect(l.focused()).toBe(TERM)
    l.moveFocus(-1)
    expect(l.focused()).toBe(CHAT)
  })

  it('focus 移動は minimized を飛ばす', () => {
    const l = setup()
    l.minimize(CHAT)
    l.moveFocus(1)
    expect(l.focused()).toBe(TERM)
  })

  it('未登録 id への focus は no-op（現在の focus を奪わない）', () => {
    const l = setup()
    l.focus('ghost')
    expect(l.focused()).toBe(TERM)
  })

  it('docked な Pane への restore は no-op（focus を奪わない）', () => {
    const l = setup()
    l.focus(CHAT)
    l.restore(TERM) // TERM は畳まれていない
    // restore は minimized のものにだけ効く（docked に呼んでも focus を奪わない）
    expect(l.focused()).toBe(CHAT)
    expect(l.dockedIds()).toEqual([TERM, CHAT])
  })

  it('minimized 中の id を再 dock しても畳んだままにする', () => {
    // 仕様の固定: dock は「登録 / label 更新」であって「開く」ではない。
    // ここが変わると、内容が更新されるたび畳んだ Pane が勝手に開く。
    const l = setup()
    l.minimize(CHAT)
    l.dock({ id: CHAT, label: 'Chat (codex)' })
    expect(l.isMinimized(CHAT)).toBe(true)
  })

  it('Pane が 1 枚も無ければ focus 移動は何もしない', () => {
    const l = new PaneLayout()
    l.moveFocus(1)
    expect(l.focused()).toBeNull()
  })

  it('同じ id の dock は差し替え（重複させない）', () => {
    const l = setup()
    l.dock({ id: CHAT, label: 'Chat (codex)' })
    expect(l.all()).toHaveLength(2)
    expect(l.minimizedPanes()).toHaveLength(0)
    expect(l.all().find((p) => p.id === CHAT)?.label).toBe('Chat (codex)')
  })
})

describe('newPaneChoices（doc 46 P2 要件 4: Engine × Act）', () => {
  it('chat 非対応 engine は Act II を出さない（行き止まりを作らない）', () => {
    const choices = newPaneChoices([
      { name: 'echoes', label: 'Claude', chat_capable: true },
      { name: 'shell', label: 'Shell', chat_capable: false },
    ])
    expect(choices).toEqual([
      { engine: 'echoes', engineLabel: 'Claude', act: 'tui' },
      { engine: 'echoes', engineLabel: 'Claude', act: 'chat' },
      // shell は chat host を持たないので tui だけ
      { engine: 'shell', engineLabel: 'Shell', act: 'tui' },
    ])
  })

  it('label 未設定なら stand 名をそのまま出す', () => {
    const choices = newPaneChoices([{ name: 'codex', chat_capable: true }])
    expect(choices.map((c) => c.engineLabel)).toEqual(['codex', 'codex'])
  })

  it('name が空の entry は捨てる（送っても解決できない）', () => {
    expect(newPaneChoices([{ name: '', chat_capable: true }])).toEqual([])
  })

  it('chat_capable 未指定は非対応扱い（不明なら行き止まりを作らない側に倒す）', () => {
    const choices = newPaneChoices([{ name: 'unknown' }])
    expect(choices.map((c) => c.act)).toEqual(['tui'])
  })
})
