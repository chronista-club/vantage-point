/** Agent activity shared by editor-host and sidebar bundles in the same document. */
import type { ConversationEvent } from './console'
import { sessionNowKey } from './session-now-bridge'

export type AgentActivity = 'thinking' | 'working' | 'waiting' | 'completed' | 'idle' | 'error'
export type ActivityStamp = { phase: AgentActivity; at: number; needsUser?: boolean; completedAt?: number }
export type ActivityNotice = { lane: string; session: number; stamp: ActivityStamp }
const EVENT = 'vp:session-activity'
type ActivityWindow = Window & { __vpAgentActivity?: Record<string, ActivityStamp> }

export function readAgentActivity(lane: string, session: number): ActivityStamp | undefined {
  return (window as ActivityWindow).__vpAgentActivity?.[sessionNowKey(lane, session)]
}

export function emitAgentActivity(lane: string, session: number, event: ConversationEvent): void {
  const previous = readAgentActivity(lane, session)
  let phase: AgentActivity
  let needsUser = previous?.needsUser ?? false
  let completedAt = previous?.completedAt
  switch (event.kind) {
    case 'thought_chunk': phase = 'thinking'; completedAt = undefined; break
    case 'message_chunk': case 'codex_message': case 'tool_call': case 'tool_call_update': phase = 'working'; completedAt = undefined; break
    case 'question': case 'permission_request': phase = 'waiting'; break
    case 'codex_interactions':
      needsUser = event.requests.some(r => r.can_accept || r.item_id == null)
      if (needsUser) phase = 'waiting'
      else if (previous?.phase === 'waiting') phase = completedAt ? 'completed' : 'working'
      else return
      break
    case 'turn_completed': completedAt = Date.now(); phase = needsUser ? 'waiting' : 'completed'; break
    case 'error': phase = 'error'; break
    case 'submit_result': phase = event.error ? 'error' : 'working'; completedAt = undefined; break
    case 'engine_exited':
      if (previous?.phase === 'completed') return
      phase = 'idle'; break
    case 'replay_end': case 'codex_history':
      if (event.in_flight) phase = 'working'
      else if (previous?.phase === 'completed' || previous?.phase === 'waiting') return
      else phase = 'idle'
      break
    case 'session_init': phase = 'idle'; needsUser = false; completedAt = undefined; break
    default: return
  }
  if (needsUser && (phase === 'thinking' || phase === 'working')) phase = 'waiting'
  if (previous?.phase === phase && previous?.needsUser === needsUser && previous?.completedAt === completedAt && event.kind !== 'turn_completed') return
  const stamp = { phase, at: phase === 'completed' ? completedAt! : Date.now(), needsUser, completedAt }
  const cache = ((window as ActivityWindow).__vpAgentActivity ??= {})
  cache[sessionNowKey(lane, session)] = stamp
  window.dispatchEvent(new CustomEvent<ActivityNotice>(EVENT, {detail: {lane, session, stamp}}))
}

export function onAgentActivity(listener: (notice: ActivityNotice) => void): () => void {
  const handler = (event: Event) => listener((event as CustomEvent<ActivityNotice>).detail)
  window.addEventListener(EVENT, handler)
  return () => window.removeEventListener(EVENT, handler)
}
