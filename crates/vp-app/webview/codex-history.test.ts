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
    expect(s.items).toHaveLength(1)
    expect(s.unconfirmedCodexInputs).toMatchObject([{ id: 'request-b', text: '同じ文' }])
    expect(s.submission?.id).toBe('request-b')
  })

  it('snapshot に未包含の送信は ACK 後でも消さない', () => {
    const s = emptyChatState()
    beginSubmission(s, 'request-a', '送った文', [])
    foldInto(s, { kind: 'submit_result', request_id: 'request-a', error: null })
    foldInto(s, snapshot([{ kind: 'user_message', text: '昔の文' }]))
    expect(s.items.map(i => 'text' in i ? i.text : i.kind)).toEqual(['昔の文'])
    expect(s.unconfirmedCodexInputs).toEqual([{ id: 'request-a', text: '送った文', images: [] }])
  })

  it('照合 ID が欠けても古い発言を新しい発言の後ろに付け直さない', () => {
    const s = emptyChatState()
    const images = [{ media_type: 'image/png', data: 'aGVsbG8=' }]
    beginSubmission(s, 'old', '進めていこう', images)
    foldInto(s, { kind: 'submit_result', request_id: 'old', error: null })
    for (const text of ['マージして', '次の作業', '状況は？']) {
      foldInto(s, snapshot([
        { kind: 'user_message', text: '進めていこう' },
        { kind: 'user_message', text },
      ]))
      expect(s.items.map(i => 'text' in i ? i.text : i.kind)).toEqual(['進めていこう', text])
      expect(s.unconfirmedCodexInputs).toEqual([{ id: 'old', text: '進めていこう', images }])
    }
    foldInto(s, snapshot([{ kind: 'user_message', text: '進めていこう' }], ['old']))
    expect(s.unconfirmedCodexInputs).toEqual([])
  })

  it('snapshot が ACK より先でも保持し、拒否時は失敗した送信に一本化する', () => {
    const s = emptyChatState()
    beginSubmission(s, 'pending', '入力', [])
    foldInto(s, snapshot([]))
    expect(s.items).toEqual([])
    expect(s.unconfirmedCodexInputs).toHaveLength(1)
    foldInto(s, { kind: 'submit_result', request_id: 'pending', error: 'rejected' })
    expect(s.unconfirmedCodexInputs).toEqual([])
    expect(s.submission).toMatchObject({ text: '入力', status: 'failed' })
  })

  it('同文の別送信はまとめず、別 thread には照合待ちを持ち越さない', () => {
    const s = emptyChatState()
    for (const id of ['a', 'b']) {
      beginSubmission(s, id, '同じ文', [])
      foldInto(s, { kind: 'submit_result', request_id: id, error: null })
      foldInto(s, snapshot([]))
    }
    expect(s.unconfirmedCodexInputs?.map(i => i.id)).toEqual(['a', 'b'])
    foldInto(s, snapshot([{ kind: 'user_message', text: '同じ文' }], ['b']))
    expect(s.unconfirmedCodexInputs?.map(i => i.id)).toEqual(['a'])
    foldInto(s, { ...snapshot([]), thread_id: 'other' } as ConversationEvent)
    expect(s.unconfirmedCodexInputs).toEqual([])
  })
})
