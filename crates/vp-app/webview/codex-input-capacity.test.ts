import { expect, it } from 'vitest'
import { beginSubmission, emptyChatState, foldInto } from './chat-model'

it('reserves capacity before sending and stops at 20 unmatched inputs without dropping any', () => {
  const s = emptyChatState()
  for (let n = 0; n < 20; n++) {
    expect(beginSubmission(s, `id-${n}`, `input-${n}`, [], true)).toBe(true)
    foldInto(s, { kind: 'submit_result', request_id: `id-${n}`, error: null })
  }
  expect(beginSubmission(s, 'overflow', 'keep in composer', [], true)).toBe(false)
  expect(s.submission).toBeNull()
  expect(s.unconfirmedCodexInputs).toHaveLength(20)
  expect(s.codexInputCapacityError).toContain('20件')
  foldInto(s, { kind: 'codex_history', thread_id: 't', events: [],
    user_message_ids: ['id-0'], in_flight: false, truncated: false })
  expect(beginSubmission(s, 'next', 'next', [], true)).toBe(true)
  expect(s.codexInputCapacityError).toBeNull()
})

it('bounds text and base64 together and retains the exact images already accepted', () => {
  const s = emptyChatState()
  const images = [{ media_type: 'image/png', data: 'A'.repeat(8 * 1024 * 1024) }]
  expect(beginSubmission(s, 'first', '日本語', images, true)).toBe(true)
  foldInto(s, { kind: 'submit_result', request_id: 'first', error: null })
  expect(beginSubmission(s, 'second', '日本語', images, true)).toBe(false)
  expect(s.unconfirmedCodexInputs).toEqual([{ id: 'first', text: '日本語', images }])
  expect(s.codexInputCapacityError).toContain('32 MiB')
  expect(s.submission).toBeNull()
  const huge = emptyChatState()
  expect(beginSubmission(huge, 'text', '字'.repeat(16 * 1024 * 1024), [], true)).toBe(false)
  expect(huge.items).toEqual([])
})

it('does not keep image data in non-Codex user bubbles after acknowledgement', () => {
  const s = emptyChatState()
  beginSubmission(s, 'claude', 'image', [{ media_type: 'image/png', data: 'AAAA' }], false)
  foldInto(s, { kind: 'submit_result', request_id: 'claude', error: null })
  expect(s.items).toEqual([{ kind: 'user', text: 'image', clientId: 'claude' }])
  expect(s.unconfirmedCodexInputs).toBeUndefined()
})
