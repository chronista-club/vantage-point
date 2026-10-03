/**
 * Codex の native 入力（queue / mode / permission）を `conversation:codex_input` で送る。
 * chatview と codex-settings-panel の両方から呼ぶので store 操作ごと関数にしてある。
 */
import { produce } from 'solid-js/store'
import { foldInto, type LaneChat, type Submission } from './chat-model'
import { nextRequestId } from './console'

type Ipc = { postMessage(m: string): void }

/** 送信中の入力があるか（= 次の入力を受けない）。 */
export function codexInputBusy(lc: LaneChat): boolean {
  return !!lc.state.codexInput
}

/** 戻り値 = 送れたか。ready でない queue には `refresh` 以外を送らない。 */
export function sendCodexInput(
  lc: LaneChat,
  lane: string,
  session: number,
  action: Record<string, unknown>,
  text = '',
  images: Submission['images'] = [],
): boolean {
  const queue = lc.state.codexQueue
  if (!queue || (!queue.ready && action.kind !== 'refresh') || codexInputBusy(lc)) return false
  const requestId = nextRequestId('codex-input')
  lc.set('codexInput', { id: requestId, text, images, status: 'sending', error: null })
  try {
    const ipc = (globalThis as unknown as { ipc?: Ipc }).ipc
    if (!ipc) throw new Error('接続がありません。入力は送信されていません。')
    ipc.postMessage(JSON.stringify({ t: 'conversation:codex_input', lane,
      session, thread_id: queue.thread_id, request_id: requestId,
      action: { ...action, images, client_id: requestId } }))
    setTimeout(() => {
      if (lc.state.codexInput?.id === requestId && lc.state.codexInput.status === 'sending') {
        lc.set(produce(s => foldInto(s, { kind: 'codex_queue', queue: null, request_id: requestId,
          error: '送信結果を確認できません。自動再送はしていません。待機一覧と会話履歴を確認してください。' })))
      }
    }, 45_000)
  } catch (error) {
    lc.set(produce(s => foldInto(s, { kind: 'codex_queue', queue: null, request_id: requestId,
      error: error instanceof Error ? error.message : String(error) })))
    return false
  }
  return true
}
