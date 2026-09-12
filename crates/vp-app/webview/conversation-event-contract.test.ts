/**
 * ConversationEvent の Rust ↔ TS 契約 test（棚卸し 項目 8 / 8-1）。
 *
 * fixture は Rust が実際に serialize した送信形（`src/generated/ConversationEventFixtures.ts`、
 * `cargo test -p vantage-point --test conversation_event_fixtures` で再生成）。型検査（`satisfies`）は
 * tsc が担うので、ここでは
 *   1. kind の網羅（TS union の kind 集合 = fixture の kind 集合）
 *   2. 実行時 shape（kind ごとの必須 field と型）を any / 型断言を経由せずに検査
 *   3. 代表列（replay → live）が chat-model の foldInto で状態になる
 * を固定する。
 */
import { describe, expect, it } from 'vitest'
import { CONVERSATION_EVENT_FIXTURES } from './src/generated/ConversationEventFixtures'
import type { EngineConversationEvent } from './console'
import { emptyChatState, foldInto } from './chat-model'
import { TURN_CLOSING_KINDS } from './session-now-bridge'

type Kind = EngineConversationEvent['kind']

/** TS union の kind 一覧。union に variant を足したら **ここにも足す**（fixture の網羅 assert が落ちる）。 */
const ENGINE_KINDS = [
  'codex_interactions',
  'codex_interaction_result',
  'session_init',
  'replay_start',
  'replay_end',
  'codex_config',
  'codex_history',
  'user_message',
  'message_chunk',
  'thought_chunk',
  'tool_call',
  'tool_call_update',
  'subagent_message',
  'plan',
  'turn_completed',
  'now_line',
  'error',
  'engine_exited',
  'question',
  'permission_request',
] as const satisfies readonly Kind[]

// union 側に ENGINE_KINDS に無い kind があれば型 error（網羅の逆向き）。
type Missing = Exclude<Kind, (typeof ENGINE_KINDS)[number]>
const _missing: Missing extends never ? true : never = true
void _missing

/** kind ごとの必須 field とその typeof。optional（Rust `skip_serializing_if`）は載せない。 */
const REQUIRED: Record<Kind, Record<string, 'string' | 'boolean' | 'number' | 'object' | 'array' | 'null'>> = {
  codex_interactions: { requests: 'array' },
  codex_interaction_result: { request_id: 'string', error: 'null' },
  session_init: { session_id: 'string' },
  replay_start: {},
  replay_end: { in_flight: 'boolean' },
  codex_config: { config: 'object', request_id: 'null', error: 'null' },
  codex_history: { thread_id: 'string', events: 'array', user_message_ids: 'array', in_flight: 'boolean', truncated: 'boolean' },
  user_message: { text: 'string' },
  message_chunk: { text: 'string' },
  thought_chunk: { text: 'string' },
  tool_call: { id: 'string', name: 'string', input: 'object' },
  tool_call_update: { tool_use_id: 'string', content: 'string', is_error: 'boolean' },
  subagent_message: { parent_tool_use_id: 'string', role: 'string', text: 'string' },
  plan: { entries: 'array' },
  turn_completed: { session_id: 'string' },
  now_line: { text: 'string' },
  error: { message: 'string' },
  engine_exited: { message: 'string' },
  question: { request_id: 'string', questions: 'array' },
  permission_request: { request_id: 'string', tool_name: 'string', input: 'object' },
}

function typeOf(v: unknown): string {
  if (Array.isArray(v)) return 'array'
  if (v === null) return 'null'
  return typeof v
}

const fixtures = Object.entries(CONVERSATION_EVENT_FIXTURES) as [string, EngineConversationEvent][]

describe('ConversationEvent contract (Rust fixture ↔ TS mirror)', () => {
  it('fixture が全 kind を 1 つ以上含む（Rust variant = TS union）', () => {
    const seen = new Set(fixtures.map(([, ev]) => ev.kind))
    expect([...seen].sort()).toEqual([...ENGINE_KINDS].sort())
  })

  it('kind ごとの必須 field が送信形に存在し型が合う', () => {
    for (const [name, ev] of fixtures) {
      const rec = ev as unknown as Record<string, unknown>
      for (const [field, ty] of Object.entries(REQUIRED[ev.kind])) {
        expect(field in rec, `${name}: ${field} が無い`).toBe(true)
        expect(typeOf(rec[field]), `${name}: ${field} の型`).toBe(ty)
      }
    }
  })

  it('serde(default) だけの field は省略されず必ず出る（TS 側で必須にした根拠）', () => {
    const tcu = fixtures.filter(([, ev]) => ev.kind === 'tool_call_update')
    expect(tcu.length).toBeGreaterThan(0)
    for (const [, ev] of tcu) expect('is_error' in ev).toBe(true)
    const q = CONVERSATION_EVENT_FIXTURES.question
    expect('multi_select' in q.questions[0]).toBe(true)
    for (const opt of q.questions[0].options) expect(typeof opt.description).toBe('string')
  })

  it('skip_serializing_if の field は無い形と有る形の両方が fixture にある', () => {
    expect('model' in CONVERSATION_EVENT_FIXTURES.session_init_minimal).toBe(false)
    expect(CONVERSATION_EVENT_FIXTURES.session_init_full.model).toBe('claude-fable-5-1')
    expect('context_tokens' in CONVERSATION_EVENT_FIXTURES.turn_completed_minimal).toBe(false)
    expect(CONVERSATION_EVENT_FIXTURES.turn_completed_full.context_tokens).toBe(12345)
    expect('active_form' in CONVERSATION_EVENT_FIXTURES.plan.entries[0]).toBe(false)
    expect(CONVERSATION_EVENT_FIXTURES.plan.entries[1].active_form).toBe('直している')
  })

  it('turn を閉じる kind は union の kind である', () => {
    for (const k of TURN_CLOSING_KINDS) expect(ENGINE_KINDS).toContain(k)
  })

  it('代表列（replay → live turn）が chat-model に畳める', () => {
    const F = CONVERSATION_EVENT_FIXTURES
    const s = emptyChatState()
    const seq: EngineConversationEvent[] = [
      F.replay_start,
      F.session_init_full,
      F.user_message,
      F.message_chunk,
      F.replay_end_idle,
      F.tool_call,
      F.tool_call_update_ok,
      F.plan,
      F.question,
      F.turn_completed_full,
    ]
    for (const ev of seq) foldInto(s, ev)
    // 状態の細部は chat-model の test が固定する。ここでは「全 variant が例外なく畳める」ことだけ。
    for (const [, ev] of fixtures) foldInto(emptyChatState(), ev)
    expect(s).toBeTruthy()
  })
})
