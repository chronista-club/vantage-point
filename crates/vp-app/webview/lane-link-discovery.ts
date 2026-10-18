import type { ConversationEvent } from './console'

/** console / chat 出力からの URL 抽出と正規化（両方がここだけを通る）。 */
export function extractLoopbackUrls(text: string): string[] {
  // CSI は区切りにして、cursor 移動の前後にある文字列を結合しない。
  const plain = text.replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, '').replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, ' ')
  const urls = new Set<string>()
  for (const match of plain.matchAll(/https?:\/\/[^\s<>"'`\x00-\x1f]+/gi)) {
    const candidate = match[0].replace(/[),.;!?}]+$/, '').replace(/\](?!.*\])/g, (bracket, offset: number, value: string) => value.includes('://[') && offset === value.indexOf(']') ? bracket : '')
    try {
      const url = new URL(candidate)
      if (url.username || url.password || url.port === '0') continue
      // 自動表示 / probe に資格情報らしい query を渡さない。明示登録の契約とは独立。
      if ([...url.searchParams.keys()].some(key => /token|key|secret|password|auth|sig/i.test(key))) continue
      if (url.hostname === '0.0.0.0') url.hostname = '127.0.0.1'
      if (url.hostname !== 'localhost' && url.hostname !== '[::1]' && !/^127(?:\.\d{1,3}){3}$/.test(url.hostname)) continue
      urls.add(url.href)
    } catch { /* 未完成 / 不正 URL は次の出力で再判定する。 */ }
  }
  return [...urls]
}

export class LoopbackUrlHistory {
  private lanes = new Map<string, { urls: string[]; tails: Map<string, string> }>()
  private listeners = new Set<(lane: string) => void>()
  constructor(private limit = 32) {}

  /** 未完の token は次の chunk まで保留。末尾が URL の全文出力は complete=true。 */
  feed(lane: string, stream: string, text: string, complete = false): void {
    const state = this.lanes.get(lane) ?? { urls: [], tails: new Map<string, string>() }
    this.lanes.delete(lane)
    this.lanes.set(lane, state)
    if (this.lanes.size > 64) this.lanes.delete(this.lanes.keys().next().value!)
    const combined = (state.tails.get(stream) ?? '') + text
    const boundary = complete ? combined.length : Math.max(combined.lastIndexOf('\n'), combined.lastIndexOf(' '), combined.lastIndexOf('\t'), combined.lastIndexOf('\r')) + 1
    state.tails.delete(stream)
    if (!complete) state.tails.set(stream, combined.slice(boundary).slice(-16384))
    if (state.tails.size > 16) state.tails.delete(state.tails.keys().next().value!)
    const previous = state.urls.join('\n')
    for (const url of extractLoopbackUrls(combined.slice(0, boundary))) {
      state.urls = [...state.urls.filter(value => value !== url), url].slice(-this.limit)
    }
    if (state.urls.join('\n') !== previous) for (const listener of this.listeners) listener(lane)
  }

  get(lane: string): string[] { return [...(this.lanes.get(lane)?.urls ?? [])] }
  remove(lane: string): void {
    const chunks = terminalChunks.get(this)
    for (const [key, chunk] of chunks ?? []) {
      if (!key.startsWith(`${lane}\u0000`)) continue
      clearTimeout(chunk.timer)
      chunks!.delete(key)
    }
    if (this.lanes.delete(lane)) for (const listener of this.listeners) listener(lane)
  }
  replace(lane: string, stream: string, text: string): void {
    this.lanes.get(lane)?.tails.delete(stream)
    this.feed(lane, stream, text, true)
  }
  discardPending(lane: string, prefix: string): void {
    const tails = this.lanes.get(lane)?.tails
    for (const stream of tails?.keys() ?? []) if (stream.startsWith(prefix)) tails?.delete(stream)
  }
  finish(lane: string, prefix: string): void {
    for (const stream of [...(this.lanes.get(lane)?.tails.keys() ?? [])]) {
      if (stream.startsWith(prefix)) this.feed(lane, stream, '', true)
    }
  }
  subscribe(listener: (lane: string) => void): () => void {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }
}

/** main bundle の stream tap と R sidebar が共有。disk へは書かない。 */
export const discoveredLaneUrls = new LoopbackUrlHistory()

/** renderer の有無に依存しない console.ts の受信点から呼ぶ。ユーザー入力 / tool 引数は対象外。 */
export function observeConversationUrls(lane: string, event: ConversationEvent, session: number, history = discoveredLaneUrls): void {
  const prefix = `chat:${session}:`
  switch (event.kind) {
    case 'message_chunk':
      history.feed(lane, `${prefix}message`, event.text)
      break
    case 'codex_message':
      if (event.append) history.feed(lane, `${prefix}${event.item_id}`, event.text)
      else history.replace(lane, `${prefix}${event.item_id}`, event.text)
      break
    case 'tool_call_update':
      history.feed(lane, `${prefix}tool:${event.tool_use_id}`, event.content, true)
      break
    case 'subagent_message':
      if (event.role === 'text' || event.role === 'tool_result') history.feed(lane, `${prefix}sub:${event.parent_tool_use_id}`, event.text, true)
      break
    case 'codex_history':
      history.discardPending(lane, prefix)
      for (const child of event.events) observeConversationUrls(lane, child, session, history)
      if (!event.in_flight) history.finish(lane, prefix)
      break
    case 'replay_start':
      history.discardPending(lane, prefix)
      break
    case 'replay_end':
      if (!event.in_flight) history.finish(lane, prefix)
      break
    case 'turn_completed':
    case 'error':
    case 'engine_exited':
      history.finish(lane, prefix)
  }
}

type TerminalChunk = { decoder: TextDecoder; timer: ReturnType<typeof setTimeout> }
const terminalChunks = new WeakMap<LoopbackUrlHistory, Map<string, TerminalChunk>>()

/** replay と live は同じ表示材料。UTF-8 / URL の途中を packet 境界で確定しない。 */
export function observeTerminalUrls(lane: string, session: number, bytes: Uint8Array, history = discoveredLaneUrls): void {
  const chunks = terminalChunks.get(history) ?? new Map<string, TerminalChunk>()
  terminalChunks.set(history, chunks)
  const stream = `pty:${session}`
  const key = `${lane}\u0000${session}`
  const previous = chunks.get(key)
  if (previous) clearTimeout(previous.timer)
  const decoder = previous?.decoder ?? new TextDecoder()
  history.feed(lane, stream, decoder.decode(bytes, { stream: true }))
  // 改行せず prompt に戻る出力も、短い idle 後に末尾を確定する。
  const timer = setTimeout(() => {
    history.feed(lane, stream, decoder.decode(), true)
    chunks.delete(key)
  }, 250)
  chunks.delete(key)
  chunks.set(key, { decoder, timer })
  if (chunks.size > 128) {
    const oldest = chunks.keys().next().value!
    clearTimeout(chunks.get(oldest)!.timer)
    chunks.delete(oldest)
  }
}
