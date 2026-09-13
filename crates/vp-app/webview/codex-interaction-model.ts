import type { ConversationEvent } from './console'
import type { CodexInteraction } from './src/generated/CodexInteraction'

export type CodexInteractionState = {
  requests: CodexInteraction[]
  sending: string[]
  errors: Record<string, string>
  drafts?: Record<string, Record<string, string>>
}

type Holder = { codexInteractions?: CodexInteractionState }

/** 会話本文・streaming・type-ahead とは独立した制御状態。 */
export function foldCodexInteractions(s: Holder, ev: ConversationEvent): boolean {
  if (ev.kind !== 'codex_interactions' && ev.kind !== 'codex_interaction_result') return false
  const state = s.codexInteractions ??= { requests: [], sending: [], errors: {} }
  if (ev.kind === 'codex_interactions') {
    state.requests = ev.requests.map(request => {
      const existing = state.requests.find(r => r.request_id === request.request_id)
      return existing && JSON.stringify(existing) === JSON.stringify(request) ? existing : request
    })
    const ids = new Set(ev.requests.map(r => r.request_id))
    state.sending = state.sending.filter(id => {
      const request = state.requests.find(r => r.request_id === id)
      return request && !(request.kind === 'async_question' && !request.can_accept)
    })
    for (const id of Object.keys(state.drafts ?? {})) if (!ids.has(id)) delete state.drafts![id]
    for (const id of Object.keys(state.errors)) if (!ids.has(id)) delete state.errors[id]
  } else if (state.sending.includes(ev.request_id)) {
    state.sending = state.sending.filter(id => id !== ev.request_id)
    if (ev.error) state.errors[ev.request_id] = ev.error
    else {
      state.requests = state.requests.filter(r => r.request_id !== ev.request_id)
      if (state.drafts) delete state.drafts[ev.request_id]
    }
  }
  return true
}

/** クリックだけでは回答済みにしない。複数カードもそれぞれ一度だけ送信。 */
export function beginCodexResponse(s: Holder, id: string): boolean {
  const state = s.codexInteractions
  if (!state?.requests.some(r => r.request_id === id) || state.sending.includes(id)) return false
  state.sending.push(id)
  delete state.errors[id]
  return true
}
