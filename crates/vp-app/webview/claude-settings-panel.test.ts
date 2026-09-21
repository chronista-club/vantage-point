// model select の現在値 — intent 優先（実測は次の turn まで届かないため）。
import { describe, expect, it } from 'vitest'
import { claudeModelCurrent } from './claude-settings-panel'
import type { ConversationSession } from './console'

const entry = (settings: ConversationSession['settings']): ConversationSession =>
  ({ key: 1, agent: 'claude', engine_session_id: null, live: true, focused: true, settings })

describe('claudeModelCurrent', () => {
  it('intent の model を実測より優先する（切替直後に旧 model へ戻らない）', () => {
    expect(claudeModelCurrent(entry({ claude: { model: 'claude-opus-5', effort: 'high' } }), 'claude-sonnet-5'))
      .toBe('claude-opus-5')
  })
  it('intent はあるが model 無し = Default を選んだ → ""', () => {
    expect(claudeModelCurrent(entry({ claude: { effort: 'high' } }), 'claude-sonnet-5')).toBe('')
  })
  it('intent 無し（VP が何も指定していない）→ 実測', () => {
    expect(claudeModelCurrent(entry(null), 'claude-sonnet-5')).toBe('claude-sonnet-5')
    expect(claudeModelCurrent(undefined, 'claude-sonnet-5')).toBe('claude-sonnet-5')
  })
})
