/**
 * console.ts — doc 38 Phase 2 の session routing テスト。
 *
 * 固めるのは 3 点（1 Lane = N session の要）:
 *  - handleEvent の session 既定（未指定 = 1 = focused = 旧 SP 互換）で renderer に渡す
 *  - handleSessionList の CustomEvent 中継（cache 取り込み + 'vp:conversation-sessions' 発火）
 *  - focusedOf の既定（未知 lane = 1）
 *
 * 加えて doc 47 §6（共有 bus の相関 id）: 'vp:conversation-agents' は複数の「+」menu が購読する
 * broadcast なので、**別の要求元の応答では発火しない**ことを固定する。
 *
 *
 * 純関数（normalizeSession / noteSessionList / noteFocus / focusedOf）は document 非依存。
 * CustomEvent を伴う facade メソッドは最小の DOM stub（document + window + CustomEvent）を
 * globalThis に据えて検証する（vitest = node env のため native DOM が無い）。
 * NB: module-level cache（laneSessions / lanes buffer）はテスト間で持続するので、各 it は
 * 一意な lane 名を使って相互汚染を避ける。
 */
import { describe, it, expect, beforeEach, vi } from 'vitest'
import {
  installConsole,
  normalizeSession,
  noteSessionList,
  noteFocus,
  noteSessionMode,
  sessionModeOf,
  focusedOf,
  syncHeaderSessionId,
  nextRequestId,
  isMyResponse,
  type BusRequestId,
  type AgentsDetail,
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
  // window 側にも同じ listener 実装を据える（doc 58 ②-a: now-line tee は
  // window event で bundle 間を渡るため。既存テストへは純増）。
  const winListeners = new Map<string, Listener[]>()
  g.window = {
    addEventListener(type: string, cb: Listener) {
      const a = winListeners.get(type) ?? []
      a.push(cb)
      winListeners.set(type, a)
    },
    removeEventListener(type: string, cb: Listener) {
      const a = winListeners.get(type)
      if (a) winListeners.set(type, a.filter((f) => f !== cb))
    },
    dispatchEvent(e: { type: string }) {
      for (const cb of winListeners.get(e.type) ?? []) cb(e as { type: string; detail?: unknown })
      return true
    },
  }
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
      { key: 1, agent: 'claude', engine_session_id: null, live: false, focused: false },
      { key: 3, agent: 'codex', engine_session_id: 'abc', live: true, focused: true },
    ])
    expect(focusedOf('proj/lane-b')).toBe(3)
  })
  it('noteFocus が楽観的に focused を切替える（cache 未作成でも作る）', () => {
    noteFocus('proj/lane-c', 2)
    expect(focusedOf('proj/lane-c')).toBe(2)
  })
})

describe('sessionModeOf / noteSessionMode — mode の読み手 cache', () => {
  it('未知 lane / 未知 session は tui に倒す（旧 SP wire 互換の既定）', () => {
    expect(sessionModeOf('proj/unknown-mode-lane', 1)).toBe('tui')
  })

  it('**API の setSessionMode 経由で即座に読める**（full fetch を待たない）', () => {
    // ⚠️ 検証は必ず **`setSessionMode`（実際の書き手）** を通す。helper（`noteSessionMode`）を
    // 直接呼ぶ形だと、`setSessionMode` が helper を呼び忘れていても緑になる — 実際に最初は
    // そう書いてしまい、fix を外しても落ちなかった（罠を検出しないテスト、4 回目）。
    //
    // 落ちるべき壊し方 = `setSessionMode` から cache 更新を外す。それを検出する。
    // `laneSessions` は `conversation_session_list` の full fetch でしか埋まらないが、mode 切替は
    // その fetch を伴わない — 更新漏れは `ink.ts` の誤配送（畳まれた PtySlot へ term:write が
    // 飛んで黙って消える）になる（team-b 9 回目 2026-07-25 score 92）。
    const con = installConsole()
    noteSessionList('proj/mode-lane', 5, [
      { key: 5, agent: 'claude', engine_session_id: null, live: false, focused: true, mode: 'tui' },
    ])
    expect(sessionModeOf('proj/mode-lane', 5)).toBe('tui')

    con.setSessionMode('proj/mode-lane', 5, 'gui')
    expect(sessionModeOf('proj/mode-lane', 5)).toBe('gui')

    // 逆向きも（chat→tui の直後に ink を送ると conversation:submit が飛ぶ側）。
    con.setSessionMode('proj/mode-lane', 5, 'tui')
    expect(sessionModeOf('proj/mode-lane', 5)).toBe('tui')
  })

  it('一覧を知らない lane では no-op（次の full fetch が埋める）', () => {
    noteSessionMode('proj/mode-unknown', 9, 'gui')
    expect(sessionModeOf('proj/mode-unknown', 9)).toBe('tui')
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

describe('handleSessionList — cache 取り込み + vp:conversation-sessions 発火', () => {
  it('focused を registry に反映し CustomEvent を detail 付きで発火する', () => {
    const con = installConsole()
    let detail: { lane?: string; focused?: number; sessions?: unknown[] } | null = null
    document.addEventListener('vp:conversation-sessions', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleSessionList('proj/lane-f', {
      lane: 'proj/lane-f',
      focused: 2,
      sessions: [
        { key: 1, agent: 'claude', engine_session_id: null, live: false, focused: false },
        { key: 2, agent: 'claude', engine_session_id: null, live: true, focused: true },
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

describe('handleAgents — vp:conversation-agents で中継', () => {
  it('agents を detail に載せて発火する', () => {
    const con = installConsole()
    let detail: { lane?: string; agents?: unknown[] } | null = null
    document.addEventListener('vp:conversation-agents', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleAgents('proj/lane-h', { agents: [{ name: 'claude' }, { name: 'codex' }] })
    expect(detail).not.toBeNull()
    expect(detail!.lane).toBe('proj/lane-h')
    expect(detail!.agents).toHaveLength(2)
  })
  it('req を detail に載せる（要求元タグの往復）', () => {
    const con = installConsole()
    let detail: AgentsDetail | null = null
    document.addEventListener('vp:conversation-agents', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleAgents('proj/lane-h2', { agents: [] }, 'pane-new#7')
    expect(detail!.req).toBe('pane-new#7')
  })
  it('req 省略時は null（要求外の発火 = 誰も拾わない）', () => {
    const con = installConsole()
    let detail: AgentsDetail | null = null
    document.addEventListener('vp:conversation-agents', (e) => {
      detail = (e as CustomEvent).detail
    })
    con.handleAgents('proj/lane-h3', { agents: [] })
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

describe('vp:conversation-agents — 別の要求元の応答では発火しない（#838 の凌ぎの根治）', () => {
  /** 「+」menu 相当の購読側。要求を出し、自分の応答でだけ open する。 */
  function subscriber(scope: string) {
    const opened: unknown[][] = []
    let pending: BusRequestId | null = null
    document.addEventListener('vp:conversation-agents', (e) => {
      const d = (e as CustomEvent<AgentsDetail>).detail
      if (!isMyResponse(pending, d?.req)) return
      pending = null
      opened.push(d.agents)
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
    con.handleAgents('proj/lane-req-a', { agents: [{ name: 'claude' }] }, req)
    expect(paneNew.opened).toHaveLength(1)
    expect(chatAdd.opened).toHaveLength(0)
  })

  it('chat の「+」の応答で Pane の「+ New」menu は開かない（逆向きも）', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    const chatAdd = subscriber('chat-add')
    const req = chatAdd.request()
    con.handleAgents('proj/lane-req-b', { agents: [{ name: 'codex' }] }, req)
    expect(chatAdd.opened).toHaveLength(1)
    expect(paneNew.opened).toHaveLength(0)
  })

  it('両方が要求中でも、応答は id が一致した側だけに届く', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    const chatAdd = subscriber('chat-add')
    paneNew.request()
    const chatReq = chatAdd.request()
    con.handleAgents('proj/lane-req-c', { agents: [] }, chatReq)
    expect(chatAdd.opened).toHaveLength(1)
    expect(paneNew.opened).toHaveLength(0) // 要求中でも他人の応答では開かない
  })

  it('同じ要求元でも古い id の応答は捨てる（連打の遅延応答）', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    const stale = paneNew.request()
    paneNew.request() // 2 回目の click で pending が更新される
    con.handleAgents('proj/lane-req-d', { agents: [] }, stale)
    expect(paneNew.opened).toHaveLength(0)
  })

  it('req 無しの応答はどの購読側も拾わない', () => {
    const con = installConsole()
    const paneNew = subscriber('pane-new')
    paneNew.request()
    con.handleAgents('proj/lane-req-e', { agents: [] })
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
      { key: 1, agent: 'claude', engine_session_id: 'old-id', live: true, focused: false },
      { key: 2, agent: 'claude', engine_session_id: null, live: false, focused: true },
    ])
    expect(syncHeaderSessionId('proj/lane-sync-a')).toBe(true)
    expect(con.headerState('proj/lane-sync-a').sessionId).toBeUndefined()
  })

  it('id 持ち session への切替は chip がその id になる（変化なしなら false）', () => {
    const con = installConsole()
    noteSessionList('proj/lane-sync-b', 3, [
      { key: 1, agent: 'claude', engine_session_id: 'aaa', live: false, focused: false },
      { key: 3, agent: 'codex', engine_session_id: 'ccc', live: true, focused: true },
    ])
    expect(syncHeaderSessionId('proj/lane-sync-b')).toBe(true)
    expect(con.headerState('proj/lane-sync-b').sessionId).toBe('ccc')
    // 同じ値への再同期は変化なし
    expect(syncHeaderSessionId('proj/lane-sync-b')).toBe(false)
  })

  it('handleSessionList 経由で自動同期され vp:lane-header が飛ぶ', () => {
    const con = installConsole()
    con.handleEvent(
      'proj/lane-sync-c',
      { kind: 'session_init', session_id: 'stale-id' },
      1,
    )
    let headerFired = 0
    document.addEventListener('vp:lane-header', () => {
      headerFired += 1
    })
    con.handleSessionList('proj/lane-sync-c', {
      lane: 'proj/lane-sync-c',
      focused: 2,
      sessions: [
        { key: 1, agent: 'claude', engine_session_id: 'stale-id', live: true, focused: false },
        { key: 2, agent: 'claude', engine_session_id: null, live: false, focused: true },
      ],
    })
    expect(con.headerState('proj/lane-sync-c').sessionId).toBeUndefined()
    expect(headerFired).toBe(1)
  })

  it('未知 lane は no-op（false）', () => {
    expect(syncHeaderSessionId('proj/lane-sync-unknown')).toBe(false)
  })
})

describe('now-line tee（doc 58 ②-a）— handleEvent が sidebar 名簿へ流す', () => {
  const catchNow = () => {
    const got: Array<{ lane: string; session: number; text: string | null }> = []
    ;(globalThis as unknown as { window: { addEventListener(t: string, cb: (e: unknown) => void): void } }).window.addEventListener(
      'vp:session-now',
      (e) => got.push((e as { detail: { lane: string; session: number; text: string | null } }).detail),
    )
    return got
  }

  it('renderer 不在（背景 lane）でも now_line は即 emit / turn_completed は null', () => {
    const con = installConsole()
    const got = catchNow()
    // renderer を張らない = showLane していない背景 lane（初版が無音だった形）
    con.handleEvent('tee-bg/lane/main', { kind: 'now_line', text: '実機検証中' } as never, 13)
    expect(got).toEqual([{ lane: 'tee-bg/lane/main', session: 13, text: '実機検証中' }])
    con.handleEvent('tee-bg/lane/main', { kind: 'turn_completed' } as never, 13)
    expect(got[1]).toEqual({ lane: 'tee-bg/lane/main', session: 13, text: null })
  })

  it('replay 中は溜めて replay_end で最終値を一度だけ flush（過去の今を偽らない）', () => {
    const con = installConsole()
    const got = catchNow()
    con.handleEvent('tee-rp/lane/main', { kind: 'replay_start' } as never, 1)
    con.handleEvent('tee-rp/lane/main', { kind: 'now_line', text: '古い今' } as never, 1)
    con.handleEvent('tee-rp/lane/main', { kind: 'now_line', text: '新しい今' } as never, 1)
    expect(got).toEqual([]) // replay 中は無音
    con.handleEvent('tee-rp/lane/main', { kind: 'replay_end' } as never, 1)
    expect(got).toEqual([{ lane: 'tee-rp/lane/main', session: 1, text: '新しい今' }])
  })

  it('replay 内で turn が閉じていれば flush は null（turn より長生きしない）', () => {
    const con = installConsole()
    const got = catchNow()
    con.handleEvent('tee-cl/lane/main', { kind: 'replay_start' } as never, 1)
    con.handleEvent('tee-cl/lane/main', { kind: 'now_line', text: '途中の今' } as never, 1)
    con.handleEvent('tee-cl/lane/main', { kind: 'turn_completed' } as never, 1)
    con.handleEvent('tee-cl/lane/main', { kind: 'replay_end' } as never, 1)
    expect(got).toEqual([{ lane: 'tee-cl/lane/main', session: 1, text: null }])
  })

  it('session が違えば別の「今」/ 関与しない event は無音', () => {
    const con = installConsole()
    const got = catchNow()
    con.handleEvent('tee-s/lane/main', { kind: 'text_chunk', text: 'x' } as never, 1)
    expect(got).toEqual([])
    con.handleEvent('tee-s/lane/main', { kind: 'now_line', text: 'root の今' } as never, 1)
    con.handleEvent('tee-s/lane/main', { kind: 'now_line', text: 'slot 2 の今' } as never, 2)
    expect(got.map((g) => [g.session, g.text])).toEqual([[1, 'root の今'], [2, 'slot 2 の今']])
  })
})

describe('now-line replay watchdog（replay_end 不着の安全網）', () => {
  it('REPLAY_WATCHDOG_MS 経過で追跡値を強制 flush + 以降の now_line は通常配送に復帰', () => {
    vi.useFakeTimers()
    try {
      const con = installConsole()
      const got: Array<{ session: number; text: string | null }> = []
      ;(globalThis as unknown as { window: { addEventListener(t: string, cb: (e: unknown) => void): void } }).window.addEventListener(
        'vp:session-now',
        (e) => got.push((e as { detail: { session: number; text: string | null } }).detail),
      )
      con.handleEvent('tee-wd/lane/main', { kind: 'replay_start' } as never, 1)
      con.handleEvent('tee-wd/lane/main', { kind: 'now_line', text: '中断前の今' } as never, 1)
      expect(got).toEqual([]) // replay 中は飲み込む
      vi.advanceTimersByTime(10_000) // replay_end は来ない
      expect(got).toEqual([{ lane: 'tee-wd/lane/main', session: 1, text: '中断前の今' }]) // 強制 flush
      // 飲み込みの根 (replayingSessions) も消えている = 以降は通常配送
      con.handleEvent('tee-wd/lane/main', { kind: 'now_line', text: '復帰後の今' } as never, 1)
      expect(got[1]).toEqual({ lane: 'tee-wd/lane/main', session: 1, text: '復帰後の今' })
    } finally {
      vi.useRealTimers()
    }
  })
})
