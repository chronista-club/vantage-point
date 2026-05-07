/**
 * `mcp__show` → PP body markdown render の bridge subscriber。
 *
 * VP-142 / PR-ε-3。 doc 13 §9 PR-ε series の S3 段、 origin Goal **B 達成**
 * (= 何某かの markdown が Canvas に表示される) の最後の物理化。
 *
 * 設計の核 (大幅縮小版):
 * - **既存 `/ws` endpoint を再利用**: server.rs:444 + ws.rs:26 の `state.hub.subscribe()` が
 *   `ProcessMessage` を全 subscriber に stream。 `/api/show` (= mcp__show) は既に hub に broadcast
 *   しているので、 JS 側で `/ws` を subscribe するだけで pipeline 完成
 * - **新 endpoint 不要**: 当初 plan「新 `/ws/show`」 → 廃案、 既存基盤で十分
 * - **port 解決は `window.ensureLane` wrap** (Approach E、 Rust 改修ゼロ): Lane → SP port mapping は
 *   既に Rust が `evaluate_script` で渡しているので、 ensureLane を wrap して `lanePortRegistry` Map を
 *   構築、 Lane 切替で port lookup
 *
 * 公開 API (entry.tsx で配線):
 * - `connectShowWs(port)`: 指定 port の `/ws` に subscribe、 既存接続あれば close して付け替え
 * - `disconnectShowWs()`: close current
 * - `getLanePort(address)`: Lane address から port lookup (lanePortRegistry の view)
 * - 副作用: import 時に `window.ensureLane` を wrap して registry を構築
 */

import { renderPP, clearPP, type ContentType } from './pp'

/** Lane address (例: `"vantage-point/lead"`) → SP port mapping. window.ensureLane wrap で populate. */
const lanePortRegistry = new Map<string, number>()

/**
 * 「subscribe したい Lane address」 を保持する slot。
 *
 * Startup race 救済の核: Rust が auto-select Lane で setActivePane を 1 回発火するタイミングは
 * ensureLane より先で、 lanePortRegistry が空のため connectShowWs が skip される。 ensureLane が
 * 後から呼ばれた時点で wantedLaneAddress と一致していれば自動 connect させる event-driven recovery。
 */
let wantedLaneAddress: string | null = null

// =============================================================================
// window.ensureLane wrap で Lane→port を track
// =============================================================================
// Rust 側 lane_js::ensure_lane (app.rs:1810) が `window.ensureLane(address, port)` を
// evaluate_script で呼ぶたび、 我々の registry にも記録する。 既存挙動 (laneInstances 構築) は壊さない。

interface EnsureLaneFn {
  (address: string, port: number): void
}

interface EnsureLaneWindow {
  ensureLane?: EnsureLaneFn
}

const installEnsureLaneWrap = (): void => {
  const w = window as unknown as EnsureLaneWindow
  const original = w.ensureLane
  if (typeof original !== 'function') {
    // ensureLane 未定義 (= main_area.rs JS が後で定義する pattern)。 polling で待つ。
    setTimeout(installEnsureLaneWrap, 50)
    return
  }
  // 二重 install 防止: wrap 済 marker
  const marker = '__vpShowSubscriberWrapped__'
  if ((original as unknown as Record<string, boolean>)[marker]) return
  const wrapped: EnsureLaneFn = (address, port) => {
    const previous = lanePortRegistry.get(address)
    lanePortRegistry.set(address, port)
    // Race recovery: registry が新規 populate された時、 wanted Lane と一致するなら auto connect。
    // (auto-select Lane の subscription が startup race で skip された場合の救済 path)
    if (address === wantedLaneAddress && previous !== port) {
      connectShowWs(port)
    }
    original(address, port)
  }
  ;(wrapped as unknown as Record<string, boolean>)[marker] = true
  w.ensureLane = wrapped
  console.info('[show-subscriber] wrapped window.ensureLane for lane→port tracking')
}

// 即時 install を試行 (まだ ensureLane 未定義なら polling)
installEnsureLaneWrap()

/** Lane address から SP port を lookup (entry.tsx bridge 等で利用). */
export function getLanePort(address: string): number | undefined {
  return lanePortRegistry.get(address)
}

/**
 * 「subscribe したい Lane」 を宣言する。 registry に既に port があれば即 connect、
 * なければ wantedLaneAddress slot を保持しておき、 後で ensureLane で同 address が register された
 * 時に auto connect させる (startup race 救済)。
 *
 * `null` を渡すと wanted を解除 (Lane unselect 等)。
 */
export function setWantedLane(address: string | null): void {
  wantedLaneAddress = address
  if (!address) return
  const port = lanePortRegistry.get(address)
  if (port !== undefined) {
    connectShowWs(port)
  }
}

// =============================================================================
// /ws subscribe + ProcessMessage handling
// =============================================================================

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

let currentWs: WebSocket | null = null
let currentPort: number | null = null

function dispatchShow(msg: ShowMessage): void {
  // content variant 判定 → vpPP API に振り分け
  // (image_base64 / url は v1 skip、 PR-ε-4 以降で canvas iframe 等で対応)
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
    console.info('[show-subscriber] skip unsupported content variant:', Object.keys(msg.content))
    return
  }
  // pane_id 別 routing は v1 では未対応 (全 show を PP body 一本に集約)。
  // 将来 PR-ε-4+ で pane_id → 複数 surface 振り分け実装。
  renderPP(body, contentType)
}

function handleMessage(msg: AnyMessage): void {
  if (msg.type === 'show') {
    dispatchShow(msg as ShowMessage)
  } else if (msg.type === 'clear') {
    clearPP()
  }
  // 他 ProcessMessage variant (ChatChunk / DebugInfo / SessionList 等) は PP には流さない (no-op)
}

/**
 * 指定 port の `/ws` endpoint に subscribe。
 * 既に同 port に接続中なら no-op、 別 port なら old を close して付け替え。
 */
export function connectShowWs(port: number): void {
  if (currentPort === port && currentWs?.readyState === WebSocket.OPEN) {
    return
  }
  if (currentWs) {
    try {
      currentWs.close()
    } catch (_) {
      /* noop */
    }
    currentWs = null
  }
  currentPort = port
  const url = `ws://127.0.0.1:${port}/ws`
  console.info(`[show-subscriber] connecting to ${url}`)
  let ws: WebSocket
  try {
    ws = new WebSocket(url)
  } catch (e) {
    console.warn(`[show-subscriber] failed to construct WebSocket(${url})`, e)
    return
  }
  currentWs = ws
  ws.onopen = () => {
    console.info(`[show-subscriber] open: ${url}`)
  }
  ws.onmessage = (e) => {
    try {
      const msg = JSON.parse(e.data as string) as AnyMessage
      handleMessage(msg)
    } catch (err) {
      console.warn('[show-subscriber] parse error', err)
    }
  }
  ws.onerror = (e) => {
    console.warn('[show-subscriber] error', e)
  }
  ws.onclose = () => {
    if (currentWs === ws) {
      currentWs = null
    }
    console.info(`[show-subscriber] close: ${url}`)
  }
}

/** Current WS を close (Lane unselect / app shutdown 時等). */
export function disconnectShowWs(): void {
  if (currentWs) {
    try {
      currentWs.close()
    } catch (_) {
      /* noop */
    }
  }
  currentWs = null
  currentPort = null
}

/** DevTools 検査用: 現状の subscriber state. */
export function getShowSubscriberStatus(): {
  port: number | null
  readyState: number | null
  registrySize: number
} {
  return {
    port: currentPort,
    readyState: currentWs?.readyState ?? null,
    registrySize: lanePortRegistry.size,
  }
}
