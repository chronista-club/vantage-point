// mem_1Cex9hm7knkwwNWrjqTEBu — Codex の履歴と送信が交差する表示契約。
import { describe, expect, it } from 'vitest'
import { beginSubmission, emptyChatState, foldInto } from './chat-model'
import type { ConversationEvent } from './console'

// 新しい wire の入力を JSON として与え、型生成より先に実際の表示の失敗を確認する。
function snapshot(events: ConversationEvent[], ids: string[] = [], active = false): ConversationEvent {
  return JSON.parse(JSON.stringify({
    kind: 'codex_history', thread_id: 'codex-thread', events,
    user_message_ids: ids, in_flight: active, truncated: false,
  }))
}

describe('Codex native history', () => {
  it('省略の有無を表示状態に反映する', () => {
    const s = emptyChatState()
    foldInto(s, JSON.parse(JSON.stringify({ ...snapshot([]), truncated: true })))
    expect(s.historyTruncated).toBe(true)
    foldInto(s, snapshot([]))
    expect(s.historyTruncated).toBe(false)
  })
  it('Console 由来の発話・応答を置き換え、続く delta は復元した本文へ一度だけ足す', () => {
    const s = emptyChatState()
    foldInto(s, { kind: 'message_chunk', text: '古い表示' })
    const history = snapshot([
      { kind: 'user_message', text: '質問' },
      { kind: 'message_chunk', text: '応答の前半' },
    ], [], true)
    foldInto(s, history)
    foldInto(s, history)
    foldInto(s, { kind: 'message_chunk', text: 'と後半' })
    expect(s.items.map(i => 'text' in i ? i.text : i.kind)).toEqual(['質問', '応答の前半と後半'])
    expect(s.streaming).toBe(true)
    expect(s.replaying).toBe(false)
  })

  it('過去の完了を畳んでも最新のツール実行中状態と待機入力を保つ', () => {
    const s = emptyChatState()
    s.pending = '次に送る文'
    foldInto(s, snapshot([
      { kind: 'turn_completed', session_id: 'codex-thread' },
      { kind: 'tool_call', id: 'tool-1', name: 'shell', input: { command: 'test' } },
    ], [], true))
    expect(s.items).toMatchObject([{ kind: 'tool', id: 'tool-1', done: false }])
    expect(s.streaming).toBe(true)
    expect(s.pending).toBe('次に送る文')
  })

  it('ACK 後も送信 ID を残し、履歴に含まれた同じ送信だけを置き換える', () => {
    const s = emptyChatState()
    beginSubmission(s, 'request-a', '同じ文', [])
    foldInto(s, { kind: 'submit_result', request_id: 'request-a', error: null })
    expect(s.items[0]).toMatchObject({ clientId: 'request-a' })
    foldInto(s, snapshot([{ kind: 'user_message', text: '同じ文' }], ['request-a']))
    expect(s.items).toHaveLength(1)
    beginSubmission(s, 'request-b', '同じ文', [])
    foldInto(s, snapshot([{ kind: 'user_message', text: '同じ文' }], ['request-a']))
    expect(s.items).toHaveLength(2)
    expect(s.submission?.id).toBe('request-b')
  })

  it('snapshot に未包含の送信は ACK 後でも消さない', () => {
    const s = emptyChatState()
    beginSubmission(s, 'request-a', '送った文', [])
    foldInto(s, { kind: 'submit_result', request_id: 'request-a', error: null })
    foldInto(s, snapshot([{ kind: 'user_message', text: '昔の文' }]))
    expect(s.items.map(i => 'text' in i ? i.text : i.kind)).toEqual(['昔の文', '送った文'])
  })
})
