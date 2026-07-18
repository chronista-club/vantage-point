/**
 * EchoesHeader — 共通ヘッダの純関数 + console.ts の header summary 畳み込みのテスト。
 *
 * component 本体（mountEchoesHeader）は solid-js/web の render を呼ぶため vitest(node) では
 * 対象外。ここは presence-driven / copy 表示 / 途絶検知の判定を支える純関数だけを固める。
 */
import { describe, it, expect } from 'vitest'
import {
  tildify,
  middleEllipsis,
  laneShortName,
  permModeLabel,
  sessionChipPrefix,
  rootPickerItems,
} from './EchoesHeader'
import { foldHeaderState, type EchoesHeaderState } from './console'
import type { EchoesEvent } from './console'

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
    const long = '~/repos/vantage-point/.vp/lanes/echoes-header/crates/vp-app'
    const out = middleEllipsis(long, 30)
    expect(out.length).toBeLessThanOrEqual(30)
    expect(out).toContain('…')
    // 末尾（情報が濃い side）を保つ
    expect(out.endsWith('vp-app')).toBe(true)
  })
})

describe('laneShortName — address から表示短名', () => {
  it('conductor', () => {
    expect(laneShortName('vantage-point/conductor')).toBe('conductor')
  })
  it('performer は name 部分', () => {
    expect(laneShortName('vantage-point/performer/echoes-header')).toBe('echoes-header')
  })
  it('legacy lead / wing も受理', () => {
    expect(laneShortName('vp/lead')).toBe('conductor')
    expect(laneShortName('vp/wing/foo')).toBe('foo')
  })
})

describe('permModeLabel — 長い canonical 名だけ縮める', () => {
  it('bypassPermissions → bypass', () => {
    expect(permModeLabel('bypassPermissions')).toBe('bypass')
  })
  it('未知値は素通し', () => {
    expect(permModeLabel('default')).toBe('default')
    expect(permModeLabel('plan')).toBe('plan')
  })
})

describe('foldHeaderState — session summary の畳み込み（変化検知）', () => {
  const sessionInit = (over: Partial<Extract<EchoesEvent, { kind: 'session_init' }>> = {}): EchoesEvent => ({
    kind: 'session_init',
    session_id: 'sid-1',
    model: 'claude',
    permission_mode: 'default',
    ...over,
  })

  it('session_init は sessionId / model / perm を畳み、変化ありで true', () => {
    const h: EchoesHeaderState = {}
    expect(foldHeaderState(h, sessionInit())).toBe(true)
    expect(h.sessionId).toBe('sid-1')
    expect(h.model).toBe('claude')
    expect(h.permissionMode).toBe('default')
    expect(h.engineError).toBeUndefined()
  })

  it('同値 session_init は冪等 = false（無駄な再描画を出さない）', () => {
    const h: EchoesHeaderState = {}
    foldHeaderState(h, sessionInit())
    expect(foldHeaderState(h, sessionInit())).toBe(false)
  })

  it('error は engineError を立て、true', () => {
    const h: EchoesHeaderState = { sessionId: 'sid-1' }
    const err: EchoesEvent = { kind: 'error', message: 'エンジンとの接続が途絶しました' }
    expect(foldHeaderState(h, err)).toBe(true)
    expect(h.engineError).toBe('エンジンとの接続が途絶しました')
  })

  it('error 後の session_init は engineError を clear（engine 復帰）', () => {
    const h: EchoesHeaderState = {}
    foldHeaderState(h, { kind: 'error', message: '途絶' })
    expect(foldHeaderState(h, sessionInit({ session_id: 'sid-2' }))).toBe(true)
    expect(h.engineError).toBeUndefined()
    expect(h.sessionId).toBe('sid-2')
  })

  it('turn_completed は生存証拠として engineError を clear', () => {
    const h: EchoesHeaderState = { sessionId: 'sid-1', engineError: '途絶' }
    const done: EchoesEvent = { kind: 'turn_completed', session_id: 'sid-1' }
    expect(foldHeaderState(h, done)).toBe(true)
    expect(h.engineError).toBeUndefined()
  })

  it('高頻度 event（message_chunk）は summary を変えず false（ヘッダ再描画を出さない）', () => {
    const h: EchoesHeaderState = { sessionId: 'sid-1' }
    const chunk: EchoesEvent = { kind: 'message_chunk', text: 'hi' }
    expect(foldHeaderState(h, chunk)).toBe(false)
    expect(h.sessionId).toBe('sid-1')
  })
})

describe('sessionChipPrefix — session chip の engine 別 prefix（doc 37）', () => {
  it('claude（echoes / 旧名 hd）は歴史的な cc を維持する', () => {
    expect(sessionChipPrefix('echoes')).toBe('cc')
    expect(sessionChipPrefix('hd')).toBe('cc')
  })
  it('cursor / codex / grok / agy は engine 別 prefix（chip が engine indicator を兼ねる）', () => {
    expect(sessionChipPrefix('cursor')).toBe('cur')
    expect(sessionChipPrefix('codex')).toBe('cdx')
    expect(sessionChipPrefix('grok')).toBe('grok')
    expect(sessionChipPrefix('agy')).toBe('agy')
  })
  it('未知 / 欠落 stand は中立の sid（chip は出せるが engine は主張しない）', () => {
    expect(sessionChipPrefix('shell')).toBe('sid')
    expect(sessionChipPrefix(null)).toBe('sid')
    expect(sessionChipPrefix(undefined)).toBe('sid')
  })
})

describe('rootPickerItems — Root 切替 picker の行導出（doc 39 P3）', () => {
  it('engine prefix + 会話 id 先頭 8 桁で行を作り、root flag と登録順を保つ', () => {
    const items = rootPickerItems([
      {
        key: 1,
        stand: 'echoes',
        engine_session_id: '3d91933b-aaaa-bbbb',
        live: true,
        focused: false,
        root: true,
      },
      { key: 2, stand: 'codex', engine_session_id: '0199a2ffee', live: false, focused: true },
    ])
    expect(items).toEqual([
      { key: 1, label: 'cc:3d91933b', isRoot: true },
      { key: 2, label: 'cdx:0199a2ff', isRoot: false },
    ])
  })
  it('会話 id 未発行（Draft / 未発話）は「新品」、root 欠落（旧 SP）は非 root 扱い', () => {
    const items = rootPickerItems([
      { key: 3, stand: 'grok', engine_session_id: null, live: false, focused: false },
    ])
    expect(items).toEqual([{ key: 3, label: 'grok:新品', isRoot: false }])
  })
})
