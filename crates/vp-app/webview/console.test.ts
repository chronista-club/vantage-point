/**
 * console.ts — doc 38 Phase 2 の session routing テスト。
 *
 * 固めるのは 3 点（1 Lane = N session の要）:
 *  - handleEvent の session 既定（未指定 = 1 = focused = 旧 SP 互換）で renderer に渡す
 *  - handleSessionList の CustomEvent 中継（cache 取り込み + 'vp:echoes-sessions' 発火）
 *  - focusedOf の既定（未知 lane = 1）
 *
 * 加えて doc 47 §6（共有 bus の相関 id）: 'vp:echoes-stands' は複数の「+」menu が購読する
 * broadcast なので、**別の要求元の応答では発火しない**ことを固定する。
 *
 *
 * 純関数（normalizeSession / noteSessionList / noteFocus / focusedOf）は document 非依存。
 * CustomEvent を伴う facade メソッドは最小の DOM stub（document + window + CustomEvent）を
 * globalThis に据えて検証する（vitest = node env のため native DOM が無い）。
 * NB: module-level cache（laneSessions / lanes buffer）はテスト間で持続するので、各 it は
 * 一意な lane 名を使って相互汚染を避ける。
 */
import { describe, it, expect, beforeEach } from 'vitest'
import {
  installConsole,
  normalizeSession,
  noteSessionList,
  noteFocus,
  noteSessionAct,
  sessionActOf,
  focusedOf,
  syncHeaderSessionId,
  nextRequestId,
  isMyResponse,
  type BusRequestId,
  type EchoesStandsDetail,
} from './console'

// --- 最小 DOM stub -----------------------------------------------------------------------------
type Listener = (e: { type: string; detail?: unknown }) => void
let listeners: Map<string, Listener[]>

function installDomStub(): void {
  listeners = new Map()
  const doc = {
    addEventListener(type: string, cb: Listener) {
      const a = listeners.get(type) ?? []
      a.push(cb)
      listeners.set(type, a)
    },
    removeEventListener(type: string, cb: Listener) {
      const a = listeners.get(type)
      if (a) listeners.set(type, a.filter((f) => f !== cb))
    },
    dispatchEvent(e: { type: string }) {
      for (const cb of listeners.get(e.type) ?? []) cb(e as { type: string; detail?: unknown })
      return true
    },
  }
  class FakeCustomEvent<T> {
    type: string
    detail: T
    constructor(type: string, init?: { detail?: T }) {
      this.type = type
      this.detail = init?.detail as T
    }
  }
  const g = globalThis as unknown as {
    window: unknown
    document: unknown
    CustomEvent: unknown
  }
  g.window = g.window ?? {}
  g.document = doc
  g.CustomEvent = FakeCustomEvent
}

beforeEach(() => installDomStub())

describe('normalizeSession — envelope session の正規化（未指定 = 1）', () => {
  it('未指定は 1（旧 SP / N=1 の後方互換）', () => {
    expect(normalizeSession(undefined)).toBe(1)
  })
  it('指定値はそのまま', () => {
    expect(normalizeSession(2)).toBe(2)
    expect(normalizeSession(1)).toBe(1)
  })
})

describe('focusedOf / noteSessionList / noteFocus — per-lane focused registry', () => {
  it('未知 lane は既定 1', () => {
    expect(focusedOf('proj/unknown-lane-a')).toBe(1)
  })
  it('noteSessionList が focused を反映する', () => {
    noteSessionList('proj/lane-b', 3, [
      { key: 1, stand: 'echoes', engine_session_id: null, live: false, focused: false },
      { key: 3, stand: 'codex', engine_session_id: 'abc', live: true, focused: true },
    ])
    expect(focusedOf('proj/lane-b')).toBe(3)
  })
  it('noteFocus が楽観的に focused を切替える（cache 未作成でも作る）', () => {
    noteFocus('proj/lane-c', 2)
    expect(focusedOf('proj/lane-c')).toBe(2)
  })
})

describe('sessionActOf / noteSessionAct — act の読み手 cache', () => {
  it('未知 lane / 未知 session は tui に倒す（旧 SP wire 互換の既定）', () => {
    expect(sessionActOf('proj/unknown-act-lane', 1)).toBe('tui')
  })

  it('**API の setSessionAct 経由で即座に読める**（full fetch を待たない）', () => {
    // ⚠️ 検証は必ず **`setSessionAct`（実際の書き手）** を通す。helper（`noteSessionAct`）を
    // 直接呼ぶ形だと、`setSessionAct` が helper を呼び忘れていても緑になる — 実際に最初は
    // そう書いてしまい、fix を外しても落ちなかった（罠を検出しないテスト、4 回目）。
    //
    // 落ちるべき壊し方 = `setSessionAct` から cache 更新を外す。それを検出する。
    // `laneSessions` は `echoes_session_list` の full fetch でしか埋まらないが、act 切替は
    // その fetch を伴わない — 更新漏れは `ink.ts` の誤配送（畳まれた PtySlot へ term:write が
    // 飛んで黙って消える）になる（team-b 9 回目 2026-07-25 score 92）。
    const con = installConsole()
    noteSessionList('proj/act-lane', 5, [
      { key: 5, stand: 'echoes', engine_session_id: null, live: false, focused: true, act: 'tui' },
    ])
    expect(sessionActOf('proj/act-lane', 5)).toBe('tui')

    con.setSessionAct('proj/act-lane', 5, 'chat')
    expect(sessionActOf('proj/act-lane', 5)).toBe('chat')

    // 逆向きも（chat→tui の直後に ink を送ると echoes:submit が飛ぶ側）。
    con.setSessionAct('proj/act-lane', 5, 'tui')
    expect(sessionActOf('proj/act-lane', 5)).toBe('tui')
  })

  it('一覧を知らない lane では no-op（次の full fetch が埋める）', () => {
    noteSessionAct('proj/act-unknown', 9, 'chat')
    expect(sessionActOf('proj/act-unknown', 9)).toBe('tui')
  })
})

describe('handleEvent — session 既定 1 で renderer に渡す', () => {
  it('session 省略時 renderer は 1 を受ける', () => {
    const con = installConsole()
    const got: number[] = []
    con.attachRenderer('proj/lane-d', (_ev, session) => got.push(session))
    con.handleEvent('proj/lane-d', { kind: 'message_chunk', text: 'x' })
    expect(got).toEqual([1])
  })
  it('session 指定時はその値を渡す', () => {
    const con = installConsole()
    const got: number[] = []
    con.attachRenderer('proj/lane-e', (_ev, session) => got.push(session))
    con.handleEvent('proj/lane-e', { kind: 'message_chunk', text: 'y' }, 2)
    expect(got).toEqual([2])
  })
})

describe('handleSessionList — cache 取り込み + vp:echoes-sessions 発火', () => {
  it('focused を registry に反映し CustomEvent を detail 付きで発火する', () => {
    const con = installConsole()
    let detail: { lane?: string; focused?: number; sessions?: unknown[] } | null = null
    document.addEventListener('vp:echoes-sessions', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleSessionList('proj/lane-f', {
      lane: 'proj/lane-f',
      focused: 2,
      sessions: [
        { key: 1, stand: 'echoes', engine_session_id: null, live: false, focused: false },
        { key: 2, stand: 'echoes', engine_session_id: null, live: true, focused: true },
      ],
    })
    expect(focusedOf('proj/lane-f')).toBe(2)
    expect(detail).not.toBeNull()
    expect(detail!.lane).toBe('proj/lane-f')
    expect(detail!.focused).toBe(2)
    expect(detail!.sessions).toHaveLength(2)
  })
  it('focused=null（session 無し）は既定 1 に解決する', () => {
    const con = installConsole()
    con.handleSessionList('proj/lane-g', { focused: null, sessions: [] })
    expect(focusedOf('proj/lane-g')).toBe(1)
  })
})

describe('handleStands — vp:echoes-stands で中継', () => {
  it('stands を detail に載せて発火する', () => {
    const con = installConsole()
    let detail: { lane?: string; stands?: unknown[] } | null = null
    document.addEventListener('vp:echoes-stands', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleStands('proj/lane-h', { stands: [{ name: 'echoes' }, { name: 'codex' }] })
    expect(detail).not.toBeNull()
    expect(detail!.lane).toBe('proj/lane-h')
    expect(detail!.stands).toHaveLength(2)
  })
  it('req を detail に載せる（要求元タグの往復）', () => {
    const con = installConsole()
    let detail: EchoesStandsDetail | null = null
    document.addEventListener('vp:echoes-stands', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleStands('proj/lane-h2', { stands: [] }, 'pane-new#7')
    expect(detail!.req).toBe('pane-new#7')
  })
  it('req 省略時は null（要求外の発火 = 誰も拾わない）', () => {
    const con = installConsole()
    let detail: EchoesStandsDetail | null = null
    document.addEventListener('vp:echoes-stands', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleStands('proj/lane-h3', { stands: [] })
    expect(detail!.req).toBeNull()
  })
})

// --- doc 47 §6: 共有 bus の相関 id ---------------------------------------------------------------
describe('nextRequestId / isMyResponse — 共有 bus の要求元タグ', () => {
  it('採番は scope prefix 付きで毎回異なる', () => {
    const a = nextRequestId('pane-new')
    const b = nextRequestId('pane-new')
    expect(a.startsWith('pane-new#')).toBe(true)
    expect(a).not.toBe(b)
  })
  it('scope が違えば当然一致しない', () => {
    expect(nextRequestId('pane-new')).not.toBe(nextRequestId('chat-add'))
  })
  it('自分の id と一致した時だけ true', () => {
    expect(isMyResponse('pane-new#1', 'pane-new#1')).toBe(true)
    expect(isMyResponse('pane-new#1', 'chat-add#2')).toBe(false)
  })
  it('要求していない購読側（pending=null）は req 無し応答でも反応しない', () => {
    // 素の `===` にすると null === null で一致してしまう罠を固定する。
    expect(isMyResponse(null, null)).toBe(false)
    expect(isMyResponse(null, undefined)).toBe(false)
    expect(isMyResponse(null, 'pane-new#1')).toBe(false)
  })
})

describe('vp:echoes-stands — 別の要求元の応答では発火しない（#838 の凌ぎの根治）', () => {
  /** 「+」menu 相当の購読側。要求を出し、自分の応答でだけ open する。 */
  function subscriber(scope: string) {
    const opened: unknown[][] = []
    let pending: BusRequestId | null = null
    document.addEventListener('vp:echoes-stands', (e) => {
      const d = (e as CustomEvent<EchoesStandsDetail>).detail
      if (!isMyResponse(pending, d?.req)) return
      pending = null
      opened.push(d.stands)
    })
    return {
      opened,
      request(): BusRequestId {
        pending = nextRequestId(scope)
        return pending
      },
    }
  }

  it('Pane の「+ New」の応答で chat の「+」menu は開かない', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    const chatAdd = subscriber('chat-add')
    const req = paneNew.request() // 要求したのは Pane 側だけ
    con.handleStands('proj/lane-req-a', { stands: [{ name: 'echoes' }] }, req)
    expect(paneNew.opened).toHaveLength(1)
    expect(chatAdd.opened).toHaveLength(0)
  })

  it('chat の「+」の応答で Pane の「+ New」menu は開かない（逆向きも）', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    const chatAdd = subscriber('chat-add')
    const req = chatAdd.request()
    con.handleStands('proj/lane-req-b', { stands: [{ name: 'codex' }] }, req)
    expect(chatAdd.opened).toHaveLength(1)
    expect(paneNew.opened).toHaveLength(0)
  })

  it('両方が要求中でも、応答は id が一致した側だけに届く', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    const chatAdd = subscriber('chat-add')
    paneNew.request()
    const chatReq = chatAdd.request()
    con.handleStands('proj/lane-req-c', { stands: [] }, chatReq)
    expect(chatAdd.opened).toHaveLength(1)
    expect(paneNew.opened).toHaveLength(0) // 要求中でも他人の応答では開かない
  })

  it('同じ要求元でも古い id の応答は捨てる（連打の遅延応答）', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    const stale = paneNew.request()
    paneNew.request() // 2 回目の click で pending が更新される
    con.handleStands('proj/lane-req-d', { stands: [] }, stale)
    expect(paneNew.opened).toHaveLength(0)
  })

  it('req 無しの応答はどの購読側も拾わない', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    paneNew.request()
    con.handleStands('proj/lane-req-e', { stands: [] })
    expect(paneNew.opened).toHaveLength(0)
  })
})

describe('syncHeaderSessionId — chip は focused session の真値に追従（D1）', () => {
  it('draft (engine_session_id=null) を focus すると sessionId が消える', () => {
    const con = installConsole()
    // 旧 session の session_init で chip が付いた状態を作る
    con.handleEvent(
      'proj/lane-sync-a',
      { kind: 'session_init', session_id: 'old-id' },
      1,
    )
    expect(con.headerState('proj/lane-sync-a').sessionId).toBe('old-id')
    // 新 draft (key 2, id なし) を focus した list が届く
    noteSessionList('proj/lane-sync-a', 2, [
      { key: 1, stand: 'echoes', engine_session_id: 'old-id', live: true, focused: false },
      { key: 2, stand: 'echoes', engine_session_id: null, live: false, focused: true },
    ])
    expect(syncHeaderSessionId('proj/lane-sync-a')).toBe(true)
    expect(con.headerState('proj/lane-sync-a').sessionId).toBeUndefined()
  })

  it('id 持ち session への切替は chip がその id になる（変化なしなら false）', () => {
    const con = installConsole()
    noteSessionList('proj/lane-sync-b', 3, [
      { key: 1, stand: 'echoes', engine_session_id: 'aaa', live: false, focused: false },
      { key: 3, stand: 'codex', engine_session_id: 'ccc', live: true, focused: true },
    ])
    expect(syncHeaderSessionId('proj/lane-sync-b')).toBe(true)
    expect(con.headerState('proj/lane-sync-b').sessionId).toBe('ccc')
    // 同じ値への再同期は変化なし
    expect(syncHeaderSessionId('proj/lane-sync-b')).toBe(false)
  })

  it('handleSessionList 経由で自動同期され vp:echoes-header が飛ぶ', () => {
    const con = installConsole()
    con.handleEvent(
      'proj/lane-sync-c',
      { kind: 'session_init', session_id: 'stale-id' },
      1,
    )
    let headerFired = 0
    document.addEventListener('vp:echoes-header', () => {
      headerFired += 1
    })
    con.handleSessionList('proj/lane-sync-c', {
      lane: 'proj/lane-sync-c',
      focused: 2,
      sessions: [
        { key: 1, stand: 'echoes', engine_session_id: 'stale-id', live: true, focused: false },
        { key: 2, stand: 'echoes', engine_session_id: null, live: false, focused: true },
      ],
    })
    expect(con.headerState('proj/lane-sync-c').sessionId).toBeUndefined()
    expect(headerFired).toBe(1)
  })

  it('未知 lane は no-op（false）', () => {
    expect(syncHeaderSessionId('proj/lane-sync-unknown')).toBe(false)
  })
})
