// mem_1CexxKRvy7R87G6RL6RzuX — 設定結果は会話の終了イベントではない。
import { describe, expect, it } from 'vitest'
import { emptyChatState, foldInto } from './chat-model'

describe('Codex model and effort settings', () => {
  it('設定 snapshot を保持し、会話本文と生成状態を変えない', () => {
    const s = emptyChatState()
    foldInto(s, { kind: 'message_chunk', text: 'working' })
    const config = { models: [{ model: 'test-model', label: 'Test', efforts: ['low', 'high'], default_effort: 'low' }], model: 'test-model', effort: 'high', selection: null, error: null }
    foldInto(s, { kind: 'codex_config', config, request_id: null, error: null })
    expect(s).toMatchObject({ codexConfig: config, streaming: true })
    expect(s.items).toEqual([{ kind: 'assistant', text: 'working' }])
  })

  it('設定の拒否は進行中の応答を終わらせない', () => {
    const s = emptyChatState()
    Object.assign(s, { codexSettingsRequest: 'request-1' })
    foldInto(s, { kind: 'message_chunk', text: 'working' })
    foldInto(s, { kind: 'codex_config', config: null, request_id: 'request-1', error: '応答中は変更できません' })
    expect(s).toMatchObject({ streaming: true, codexSettingsRequest: null, codexSettingsError: '応答中は変更できません' })
  })
  it('古い設定結果は現在の要求を完了させない', () => {
    const s = emptyChatState()
    s.codexSettingsRequest = 'current'
    foldInto(s, { kind: 'codex_config', config: null, request_id: 'old', error: 'old failure' })
    expect(s.codexSettingsRequest).toBe('current')
    expect(s.codexSettingsError).toBeUndefined()
  })

  it('遅れた成功ACKが新しいhost snapshotを巻き戻さない', () => {
    const s = emptyChatState()
    const config = { models: [], model: 'native', effort: 'low', selection: { model: 'B', effort: 'high' }, error: null }
    s.codexSettingsRequest = 'request-A'
    foldInto(s, { kind: 'codex_config', config, request_id: null, error: null })
    foldInto(s, { kind: 'codex_config', config: { ...config, selection: { model: 'A', effort: 'low' } }, request_id: 'request-A', error: null })
    expect(s.codexConfig?.selection?.model).toBe('B')
    expect(s.codexSettingsRequest).toBeNull()
  })

})
