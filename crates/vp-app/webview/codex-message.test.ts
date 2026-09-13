import { expect, it } from 'vitest'
import { emptyChatState, foldInto, deriveStatus } from './chat-model'
import type { ConversationEvent } from './console'
import { beginCodexResponse } from './codex-interaction-model'

it('Codex の発話境界と質問を保ち、完成通知で delta を重複しない', () => {
  const s = emptyChatState()
  const emit = (event: unknown) => foldInto(s, event as ConversationEvent)
  emit({ kind: 'codex_message', item_id: 'turn/q', text: '途中', questions: [], append: true })
  emit({ kind: 'codex_message', item_id: 'turn/q', text: '質問本文', questions: [{ id: '0', question: 'どちら？', header: '', options: [], is_secret: false }], append: false })
  emit({ kind: 'codex_message', item_id: 'turn/final', text: '続けます', questions: [], append: false })
  expect(s.items).toHaveLength(2)
  expect(s.items[0]).toMatchObject({ text: '質問本文', codexItemId: 'turn/q', codexQuestions: [{ id: '0' }] })
  expect(s.items[1]).toMatchObject({ text: '続けます' })
  expect(s.streaming).toBe(true)
})

it('質問履歴の復元は未回答要求を作らず、別の turn の同じ item 名と混ざらない', () => {
  const s = emptyChatState()
  const question = { kind: 'codex_message', item_id: 'old/q', text: '質問', append: false,
    questions: [{ id: '0', question: 'どちら？', header: '', options: [], is_secret: false }] }
  foldInto(s, { kind: 'codex_history', thread_id: 'thread', events: [question],
    user_message_ids: [], in_flight: false, truncated: false } as ConversationEvent)
  expect(s.streaming).toBe(false)
  expect(s.codexInteractions?.requests ?? []).toEqual([])
  foldInto(s, { ...question, item_id: 'new/q' } as ConversationEvent)
  expect(s.items).toHaveLength(2)
})

it('質問の下書きは履歴再描画を越えて保持し、失効した質問の下書きは破棄する', () => {
  const s = emptyChatState()
  foldInto(s, { kind: 'codex_interactions', requests: [{ request_id: 'q' }] } as ConversationEvent)
  Object.assign(s.codexInteractions!, { drafts: { q: { '0': '入力途中' } } })
  foldInto(s, { kind: 'codex_history', thread_id: 'thread', events: [], user_message_ids: [], in_flight: true, truncated: false })
  expect(s.codexInteractions).toMatchObject({ drafts: { q: { '0': '入力途中' } } })
  foldInto(s, { kind: 'codex_interactions', requests: [] })
  expect((s.codexInteractions as unknown as { drafts: unknown }).drafts).toEqual({})
})

it('非同期質問があっても agent の進行を承認待ちに変えない', () => {
  const s = emptyChatState()
  foldInto(s, { kind: 'message_chunk', text: '続行中' })
  foldInto(s, { kind: 'codex_interactions', requests: [{ request_id: 'q', kind: 'async_question', blocking: false }] } as ConversationEvent)
  expect(deriveStatus(s).kind).not.toBe('awaiting')
  expect(s.streaming).toBe(true)
})

it('切断 snapshot だけでも送信中表示を解除し、回答の下書きを保持する', () => {
  const s = emptyChatState()
  const request = { request_id: 'q', kind: 'async_question', item_id: 'turn/q', can_accept: true }
  foldInto(s, { kind: 'codex_interactions', requests: [request] } as ConversationEvent)
  Object.assign(s.codexInteractions!, { drafts: { q: { '0': '保持する回答' } } })
  expect(beginCodexResponse(s, 'q')).toBe(true)
  foldInto(s, { kind: 'codex_interactions', requests: [{ ...request, can_accept: false }] } as ConversationEvent)
  expect(s.codexInteractions?.sending).toEqual([])
  expect(s.codexInteractions?.drafts?.q).toEqual({ '0': '保持する回答' })
})

it('同じ質問 ID を停止・再開 snapshot で引き継ぎ、選択と自由入力を回答に使える', () => {
  const s = emptyChatState()
  const request = { request_id: 'q', kind: 'async_question', item_id: 'turn/q',
    title: '質問', details: '', blocking: false, can_accept: true, questions: [] }
  foldInto(s, { kind: 'codex_interactions', requests: [request] })
  s.codexInteractions!.drafts = { q: { '0': '選択肢 B', '1': '下書きテスト123' } }
  for (const can_accept of [false, false, true]) {
    foldInto(s, { kind: 'codex_interactions', requests: [{ ...request, can_accept }] })
    foldInto(s, { kind: 'codex_history', thread_id: 'thread', events: [],
      user_message_ids: [], in_flight: false, truncated: false })
    expect(s.codexInteractions?.drafts?.q).toEqual({ '0': '選択肢 B', '1': '下書きテスト123' })
  }
  expect(beginCodexResponse(s, 'q')).toBe(true)
  foldInto(s, { kind: 'codex_interaction_result', request_id: 'q', error: null })
  expect(s.codexInteractions?.requests).toEqual([])
  expect(s.codexInteractions?.drafts).toEqual({})
})
