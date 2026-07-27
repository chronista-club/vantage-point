/**
 * LaneHeader — 共通ヘッダの純関数 + console.ts の header summary 畳み込みのテスト。
 *
 * component 本体（mountLaneHeader）は solid-js/web の render を呼ぶため vitest(node) では
 * 対象外。ここは presence-driven / copy 表示 / 途絶検知の判定を支える純関数だけを固める。
 */
import { describe, it, expect } from 'vitest'
import {
  tildify,
  middleEllipsis,
  laneShortName,
  sessionChipPrefix,
  rootPickerItems,
} from './LaneHeader'
import { foldHeaderState, type LaneHeaderState } from './console'
import type { ConversationEvent } from './console'

describe('tildify — $HOME prefix を ~ に畳む', () => {
  it('Mac の /Users/<u> を ~ にする', () => {
    expect(tildify('/Users/mako/repos/vp')).toBe('~/repos/vp')
  })
  it('Linux の /home/<u> を ~ にする', () => {
    expect(tildify('/home/mako/repos/vp')).toBe('~/repos/vp')
  })
  it('home 直下ちょうど（末尾）も畳む', () => {
    expect(tildify('/Users/mako')).toBe('~')
  })
  it('home 外の絶対 path は素通し', () => {
    expect(tildify('/opt/vp/lane')).toBe('/opt/vp/lane')
  })
})

describe('middleEllipsis — 長い path を頭残し末尾厚めで中略', () => {
  it('maxLen 以内はそのまま', () => {
    expect(middleEllipsis('~/repos/vp', 42)).toBe('~/repos/vp')
  })
  it('超過時は … を含み maxLen を超えない', () => {
    const long = '~/repos/vantage-point/.vp/lanes/lane-header/crates/vp-app'
    const out = middleEllipsis(long, 30)
    expect(out.length).toBeLessThanOrEqual(30)
    expect(out).toContain('…')
    // 末尾（情報が濃い side）を保つ
    expect(out.endsWith('vp-app')).toBe(true)
  })
})

describe('laneShortName — address から表示短名', () => {
  it('conductor', () => {
    expect(laneShortName('vantage-point/root')).toBe('conductor')
  })
  it('performer は name 部分', () => {
    expect(laneShortName('vantage-point/performer/lane-header')).toBe('lane-header')
  })
  it('legacy lead / wing も受理', () => {
    expect(laneShortName('vp/lead')).toBe('conductor')
    expect(laneShortName('vp/wing/foo')).toBe('foo')
  })
})

describe('foldHeaderState — session summary の畳み込み（変化検知）', () => {
  // doc 50: 名札の縮約で summary は sessionId 1 本になった。model / perm は composer、
  // engine 異常は status 行（deriveStatus）が別経路で担うので、ここでは畳まない。
  const sessionInit = (over: Partial<Extract<ConversationEvent, { kind: 'session_init' }>> = {}): ConversationEvent => ({
    kind: 'session_init',
    session_id: 'sid-1',
    ...over,
  })

  it('session_init は sessionId を畳み、変化ありで true', () => {
    const h: LaneHeaderState = {}
    expect(foldHeaderState(h, sessionInit())).toBe(true)
    expect(h.sessionId).toBe('sid-1')
  })

  it('同値 session_init は冪等 = false（無駄な再描画を出さない）', () => {
    const h: LaneHeaderState = {}
    foldHeaderState(h, sessionInit())
    expect(foldHeaderState(h, sessionInit())).toBe(false)
  })

  it('turn_completed も sessionId を追従する（resume で id が変わる経路）', () => {
    const h: LaneHeaderState = { sessionId: 'sid-1' }
    expect(foldHeaderState(h, { kind: 'turn_completed', session_id: 'sid-2' })).toBe(true)
    expect(h.sessionId).toBe('sid-2')
    // 同値なら冪等
    expect(foldHeaderState(h, { kind: 'turn_completed', session_id: 'sid-2' })).toBe(false)
  })

  it('高頻度 event（message_chunk）は summary を変えず false（ヘッダ再描画を出さない）', () => {
    const h: LaneHeaderState = { sessionId: 'sid-1' }
    const chunk: ConversationEvent = { kind: 'message_chunk', text: 'hi' }
    expect(foldHeaderState(h, chunk)).toBe(false)
    expect(h.sessionId).toBe('sid-1')
  })
})

describe('sessionChipPrefix — session chip の engine 別 prefix（doc 37）', () => {
  it('claude（conversation / 旧名 hd）は歴史的な cc を維持する', () => {
    expect(sessionChipPrefix('claude')).toBe('cc')
    expect(sessionChipPrefix('hd')).toBe('cc')
  })
  it('codex / grok / opencode は engine 別 prefix（chip が engine indicator を兼ねる）', () => {
    expect(sessionChipPrefix('codex')).toBe('cdx')
    expect(sessionChipPrefix('grok')).toBe('grok')
    expect(sessionChipPrefix('opencode')).toBe('oc')
  })
  it('撤去済み engine（cursor / agy）と未知 / 欠落 agent は中立の sid（graceful degradation）', () => {
    // sweep 6.5: cursor / agy は engine として撤去。disk / wire に残る旧 agent 文字列は sid に倒れる。
    expect(sessionChipPrefix('cursor')).toBe('sid')
    expect(sessionChipPrefix('agy')).toBe('sid')
    expect(sessionChipPrefix('shell')).toBe('sid')
    expect(sessionChipPrefix(null)).toBe('sid')
    expect(sessionChipPrefix(undefined)).toBe('sid')
  })
})

describe('rootPickerItems — Root 切替 picker の行導出（doc 39 P3 → P4）', () => {
  it('engine prefix + 会話 id 先頭 8 桁で行を作り、root flag と登録順を保つ', () => {
    const items = rootPickerItems([
      {
        key: 1,
        agent: 'claude',
        engine_session_id: '3d91933b-aaaa-bbbb',
        live: true,
        focused: false,
        root: true,
      },
      { key: 2, agent: 'codex', engine_session_id: '0199a2ffee', live: false, focused: true },
    ])
    expect(items).toEqual([
      { key: 1, label: 'cc:3d91933b', isRoot: true, disabled: false },
      { key: 2, label: 'cdx:0199a2ff', isRoot: false, disabled: false },
    ])
  })
  it('会話 id 未発行（Draft / 未発話）は「新品」、root 欠落（旧 SP）は非 root 扱い', () => {
    const items = rootPickerItems([
      { key: 3, agent: 'grok', engine_session_id: null, live: false, focused: false },
    ])
    expect(items).toEqual([{ key: 3, label: 'grok:新品', isRoot: false, disabled: false }])
  })
  it('cross-engine 行は enabled、未知 engine（撤去済み cursor）行だけ disabled（doc 39 P4）', () => {
    const items = rootPickerItems([
      { key: 1, agent: 'hd', engine_session_id: 'aaaa1111', live: true, focused: true, root: true },
      { key: 2, agent: 'codex', engine_session_id: 'bbbb2222', live: false, focused: false },
      { key: 3, agent: 'cursor', engine_session_id: null, live: false, focused: false },
    ])
    // cross-engine（codex）は P4 で解禁 = enabled、legacy/撤去済み agent（cursor → prefix sid）のみ disabled。
    expect(items.map((i) => i.disabled)).toEqual([false, false, true])
  })
})
