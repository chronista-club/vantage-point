/**
 * 音声入力（push-to-talk）の webview 側の線。
 *
 * - 送り: 🎙を押した / 離した → IPC `voice:start` / `voice:stop`（録音・認識は Rust の `voice/`）
 * - 受け: push `voice:text` / `voice:error` → 押した入力欄（lane + session）に届ける
 *
 * 入力欄の文字は chat の component の中の signal にしか無いので、component が mount 時に
 * 受け口を登録し、push はこの登録簿で宛先の欄に振り分ける。
 */

/** 認識結果の受け口（chat の入力欄 1 つ分）。 */
export interface VoiceSink {
  text(text: string): void
  error(message: string): void
}

const sinks = new Map<string, VoiceSink>()
const sinkKey = (lane: string, session: number) => `${lane}#${session}`

/** 入力欄の受け口を登録する。戻り値を呼ぶと外れる（component の cleanup で呼ぶ）。 */
export function registerVoiceSink(lane: string, session: number, sink: VoiceSink): () => void {
  const key = sinkKey(lane, session)
  sinks.set(key, sink)
  return () => {
    if (sinks.get(key) === sink) sinks.delete(key)
  }
}

/** push `voice:text`: 認識した文字（空 = 何も聞き取れなかった）。 */
export function deliverVoiceText(lane: string, session: number, text: string): void {
  sinks.get(sinkKey(lane, session))?.text(text)
}

/** push `voice:error`: 録音・認識の失敗理由。 */
export function deliverVoiceError(lane: string, session: number, message: string): void {
  sinks.get(sinkKey(lane, session))?.error(message)
}

/** 🎙を押した（start）/ 離した（stop）を Rust に伝える。 */
export function sendVoice(action: 'start' | 'stop', lane: string, session: number): void {
  const ipc = (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc
  ipc?.postMessage(JSON.stringify({ t: `voice:${action}`, lane, session }))
}
