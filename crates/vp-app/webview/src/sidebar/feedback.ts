import { createSignal } from 'solid-js'
import type { IpcEnvelope } from '../generated/SidebarIpc'

export const [sidebarError, reportSidebarError] = createSignal<string | null>(null)
type ResultListener = (repo: string, name: string, error: string | null) => void
const results = new Set<ResultListener>()
export function reportSubCreateResult(repo: string, name: string, error: string | null): void {
  for (const receive of results) receive(repo, name, error)
}

/** One mounted form owns one outstanding request. Inputs stay with the form. */
export function createLaneCreation(
  repo: string,
  send: (message: IpcEnvelope) => void,
  close: () => void,
) {
  const [pending, setPending] = createSignal(false)
  const [error, setError] = createSignal<string | null>(null)
  let submittedName = ''
  const receive: ResultListener = (path, name, reason) => {
    if (!pending() || path !== repo || name !== submittedName) return
    setPending(false)
    setError(reason)
    if (reason === null) close()
  }
  results.add(receive)
  return {
    pending, error,
    submit(input: { name: string; branch?: string; agent?: string }) {
      if (pending() || !input.name.trim()) return
      submittedName = input.name.trim()
      setError(null)
      setPending(true)
      try {
        send({ ...input, t: 'lane:add_sub', path: repo, name: submittedName })
      } catch (e) {
        setPending(false)
        setError(e instanceof Error ? e.message : String(e))
      }
    },
    dispose: () => { results.delete(receive) },
  }
}

type AgentsPayload = { repo_path?: string; agents?: unknown[]; error?: string | null }
const agents = new Set<(payload: AgentsPayload) => void>()
export function reportAgentsResult(payload: AgentsPayload): void {
  for (const receive of agents) receive(payload)
}
export function subscribeAgents(receive: (payload: AgentsPayload) => void): () => void {
  agents.add(receive)
  return () => { agents.delete(receive) }
}
