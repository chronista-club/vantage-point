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
 * scope: **'lane' のみ**（mako 決定 2026-07-23 — board は注視中 lane に一本化。旧 'proj' は
 * 撤去、'vp'（全体）構想も同決定で消滅）。canvas channel は project 単位で全 lane の
 * `BoardUpdated`(retained) を配信するので、 lane board は lane ごとに保持（`laneBoards`）し、
 * active lane のものを表示する（lane 切替後も board が残る）。旧 proj board の retained topic /
 * DB 行は SP 側に残りうるが、client は scope !== 'lane' を無視するので表示に混ざらない。
 */

import { renderPP, clearPP, type ContentType } from './pp'
import { appLayoutReady, applyAppScene, isAppPaneVisible } from './app-panes'


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
  /** SP 側の board scope。client が扱うのは 'lane' のみ（他は applyBoardUpdated が無視）。 */
  scope: string
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
  activeLane: string // lane board のキー。 'conductor' = lead。
  laneBoards: Record<string, Board>
} = {
  activeLane: 'conductor',
  laneBoards: {},
}

/** active board（表示対象）を返す。 lane board は active lane のもの。 */
function activeBoard(): Board {
  return canvasState.laneBoards[canvasState.activeLane] ?? emptyBoard()
}

/** 指定 lane が現在の表示 view と一致するか（描画/auto-open の判定用）。 */
function isActiveView(lane: string | null | undefined): boolean {
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
  // ReferenceError にならない。
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

/** board mutate 用の lane キー（conductor は null）。 */
function boardLaneKey(): string | null {
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
 * （「配送されたのに見えない」を防ぐ）。 既に PP が見えていれば何もしない。
 * appLayoutReady guard: boot で default 配置が乗る前の暴発（何も無い場に overlay を
 * 焼き付ける）を防ぐ — 旧実装の `if (!sceneId) return` と同じ位置づけ。
 */
function maybeAutoOpenPP(): void {
  if (!appLayoutReady()) return
  if (!isAppPaneVisible('pp')) {
    applyAppScene('pp-overlay')
  }
}

// ============================================================================
// 公開 read API（HistoryStrip 用）
// ============================================================================

/** active board の readonly snapshot + activeScope。 listener 経由で更新を購読する。 */
export function getCanvasState(): {
  items: ReadonlyArray<CanvasItem>
  cursor: string | null
} {
  const b = activeBoard()
  return { items: b.items.slice(), cursor: b.cursor }
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
    scope: 'lane',
    lane: boardLaneKey(),
    item_id: id,
  })
}

/** active board を空にする（Clear ボタン）。 SP に依頼。 */
export function clearActiveBoard(): void {
  sendIpc({ t: 'board:clear', scope: 'lane', lane: boardLaneKey() })
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
  // proj scope 撤去後も、SP の retained topic には旧 proj board の再配信が残りうる。
  // 未知 scope（将来の追加も含む）ごと無視する = 表示は lane board だけ（fail-quiet）。
  if (msg.scope !== 'lane') return
  const laneKey = msg.lane ?? 'conductor'
  const prev = canvasState.laneBoards[laneKey]
  const prevIds = new Set((prev?.items ?? []).map((i) => i.id))
  const board: Board = {
    items: Array.isArray(msg.items) ? msg.items : [],
    cursor: msg.cursor ?? null,
  }
  canvasState.laneBoards[laneKey] = board
  // 表示中の board が更新されたときだけ main を再描画。 live 新着のときだけ PP を軽く開く
  // （起動時の retained replay で毎回 PP が開いてしまう regression の根治）。
  if (isActiveView(msg.lane)) {
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
  canvasState.activeLane = 'conductor'
  canvasState.laneBoards = {}
  stateListeners.clear()
}
