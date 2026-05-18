/**
 * Canvas (Paisley Park) ProcessMessage の handler。
 *
 * wiremsg Stage 2。vp-app Rust が SP の "canvas" Unison channel を QUIC 購読し、
 * 受信した ProcessMessage を `window.vpCanvas.handleMessage(msg)` 経由でこの handler
 * に注入する (wry-IPC edge)。旧 `show-subscriber.ts` の `/ws` WebSocket 直接購読を置換。
 *
 * Stage 2 で WebView は SP と直接 WS を張らず、Rust host を edge に挟む設計に統一
 * (wiremsg 設計 mem_1CbA198fsHJsoKpu2jDUCv の「WebView edge = wry-IPC」原則)。port
 * 解決・channel 購読・再接続・active project 判定はすべて Rust 側 (`spawn_canvas_subscription`)。
 *
 * 公開 API (entry.tsx で `window.vpCanvas` に配線):
 * - `handleMessage(msg)`: ProcessMessage 1 件を PP body に dispatch
 */

import { renderPP, clearPP, type ContentType } from './pp'

/** ProcessMessage::Show JSON (serde rename_all = "snake_case", tag = "type"). */
interface ShowMessage {
  type: 'show'
  pane_id: string
  content: {
    markdown?: string
    html?: string
    log?: string
    url?: string
    image_base64?: { data: string; mime_type: string }
  }
  append: boolean
  title?: string
}

interface ClearMessage {
  type: 'clear'
  pane_id: string
}

type AnyMessage = ShowMessage | ClearMessage | { type: string; [key: string]: unknown }

function dispatchShow(msg: ShowMessage): void {
  // content variant 判定 → vpPP API に振り分け。
  // (image_base64 / url は v1 skip、後続で canvas iframe 等で対応)
  let body: string | undefined
  let contentType: ContentType = 'markdown'
  if (msg.content.markdown !== undefined) {
    body = msg.content.markdown
    contentType = 'markdown'
  } else if (msg.content.html !== undefined) {
    body = msg.content.html
    contentType = 'html'
  } else if (msg.content.log !== undefined) {
    body = msg.content.log
    contentType = 'text'
  } else {
    console.info('[vp-canvas] skip unsupported content variant:', Object.keys(msg.content))
    return
  }
  // pane_id 別 routing は未対応 (全 show を PP body 一本に集約)。
  renderPP(body, contentType)
}

/**
 * Rust 注入口。ProcessMessage 1 件 (parse 済 object) を受け取り PP body に dispatch する。
 * Rust 側 `spawn_canvas_subscription` が active project の canvas message ごとに呼ぶ。
 */
export function handleMessage(msg: AnyMessage): void {
  if (msg.type === 'show') {
    dispatchShow(msg as ShowMessage)
  } else if (msg.type === 'clear') {
    clearPP()
  }
  // 他 ProcessMessage variant (ChatChunk / DebugInfo / SessionList 等) は PP には流さない (no-op)
}
