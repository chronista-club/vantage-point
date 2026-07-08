import { describe, it, expect } from 'vitest'
import { foldInto, emptyChatState } from './chatview'
import type { EchoesEvent } from './console'

/** EchoesEvent 列を空 state に順に畳んで結果を返す helper。 */
function fold(events: EchoesEvent[]) {
  const s = emptyChatState()
  for (const ev of events) foldInto(s, ev)
  return s
}

describe('foldInto — EchoesEvent → ChatState 畳み込み (doc 33 C2)', () => {
  it('session_init が header を立てる', () => {
    const s = fold([
      { kind: 'session_init', session_id: 'sid-1', model: 'claude-haiku-4-5' },
    ])
    expect(s.header).toEqual({ model: 'claude-haiku-4-5', sessionId: 'sid-1' })
  })

  it('連続 message_chunk が 1 つの assistant item に accumulate する', () => {
    const s = fold([
      { kind: 'message_chunk', text: 'こん' },
      { kind: 'message_chunk', text: 'にちは' },
    ])
    expect(s.items).toEqual([{ kind: 'assistant', text: 'こんにちは' }])
    expect(s.streaming).toBe(true)
  })

  it('thinking と message は別 item に分かれ、各々 accumulate する', () => {
    const s = fold([
      { kind: 'thought_chunk', text: '考え' },
      { kind: 'thought_chunk', text: '中' },
      { kind: 'message_chunk', text: '答え' },
    ])
    expect(s.items).toEqual([
      { kind: 'thinking', text: '考え中' },
      { kind: 'assistant', text: '答え' },
    ])
  })

  it('tool_call → tool_call_update が id 一致で done 化する', () => {
    const s = fold([
      { kind: 'tool_call', id: 'tu-1', name: 'Bash', input: { command: 'ls' } },
      { kind: 'tool_call_update', tool_use_id: 'tu-1', content: 'file.txt' },
    ])
    expect(s.items).toEqual([{ kind: 'tool', id: 'tu-1', name: 'Bash', done: true, error: false }])
  })

  it('tool_call_update の is_error が error flag に載る', () => {
    const s = fold([
      { kind: 'tool_call', id: 'tu-x', name: 'Bash', input: {} },
      { kind: 'tool_call_update', tool_use_id: 'tu-x', content: 'boom', is_error: true },
    ])
    const tool = s.items[0]
    expect(tool.kind === 'tool' && tool.error).toBe(true)
  })

  it('id 不一致の update は既存 tool を触らない', () => {
    const s = fold([
      { kind: 'tool_call', id: 'tu-1', name: 'Read', input: {} },
      { kind: 'tool_call_update', tool_use_id: 'other', content: 'x' },
    ])
    expect(s.items[0]).toEqual({ kind: 'tool', id: 'tu-1', name: 'Read', done: false, error: false })
  })

  it('plan は毎回 replace（累積しない）', () => {
    const s = fold([
      { kind: 'plan', entries: [{ content: 'a', status: 'pending' }] },
      {
        kind: 'plan',
        entries: [
          { content: 'a', status: 'completed' },
          { content: 'b', status: 'in_progress', active_form: 'doing b' },
        ],
      },
    ])
    expect(s.plan).toHaveLength(2)
    expect(s.plan[0].status).toBe('completed')
    expect(s.plan[1].active_form).toBe('doing b')
  })

  it('turn_completed で streaming が下り、cost が載る', () => {
    const s = fold([
      { kind: 'message_chunk', text: 'hi' },
      { kind: 'turn_completed', session_id: 'sid-1', cost_usd: 0.012 },
    ])
    expect(s.streaming).toBe(false)
    expect(s.cost).toBe(0.012)
  })

  it('error は streaming を下ろし assistant item に警告を積む', () => {
    const s = fold([{ kind: 'error', message: 'boom' }])
    expect(s.streaming).toBe(false)
    const last = s.items[s.items.length - 1]
    expect(last.kind === 'assistant' && last.text.includes('boom')).toBe(true)
  })

  it('実ターン相当（init→thinking→tool→result→text→done）で item 構成が正しい', () => {
    const s = fold([
      { kind: 'session_init', session_id: 's', model: 'm' },
      { kind: 'thought_chunk', text: 'plan it' },
      { kind: 'tool_call', id: 't1', name: 'Read', input: {} },
      { kind: 'tool_call_update', tool_use_id: 't1', content: 'lines' },
      { kind: 'message_chunk', text: 'done' },
      { kind: 'turn_completed', session_id: 's' },
    ])
    expect(s.items.map((i) => i.kind)).toEqual(['thinking', 'tool', 'assistant'])
    expect(s.streaming).toBe(false)
  })
})
