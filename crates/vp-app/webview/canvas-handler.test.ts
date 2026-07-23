/**
 * canvas-handler.ts の unit tests（board モデル 2026-07-15）
 *
 * board = SP truth のミラー view。 SP からの BoardUpdated 受信で board を置換し、
 * scope 切替 / lane 別保持 / cursor(local) / delete・clear(IPC 依頼) を検証する。
 * DOM 依存 (pp.ts renderPP/clearPP) は vi.mock でモック。 IPC (window.ipc) もモック。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

// pp.ts の DOM 操作をモック (JSDOM 不要)
vi.mock('./pp', () => ({
  renderPP: vi.fn(),
  clearPP: vi.fn(),
}))

// app-panes（pp-overlay auto-open の依存）をモック — engine / DOM を持ち込まない
vi.mock('./app-panes', () => ({
  appLayoutReady: vi.fn(() => true),
  applyAppScene: vi.fn(),
  isAppPaneVisible: vi.fn(() => false),
}))

import { appLayoutReady, applyAppScene, isAppPaneVisible } from './app-panes'

import {
  _resetForTest,
  clearActiveBoard,
  deleteItem,
  getActiveBoard,
  getCanvasState,
  handleMessage,
  setActiveBoard,
  setActiveLaneName,
  setCursor,
  subscribeCanvasState,
  type BoardScope,
  type CanvasItem,
} from './canvas-handler'

// SP からの BoardUpdated message を組み立てるヘルパ。
function boardUpdated(
  scope: BoardScope,
  lane: string | null,
  items: Array<{
    id: string
    title?: string
    content?: string
    contentType?: string
    createdAt?: string
  }>,
  cursor: string | null = items[0]?.id ?? null,
) {
  return {
    type: 'board_updated' as const,
    scope,
    lane,
    items: items.map((i) => ({
      id: i.id,
      content: i.content ?? i.id,
      contentType: (i.contentType ?? 'markdown') as CanvasItem['contentType'],
      title: i.title,
      // 既定は過去時刻 = retained replay 相当。 live 新着を作るときだけ明示指定する。
      createdAt: i.createdAt ?? '2026-07-15T00:00:00Z',
    })),
    cursor,
  }
}

// window.ipc モック（board:delete / board:clear の送信検証用）。
let ipcSpy: ReturnType<typeof vi.fn>
beforeEach(() => {
  _resetForTest()
  vi.clearAllMocks()
  ipcSpy = vi.fn()
  ;(globalThis as unknown as { ipc: { postMessage: typeof ipcSpy } }).ipc = { postMessage: ipcSpy }
})

// ============================================================================
// BoardUpdated 受信（SP truth のミラー）
// ============================================================================

describe('BoardUpdated 受信', () => {
  it('lane board（active=conductor）を受けて items/cursor が反映される', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a', title: 'A' }]))
    const { items, cursor, activeScope } = getCanvasState()
    expect(activeScope).toBe('lane')
    expect(items).toHaveLength(1)
    expect(items[0].title).toBe('A')
    expect(cursor).toBe('a')
  })

  it('items は SP の順序（新→古）そのまま反映される', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'c' }, { id: 'b' }, { id: 'a' }], 'c'))
    expect(getCanvasState().items.map((i) => i.id)).toEqual(['c', 'b', 'a'])
  })
})

// ============================================================================
// board 切替（scope）
// ============================================================================

describe('board 切替（scope）', () => {
  it('proj board は setActiveBoard("proj") で見える。 lane board とは独立', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'L' }]))
    handleMessage(boardUpdated('proj', null, [{ id: 'P' }]))
    // 既定は lane
    expect(getCanvasState().items[0].id).toBe('L')
    setActiveBoard('proj')
    expect(getActiveBoard()).toBe('proj')
    expect(getCanvasState().items[0].id).toBe('P')
    setActiveBoard('lane')
    expect(getCanvasState().items[0].id).toBe('L')
  })
})

// ============================================================================
// lane board の lane 別保持
// ============================================================================

describe('lane board の lane 別保持', () => {
  it('複数 lane の board を保持し、 active lane のものを表示する（切替後も残る）', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'cond' }])) // conductor
    handleMessage(boardUpdated('lane', 'feat-api', [{ id: 'perf' }])) // performer
    // active=conductor
    expect(getCanvasState().items[0].id).toBe('cond')
    // performer に切替
    setActiveLaneName('feat-api')
    expect(getCanvasState().items[0].id).toBe('perf')
    // conductor に戻すと board が残っている（retained を捨てない）
    setActiveLaneName(null)
    expect(getCanvasState().items[0].id).toBe('cond')
  })
})

// ============================================================================
// setCursor（view local）
// ============================================================================

describe('setCursor（view local）', () => {
  it('存在する id に cursor が動く', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'b' }, { id: 'a' }], 'b'))
    setCursor('a')
    expect(getCanvasState().cursor).toBe('a')
  })

  it('存在しない id は no-op（warn）', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }]))
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    setCursor('ghost')
    expect(getCanvasState().cursor).toBe('a')
    expect(warn).toHaveBeenCalled()
    warn.mockRestore()
  })
})

// ============================================================================
// delete / clear は SP に IPC 依頼（webview は truth を持たない）
// ============================================================================

describe('delete / clear は SP に IPC 依頼', () => {
  it('deleteItem は board:delete を active scope/lane で送る', () => {
    handleMessage(boardUpdated('lane', 'feat-api', [{ id: 'x' }]))
    setActiveLaneName('feat-api')
    deleteItem('x')
    expect(ipcSpy).toHaveBeenCalledTimes(1)
    const payload = JSON.parse(ipcSpy.mock.calls[0][0] as string)
    expect(payload).toMatchObject({
      t: 'board:delete',
      scope: 'lane',
      lane: 'feat-api',
      item_id: 'x',
    })
  })

  it('conductor lane の delete は lane=null', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'x' }]))
    deleteItem('x')
    const payload = JSON.parse(ipcSpy.mock.calls[0][0] as string)
    expect(payload.lane).toBeNull()
  })

  it('proj board の clearActiveBoard は board:clear scope=proj lane=null', () => {
    setActiveBoard('proj')
    clearActiveBoard()
    const payload = JSON.parse(ipcSpy.mock.calls[0][0] as string)
    expect(payload).toMatchObject({ t: 'board:clear', scope: 'proj', lane: null })
  })

  it('deleteItem は local state を変えない（SP truth の反映を待つ）', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'x' }]))
    deleteItem('x')
    // BoardUpdated が来るまで items は残る
    expect(getCanvasState().items).toHaveLength(1)
  })
})

// ============================================================================
// subscribeCanvasState
// ============================================================================

describe('subscribeCanvasState', () => {
  it('BoardUpdated で listener が呼ばれる', () => {
    const listener = vi.fn()
    subscribeCanvasState(listener)
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }]))
    expect(listener).toHaveBeenCalledTimes(1)
  })

  it('unsubscribe 後は呼ばれない', () => {
    const listener = vi.fn()
    const unsub = subscribeCanvasState(listener)
    unsub()
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }]))
    expect(listener).not.toHaveBeenCalled()
  })

  it('setActiveBoard 切替でも呼ばれる', () => {
    const listener = vi.fn()
    subscribeCanvasState(listener)
    setActiveBoard('proj')
    expect(listener).toHaveBeenCalledTimes(1)
  })
})

// ============================================================================
// getCanvasState の immutability
// ============================================================================

describe('getCanvasState immutability', () => {
  it('返却 items を変更しても internal state に影響しない (shallow copy)', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }]))
    const snap = getCanvasState()
    ;(snap.items as CanvasItem[]).push({
      id: 'x',
      content: '',
      contentType: 'markdown',
      createdAt: '',
    })
    expect(getCanvasState().items).toHaveLength(1)
  })
})

// ============================================================================
// pp-overlay auto-open（live/replay 区別）
// ============================================================================

/** app-panes モックの状態を組む（PP 可視状態と layout 準備状態）。 */
function primeAppPanes({ ppVisible = false, ready = true } = {}) {
  vi.mocked(appLayoutReady).mockReturnValue(ready)
  vi.mocked(isAppPaneVisible).mockReturnValue(ppVisible)
  return vi.mocked(applyAppScene)
}

describe('pp-overlay auto-open（live/replay 区別）', () => {
  it('起動時の retained replay（過去 createdAt）では auto-open しない', () => {
    const spy = primeAppPanes()
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }, { id: 'b' }]))
    expect(spy).not.toHaveBeenCalled()
  })

  it('live 新着（起動後 createdAt の未知 item）で pp-overlay が開く', () => {
    const spy = primeAppPanes()
    handleMessage(boardUpdated('lane', null, [{ id: 'fresh', createdAt: new Date().toISOString() }]))
    expect(spy).toHaveBeenCalledWith('pp-overlay')
  })

  it('既知 item の再配信（SP re-seed 相当）は createdAt が新しくても auto-open しない', () => {
    const spy = primeAppPanes()
    const fresh = new Date().toISOString()
    handleMessage(boardUpdated('lane', null, [{ id: 'x', createdAt: fresh }]))
    spy.mockClear()
    handleMessage(boardUpdated('lane', null, [{ id: 'x', createdAt: fresh }]))
    expect(spy).not.toHaveBeenCalled()
  })

  it('PP が既に見えていれば live 新着でも applyScene しない', () => {
    const spy = primeAppPanes({ ppVisible: true })
    handleMessage(boardUpdated('lane', null, [{ id: 'y', createdAt: new Date().toISOString() }]))
    expect(spy).not.toHaveBeenCalled()
  })

  it('default 配置が乗る前（appLayoutReady = false）は auto-open しない', () => {
    const spy = primeAppPanes({ ready: false })
    handleMessage(boardUpdated('lane', null, [{ id: 'z', createdAt: new Date().toISOString() }]))
    expect(spy).not.toHaveBeenCalled()
  })
})
