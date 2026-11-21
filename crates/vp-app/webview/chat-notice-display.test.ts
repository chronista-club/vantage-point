// mem_1ChDPjq74fG87DYtSn3ns3 — 実 ChatView の帯の位置・閉じる操作。
import { expect, it } from 'vitest'
import { build } from 'esbuild'
import { solidPlugin } from 'esbuild-plugin-solid'
import { Window } from 'happy-dom'
import type { ConversationEvent } from './console'

it('知らせを内外の帯に描き、閉じる操作と再配信を反映する', async () => {
  const result = await build({ stdin: { contents: `
    import { installChatView } from './chatview'
    window.ipc = { postMessage: () => {} }
    const api = installChatView({ attachRenderer: (lane, fn) => window.emit = fn })
    api.showLane('notice/lead')
    document.dispatchEvent(new CustomEvent('vp:conversation-sessions', { detail: {
      lane: 'notice/lead', focused: 1, sessions: [
        { key: 1, agent: 'agy', kind: 'chat', root: true, model_choices: [], permission_choices: [] }
      ]
    }}))
    api.mountSession(document.body, 'notice/lead', 1)
  `, resolveDir: process.cwd(), loader: 'tsx' }, bundle: true, write: false,
  format: 'iife', conditions: ['browser'], preserveSymlinks: true, plugins: [solidPlugin()] })
  const window = new Window()
  try {
    window.eval(result.outputFiles[0].text)
    const app = window as unknown as { emit: (event: ConversationEvent, session: number) => void }
    const emit = (event: ConversationEvent) => app.emit(event, 1)
    const notices = () => Array.from(window.document.querySelectorAll('.chat-history-notice'))
    const live = '新しい会話を始めました: <b>そのまま表示</b>'
    emit({ kind: 'notice', message: live })
    expect(notices()).toHaveLength(1)
    expect(notices()[0].closest('.conversation-stream')).toBeNull()
    expect(notices()[0].textContent).toContain(live)
    expect(notices()[0].querySelector('b')).toBeNull()
    for (let n = 0; n < 2; n++) {
      emit({ kind: 'replay_start' })
      emit({ kind: 'notice', message: '履歴を省いています' })
      emit({ kind: 'notice', message: '履歴を省いています' })
      emit({ kind: 'user_message', text: '過去の質問' })
      emit({ kind: 'replay_end', in_flight: false })
      expect(notices()).toHaveLength(2)
    }
    const stream = window.document.querySelector('.conversation-stream')!
    expect(stream.querySelector('.chat-history-notice')?.textContent).toBe('履歴を省いています')
    expect(stream.textContent!.indexOf('履歴を省いています')).toBeLessThan(stream.textContent!.indexOf('過去の質問'))
    expect(stream.querySelector('.chat-history-notice button')).toBeNull()
    const button = window.document.querySelector('.chat-live-notice button')!
    expect(button.getAttribute('aria-label')).toBe('知らせを閉じる')
    button.dispatchEvent(new window.MouseEvent('click', { bubbles: true }))
    expect(notices()).toHaveLength(1)
    expect(window.document.body.textContent).not.toContain(live)
    emit({ kind: 'replay_start' })
    emit({ kind: 'replay_end', in_flight: false })
    expect(notices()).toHaveLength(0)
  } finally { await window.happyDOM.close() }
}, 20_000)
