/**
 * Canvas (Paisley Park) の board handler（board モデル 2026-07-15）。
 *
 * ## board モデル
 * PP Canvas を scope 別の永続 board にする。 board = show した item の scope 別リストで、
 * **SP が唯一の truth を持つ**（SurrealDB durable）。 webview はそれを表示する view:
 *  - SP が mcp__show 着信で item を生成し DB append → `BoardUpdated`(retained topic
 *    `process/paisley-park/state/board/{scope}/{lane}`) を broadcast する。
 *  - webview は canvas channel で `BoardUpdated` を受けて boards を置換する（自前 save はしない）。
 *  - thumbnail ✕ / Clear は `board:delete` / `board:clear` IPC で SP に依頼し、 SP が DB 更新 →
 *    `BoardUpdated` で反映する（optimistic 更新はせず SP truth に一本化）。
 *  - cursor（main に出す item）だけは view local。
 *
 * scope: 'lane'（lane ごと）/ 'proj'（project 共有）。 'vp'（全体）は Phase 2（World store 要）。
 * canvas channel は project 単位で全 lane の `BoardUpdated`(retained) を配信するので、 lane board は
 * lane ごとに保持（`laneBoards`）し、 active lane のものを表示する（lane 切替後も board が残る）。
 */

import { renderPP, clearPP, type ContentType } from './pp'
import type { FrameEngine } from './frame-engine'

/** board の scope。 'vp'（全体）は Phase 2 で追加予定。 */
export type BoardScope = 'lane' | 'proj'

/** board の 1 item。 id は SP が一元発行する（webview は自前生成しない）。 */
export interface CanvasItem {
  id: string
  content: string
  contentType: ContentType
  title?: string
  createdAt: string
}

interface Board {
  items: CanvasItem[]
  cursor: string | null
}

/** SP から canvas channel で届く BoardUpdated message（protocol::ProcessMessage::BoardUpdated）。 */
interface BoardUpdatedMessage {
  type: 'board_updated'
  scope: BoardScope
  lane?: string | null
  items: CanvasItem[]
  cursor?: string | null
}

type AnyMessage = BoardUpdatedMessage | { type: string; [key: string]: unknown }

function emptyBoard(): Board {
  return { items: [], cursor: null }
}

/** module-local state。 SP truth のミラー（view）。 bundle reload で reset、 再購読で retained 復元。 */
const canvasState: {
  activeScope: BoardScope
  activeLane: string // lane board のキー。 'conductor' = lead。
  proj: Board
  laneBoards: Record<string, Board>
} = {
  activeScope: 'lane',
  activeLane: 'conductor',
  proj: emptyBoard(),
  laneBoards: {},
}

/** active board（表示対象）を返す。 lane board は active lane のもの。 */
function activeBoard(): Board {
  if (canvasState.activeScope === 'proj') return canvasState.proj
  return canvasState.laneBoards[canvasState.activeLane] ?? emptyBoard()
}

/** 指定 (scope, lane) が現在の表示 view と一致するか（描画/auto-open の判定用）。 */
function isActiveView(scope: BoardScope, lane: string | null | undefined): boolean {
  if (scope !== canvasState.activeScope) return false
  if (scope === 'proj') return true
  return (lane ?? 'conductor') === canvasState.activeLane
}

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

// ============================================================================
// IPC (webview → vp-app → World process-proxy → SP)
// ============================================================================

function sendIpc(payload: Record<string, unknown>): void {
  // browser では window===globalThis（window.ipc）。 globalThis 経由なら node 単体テストでも
  // ReferenceError にならない（maybeAutoOpenPP の vpFrame 参照と同じ流儀）。
  const ipc = (globalThis as unknown as { ipc?: { postMessage(msg: string): void } }).ipc
  if (!ipc || typeof ipc.postMessage !== 'function') {
    // 単体テスト等 IPC 不在環境では silent skip（prod では必ず存在する）。
    return
  }
  try {
    ipc.postMessage(JSON.stringify(payload))
  } catch (e) {
    console.warn('[vp-canvas] ipc failed', e)
  }
}

/** board mutate 用の lane キー（lane board のみ。 conductor / proj は null）。 */
function boardLaneKey(): string | null {
  if (canvasState.activeScope !== 'lane') return null
  return canvasState.activeLane === 'conductor' ? null : canvasState.activeLane
}

// ============================================================================
// active lane / board 切替
// ============================================================================

/** active lane を更新する（entry.tsx の lane 切替 bridge から呼ぶ）。 lane board のキーになる。 */
export function setActiveLaneName(lane: string | null): void {
  canvasState.activeLane = lane ?? 'conductor'
  renderCurrentMain()
  notifyStateChange()
}

/** 現在の active lane name（'conductor' は null に正規化して返す）。 */
export function getActiveLaneName(): string | null {
  return canvasState.activeLane === 'conductor' ? null : canvasState.activeLane
}

/** 表示 board の scope を切替（[Lane | Proj] segment click）。 */
export function setActiveBoard(scope: BoardScope): void {
  canvasState.activeScope = scope
  renderCurrentMain()
  notifyStateChange()
}

/** 現在の表示 scope。 */
export function getActiveBoard(): BoardScope {
  return canvasState.activeScope
}

// ============================================================================
// render
// ============================================================================

/** active board の cursor が指す item を main pane に描画。 cursor null なら空表示。 */
function renderCurrentMain(): void {
  const b = activeBoard()
  if (b.cursor === null) {
    clearPP()
    return
  }
  const item = b.items.find((i) => i.id === b.cursor)
  if (!item) {
    clearPP()
    return
  }
  renderPP(item.content, item.contentType)
}

/**
 * active board に新規 item が増えたのに PP panel が非表示なら、 pp-overlay で軽く開く
 * （「配送されたのに見えない」を防ぐ）。 既に PP が見える scene なら何もしない。
 */
function maybeAutoOpenPP(): void {
  const frame = (globalThis as unknown as { vpFrame?: FrameEngine }).vpFrame
  if (!frame) return
  const sceneId = frame.getCurrentSceneId()
  if (!sceneId) return
  const pp = frame.getScene(sceneId)?.panes['pp']
  const ppVisible = !!pp && pp.state !== 'hidden' && pp.opacity > 0
  if (!ppVisible) {
    frame.applyScene('pp-overlay')
  }
}

// ============================================================================
// 公開 read API（HistoryStrip 用）
// ============================================================================

/** active board の readonly snapshot + activeScope。 listener 経由で更新を購読する。 */
export function getCanvasState(): {
  activeScope: BoardScope
  items: ReadonlyArray<CanvasItem>
  cursor: string | null
} {
  const b = activeBoard()
  return { activeScope: canvasState.activeScope, items: b.items.slice(), cursor: b.cursor }
}

/** state 変更 listener を登録。 解除関数を返す。 */
export function subscribeCanvasState(listener: StateListener): () => void {
  stateListeners.add(listener)
  return () => stateListeners.delete(listener)
}

// ============================================================================
// view 操作（cursor は local、 delete/clear は SP に依頼）
// ============================================================================

/** cursor を移動（thumbnail click）。 view local（durable でない）。 */
export function setCursor(id: string): void {
  const b = activeBoard()
  if (!b.items.some((i) => i.id === id)) {
    console.warn('[vp-canvas] setCursor: item not found:', id)
    return
  }
  b.cursor = id
  renderCurrentMain()
  notifyStateChange()
}

/** item 削除（thumbnail ✕）。 SP に依頼し、 SP が更新後 board を BoardUpdated で反映する。 */
export function deleteItem(id: string): void {
  sendIpc({
    t: 'board:delete',
    scope: canvasState.activeScope,
    lane: boardLaneKey(),
    item_id: id,
  })
}

/** active board を空にする（Clear ボタン）。 SP に依頼。 */
export function clearActiveBoard(): void {
  sendIpc({ t: 'board:clear', scope: canvasState.activeScope, lane: boardLaneKey() })
}

// ============================================================================
// Rust 注入口
// ============================================================================

/** bundle load 時刻。 これより前に生まれた item は「既読 backlog」扱い。 */
const BOOT_TS = Date.now()

/**
 * board 更新に「webview 起動後に生まれた未知 item」= live 新着が含まれるか。
 * retained replay（subscribe 直後の再配信）も daemon 再起動後の SP re-seed も、
 * 中身は既存 item（createdAt が BOOT_TS より古い）の再配信なのでここで false になる —
 * board 化(#771)で失われた旧 show / pp:state:loaded の live/replay 区別の再導入。
 * createdAt が parse 不能(NaN)な item は fresh 扱いしない（board / badge には載るので
 * 静かな側に倒す）。
 */
function hasFreshArrival(items: CanvasItem[], prevIds: Set<string>): boolean {
  return items.some((i) => !prevIds.has(i.id) && Date.parse(i.createdAt) >= BOOT_TS)
}

function applyBoardUpdated(msg: BoardUpdatedMessage): void {
  const laneKey = msg.lane ?? 'conductor'
  const prev = msg.scope === 'proj' ? canvasState.proj : canvasState.laneBoards[laneKey]
  const prevIds = new Set((prev?.items ?? []).map((i) => i.id))
  const board: Board = {
    items: Array.isArray(msg.items) ? msg.items : [],
    cursor: msg.cursor ?? null,
  }
  if (msg.scope === 'proj') {
    canvasState.proj = board
  } else {
    canvasState.laneBoards[laneKey] = board
  }
  // 表示中の board が更新されたときだけ main を再描画。 live 新着のときだけ PP を軽く開く
  // （起動時の retained replay で毎回 PP が開いてしまう regression の根治）。
  if (isActiveView(msg.scope, msg.lane)) {
    renderCurrentMain()
    if (hasFreshArrival(board.items, prevIds)) {
      maybeAutoOpenPP()
    }
  }
  notifyStateChange()
}

/**
 * Rust 注入口。 canvas channel から受けた ProcessMessage 1 件を処理する。
 * board モデルでは BoardUpdated のみ扱う（旧 show/clear/pp:state:loaded は SP 側で board に畳まれた）。
 */
export function handleMessage(msg: AnyMessage): void {
  if (msg.type === 'board_updated') {
    applyBoardUpdated(msg as BoardUpdatedMessage)
  }
  // 他 message（switch_lane 等）は board と無関係（app.rs 側で処理済）。
}

// ============================================================================
// テスト専用 API
// ============================================================================

/** module-local state をリセットする（**テスト専用**）。 */
export function _resetForTest(): void {
  canvasState.activeScope = 'lane'
  canvasState.activeLane = 'conductor'
  canvasState.proj = emptyBoard()
  canvasState.laneBoards = {}
  stateListeners.clear()
}
