/**
 * board-handler.ts の unit tests（board モデル 2026-07-15）
 *
 * board = repo truth のミラー view。 repo からの BoardUpdated 受信で board を置換し、
 * scope 切替 / lane 別保持 / cursor(local) / delete・clear(IPC 依頼) を検証する。
 * DOM 依存 (board-render.ts renderBoard/clearBoard) は vi.mock でモック。 IPC (window.ipc) もモック。
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'

// board-render.ts の DOM 操作をモック (JSDOM 不要)
vi.mock('./board-render', () => ({
  renderBoard: vi.fn(),
  clearBoard: vi.fn(),
}))

// doc 52 §10 wave 0: 旧 pp-overlay auto-open（app-panes 依存）は退役。board pane 化で
// presence は 'vp:board-presence' event に載る（薄い action 層 = node 環境では silent skip）。
// live/replay 区別の判定は hasFreshArrival を純関数として直接検証する（下方）。

import {
  _resetForTest,
  clearActiveBoard,
  computeUnread,
  deleteItem,
  formatFreshness,
  freshNewIds,
  getCanvasState,
  handleMessage,
  hasFreshArrival,
  setActiveBoard,
  setCursor,
  subscribeCanvasState,
  type BoardItem,
} from './board-handler'

// repo からの BoardUpdated message を組み立てるヘルパ。
function boardUpdated(
  scope: string,
  lane: string | null,
  items: Array<{
    id: string
    title?: string
    content?: string
    contentType?: string
    createdAt?: string
  }>,
  cursor: string | null = items[0]?.id ?? null,
  /**
   * 送信元 repo（vp-app が stamp する）。board の同一性は `(repo, lane)` の対。
   *
   * 既定は空 = 初期 active と同じ箱。**repo 次元を問わない test はこれで従来どおり**書け、
   * repo をまたぐ挙動を見る test だけが明示的に渡す（下の「repo 次元」describe）。
   */
  repo = '',
) {
  return {
    type: 'board_updated' as const,
    scope,
    repo,
    lane,
    items: items.map((i) => ({
      id: i.id,
      content: i.content ?? i.id,
      contentType: (i.contentType ?? 'markdown') as BoardItem['contentType'],
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
// BoardUpdated 受信（repo truth のミラー）
// ============================================================================

describe('BoardUpdated 受信', () => {
  it('lane board（active=conductor）を受けて items/cursor が反映される', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a', title: 'A' }]))
    const { items, cursor } = getCanvasState()
    expect(items).toHaveLength(1)
    expect(items[0].title).toBe('A')
    expect(cursor).toBe('a')
  })

  it('items は repo の順序（新→古）そのまま反映される', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'c' }, { id: 'b' }, { id: 'a' }], 'c'))
    expect(getCanvasState().items.map((i) => i.id)).toEqual(['c', 'b', 'a'])
  })
})

// ============================================================================
// board 切替（scope）
// ============================================================================

describe('lane 以外の scope は無視される（proj 撤去 2026-07-23）', () => {
  it('旧 proj board の retained 再配信が lane board に混ざらない', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'L' }]))
    handleMessage(boardUpdated('proj', null, [{ id: 'P' }]))
    expect(getCanvasState().items.map((i) => i.id)).toEqual(['L'])
  })

  it('未知 scope（将来の追加）も fail-quiet で無視する', () => {
    handleMessage(boardUpdated('vp', null, [{ id: 'V' }]))
    expect(getCanvasState().items).toHaveLength(0)
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
    setActiveBoard('', 'feat-api')
    expect(getCanvasState().items[0].id).toBe('perf')
    // conductor に戻すと board が残っている（retained を捨てない）
    setActiveBoard('', null)
    expect(getCanvasState().items[0].id).toBe('cond')
  })
})

// ============================================================================
// repo 次元（2026-08-04 根治）
// ============================================================================

describe('board の同一性は (repo, lane) の対', () => {
  it('⚠️ board を持たない repo に切り替えたら空になる（前の repo の board が残らない）', () => {
    // 全 repo の root lane は同じ 'conductor' を名乗る。repo 次元を落とすと 13 repo が
    // 1 つの箱を奪い合い、**board 行を持たない repo で前の repo の board が出続けた**。
    handleMessage(boardUpdated('lane', null, [{ id: 'vp-item' }], null, 'vantage-point'))
    setActiveBoard('vantage-point', null)
    expect(getCanvasState().items[0].id).toBe('vp-item')

    // board を一度も使っていない repo の root lane へ
    setActiveBoard('nexus', null)
    expect(getCanvasState().items).toEqual([])
  })

  it('別 repo の同名 lane が互いを上書きしない', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }], null, 'repo-a'))
    handleMessage(boardUpdated('lane', null, [{ id: 'b' }], null, 'repo-b'))
    setActiveBoard('repo-a', null)
    expect(getCanvasState().items[0].id).toBe('a')
    setActiveBoard('repo-b', null)
    expect(getCanvasState().items[0].id).toBe('b')
  })

  it('同名の Sub lane も repo をまたいで混ざらない', () => {
    handleMessage(boardUpdated('lane', 'stack-land', [{ id: 'in-a' }], null, 'repo-a'))
    handleMessage(boardUpdated('lane', 'stack-land', [{ id: 'in-b' }], null, 'repo-b'))
    setActiveBoard('repo-a', 'stack-land')
    expect(getCanvasState().items[0].id).toBe('in-a')
    setActiveBoard('repo-b', 'stack-land')
    expect(getCanvasState().items[0].id).toBe('in-b')
  })

  it('戻れば元の board が残っている（切替は捨てない）', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }], null, 'repo-a'))
    setActiveBoard('repo-a', null)
    setActiveBoard('repo-b', null)
    expect(getCanvasState().items).toEqual([])
    setActiveBoard('repo-a', null)
    expect(getCanvasState().items[0].id).toBe('a')
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
// delete / clear は repo に IPC 依頼（webview は truth を持たない）
// ============================================================================

describe('delete / clear は repo に IPC 依頼', () => {
  it('deleteItem は board:delete を active scope/lane で送る', () => {
    handleMessage(boardUpdated('lane', 'feat-api', [{ id: 'x' }]))
    setActiveBoard('', 'feat-api')
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

  it('clearActiveBoard は常に scope=lane を送る（proj 撤去後の canonical）', () => {
    clearActiveBoard()
    const payload = JSON.parse(ipcSpy.mock.calls[0][0] as string)
    expect(payload).toMatchObject({ t: 'board:clear', scope: 'lane', lane: null })
  })

  it('deleteItem は local state を変えない（repo truth の反映を待つ）', () => {
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
})

// ============================================================================
// getCanvasState の immutability
// ============================================================================

describe('getCanvasState immutability', () => {
  it('返却 items を変更しても internal state に影響しない (shallow copy)', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }]))
    const snap = getCanvasState()
    ;(snap.items as BoardItem[]).push({
      id: 'x',
      content: '',
      contentType: 'markdown',
      createdAt: '',
    })
    expect(getCanvasState().items).toHaveLength(1)
  })
})

// ============================================================================
// hasFreshArrival（live/replay 区別 — board pane の focus 判定の核）
// ============================================================================

/** BOOT_TS より未来 = live 新着相当の createdAt。テスト実行より確実に後の固定値を使う
 *  （Date.now を使わず — 現在時刻 >= BOOT_TS は自明なので、十分先の固定 ISO で十分）。 */
const FUTURE_ISO = '2999-01-01T00:00:00.000Z'
/** BOOT_TS より過去 = retained replay 相当。 */
const PAST_ISO = '2000-01-01T00:00:00.000Z'

const item = (id: string, createdAt: string): BoardItem => ({
  id,
  content: id,
  contentType: 'markdown',
  createdAt,
})

describe('hasFreshArrival（board pane focus の live/replay 区別）', () => {
  it('起動後 createdAt の未知 item = live 新着（focus を寄せる対象）', () => {
    expect(hasFreshArrival([item('fresh', FUTURE_ISO)], new Set())).toBe(true)
  })

  it('過去 createdAt（retained replay 相当）は新着でない', () => {
    expect(hasFreshArrival([item('a', PAST_ISO), item('b', PAST_ISO)], new Set())).toBe(false)
  })

  it('既知 item の再配信（repo re-seed 相当）は createdAt が新しくても新着でない', () => {
    expect(hasFreshArrival([item('x', FUTURE_ISO)], new Set(['x']))).toBe(false)
  })

  it('createdAt が parse 不能な item は新着扱いしない（静かな側に倒す）', () => {
    expect(hasFreshArrival([item('bad', 'not-a-date')], new Set())).toBe(false)
  })
})

// ============================================================================
// board presence（handleMessage が board 状態を正しく持つ — event は action 層で対象外）
// ============================================================================

describe('board presence（board 状態の反映）', () => {
  it('items 到着で board が非空になる（presence event の元 = items.length>0）', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }]))
    expect(getCanvasState().items).toHaveLength(1)
  })

  it('空 board 受信で board が空になる', () => {
    handleMessage(boardUpdated('lane', null, [{ id: 'a' }]))
    handleMessage(boardUpdated('lane', null, []))
    expect(getCanvasState().items).toHaveLength(0)
  })
})

// ============================================================================
// wave 3 計器盤（doc 52 §5）: 未読 dot / 鮮度 の純関数
// ============================================================================

describe('computeUnread（cursor に流されなかった新着 = 未読 dot）', () => {
  const ids = (s: Iterable<string>) => [...s].sort()

  it('cursor が follow した新着は未読にしない（cursor 自身）', () => {
    // 新着 B に cursor が follow → B は表示中なので dot 不要
    const u = computeUnread(new Set(), new Set(['b', 'a']), ['b'], 'b')
    expect(ids(u)).toEqual([])
  })

  it('cursor 据え置きで届いた新着は未読になる（scrollback の据え置き）', () => {
    // cursor は古い A、新着 C が届いた → C は未読
    const u = computeUnread(new Set(), new Set(['c', 'b', 'a']), ['c'], 'a')
    expect(ids(u)).toEqual(['c'])
  })

  it('前回の未読を引き継ぐ（存在する id かつ cursor 以外）', () => {
    const u = computeUnread(new Set(['x', 'gone']), new Set(['x', 'a']), [], 'a')
    expect(ids(u)).toEqual(['x']) // gone は消滅、x は残る
  })

  it('cursor に移った未読は既読化される', () => {
    const u = computeUnread(new Set(['x']), new Set(['x', 'a']), [], 'x')
    expect(ids(u)).toEqual([]) // cursor が x を指す = 既読
  })
})

describe('freshNewIds（live 新着の id — 未読 / focus の一次ソース）', () => {
  const FUTURE = new Date(Date.now() + 60_000).toISOString()
  const PAST = new Date(Date.now() - 60_000).toISOString()
  const it_ = (id: string, createdAt: string): BoardItem => ({
    id,
    content: '',
    contentType: 'markdown',
    createdAt,
  })

  it('未知かつ BOOT 後生成のみ拾う', () => {
    expect(freshNewIds([it_('new', FUTURE), it_('old', PAST)], new Set())).toEqual(['new'])
  })

  it('既知 id（prevIds）は replay 扱いで拾わない', () => {
    expect(freshNewIds([it_('x', FUTURE)], new Set(['x']))).toEqual([])
  })
})

describe('formatFreshness（額縁の鮮度表示）', () => {
  const base: BoardItem = { id: 'a', content: '', contentType: 'markdown', createdAt: '2026-07-24T10:00:00Z' }

  it('updatedAt を優先して「更新 …」で出す', () => {
    const s = formatFreshness({ ...base, updatedAt: '2026-07-24T11:30:00Z' })
    expect(s.startsWith('更新 ')).toBe(true)
  })

  it('updatedAt が無ければ createdAt に fallback（旧 item）', () => {
    expect(formatFreshness(base).startsWith('更新 ')).toBe(true)
  })

  it('item 無し / parse 不能は空文字（額縁に嘘を出さない）', () => {
    expect(formatFreshness(undefined)).toBe('')
    expect(formatFreshness({ ...base, createdAt: 'not-a-date', updatedAt: undefined })).toBe('')
  })
})
