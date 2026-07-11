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

  it('thought_chunk で streaming が立つ（thinking も active turn = shimmer 判定用）', () => {
    const s = fold([{ kind: 'thought_chunk', text: '考え' }])
    expect(s.streaming).toBe(true)
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

  it('turn_completed が context ゲージ（tokens/window）を載せ、欠落 turn では前値を保つ', () => {
    const s = emptyChatState()
    foldInto(s, {
      kind: 'turn_completed',
      session_id: 's',
      context_tokens: 38403,
      context_window: 200000,
    })
    expect(s.contextTokens).toBe(38403)
    expect(s.contextWindow).toBe(200000)
    // 値を運ばない turn（別 engine / 旧版）ではゲージを消さない。
    foldInto(s, { kind: 'turn_completed', session_id: 's' })
    expect(s.contextTokens).toBe(38403)
    expect(s.contextWindow).toBe(200000)
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

describe('transcript replay — Act II replay-on-attach', () => {
  /** SP が attach 時に送る replay 列（ReplayStart + 過去会話）。 */
  const replay: EchoesEvent[] = [
    { kind: 'replay_start' },
    { kind: 'user_message', text: '直して' },
    { kind: 'tool_call', id: 't1', name: 'Edit', input: {} },
    { kind: 'tool_call_update', tool_use_id: 't1', content: 'ok' },
    { kind: 'message_chunk', text: '直しました' },
  ]

  it('user_message が user bubble として復元される', () => {
    const s = fold(replay)
    expect(s.items).toEqual([
      { kind: 'user', text: '直して' },
      { kind: 'tool', id: 't1', name: 'Edit', done: true, error: false },
      { kind: 'assistant', text: '直しました' },
    ])
  })

  it('replay_start は既存の会話をクリアする（live 途中で attach しても混ざらない）', () => {
    const s = emptyChatState()
    foldInto(s, { kind: 'message_chunk', text: '古い応答' })
    foldInto(s, { kind: 'plan', entries: [{ content: 'old', status: 'pending' }] })
    expect(s.items).toHaveLength(1)

    for (const ev of replay) foldInto(s, ev)
    expect(s.items.map((i) => i.kind)).toEqual(['user', 'tool', 'assistant'])
    expect(s.plan).toEqual([])
  })

  /** terminal replay の clear-prefix と同じ教訓: backend は新規 attach と reconnect を
   *  区別できないので、replay は冪等でなければならない。 */
  it('replay が二重に届いても会話は二重化しない（冪等 — reconnect / demand 再発火）', () => {
    const once = fold(replay)
    const twice = fold([...replay, ...replay])
    expect(twice.items).toEqual(once.items)
  })

  it('replay 後に live event が続いても正しく積み上がる', () => {
    const s = fold([...replay, { kind: 'user_message', text: '次' }])
    expect(s.items.map((i) => i.kind)).toEqual(['user', 'tool', 'assistant', 'user'])
  })

  it('replay_start は header を保持する（live engine の session_init 由来）', () => {
    const s = emptyChatState()
    foldInto(s, { kind: 'session_init', session_id: 'sid', model: 'opus' })
    foldInto(s, { kind: 'replay_start' })
    expect(s.header).toEqual({ model: 'opus', sessionId: 'sid' })
  })

  it('replay_start は context ゲージを保持する（transcript は turn_completed を運ばないため）', () => {
    const s = emptyChatState()
    foldInto(s, {
      kind: 'turn_completed',
      session_id: 'sid',
      context_tokens: 50000,
      context_window: 200000,
    })
    foldInto(s, { kind: 'replay_start' })
    expect(s.contextTokens).toBe(50000)
    expect(s.contextWindow).toBe(200000)
  })
})
