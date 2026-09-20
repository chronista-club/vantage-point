// mem_1CfFDAworjWMfDtjW5sPud — 照合待ちを会話末尾へ再挿入しない。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window, type HTMLDetailsElement } from 'happy-dom'

it('keeps unmatched input outside the timeline without resending, and restores it only on request', async () => {
  const result = await build({ stdin: { contents: `
    import { installChatView, sendSubmission } from './chatview'
    window.sent = []; window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) }
    const api = installChatView({ attachRenderer: (lane, fn) => window.emit = fn })
    api.showLane('unconfirmed/main')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions', { detail: {
      lane: 'unconfirmed/main', focused: 1, sessions: [
        { key: 1, agent: 'codex', kind: 'chat', root: true, model_choices: [], permission_choices: [] }
      ]
    }}))
    api.mountSession(document.body, 'unconfirmed/main', 1)
    sendSubmission('unconfirmed/main', 1, '進めていこう', [])
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false,
  format: 'iife', conditions: ['browser'], plugins: [solidPlugin()] })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const app = window as unknown as { sent: any[]; emit: (event: any, session: number) => void }
    const sent = () => app.sent.filter(m => m.t === 'conversation:submit')
    const id = sent()[0].request_id
    app.emit({ kind: 'submit_result', request_id: id, error: null }, 1)
    for (let i = 0; i < 3; i++) app.emit({ kind: 'codex_history', thread_id: 'thread',
      events: [{ kind: 'user_message', text: 'マージして' }], user_message_ids: [],
      in_flight: false, truncated: false }, 1)
    expect(window.document.querySelector('.conversation-stream')?.textContent).not.toContain('進めていこう')
    const notice = window.document.querySelector<HTMLDetailsElement>('.codex-unconfirmed')!
    expect(notice).not.toBeNull()
    expect(notice.open).toBe(false)
    expect(notice.textContent).toContain('進めていこう')
    expect(sent()).toHaveLength(1)
    notice.querySelector('button')!.click()
    expect(window.document.querySelector('textarea')?.value).toBe('進めていこう')
    expect(sent()).toHaveLength(1)
    window.document.querySelector('button.conversation-send')!.dispatchEvent(new window.MouseEvent('click', { bubbles: true }))
    expect(sent()).toHaveLength(2)
    app.emit({ kind: 'codex_history', thread_id: 'thread',
      events: [{ kind: 'user_message', text: '進めていこう' }], user_message_ids: [sent()[1].request_id],
      in_flight: true, truncated: false }, 1)
    // Native history can confirm the input before the independent submit ACK arrives.
    expect(window.document.querySelector('.conversation-stream')?.textContent?.match(/進めていこう/g)).toHaveLength(1)
  } finally { await window.happyDOM.close() }
}, 20_000)
