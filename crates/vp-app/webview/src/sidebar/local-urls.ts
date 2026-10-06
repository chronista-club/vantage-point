import type { LocalUrlsResult } from '../generated/SidebarIpc'
import { sendIpc } from './ipc'

export type LocalUrlEntry = { id: string; url: string; label: string }
export type UrlProbe = { state: 'responding'; status: number } | { state: 'refused' } | { state: 'failed'; message: string }
export type UrlAction = { action: 'load' } | { action: 'save'; expected: LocalUrlEntry[]; entries: LocalUrlEntry[] } | { action: 'open' | 'probe'; id: string }
export type UrlResult = { entries?: LocalUrlEntry[]; probe?: UrlProbe }
const pending = new Map<string, (result: LocalUrlsResult) => void>()

/** 応答は request ID で一つのフォームにだけ届ける。遅延・別 Lane の応答は混ぜない。 */
export function receiveLocalUrls(result: LocalUrlsResult): void {
  pending.get(result.req)?.(result)
}
export function requestLocalUrls(path: string, address: string, payload: UrlAction): Promise<UrlResult> {
  if (!window.ipc) return Promise.reject(new Error('アプリに接続していません'))
  const req = crypto.randomUUID()
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(req); reject(new Error('応答を確認できませんでした。再読み込みしてください')) }, 8000)
    pending.set(req, result => {
      pending.delete(req); clearTimeout(timer)
      if (result.error) reject(new Error(result.error))
      else resolve(result.payload as UrlResult)
    })
    try { sendIpc({ t: 'local_urls:request', req, path, address, payload }) }
    catch (e) { pending.delete(req); clearTimeout(timer); reject(e) }
  })
}
export function probeLabel(probe: UrlProbe | undefined): string {
  if (!probe) return '未確認'
  if (probe.state === 'responding') return `応答あり · HTTP ${probe.status}`
  if (probe.state === 'refused') return '接続拒否（停止の可能性）'
  return '確認失敗'
}
