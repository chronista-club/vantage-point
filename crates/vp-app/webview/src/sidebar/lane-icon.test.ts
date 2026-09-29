// engine のロゴ表（mako 2026-09-24: Pane ヘッダ / Lane リストでロゴで見分ける）。
import { describe, expect, it } from 'vitest'
import { agentDisplayName, agentIcon } from './lane'

describe('engine のロゴ', () => {
  it('AI engine は MingCute のロゴ（focused / active は fill）', () => {
    expect(agentIcon('claude', false)).toBe('mingcute:claude-line')
    expect(agentIcon('claude', true)).toBe('mingcute:claude-fill')
    expect(agentIcon('codex', false)).toBe('mingcute:openai-line')
    expect(agentIcon('grok', true)).toBe('mingcute:grok-fill')
  })
  it('ロゴの無い engine は汎用 icon、未知は null', () => {
    expect(agentIcon('opencode', false)).toBe('ph:code')
    expect(agentIcon('vpcode', false)).toBe('ph:flask')
    expect(agentIcon('unknown-engine', false)).toBeNull()
  })
  it('tooltip の表示名は engine 名', () => {
    expect(agentDisplayName('claude')).toBe('Claude')
    expect(agentDisplayName('codex')).toBe('Codex')
    expect(agentDisplayName('grok')).toBe('Grok')
    expect(agentDisplayName('opencode')).toBe('OpenCode')
  })
})
