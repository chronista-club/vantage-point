// engine 別 settings panel の束ね役 — 表の中身と IPC の形を固定する。
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ENGINE_SETTINGS_PANELS } from './engine-settings-panel'
import { postSettings } from './engine-settings-shared'

describe('engine settings panel table', () => {
  it('panel を持つ engine は claude / codex / vpcode の 3 つ（grok / opencode は read-only に落ちる）', () => {
    expect(Object.keys(ENGINE_SETTINGS_PANELS).sort()).toEqual(['claude', 'codex', 'vpcode'])
  })
})

describe('postSettings', () => {
  const win = globalThis as unknown as { ipc?: { postMessage(m: string): void } }
  afterEach(() => { delete win.ipc })

  it('settings を engine 所有の形のまま conversation:set_settings で送る', () => {
    const postMessage = vi.fn()
    win.ipc = { postMessage }
    expect(postSettings({ lane: 'vp/main', session: 2 }, { codex: { model: 'gpt-5', effort: 'high' } }, 'req-1')).toBe(true)
    expect(JSON.parse(postMessage.mock.calls[0][0])).toEqual({
      t: 'conversation:set_settings', lane: 'vp/main', session: 2,
      settings: { codex: { model: 'gpt-5', effort: 'high' } }, request_id: 'req-1',
    })
  })

  it('null = engine 既定へ戻す / IPC 不在は false', () => {
    const postMessage = vi.fn()
    win.ipc = { postMessage }
    expect(postSettings({ lane: 'vp/main', session: 1 }, null)).toBe(true)
    expect(JSON.parse(postMessage.mock.calls[0][0]).settings).toBeNull()
    delete win.ipc
    expect(postSettings({ lane: 'vp/main', session: 1 }, { claude: {} })).toBe(false)
  })
})
