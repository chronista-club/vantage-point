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
})

/**
 * 生成中（in-flight）の replay 着地。
 *
 * claude は message を完了時にしか transcript へ flush しない。assistant が生成している最中に
 * WS/QUIC が瞬断して demand が再発火すると、replay は「生成中 message の直前まで」しか
 * disk から復元できない。echoes topic は非 retained なので、瞬断前に届いていた delta は
 * どこにも残っていない。
 *
 * そこで backend（EchoesAgentHost）は未 commit の増分を in-flight tail として保持し、
 * replay 列を `transcript(commit 済み) ++ tail(未 commit)` として送る。以下はその契約を
 * frontend 側から固定するテスト。
 */
describe('replay が in-flight stream の途中に着地した場合', () => {
  /** 瞬断前に GUI が既に描いていた状態（committed 部 + 生成中の途中まで）。 */
  function liveStateBeforeBlip() {
    const s = emptyChatState()
    foldInto(s, { kind: 'user_message', text: '直して' })
    foldInto(s, { kind: 'tool_call', id: 't1', name: 'Edit', input: {} })
    foldInto(s, { kind: 'tool_call_update', tool_use_id: 't1', content: 'ok' })
    foldInto(s, { kind: 'message_chunk', text: '直しま' })
    return s
  }

  /** backend が瞬断後に送る replay 列。tail が生成中 message の現在までを運ぶ。 */
  const replayWithTail: EchoesEvent[] = [
    { kind: 'replay_start' },
    // --- transcript（commit 済み） ---
    { kind: 'user_message', text: '直して' },
    { kind: 'tool_call', id: 't1', name: 'Edit', input: {} },
    { kind: 'tool_call_update', tool_use_id: 't1', content: 'ok' },
    // --- in-flight tail（disk にまだ無い増分） ---
    { kind: 'message_chunk', text: '直しま' },
  ]

  it('tail が生成中の assistant バブルを復元する（瞬断前と同じ state に収束）', () => {
    const before = liveStateBeforeBlip()
    const restored = fold(replayWithTail)
    expect(restored.items).toEqual(before.items)
  })

  it('復帰後の message_chunk は既存バブルに繋がる（文の途中で新バブルを立てない）', () => {
    const s = fold(replayWithTail)
    foldInto(s, { kind: 'message_chunk', text: 'した' })

    expect(s.items.map((i) => i.kind)).toEqual(['user', 'tool', 'assistant'])
    const last = s.items[s.items.length - 1]
    expect(last.kind === 'assistant' && last.text).toBe('直しました')
  })

  it('tail が無いまま chunk が続くと文が割れる（= tail が防いでいる退行の再現）', () => {
    // tail を落とした replay 列（PR #699 時点の挙動）。
    const withoutTail = replayWithTail.filter((e) => e.kind !== 'message_chunk')
    const s = fold(withoutTail)
    foldInto(s, { kind: 'message_chunk', text: 'した' })

    const last = s.items[s.items.length - 1]
    expect(last.kind === 'assistant' && last.text).toBe('した') // 「直しま」が失われる
  })

  it('tail の message_chunk が streaming を立て直す（カーソルが戻る）', () => {
    const s = fold(replayWithTail)
    expect(s.streaming).toBe(true)
  })

  it('生成中の thinking も tail 経由なら復元される（commit 済み thinking は disk に残らない）', () => {
    const s = fold([
      { kind: 'replay_start' },
      { kind: 'user_message', text: 'やって' },
      // tail: 生成中の thinking 増分
      { kind: 'thought_chunk', text: '考え' },
      { kind: 'thought_chunk', text: '中' },
    ])
    expect(s.items).toEqual([
      { kind: 'user', text: 'やって' },
      { kind: 'thinking', text: '考え中' },
    ])
    expect(s.streaming).toBe(true)
  })

  it('tail 込みの replay も冪等（demand が何度再発火しても二重化しない）', () => {
    const once = fold(replayWithTail)
    const twice = fold([...replayWithTail, ...replayWithTail])
    expect(twice.items).toEqual(once.items)
  })

  it('replay 直後に届く tool_call_update も id で結ばれる（tool 実行中に着地）', () => {
    // 瞬断時に Edit が実行中 → transcript には tool_call だけが載っている。
    const s = fold([
      { kind: 'replay_start' },
      { kind: 'user_message', text: '直して' },
      { kind: 'tool_call', id: 't1', name: 'Edit', input: {} },
    ])
    foldInto(s, { kind: 'tool_call_update', tool_use_id: 't1', content: 'ok' })

    expect(s.items[1]).toEqual({ kind: 'tool', id: 't1', name: 'Edit', done: true, error: false })
  })

  it('孤児 tool_call_update は既存 item を壊さない（backend 不変条件の最終防衛線）', () => {
    const s = fold([
      { kind: 'replay_start' },
      { kind: 'message_chunk', text: '本文' },
      // 結び先の tool_call が無い update（起きたら backend のバグ）
      { kind: 'tool_call_update', tool_use_id: 'ghost', content: 'x' },
    ])
    expect(s.items).toEqual([{ kind: 'assistant', text: '本文' }])
  })
})
