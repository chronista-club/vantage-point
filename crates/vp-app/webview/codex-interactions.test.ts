// mem_1CeySwxuoVc17bGLnU5Np3 — 回答の成功を先取りしない。
import { expect, it } from 'vitest'
import { emptyChatState, foldInto, deriveStatus } from './chat-model'
import { beginCodexResponse } from './codex-interaction-model'
import type { ConversationEvent } from './console'

it('未回答 snapshot は履歴復元と独立し、同じ要求を重複させない', () => {
  const s = emptyChatState()
  const event = { kind: 'codex_interactions', requests: [{ request_id: 'codex:test:1', kind: 'question', title: '質問', details: '', questions: [], blocking: false }] } as unknown as ConversationEvent
  foldInto(s, { kind: 'message_chunk', text: '続行中' })
  foldInto(s, event)
  foldInto(s, event)
  expect(s).toMatchObject({ codexInteractions: { requests: [{ request_id: 'codex:test:1' }] }, streaming: true })
  foldInto(s, { kind: 'codex_history', thread_id: 't', events: [], user_message_ids: [], in_flight: true, truncated: false })
  expect(s).toMatchObject({ codexInteractions: { requests: [{ request_id: 'codex:test:1' }] } })
  foldInto(s, { kind: 'codex_interactions', requests: [] } as unknown as ConversationEvent)
  expect(s).toMatchObject({ codexInteractions: { requests: [] } })
})

it('送信中は重複回答を拒み、失敗後は元の要求に再試行できる', () => {
  const s = emptyChatState()
  foldInto(s, { kind: 'codex_interactions', requests: [{ request_id: 'codex:test:1', kind: 'command', title: '承認', details: 'ls', questions: [], blocking: true }] } as unknown as ConversationEvent)
  expect(beginCodexResponse(s, 'codex:test:1')).toBe(true)
  expect(beginCodexResponse(s, 'codex:test:1')).toBe(false)
  expect(s.codexInteractions?.requests).toHaveLength(1)
  foldInto(s, { kind: 'codex_interaction_result', request_id: 'different', error: null } as unknown as ConversationEvent)
  expect(s.codexInteractions?.sending).toEqual(['codex:test:1'])
  foldInto(s, { kind: 'codex_interaction_result', request_id: 'codex:test:1', error: '接続失敗' } as unknown as ConversationEvent)
  expect(s.codexInteractions?.errors['codex:test:1']).toBe('接続失敗')
  expect(beginCodexResponse(s, 'codex:test:1')).toBe(true)
  foldInto(s, { kind: 'codex_interaction_result', request_id: 'codex:test:1', error: null } as unknown as ConversationEvent)
  expect(s.codexInteractions?.requests).toHaveLength(0)
})

it('native 失効後に遅い送信結果が届いても要求を再生しない', () => {
  const s = emptyChatState()
  foldInto(s, { kind: 'codex_interactions', requests: [{ request_id: 'codex:test:1' }] } as unknown as ConversationEvent)
  beginCodexResponse(s, 'codex:test:1')
  foldInto(s, { kind: 'codex_interactions', requests: [] } as unknown as ConversationEvent)
  foldInto(s, { kind: 'codex_interaction_result', request_id: 'codex:test:1', error: '遅い失敗' } as unknown as ConversationEvent)
  expect(s.codexInteractions).toEqual({ requests: [], sending: [], errors: {} })
})

it('質問・承認の待機中は hang と表示しない', () => {
  const s = emptyChatState()
  foldInto(s, { kind: 'message_chunk', text: '実行中' })
  s.lastEventAt = 1
  foldInto(s, { kind: 'codex_interactions', requests: [{ request_id: 'codex:test:1', kind: 'question', blocking: true }] } as unknown as ConversationEvent)
  expect(deriveStatus(s, 100000)).toMatchObject({ kind: 'awaiting', label: '質問待ち', stalled: false })
})

it('同じ未回答の再配信はカードを作り直さず入力中の回答を保つ', () => {
  const s = emptyChatState()
  const event = { kind: 'codex_interactions', requests: [{ request_id: 'codex:test:1' }] }
  foldInto(s, structuredClone(event) as unknown as ConversationEvent)
  const original = s.codexInteractions?.requests[0]
  foldInto(s, structuredClone(event) as unknown as ConversationEvent)
  expect(s.codexInteractions?.requests[0]).toBe(original)
})
