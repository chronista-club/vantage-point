/**
 * board (Paisley Park) handler（board モデル 2026-07-15、doc 52 §6 で canvas → board 改名）。
 *
 * ## board モデル
 * PP の board = show した item の scope 別リストで、**SP が唯一の truth を持つ**
 * （SurrealDB durable）。 webview はそれを表示する view:
 *  - SP が mcp__show 着信で item を生成し DB append → `BoardUpdated`(retained topic
 *    `process/paisley-park/state/board/{scope}/{lane}`) を broadcast する。
 *  - webview は gui channel で `BoardUpdated` を受けて boards を置換する（自前 save はしない）。
 *  - thumbnail ✕ / Clear は `board:delete` / `board:clear` IPC で SP に依頼し、 SP が DB 更新 →
 *    `BoardUpdated` で反映する（optimistic 更新はせず SP truth に一本化）。
 *  - cursor（main に出す item）だけは view local。
 *  - doc 52 §10 wave 0: presence（非空か）を 'vp:board-presence' で lane-panes へ通知し、
 *    board pane を roster に出す（旧 pp-overlay app scene の auto-open はここに移った）。
 *
 * scope: **'lane' のみ**（mako 決定 2026-07-23 — board は注視中 lane に一本化。旧 'proj' は
 * 撤去、'vp'（全体）構想も同決定で消滅）。gui channel は project 単位で全 lane の
 * `BoardUpdated`(retained) を配信するので、 lane board は lane ごとに保持（`laneBoards`）し、
 * active lane のものを表示する（lane 切替後も board が残る）。旧 proj board の retained topic /
 * DB 行は SP 側に残りうるが、client は scope !== 'lane' を無視するので表示に混ざらない。
 */

import { renderPP, clearPP, type ContentType } from './pp'


/** board の 1 item。 id は SP が一元発行する（webview は自前生成しない）。 */
export interface BoardItem {
  id: string
  content: string
  contentType: ContentType
  title?: string
  createdAt: string
  /** 最終更新時刻（RFC3339、doc 52 §5 計器盤の鮮度）。旧 item は欠くので額縁は createdAt に fallback。 */
  updatedAt?: string
}

interface Board {
  items: BoardItem[]
  cursor: string | null
  /** cursor に流されず届いた新着（未読 dot、doc 52 §5）。setCursor / 消滅で減る。view-local。 */
  unread: Set<string>
}

/** SP から canvas channel で届く BoardUpdated message（protocol::ProcessMessage::BoardUpdated）。 */
interface BoardUpdatedMessage {
  type: 'board_updated'
  /** SP 側の board scope。client が扱うのは 'lane' のみ（他は applyBoardUpdated が無視）。 */
  scope: string
  lane?: string | null
  items: BoardItem[]
  cursor?: string | null
}

type AnyMessage = BoardUpdatedMessage | { type: string; [key: string]: unknown }

function emptyBoard(): Board {
  return { items: [], cursor: null, unread: new Set() }
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

/** 額縁の鮮度表示テキスト（純関数、doc 52 §5）。updatedAt（無ければ createdAt）を「更新 HH:MM:SS」に。
 *  parse 不能 / 時刻なしは空文字（額縁に何も出さない = 嘘をつかない）。 */
export function formatFreshness(item: BoardItem | undefined): string {
  if (!item) return ''
  const stamp = item.updatedAt ?? item.createdAt
  const t = Date.parse(stamp)
  if (Number.isNaN(t)) return ''
  return `更新 ${new Date(t).toLocaleTimeString('ja-JP')}`
}

/** 額縁（board-plate）の鮮度 span に cursor item の最終更新時刻を出す。DOM 不在（test）は skip。 */
function renderFreshness(item: BoardItem | undefined): void {
  if (typeof document === 'undefined') return
  const el = document.getElementById('board-freshness')
  if (el) el.textContent = formatFreshness(item)
}

/** active board の cursor が指す item を main pane に描画。 cursor null なら空表示。 */
function renderCurrentMain(): void {
  const b = activeBoard()
  if (b.cursor === null) {
    clearPP()
    renderFreshness(undefined)
    return
  }
  const item = b.items.find((i) => i.id === b.cursor)
  if (!item) {
    clearPP()
    renderFreshness(undefined)
    return
  }
  renderPP(item.content, item.contentType)
  renderFreshness(item)
}

/**
 * board の presence（非空か）を lane-panes に知らせる（doc 52 §10 wave 0 — board pane 化）。
 * lane-panes は present で roster に board pane を出し、fresh（live 新着）なら focus を寄せる
 * （旧 maybeAutoOpenPP = pp-overlay app scene の後継。「配送されたのに見えない」を防ぐ）。
 *
 * 全 lane 分 dispatch する（非 active lane の board も boardByLane に記録され、lane 切替時に
 * roster が正しく再構成される）。fresh は active view のときだけ立てる — 裏 lane の新着で
 * 表 lane の focus を奪わない。
 */
function notifyBoardPresence(lane: string | null | undefined, present: boolean, fresh: boolean): void {
  // sendIpc と同じ規律: DOM 不在環境（単体テスト等）では silent skip（prod の webview では
  // document / CustomEvent は必ず存在）。event 配線は「薄い action 層」= 単体テスト対象外で、
  // 判定ロジック（hasFreshArrival / present = items.length>0）は純関数として直接検証する。
  if (typeof document === 'undefined' || typeof CustomEvent === 'undefined') return
  document.dispatchEvent(
    new CustomEvent('vp:board-presence', {
      detail: { lane: lane ?? 'conductor', present, fresh },
    }),
  )
}

// ============================================================================
// 公開 read API（HistoryStrip 用）
// ============================================================================

/** active board の readonly snapshot + activeScope。 listener 経由で更新を購読する。 */
export function getCanvasState(): {
  items: ReadonlyArray<BoardItem>
  cursor: string | null
  unread: ReadonlySet<string>
} {
  const b = activeBoard()
  return { items: b.items.slice(), cursor: b.cursor, unread: new Set(b.unread) }
}

/** state 変更 listener を登録。 解除関数を返す。 */
export function subscribeCanvasState(listener: StateListener): () => void {
  stateListeners.add(listener)
  return () => stateListeners.delete(listener)
}

// ============================================================================
// view 操作（cursor は local、 delete/clear は SP に依頼）
// ============================================================================

/** cursor を移動（thumbnail click）。cursor は SP truth（doc 52 §5 server 昇格）なので、
 *  optimistic に local 反映しつつ SP に board:cursor を送る（SP が BoardUpdated で確定値を配る）。
 *  scrollback 規則の follow 判定は SP が cursor を知って初めて成立する。 */
export function setCursor(id: string): void {
  const b = activeBoard()
  if (!b.items.some((i) => i.id === id)) {
    console.warn('[vp-canvas] setCursor: item not found:', id)
    return
  }
  b.cursor = id
  b.unread.delete(id) // 見た = 未読解除
  sendIpc({ t: 'board:cursor', scope: 'lane', lane: boardLaneKey(), item_id: id })
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
/** webview 起動後に生まれた未知 item = live 新着の id 一覧（retained replay / re-seed は
 *  createdAt < BOOT_TS なので空）。未読 dot と focus 寄せの一次ソース。 */
export function freshNewIds(items: BoardItem[], prevIds: Set<string>): string[] {
  return items
    .filter((i) => !prevIds.has(i.id) && Date.parse(i.createdAt) >= BOOT_TS)
    .map((i) => i.id)
}

export function hasFreshArrival(items: BoardItem[], prevIds: Set<string>): boolean {
  return freshNewIds(items, prevIds).length > 0
}

/**
 * 未読 dot 集合を計算する（純関数、doc 52 §5）。前回の未読を引き継ぎ（存在する id のみ・
 * cursor が指すものは既読）、cursor が **follow しなかった** 新着を足す。scrollback 規則で
 * cursor 据え置きなら新着 = 未読 / follow したなら cursor 自身なので dot にしない。
 */
export function computeUnread(
  prevUnread: ReadonlySet<string>,
  itemIds: ReadonlySet<string>,
  freshIds: readonly string[],
  cursor: string | null,
): Set<string> {
  const unread = new Set<string>()
  for (const id of prevUnread) {
    if (itemIds.has(id) && id !== cursor) unread.add(id)
  }
  for (const id of freshIds) {
    if (id !== cursor) unread.add(id)
  }
  return unread
}

function applyBoardUpdated(msg: BoardUpdatedMessage): void {
  // proj scope 撤去後も、SP の retained topic には旧 proj board の再配信が残りうる。
  // 未知 scope（将来の追加も含む）ごと無視する = 表示は lane board だけ（fail-quiet）。
  if (msg.scope !== 'lane') return
  const laneKey = msg.lane ?? 'conductor'
  const prev = canvasState.laneBoards[laneKey]
  const prevIds = new Set((prev?.items ?? []).map((i) => i.id))
  const items = Array.isArray(msg.items) ? msg.items : []
  const cursor = msg.cursor ?? null
  const freshIds = freshNewIds(items, prevIds)
  const board: Board = {
    items,
    cursor,
    unread: computeUnread(
      prev?.unread ?? new Set(),
      new Set(items.map((i) => i.id)),
      freshIds,
      cursor,
    ),
  }
  canvasState.laneBoards[laneKey] = board
  // board pane 化（doc 52 §10 wave 0）: presence を lane-panes に知らせる。全 lane 分 dispatch
  // し、非 active lane の board も lane 切替時に roster へ正しく載る。
  // focus 寄せ（fresh）は **cursor が新着に follow したときだけ**（doc 52 §5 = 奪わない）: mako が
  // 古い item を見ていて cursor 据え置きなら、新着は dot で灯すが focus は奪わない。裏 lane も対象外。
  const cursorFollowed = cursor !== null && freshIds.includes(cursor)
  const fresh = isActiveView(msg.lane) && cursorFollowed
  notifyBoardPresence(msg.lane, board.items.length > 0, fresh)
  // 表示中の board が更新されたときだけ main を再描画。
  if (isActiveView(msg.lane)) {
    renderCurrentMain()
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
