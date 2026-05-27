/**
 * Canvas (Paisley Park) ProcessMessage の handler。
 *
 * wiremsg Stage 2。vp-app Rust が SP の "canvas" Unison channel を QUIC 購読し、
 * 受信した ProcessMessage を `window.vpCanvas.handleMessage(msg)` 経由でこの handler
 * に注入する (wry-IPC edge)。旧 `show-subscriber.ts` の `/ws` WebSocket 直接購読を置換。
 *
 * ## doc 19 PP Canvas Stack Model (2026-05-27)
 *
 * 旧版: 全 show を PP body に **上書き render** (= 直前 content 喪失)。
 * 新版: **canvas = items の ring buffer (N=10) + cursor = 現在 main 表示中 item の id**
 *       という stack model で物理化。 mcp__show 投入で items に push、 cursor を新 item
 *       に切替、 旧 main は strip (= Phase 2 で追加) に残る。
 *
 * Phase 1 (本 commit): data model (CanvasState) + push / cursor 移動 / delete API。
 *                       main pane render は items[cursor] 連動に rewire。
 *                       strip UI 自体は Phase 2 で HistoryStrip component を新規追加。
 *
 * 関連 doc: docs/design/19-canvas-stack-model.md
 *
 * 公開 API (entry.tsx で `window.vpCanvas` に配線):
 * - `handleMessage(msg)`: ProcessMessage 1 件を canvas に push (= mcp__show 入口)
 * - `getCanvasState()`: 現在の items + cursor の readonly snapshot (= HistoryStrip 購読用)
 * - `setCursor(id)`: cursor を指定 item に移動 (= thumbnail click から呼ぶ)
 * - `deleteItem(id)`: 指定 item を削除、 cursor 中なら 右隣→左隣→null fallback
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
  /** doc 19: spec から omit。 caller が送ってきても無視 (silent ignore)。 */
  append?: boolean
  title?: string
}

interface ClearMessage {
  type: 'clear'
  pane_id: string
}

type AnyMessage = ShowMessage | ClearMessage | { type: string; [key: string]: unknown }

// ============================================================================
// doc 19 PP Canvas Stack Model: data model
// ============================================================================

/** strip の 1 cell が指す 1 item。 mcp__show 投入ごとに 1 つ生成される。 */
export interface CanvasItem {
  id: string
  content: string
  contentType: ContentType
  title?: string
  createdAt: string
}

/** ring buffer 上限。 doc 19 §3 で N=10 確定。 */
export const MAX_ITEMS = 10

/** module-local singleton state。 ephemeral = bundle reload で reset。 */
const canvasState: { items: CanvasItem[]; cursor: string | null } = {
  items: [],
  cursor: null,
}

/** state 変更 listener。 HistoryStrip (Phase 2) が購読する。 */
type StateListener = () => void
const stateListeners = new Set<StateListener>()

function notifyStateChange(): void {
  for (const listener of stateListeners) {
    try {
      listener()
    } catch (e) {
      console.warn('[vp-canvas] state listener error', e)
    }
  }
}

/** unique id を生成。 crypto.randomUUID() があれば使用、 fallback で random + timestamp。 */
function generateId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID()
  }
  return `item-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`
}

/** items 内で id を持つ item の index を返す (見つからなければ -1)。 */
function indexOfItem(id: string): number {
  return canvasState.items.findIndex((i) => i.id === id)
}

/** items[cursor] を main pane に描画。 cursor === null なら何もしない (= main 空表示は別経路)。 */
function renderCurrentMain(): void {
  if (canvasState.cursor === null) {
    // main empty placeholder: 暫定で clearPP() で空にする (= Phase 2 で empty UI 整備)
    clearPP()
    return
  }
  const item = canvasState.items.find((i) => i.id === canvasState.cursor)
  if (!item) {
    // 不整合 (cursor が items 外を指す) は致命的だが、 防御的に clear で復帰
    console.warn('[vp-canvas] cursor points to missing item:', canvasState.cursor)
    canvasState.cursor = null
    clearPP()
    return
  }
  renderPP(item.content, item.contentType)
}

// ============================================================================
// Public API: HistoryStrip (Phase 2) と外部 caller 用
// ============================================================================

/** state の readonly snapshot。 listener 経由で更新を購読する想定。 */
export function getCanvasState(): { items: ReadonlyArray<CanvasItem>; cursor: string | null } {
  return { items: canvasState.items.slice(), cursor: canvasState.cursor }
}

/** state 変更 listener を登録。 解除関数を返す。 */
export function subscribeCanvasState(listener: StateListener): () => void {
  stateListeners.add(listener)
  return () => stateListeners.delete(listener)
}

/** cursor を指定 item に移動。 thumbnail click から呼ばれる。 items 順は不変。 */
export function setCursor(id: string): void {
  if (indexOfItem(id) === -1) {
    console.warn('[vp-canvas] setCursor: item not found:', id)
    return
  }
  canvasState.cursor = id
  renderCurrentMain()
  notifyStateChange()
}

/**
 * 指定 item を items から削除。 cursor 中なら **右隣 → 左隣 → null** の優先で cursor 移動。
 * doc 19 §4.1 fallback (= VS Code / Safari の tab close model)。
 */
export function deleteItem(id: string): void {
  const idx = indexOfItem(id)
  if (idx === -1) {
    console.warn('[vp-canvas] deleteItem: item not found:', id)
    return
  }
  const wasCursor = canvasState.cursor === id
  canvasState.items.splice(idx, 1)
  if (wasCursor) {
    // 削除後の items[idx] は元の「右隣」、 items[idx-1] は元の「左隣」
    if (idx < canvasState.items.length) {
      canvasState.cursor = canvasState.items[idx].id // 右隣
    } else if (idx > 0) {
      canvasState.cursor = canvasState.items[idx - 1].id // 左隣
    } else {
      canvasState.cursor = null // どちらも無し
    }
    renderCurrentMain()
  }
  notifyStateChange()
}

// ============================================================================
// テスト専用 API (テストランナーから state を操作するためだけに公開)
// ============================================================================

/**
 * module-local singleton state をリセットする。
 * **テスト専用** — production コードから呼んではいけない。
 * vitest の beforeEach 等で state 汚染を防ぐために使用。
 */
export function _resetForTest(): void {
  canvasState.items = []
  canvasState.cursor = null
  stateListeners.clear()
}

// ============================================================================
// dispatchShow: mcp__show / DirectiveInject / FilesOpenResult 等の入口
// ============================================================================

function dispatchShow(msg: ShowMessage): void {
  // content variant 判定
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

  // doc 19 PP Canvas Stack Model: 新 item を items の head に push、 cursor 自動切替、
  // 11 件目で tail drop。 pane_id / append は無視 (= dead field、 backward compat)。
  const newItem: CanvasItem = {
    id: generateId(),
    content: body,
    contentType,
    title: msg.title,
    createdAt: new Date().toISOString(),
  }
  canvasState.items.unshift(newItem)
  if (canvasState.items.length > MAX_ITEMS) {
    canvasState.items.pop() // tail drop (= 最古 item の追い出し)
  }
  canvasState.cursor = newItem.id

  // main pane render + listener 通知
  renderCurrentMain()
  notifyStateChange()
}

/**
 * Rust 注入口。ProcessMessage 1 件 (parse 済 object) を受け取り処理する。
 * Rust 側 `spawn_canvas_subscription` が active project の canvas message ごとに呼ぶ。
 */
export function handleMessage(msg: AnyMessage): void {
  if (msg.type === 'show') {
    dispatchShow(msg as ShowMessage)
  } else if (msg.type === 'clear') {
    // doc 19: clear は items + cursor を全 reset (= canvas を空にする operation)
    canvasState.items = []
    canvasState.cursor = null
    clearPP()
    notifyStateChange()
  }
  // 他 ProcessMessage variant (ChatChunk / DebugInfo / SessionList 等) は canvas に流さない (no-op)
}
