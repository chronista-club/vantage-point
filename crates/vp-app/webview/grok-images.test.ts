// mem_1ChKFMEwUYWj5Qs6VLVkU4 — Grok の roster 能力表から貼り付け・通常送信まで。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window } from 'happy-dom'

it.each([true, false])('Grok image paste follows roster capability %s', async (capable) => {
  const result = await build({
    stdin: { contents: `
      import { installChatView } from './chatview'
      window.sent = []; window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) }
      const api = installChatView({ attachRenderer: () => {} })
      api.showLane('images/grok')
      document.dispatchEvent(new CustomEvent('vp:conversation-sessions', { detail: {
        lane: 'images/grok', focused: 1, sessions: [{ key: 1, agent: 'grok', kind: 'chat',
        root: true, image_capable: ${capable}, model_choices: [], permission_choices: [] }]
      }}))
      api.mountSession(document.body, 'images/grok', 1)
    `, resolveDir: process.cwd(), loader: 'tsx' },
    bundle: true, write: false, format: 'iife', conditions: ['browser'],
    preserveSymlinks: true, plugins: [solidPlugin()],
  })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const input = window.document.querySelector('textarea')!
    const file = new window.File(['hello'], 'test.png', { type: 'image/png' })
    const event = new window.Event('paste', { bubbles: true, cancelable: true })
    Object.defineProperty(event, 'clipboardData', { value: {
      items: [{ kind: 'file', type: 'image/png', getAsFile: () => file }],
    } })
    input.dispatchEvent(event)
    await expect.poll(() => window.document.querySelectorAll('.conversation-attachment').length)
      .toBe(capable ? 1 : 0)
    expect(event.defaultPrevented).toBe(capable)
    input.value = 'describe'
    input.dispatchEvent(new window.Event('input', { bubbles: true }))
    input.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
    const app = window as unknown as { sent: unknown[] }
    expect(app.sent.at(-1)).toMatchObject({
      t: 'conversation:submit', lane: 'images/grok', session: 1, prompt: 'describe',
      images: capable ? [{ media_type: 'image/png', data: 'aGVsbG8=' }] : [],
    })
  } finally { await window.happyDOM.close() }
}, 20000)
