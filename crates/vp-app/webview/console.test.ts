/**
 * console.ts — doc 38 Phase 2 の session routing テスト。
 *
 * 固めるのは 3 点（1 Lane = N session の要）:
 *  - handleEvent の session 既定（未指定 = 1 = focused = 旧 SP 互換）で renderer に渡す
 *  - handleSessionList の CustomEvent 中継（cache 取り込み + 'vp:echoes-sessions' 発火）
 *  - focusedOf の既定（未知 lane = 1）
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
  focusedOf,
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
})
