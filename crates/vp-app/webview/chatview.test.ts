import { describe, it, expect } from 'vitest'
import {
  foldInto,
  emptyChatState,
  linkOpenPayload,
  deriveStatus,
  canDequeuePending,
  isTurnClosingEvent,
  classifyToolRun,
  toolGroupStatus,
  formatToolInput,
  formatToolResult,
  clampToolDetail,
  canCloseSession,
  chatKey,
  lampOf,
  deriveNowLine,
  clampNowLine,
} from './chatview'
import type { EchoesEvent } from './console'

/** tool ChatItem を手軽に組む helper（classifyToolRun のテスト用）。 */
let toolSeq = 0
function tool(name: string, done = true, error = false) {
  return { kind: 'tool' as const, id: `${name}-${toolSeq++}`, name, done, error }
}

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

  it('session_init が permission mode を per-lane に反映する（review #2）', () => {
    const s = fold([{ kind: 'session_init', session_id: 'sid-2', permission_mode: 'default' }])
    expect(s.permissionMode).toBe('default')
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

  it('replay_start で replaying=true、replay_end で false（再同期ローダーの trigger）', () => {
    // 初期は false
    expect(emptyChatState().replaying).toBe(false)
    // replay_start → 再同期中
    const mid = fold([{ kind: 'replay_start' }])
    expect(mid.replaying).toBe(true)
    // replay_start→replay_end で戻る
    const done = fold([{ kind: 'replay_start' }, { kind: 'replay_end', in_flight: false }])
    expect(done.replaying).toBe(false)
  })

  it('tool_call → tool_call_update が id 一致で done 化する', () => {
    const s = fold([
      { kind: 'tool_call', id: 'tu-1', name: 'Bash', input: { command: 'ls' } },
      { kind: 'tool_call_update', tool_use_id: 'tu-1', content: 'file.txt' },
    ])
    expect(s.items).toEqual([
      {
        kind: 'tool',
        id: 'tu-1',
        name: 'Bash',
        done: true,
        error: false,
        input: { command: 'ls' },
        result: 'file.txt',
      },
    ])
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
    expect(s.items[0]).toEqual({
      kind: 'tool',
      id: 'tu-1',
      name: 'Read',
      done: false,
      error: false,
      input: {},
    })
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

  it('turn 完了後の chunk は前 turn のバブルに融合しない（§5.1 封印）', () => {
    const s = fold([
      { kind: 'user_message', text: 'q1' },
      { kind: 'message_chunk', text: 'A' },
      { kind: 'turn_completed', session_id: 's' },
      { kind: 'message_chunk', text: 'B' }, // 別 turn = 新バブルになるべき
    ])
    const assistants = s.items.filter((i) => i.kind === 'assistant')
    expect(assistants.length).toBe(2) // 融合していない
    expect(assistants[0]).toMatchObject({ text: 'A', sealed: true })
    expect(assistants[1]).toMatchObject({ text: 'B' })
  })

  it('deriveStatus: streaming→応答中 / idle→待機中 / pending 反映（§5.1 status バー）', () => {
    expect(deriveStatus(null)).toMatchObject({ kind: 'idle' })
    const idle = emptyChatState()
    expect(deriveStatus(idle)).toMatchObject({ kind: 'idle', label: '待機中', pending: false })
    const streaming = fold([{ kind: 'message_chunk', text: 'hi' }])
    expect(deriveStatus(streaming)).toMatchObject({ kind: 'streaming', label: '応答中…' })
    // 待機中 かつ pending = flush 失敗の兆候を status が拾える
    idle.pending = 'buf'
    expect(deriveStatus(idle)).toMatchObject({ kind: 'idle', pending: true })
    // streaming なのに最終イベントから STALL_MS 超過 = 無反応(hang) を status が正直に暴く
    const stalled = fold([{ kind: 'message_chunk', text: 'hi' }])
    stalled.lastEventAt = 1000
    expect(deriveStatus(stalled, 1000 + 9000)).toMatchObject({ kind: 'streaming', stalled: true })
    // 本物の異常(error)は待機ではなくエラーとして出す
    const errored = fold([{ kind: 'error', message: 'x' }])
    expect(deriveStatus(errored)).toMatchObject({ kind: 'error', label: 'エラー' })
    // engine の休眠(engine_exited)は異常ではない = idle 扱いで「💤 休眠」と穏当に出す
    const dormant = fold([{ kind: 'engine_exited', message: '休眠' }])
    expect(deriveStatus(dormant)).toMatchObject({ kind: 'idle', label: '💤 休眠' })
  })

  it('lampOf: 灯 3 状態への畳み込み（doc 51 §1 A2 — 横目の視覚言語）', () => {
    // 動いている = run（streaming / thinking / tool）
    expect(lampOf(deriveStatus(fold([{ kind: 'message_chunk', text: 'hi' }])))).toBe('run')
    expect(lampOf(deriveStatus(fold([{ kind: 'thought_chunk', text: '…' }])))).toBe('run')
    // あなたが要る = need（HITL 質問/承認 + engine 異常 — 介入が要る側）
    const asking = fold([
      {
        kind: 'question',
        request_id: 'q1',
        questions: [{ question: 'どっち?', header: 'Q', options: [] }],
      },
    ])
    expect(lampOf(deriveStatus(asking))).toBe('need')
    expect(lampOf(deriveStatus(fold([{ kind: 'error', message: 'x' }])))).toBe('need')
    // 待っている = off（待機中 / 💤 休眠）— presence でなく活動の灯
    expect(lampOf(deriveStatus(emptyChatState()))).toBe('off')
    expect(lampOf(deriveStatus(fold([{ kind: 'engine_exited', message: '休眠' }])))).toBe('off')
    // stalled は run のまま（8s 無イベントは平常でも起きる — 嘘の告発は status 行の文字の領分）
    const stalled = fold([{ kind: 'message_chunk', text: 'hi' }])
    stalled.lastEventAt = 1000
    expect(lampOf(deriveStatus(stalled, 1000 + 9000))).toBe('run')
  })

  it('deriveNowLine: 質問要旨 > 契約 > 機械導出 > null（doc 51 §1 A3）', () => {
    // 待っている pane に「今」は無い — 空なら描かない
    expect(deriveNowLine(null)).toBeNull()
    expect(deriveNowLine(emptyChatState())).toBeNull()
    // turn 中の頼まれごと（機械導出の最終段 = user prompt 先頭）
    const working = fold([
      { kind: 'user_message', text: 'resize の設計を検討して\n詳細は…' },
      { kind: 'message_chunk', text: '見ています' },
    ])
    expect(deriveNowLine(working)).toBe('resize の設計を検討して')
    // 実行中 tool は頼まれごとより濃い
    const tooling = fold([
      { kind: 'user_message', text: '調査して' },
      { kind: 'tool_call', id: 't1', name: 'Grep', input: {} },
    ])
    expect(deriveNowLine(tooling)).toBe('Grep を実行中')
    // 契約（nowLine — A3b の口）は機械導出より濃い
    tooling.nowLine = 'panic 箇所を特定中'
    expect(deriveNowLine(tooling)).toBe('panic 箇所を特定中')
    // 契約の書き手 = `vp now` 発の now_line event（fold 経路）
    const reported = fold([
      { kind: 'user_message', text: '調査して' },
      { kind: 'message_chunk', text: 'x' },
      { kind: 'now_line', text: 'pty_slot の lock 順を確認中' },
    ])
    expect(deriveNowLine(reported)).toBe('pty_slot の lock 順を確認中')
    // 質問の要旨は契約より濃い（ボールが人にある）
    const asking = fold([
      {
        kind: 'question',
        request_id: 'q1',
        questions: [{ question: 'resize はどちらで束ねますか？', header: 'Q', options: [] }],
      },
    ])
    asking.nowLine = '古い自己報告'
    expect(deriveNowLine(asking)).toBe('resize はどちらで束ねますか？')
    // turn_completed で契約の「今」は消える
    const done = fold([{ kind: 'message_chunk', text: 'x' }])
    done.nowLine = '作業中'
    foldInto(done, { kind: 'turn_completed', session_id: 's1' })
    expect(done.nowLine).toBeNull()
    expect(deriveNowLine(done)).toBeNull()
  })

  it('clampNowLine: 先頭行のみ + 長すぎは…で切る（1 行らしさの保証）', () => {
    expect(clampNowLine('一行目\n二行目')).toBe('一行目')
    expect(clampNowLine(`${'あ'.repeat(70)}`)).toHaveLength(60)
    expect(clampNowLine(`${'あ'.repeat(70)}`).endsWith('…')).toBe(true)
  })

  it('isTurnClosingEvent: pending flush の発火契機（doc 35 §5.1 + engine_exited の自己修復継承）', () => {
    // turn を閉じる 3 種 = flush してよい（engine_exited は pending 送信が respawn トリガを兼ねる）
    expect(isTurnClosingEvent('turn_completed')).toBe(true)
    expect(isTurnClosingEvent('error')).toBe(true)
    expect(isTurnClosingEvent('engine_exited')).toBe(true)
    // turn 継続中 / 特殊 window では流さない（順序が壊れる — foldEvent のコメント参照）
    expect(isTurnClosingEvent('message_chunk')).toBe(false)
    expect(isTurnClosingEvent('replay_start')).toBe(false)
    expect(isTurnClosingEvent('replay_end')).toBe(false)
    expect(isTurnClosingEvent('question')).toBe(false)
    expect(isTurnClosingEvent('permission_request')).toBe(false)
  })

  it('canDequeuePending: 送信待ちを入力欄へ戻せる条件（composer が空 かつ pending あり）', () => {
    // 正常系: composer 空 + pending あり → 戻せる
    expect(canDequeuePending('', 'buffered')).toBe(true)
    // composer に打ちかけ下書き → 潰さないため戻せない（MVP グレーアウト）
    expect(canDequeuePending('typing…', 'buffered')).toBe(false)
    // 空白だけの下書きは「空」とみなす（trim）
    expect(canDequeuePending('   \n ', 'buffered')).toBe(true)
    // pending が無ければ戻す対象が無い
    expect(canDequeuePending('', null)).toBe(false)
    expect(canDequeuePending('', '')).toBe(false)
  })

  it('replay 終端の replay_end で streaming が真値に確定する（応答中の永久居座り根治）', () => {
    // replay は過去 assistant 発話を message_chunk で送るので streaming が立つ。
    // 生成中 turn が無ければ replay_end{in_flight:false} が下ろす。
    const idle = fold([
      { kind: 'replay_start' },
      { kind: 'user_message', text: 'q' },
      { kind: 'message_chunk', text: '過去の返答' },
      { kind: 'replay_end', in_flight: false },
    ])
    expect(idle.streaming).toBe(false)
    expect(deriveStatus(idle)).toMatchObject({ kind: 'idle', label: '待機中' })
    // 本当に生成中なら in_flight:true で streaming は立ったまま
    const live = fold([
      { kind: 'replay_start' },
      { kind: 'message_chunk', text: '生成中…' },
      { kind: 'replay_end', in_flight: true },
    ])
    expect(live.streaming).toBe(true)
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

  it('question が未回答の prompt item を積み、streaming を下ろす（HITL pause、doc 35 PR1）', () => {
    const s = fold([
      { kind: 'message_chunk', text: '確認です' },
      {
        kind: 'question',
        request_id: 'req-1',
        questions: [
          {
            question: 'どちらの言語？',
            header: 'Language',
            options: [
              { label: 'English', description: 'en' },
              { label: '日本語', description: 'ja' },
            ],
            multi_select: false,
          },
        ],
      },
    ])
    expect(s.streaming).toBe(false)
    const last = s.items[s.items.length - 1]
    expect(last.kind).toBe('prompt')
    if (last.kind === 'prompt') {
      expect(last.requestId).toBe('req-1')
      expect(last.answered).toBe(false)
      expect(last.questions[0].options).toHaveLength(2)
    }
  })

  it('回答後の message_chunk は新 assistant バブルを立てる（質問→継続の流れ）', () => {
    const s = fold([
      { kind: 'question', request_id: 'r1', questions: [{ question: 'Q?', header: 'H', options: [{ label: 'A' }] }] },
      { kind: 'message_chunk', text: '続き' },
    ])
    expect(s.items.map((i) => i.kind)).toEqual(['prompt', 'assistant'])
    expect(s.streaming).toBe(true)
  })

  it('permission_request が allow/deny 用 permission prompt を積む（doc 35 PR3）', () => {
    const s = fold([
      { kind: 'message_chunk', text: 'Bash 実行前' },
      { kind: 'permission_request', request_id: 'perm-1', tool_name: 'Bash', input: { command: 'ls' } },
    ])
    expect(s.streaming).toBe(false)
    const last = s.items[s.items.length - 1]
    expect(last.kind).toBe('prompt')
    if (last.kind === 'prompt') {
      expect(last.requestId).toBe('perm-1')
      expect(last.answered).toBe(false)
      expect(last.permission?.toolName).toBe('Bash')
      expect(last.questions).toHaveLength(0)
    }
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
      { kind: 'tool', id: 't1', name: 'Edit', done: true, error: false, input: {}, result: 'ok' },
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

    expect(s.items[1]).toEqual({
      kind: 'tool',
      id: 't1',
      name: 'Edit',
      done: true,
      error: false,
      input: {},
      result: 'ok',
    })
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

describe('linkOpenPayload — chat リンクの OS ブラウザ起動 一次弾き（scheme 検証）', () => {
  it('https は open-url ペイロードを返す', () => {
    expect(linkOpenPayload('https://localhost:5173/creo-ui/')).toBe(
      JSON.stringify({ t: 'open-url', url: 'https://localhost:5173/creo-ui/' }),
    )
  })

  it('http も open-url ペイロードを返す', () => {
    expect(linkOpenPayload('http://example.com')).toBe(
      JSON.stringify({ t: 'open-url', url: 'http://example.com' }),
    )
  })

  it('file: は null（webview に file:// を開かせない — 多層防御）', () => {
    expect(linkOpenPayload('file:///etc/passwd')).toBeNull()
  })

  it('javascript: は null（scheme injection を通さない）', () => {
    expect(linkOpenPayload('javascript:alert(1)')).toBeNull()
  })

  it('相対リンクは null（webview 内遷移も抑止 = ハンドラ側 preventDefault で担保、emit はしない）', () => {
    expect(linkOpenPayload('/creo-ui/')).toBeNull()
    expect(linkOpenPayload('#section')).toBeNull()
  })

  it('空 href は null', () => {
    expect(linkOpenPayload('')).toBeNull()
  })
})

describe('classifyToolRun — 連続同名 tool の accordion 集約 (描画時のみ・reducer 非依存)', () => {
  it('単発 tool は single（畳まない）', () => {
    const items = [tool('Bash')]
    expect(classifyToolRun(items, 0)).toEqual({ role: 'single' })
  })

  it('連続同名 2 件: 先頭が head で run 全体を持ち、2 件目が member', () => {
    const a = tool('Agent')
    const b = tool('Agent')
    const items = [a, b]
    const head = classifyToolRun(items, 0)
    expect(head.role).toBe('head')
    expect(head.role === 'head' && head.run).toEqual([a, b])
    expect(classifyToolRun(items, 1)).toEqual({ role: 'member' })
  })

  it('連続同名 4 件（Agent ×4）: head 1 + member 3', () => {
    const items = [tool('Agent'), tool('Agent'), tool('Agent'), tool('Agent')]
    expect(classifyToolRun(items, 0).role).toBe('head')
    expect(classifyToolRun(items, 1)).toEqual({ role: 'member' })
    expect(classifyToolRun(items, 2)).toEqual({ role: 'member' })
    expect(classifyToolRun(items, 3)).toEqual({ role: 'member' })
    const head = classifyToolRun(items, 0)
    expect(head.role === 'head' && head.run.length).toBe(4)
  })

  it('異なる名前は run を分断する（Bash / Agent×2 / Bash）', () => {
    const items = [tool('Bash'), tool('Agent'), tool('Agent'), tool('Bash')]
    expect(classifyToolRun(items, 0)).toEqual({ role: 'single' }) // 先頭 Bash 単発
    expect(classifyToolRun(items, 1).role).toBe('head') // Agent run 先頭
    expect(classifyToolRun(items, 2)).toEqual({ role: 'member' }) // Agent run 2件目
    expect(classifyToolRun(items, 3)).toEqual({ role: 'single' }) // 末尾 Bash 単発
  })

  it('非 tool（assistant 等）が run を分断する', () => {
    const items = [
      tool('Agent'),
      { kind: 'assistant' as const, text: '間の応答' },
      tool('Agent'),
    ]
    // 前後の Agent は間に非 tool が挟まるので各々 single
    expect(classifyToolRun(items, 0)).toEqual({ role: 'single' })
    expect(classifyToolRun(items, 2)).toEqual({ role: 'single' })
  })

  it('append-only で single→head へ昇格する（stream 追記の再現）', () => {
    // Agent 1 件時点は single
    const one = [tool('Agent')]
    expect(classifyToolRun(one, 0)).toEqual({ role: 'single' })
    // 同名がもう 1 件 push されると先頭は head へ（描画側は reactive に再評価）
    const two = [...one, tool('Agent')]
    expect(classifyToolRun(two, 0).role).toBe('head')
  })

  it('run は末尾でも成立（末尾で走行中の Agent×2）', () => {
    const items = [
      { kind: 'assistant' as const, text: 'やります' },
      tool('Agent', true),
      tool('Agent', false), // 走行中
    ]
    expect(classifyToolRun(items, 1).role).toBe('head')
    expect(classifyToolRun(items, 2)).toEqual({ role: 'member' })
  })
})

describe('toolGroupStatus — accordion header の集約 status（エンジン状態を偽らない）', () => {
  it('全 tool done・error 無し → ✓', () => {
    expect(toolGroupStatus([tool('Agent', true), tool('Agent', true)])).toEqual({
      running: false,
      label: '✓',
    })
  })

  it('全 tool settle・1 件 error → error', () => {
    expect(toolGroupStatus([tool('Agent', true, true), tool('Agent', true)])).toEqual({
      running: false,
      label: 'error',
    })
  })

  it('走行中は完了数 {done}/{count} を返す（畳んだままでも進捗が見える）', () => {
    expect(toolGroupStatus([tool('Agent', true), tool('Agent', false), tool('Agent', false)])).toEqual(
      { running: true, label: '1/3' },
    )
  })

  it('1 件 error で終わっても他が in-flight なら running を偽らない（moody-blues #2 の反例）', () => {
    // [done+error, 走行中, 走行中] → error/✓ に落とさず「1/3 実行中」を維持する
    const s = toolGroupStatus([tool('Agent', true, true), tool('Agent', false), tool('Agent', false)])
    expect(s.running).toBe(true)
    expect(s.label).toBe('1/3')
  })

  it('1 件も done でない開始直後 → 0/N running', () => {
    expect(toolGroupStatus([tool('Agent', false), tool('Agent', false)])).toEqual({
      running: true,
      label: '0/2',
    })
  })
})

describe('tool 詳細の保持 — accordion の個別展開の表示源（reducer 契約）', () => {
  it('tool_call の input を捨てずに保持する（詳細を開くために要る）', () => {
    const s = fold([
      { kind: 'tool_call', id: 't1', name: 'Bash', input: { command: 'ls -la', description: '一覧' } },
    ])
    const t = s.items[0]
    expect(t.kind === 'tool' && t.input).toEqual({ command: 'ls -la', description: '一覧' })
  })

  it('tool_call_update の content を result として保持する', () => {
    const s = fold([
      { kind: 'tool_call', id: 't1', name: 'Bash', input: { command: 'ls' } },
      { kind: 'tool_call_update', tool_use_id: 't1', content: 'a.txt\nb.txt' },
    ])
    const t = s.items[0]
    expect(t.kind === 'tool' && t.result).toBe('a.txt\nb.txt')
  })

  it('error の結果本文も保持する（失敗理由を個別に開いて読める）', () => {
    const s = fold([
      { kind: 'tool_call', id: 't1', name: 'Bash', input: { command: 'nope' } },
      { kind: 'tool_call_update', tool_use_id: 't1', content: 'command not found', is_error: true },
    ])
    const t = s.items[0]
    expect(t.kind === 'tool' && t.error).toBe(true)
    expect(t.kind === 'tool' && t.result).toBe('command not found')
  })

  it('連続同名 tool でも各件が別々の input/result を持つ（group を開いて個別に掘れる）', () => {
    const s = fold([
      { kind: 'tool_call', id: 'a', name: 'Bash', input: { command: 'one' } },
      { kind: 'tool_call_update', tool_use_id: 'a', content: 'r1' },
      { kind: 'tool_call', id: 'b', name: 'Bash', input: { command: 'two' } },
      { kind: 'tool_call_update', tool_use_id: 'b', content: 'r2' },
    ])
    const tools = s.items.filter((i) => i.kind === 'tool')
    expect(tools).toHaveLength(2)
    expect(tools.map((t) => (t.kind === 'tool' ? t.result : null))).toEqual(['r1', 'r2'])
    // group 集約（描画時）とは独立に、各件の詳細が残っている
    expect(classifyToolRun(s.items, 0).role).toBe('head')
    expect(classifyToolRun(s.items, 1)).toEqual({ role: 'member' })
  })
})

describe('subagent_message — Agent の子の発話を親と取り違えない（--forward-subagent-text）', () => {
  /** 親が Agent を呼び、子が thinking→text を返す最小の実 turn。 */
  const withSubagent = (): EchoesEvent[] => [
    { kind: 'message_chunk', text: '子に投げます' },
    { kind: 'tool_call', id: 'toolu_1', name: 'Agent', input: { prompt: '6x7 は?', subagent_type: 'general-purpose' } },
    { kind: 'subagent_message', parent_tool_use_id: 'toolu_1', role: 'prompt', text: '6x7 は?' },
    { kind: 'subagent_message', parent_tool_use_id: 'toolu_1', role: 'thinking', text: '掛け算する' },
    { kind: 'subagent_message', parent_tool_use_id: 'toolu_1', role: 'text', text: '42' },
    { kind: 'tool_call_update', tool_use_id: 'toolu_1', content: '42' },
    { kind: 'message_chunk', text: '答えは 42 です' },
  ]

  it('subagent の発話は親 tool にぶら下がる（item は増えない）', () => {
    const s = fold(withSubagent())
    // assistant / tool の 2 種だけ。subagent 用の独立 item は生えない
    expect(s.items.map((i) => i.kind)).toEqual(['assistant', 'tool', 'assistant'])
    const t = s.items[1]
    expect(t.kind === 'tool' && t.subagent).toEqual([
      { role: 'prompt', text: '6x7 は?' },
      { role: 'thinking', text: '掛け算する' },
      { role: 'text', text: '42' },
    ])
  })

  it('★ subagent の発話が親の吹き出しに混ざらない（親が言っていないことを言わせない）', () => {
    const s = fold(withSubagent())
    const assistants = s.items.filter((i) => i.kind === 'assistant')
    const texts = assistants.map((a) => (a.kind === 'assistant' ? a.text : ''))
    // 親の発話は 2 つとも自分の言葉だけ。子の "42" / "掛け算する" は入らない
    expect(texts).toEqual(['子に投げます', '答えは 42 です'])
    expect(texts.join('')).not.toContain('掛け算する')
  })

  it('★ subagent の prompt が user 発話として描かれない', () => {
    const s = fold(withSubagent())
    // 子への指示は user bubble を作らない（人間が打ったように見せない）
    expect(s.items.some((i) => i.kind === 'user')).toBe(false)
  })

  it('連続する同 role は 1 節に畳む（thinking が細切れにならない）', () => {
    const s = fold([
      { kind: 'tool_call', id: 'toolu_1', name: 'Agent', input: {} },
      { kind: 'subagent_message', parent_tool_use_id: 'toolu_1', role: 'thinking', text: 'まず' },
      { kind: 'subagent_message', parent_tool_use_id: 'toolu_1', role: 'thinking', text: 'つぎに' },
      { kind: 'subagent_message', parent_tool_use_id: 'toolu_1', role: 'text', text: '答え' },
    ])
    const t = s.items[0]
    expect(t.kind === 'tool' && t.subagent).toEqual([
      { role: 'thinking', text: 'まず\nつぎに' },
      { role: 'text', text: '答え' },
    ])
  })

  it('親 tool の無い subagent_message は既存 item を壊さない（孤児の最終防衛線）', () => {
    const s = fold([
      { kind: 'message_chunk', text: '本文' },
      { kind: 'subagent_message', parent_tool_use_id: 'ghost', role: 'text', text: '迷子' },
    ])
    expect(s.items).toEqual([{ kind: 'assistant', text: '本文' }])
  })

  it('並行した複数 Agent の発話が互いに混ざらない（parent id で分かれる）', () => {
    const s = fold([
      { kind: 'tool_call', id: 'a', name: 'Agent', input: {} },
      { kind: 'tool_call', id: 'b', name: 'Agent', input: {} },
      { kind: 'subagent_message', parent_tool_use_id: 'b', role: 'text', text: 'B の答え' },
      { kind: 'subagent_message', parent_tool_use_id: 'a', role: 'text', text: 'A の答え' },
    ])
    const tools = s.items.filter((i) => i.kind === 'tool')
    expect(tools.map((t) => (t.kind === 'tool' ? t.subagent?.[0]?.text : null))).toEqual([
      'A の答え',
      'B の答え',
    ])
  })

  it('subagent があれば tool は「詳細あり」扱いになる（caret が出る条件）', () => {
    const s = fold([
      { kind: 'tool_call', id: 'toolu_1', name: 'Agent', input: {} }, // input 空 = それ単体では詳細なし
      { kind: 'subagent_message', parent_tool_use_id: 'toolu_1', role: 'text', text: '42' },
    ])
    const t = s.items[0]
    // input は {} で formatToolInput→null だが、subagent があるので開ける
    expect(t.kind === 'tool' && formatToolInput(t.input)).toBeNull()
    expect(t.kind === 'tool' && (t.subagent?.length ?? 0) > 0).toBe(true)
  })
})

describe('formatToolInput / formatToolResult — 詳細の表示整形（純関数）', () => {
  it('object は pretty JSON になる', () => {
    expect(formatToolInput({ command: 'ls' })).toBe('{\n  "command": "ls"\n}')
  })

  it('string はそのまま通す', () => {
    expect(formatToolInput('raw text')).toBe('raw text')
  })

  it('空 input（{} / [] / 空文字 / null / undefined）は詳細なし = null', () => {
    // 「開いても空」を作らないための判定 — caret を出すかがこれで決まる
    expect(formatToolInput({})).toBeNull()
    expect(formatToolInput([])).toBeNull()
    expect(formatToolInput('')).toBeNull()
    expect(formatToolInput(null)).toBeNull()
    expect(formatToolInput(undefined)).toBeNull()
  })

  it('JSON 化できない入力でも落ちない（循環参照）', () => {
    const cyclic: Record<string, unknown> = {}
    cyclic.self = cyclic
    expect(() => formatToolInput(cyclic)).not.toThrow()
  })

  it('result は空文字/undefined が null、中身があればそのまま', () => {
    expect(formatToolResult(undefined)).toBeNull()
    expect(formatToolResult('')).toBeNull()
    expect(formatToolResult('done')).toBe('done')
  })
})

describe('clampToolDetail — 巨大 detail の honest clamp（黙って切らない）', () => {
  it('上限以下はそのまま・省略 0', () => {
    expect(clampToolDetail('short', 10)).toEqual({ text: 'short', omitted: 0 })
  })

  it('ちょうど上限もそのまま', () => {
    expect(clampToolDetail('12345', 5)).toEqual({ text: '12345', omitted: 0 })
  })

  it('超過は切り詰め、省略した文字数を返す（UI が「…N 文字省略」を出せる）', () => {
    expect(clampToolDetail('abcdefghij', 4)).toEqual({ text: 'abcd', omitted: 6 })
  })
})

describe('canCloseSession — session tab の × 表示条件（doc 38 Phase 3 → doc 39）', () => {
  it('1 本以下では × を出さない（最後の 1 本は backend も Err で拒否）', () => {
    expect(canCloseSession(0)).toBe(false)
    expect(canCloseSession(1)).toBe(false)
  })

  it('2 本以上の非 root タブで × を出す', () => {
    expect(canCloseSession(2)).toBe(true)
    expect(canCloseSession(5)).toBe(true)
    expect(canCloseSession(2, false)).toBe(true)
  })

  it('root タブは本数に依らず隠す（backend の「root は remove 不可」の UI 反映 — doc 39 §6）', () => {
    expect(canCloseSession(2, true)).toBe(false)
    expect(canCloseSession(5, true)).toBe(false)
  })

  it('旧 SP（root field なし = undefined）は従来挙動（本数のみ）に倒す', () => {
    expect(canCloseSession(2, undefined)).toBe(true)
  })
})


// ---------------------------------------------------------------------------
// chatKey — 会話 store の (lane, session) key（doc 50 §4.3 #1）
// ---------------------------------------------------------------------------

describe('chatKey — lane と session が衝突なく畳める', () => {
  it('lane と session を NUL で連結する', () => {
    expect(chatKey('vp/root', 1)).toBe(`vp/root\u00001`)
  })

  it('別 session は別 key', () => {
    expect(chatKey('vp/root', 1)).not.toBe(chatKey('vp/root', 2))
  })

  it('別 lane は別 key', () => {
    expect(chatKey('vp/root', 1)).not.toBe(chatKey('vp/performer/a', 1))
  })

  // key の分離が壊れる典型: 区切りが lane 名に現れうる文字だと、別の (lane, session) が
  // 同じ key に潰れる。`#` や `:` を区切りに選ぶとこの組で衝突する。
  it('lane 名に区切り候補（# : / 空白）が入っても衝突しない', () => {
    const pairs: [string, number][] = [
      ['vp/root#2', 1],
      ['vp/root', 21],
      ['vp:root', 1],
      ['vp/root 2', 1],
      ['vp/performer/a#1', 2],
    ]
    const keys = pairs.map(([l, s]) => chatKey(l, s))
    expect(new Set(keys).size).toBe(pairs.length)
  })

  // prefix 走査（clearReplaying が lane の全 session を舐める経路）が、名前の前方一致で
  // 隣の lane を巻き込まないこと。`vp/root` の prefix で `vp/root-2` を拾ってはいけない。
  it('lane prefix 走査が名前の前方一致で隣の lane を巻き込まない', () => {
    const prefix = `vp/root\u0000`
    expect(chatKey('vp/root', 3).startsWith(prefix)).toBe(true)
    expect(chatKey('vp/root-2', 3).startsWith(prefix)).toBe(false)
    expect(chatKey('vp/rootlike', 1).startsWith(prefix)).toBe(false)
  })
})
