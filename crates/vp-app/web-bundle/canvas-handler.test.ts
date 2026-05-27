/**
 * canvas-handler.ts の unit tests
 *
 * doc 19 PP Canvas Stack Model の ring buffer / cursor / delete / subscribe API を
 * Small Test (unit) として検証する。
 *
 * DOM 依存 (pp.ts の renderPP / clearPP) は vi.mock でモック。
 * module-local singleton state は _resetForTest() で各テスト前にリセットする。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

// pp.ts の DOM 操作をモック (JSDOM 不要)
vi.mock('./pp', () => ({
  renderPP: vi.fn(),
  clearPP: vi.fn(),
}))

import {
  MAX_ITEMS,
  _resetForTest,
  deleteItem,
  getCanvasState,
  handleMessage,
  setCursor,
  subscribeCanvasState,
  type CanvasItem,
} from './canvas-handler'

// テスト用の最小限 ShowMessage を組み立てるヘルパ
function makeShowMsg(title: string, content: string = '## body') {
  return { type: 'show' as const, pane_id: 'main', content: { markdown: content }, title }
}

beforeEach(() => {
  _resetForTest()
  vi.clearAllMocks()
})

// ============================================================================
// dispatchShow — ring buffer push
// ============================================================================

describe('dispatchShow (handleMessage type=show)', () => {
  it('正常系: 1 件 push で items に 1 件、cursor が新 item を指す', () => {
    handleMessage(makeShowMsg('first'))
    const { items, cursor } = getCanvasState()
    expect(items).toHaveLength(1)
    expect(items[0].title).toBe('first')
    expect(cursor).toBe(items[0].id)
  })

  it('正常系: 2 件 push で items の先頭が最新', () => {
    handleMessage(makeShowMsg('first'))
    handleMessage(makeShowMsg('second'))
    const { items, cursor } = getCanvasState()
    expect(items).toHaveLength(2)
    expect(items[0].title).toBe('second')
    expect(cursor).toBe(items[0].id)
  })

  it('ring buffer: 11 件目 push で tail drop — items は MAX_ITEMS 件', () => {
    for (let i = 0; i < MAX_ITEMS + 1; i++) {
      handleMessage(makeShowMsg(`item-${i}`))
    }
    const { items } = getCanvasState()
    expect(items).toHaveLength(MAX_ITEMS)
    // 先頭は最新 (item-10)、末尾は 2 番目 (item-1) — item-0 が drop される
    expect(items[0].title).toBe('item-10')
    expect(items[MAX_ITEMS - 1].title).toBe('item-1')
  })

  it('ring buffer: MAX_ITEMS ちょうどでは drop されない', () => {
    for (let i = 0; i < MAX_ITEMS; i++) {
      handleMessage(makeShowMsg(`item-${i}`))
    }
    expect(getCanvasState().items).toHaveLength(MAX_ITEMS)
  })

  it('html content が items に入る', () => {
    handleMessage({ type: 'show', pane_id: 'main', content: { html: '<b>bold</b>' } })
    const { items } = getCanvasState()
    expect(items[0].contentType).toBe('html')
    expect(items[0].content).toBe('<b>bold</b>')
  })

  it('log content が items に入る', () => {
    handleMessage({ type: 'show', pane_id: 'main', content: { log: 'line1\nline2' } })
    const { items } = getCanvasState()
    expect(items[0].contentType).toBe('text')
  })

  it('サポート外 content variant (url のみ等) は items に追加しない', () => {
    handleMessage({ type: 'show', pane_id: 'main', content: { url: 'https://example.com' } })
    expect(getCanvasState().items).toHaveLength(0)
  })

  it('doc 19: append field が付いていても silent ignore される (backward compat)', () => {
    // 旧クライアントが append: true を送ってきても現在のモデルでは無視
    handleMessage({
      type: 'show',
      pane_id: 'main',
      content: { markdown: 'hello' },
      append: true,
    } as Parameters<typeof handleMessage>[0])
    expect(getCanvasState().items).toHaveLength(1)
  })
})

// ============================================================================
// handleMessage type=clear
// ============================================================================

describe('handleMessage type=clear', () => {
  it('clear で items と cursor が全 reset される', () => {
    handleMessage(makeShowMsg('one'))
    handleMessage(makeShowMsg('two'))
    handleMessage({ type: 'clear', pane_id: 'main' })
    const { items, cursor } = getCanvasState()
    expect(items).toHaveLength(0)
    expect(cursor).toBeNull()
  })
})

// ============================================================================
// setCursor
// ============================================================================

describe('setCursor', () => {
  it('正常系: 存在する id に cursor が移動する', () => {
    handleMessage(makeShowMsg('new'))
    handleMessage(makeShowMsg('old'))
    const { items } = getCanvasState()
    const oldId = items[1].id // items[1] = 古い方 (= "old")
    setCursor(oldId)
    expect(getCanvasState().cursor).toBe(oldId)
  })

  it('異常系: 存在しない id を渡した場合は cursor が変わらない (warn + no-op)', () => {
    handleMessage(makeShowMsg('item'))
    const before = getCanvasState().cursor
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})
    setCursor('non-existent-id')
    expect(getCanvasState().cursor).toBe(before)
    expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('setCursor'), 'non-existent-id')
    warnSpy.mockRestore()
  })
})

// ============================================================================
// deleteItem — cursor fallback (doc 19 §4.1: 右隣 → 左隣 → null)
// ============================================================================

describe('deleteItem cursor fallback', () => {
  it('cursor item 削除: 右隣が存在する場合、右隣に cursor が移動する', () => {
    // push: C → B → A (items = [A, B, C] → 逆順 unshift)
    handleMessage(makeShowMsg('C'))
    handleMessage(makeShowMsg('B'))
    handleMessage(makeShowMsg('A'))
    const { items } = getCanvasState()
    // items = [A(0), B(1), C(2)]
    setCursor(items[1].id) // cursor = B (index 1)
    const rightNeighborId = items[2].id // C は B の右隣 (index 2)
    deleteItem(items[1].id) // B を削除
    expect(getCanvasState().cursor).toBe(rightNeighborId)
  })

  it('cursor item 削除: 右隣なし・左隣あり の場合、左隣に cursor が移動する', () => {
    handleMessage(makeShowMsg('B'))
    handleMessage(makeShowMsg('A'))
    const { items } = getCanvasState()
    // items = [A(0), B(1)]
    setCursor(items[1].id) // cursor = B (index 1, 末尾)
    const leftNeighborId = items[0].id // A
    deleteItem(items[1].id)
    expect(getCanvasState().cursor).toBe(leftNeighborId)
  })

  it('cursor item 削除: items が 1 件だけの場合、cursor が null になる', () => {
    handleMessage(makeShowMsg('only'))
    const { items } = getCanvasState()
    deleteItem(items[0].id)
    expect(getCanvasState().cursor).toBeNull()
    expect(getCanvasState().items).toHaveLength(0)
  })

  it('非 cursor item 削除: cursor は変わらない', () => {
    handleMessage(makeShowMsg('B'))
    handleMessage(makeShowMsg('A'))
    const { items } = getCanvasState()
    const cursorId = items[0].id // A
    setCursor(cursorId)
    deleteItem(items[1].id) // B を削除 (cursor でない)
    expect(getCanvasState().cursor).toBe(cursorId)
    expect(getCanvasState().items).toHaveLength(1)
  })

  it('異常系: 存在しない id を渡した場合は no-op (warn のみ)', () => {
    handleMessage(makeShowMsg('item'))
    const before = getCanvasState()
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {})
    deleteItem('ghost-id')
    expect(getCanvasState().items).toHaveLength(1)
    expect(getCanvasState().cursor).toBe(before.cursor)
    expect(warnSpy).toHaveBeenCalled()
    warnSpy.mockRestore()
  })
})

// ============================================================================
// subscribeCanvasState
// ============================================================================

describe('subscribeCanvasState', () => {
  it('push 時に listener が呼ばれる', () => {
    const listener = vi.fn()
    subscribeCanvasState(listener)
    handleMessage(makeShowMsg('event'))
    expect(listener).toHaveBeenCalledTimes(1)
  })

  it('unsubscribe 後は listener が呼ばれない', () => {
    const listener = vi.fn()
    const unsub = subscribeCanvasState(listener)
    unsub()
    handleMessage(makeShowMsg('after-unsub'))
    expect(listener).not.toHaveBeenCalled()
  })

  it('複数 listener が独立して動作する', () => {
    const a = vi.fn()
    const b = vi.fn()
    subscribeCanvasState(a)
    subscribeCanvasState(b)
    handleMessage(makeShowMsg('x'))
    expect(a).toHaveBeenCalledTimes(1)
    expect(b).toHaveBeenCalledTimes(1)
  })

  it('setCursor でも listener が呼ばれる', () => {
    handleMessage(makeShowMsg('item'))
    const { items } = getCanvasState()
    const listener = vi.fn()
    subscribeCanvasState(listener)
    setCursor(items[0].id)
    expect(listener).toHaveBeenCalledTimes(1)
  })

  it('deleteItem でも listener が呼ばれる', () => {
    handleMessage(makeShowMsg('item'))
    const { items } = getCanvasState()
    const listener = vi.fn()
    subscribeCanvasState(listener)
    deleteItem(items[0].id)
    expect(listener).toHaveBeenCalledTimes(1)
  })
})

// ============================================================================
// getCanvasState の immutability
// ============================================================================

describe('getCanvasState', () => {
  it('返却された items を変更しても internal state に影響しない (shallow copy)', () => {
    handleMessage(makeShowMsg('item'))
    const snapshot = getCanvasState()
    // snapshot.items.push は slice() コピーなので internal を変えない
    ;(snapshot.items as CanvasItem[]).push({
      id: 'injected',
      content: 'x',
      contentType: 'markdown',
      createdAt: '',
    })
    expect(getCanvasState().items).toHaveLength(1)
  })
})
