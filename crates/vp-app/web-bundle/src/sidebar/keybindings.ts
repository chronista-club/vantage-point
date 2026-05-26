/**
 * Sidebar WebView 専用のキーボードショートカット (= VP 規約 v0.3+ directive dispatcher、 sidebar 側)。
 *
 * sidebar 上の `Cmd hold + <key>` keydown を `installDirectiveHandler` で捕捉して、 key ごとに
 * sidebar 内 API を direct に呼ぶ (Rust 経由なし)。 同じ directive は main view からも発火可能で、
 * main view 側は IPC bridge で Rust に投げ、 Rust が sidebar に `window.vpSidebar.fireDirective(key)`
 * を evaluate_script で呼ぶ。 つまり sidebar 内発火と main view bridge 発火は **同じ dispatch path**
 * (= `dispatchDirective`) を通る。
 */
import { sidebar } from './store'
import { sendIpc } from './ipc'
import { isWingLane, laneAddressKey } from './lane'
import { installDirectiveHandler } from '../shortcuts/chord'
import {
  deleteHintLabel,
  deleteHintVisible,
  laneSelectHintLabel,
  laneSelectHintVisible,
  openAddWingFor,
  setDeleteHintLabel,
  setDeleteHintVisible,
  setLaneSelectHintLabel,
  setLaneSelectHintVisible,
} from './directive-state'

// hint 信号は keybindings 利用者 (Shell.tsx) からも参照されるので re-export。
export { deleteHintLabel, deleteHintVisible, laneSelectHintLabel, laneSelectHintVisible }

declare global {
  interface Window {
    /**
     * main view bridge 経由で sidebar 内 directive を発火する公開 API。
     * `app.rs` の `AppEvent::DirectiveFire` arm が `sidebar.evaluate_script` で呼ぶ。
     */
    vpSidebar?: {
      fireDirective(key: string): void
    }
  }
}

// =============================================================================
// `d` directive — 2-click confirm の pending state (module-scope singleton)
// =============================================================================
const DELETE_CONFIRM_WINDOW_MS = 1000

type PendingDeleteTarget =
  | { kind: 'lane'; path: string; address: string }
  | { kind: 'project'; path: string }

let pendingDelete: { target: PendingDeleteTarget; expireAt: number } | null = null
let pendingDeleteTimer: ReturnType<typeof setTimeout> | null = null

function clearPendingDelete(): void {
  pendingDelete = null
  if (pendingDeleteTimer !== null) {
    clearTimeout(pendingDeleteTimer)
    pendingDeleteTimer = null
  }
  setDeleteHintVisible(false)
}

function targetMatches(a: PendingDeleteTarget, b: PendingDeleteTarget): boolean {
  if (a.kind !== b.kind) return false
  if (a.kind === 'lane' && b.kind === 'lane') {
    return a.path === b.path && a.address === b.address
  }
  return a.path === b.path
}

// =============================================================================
// context lookup helpers
// =============================================================================

/** `sidebar.lanes_by_project` を逆引きして address → project_path を解決。 */
function resolveProjectPathFromAddress(address: string): string | undefined {
  const map = sidebar.lanes_by_project ?? {}
  for (const [projectPath, lanes] of Object.entries(map)) {
    if (Array.isArray(lanes) && lanes.some((l) => laneAddressKey(l) === address)) {
      return projectPath
    }
  }
  return undefined
}

/** address に対応する LaneInfo を逆引き (= isWingLane 判定等で必要)。 */
function findLaneByAddress(address: string) {
  const map = sidebar.lanes_by_project ?? {}
  for (const lanes of Object.values(map)) {
    if (Array.isArray(lanes)) {
      const lane = lanes.find((l) => laneAddressKey(l) === address)
      if (lane) return lane
    }
  }
  return null
}

/** directive `n` / context dispatch 用: active な context から project_path を 1 つ取り出す。 */
function activeProjectPath(): string | undefined {
  if (sidebar.active_lane_address) {
    return resolveProjectPathFromAddress(sidebar.active_lane_address)
  }
  if (sidebar.active_stand) {
    return sidebar.active_stand.project_path
  }
  return undefined
}

// =============================================================================
// dispatch
// =============================================================================

export function dispatchDirective(key: string): void {
  if (key === 'f') {
    // `f` directive = File Explorer overlay を open + sidebar focus 移動。
    const address = sidebar.active_lane_address
    if (!address) {
      console.debug('[directive f] active lane なし、 picker open skip')
      return
    }
    window.vpFilePicker?.open(address)
    return
  }
  if (key === 'p') {
    // `p` directive = send current/selected to PP。
    if (window.vpFilePicker?.sendSelectedToPP) {
      window.vpFilePicker.sendSelectedToPP()
    } else {
      console.debug('[directive p] no current selection (picker not visible)')
    }
    return
  }
  // PR 445 `r` (restart, context polymorphic): active_lane → lane:restart、 active_stand → process:restart。
  if (key === 'r') {
    const addr = sidebar.active_lane_address
    if (addr) {
      const path = resolveProjectPathFromAddress(addr)
      if (path) {
        sendIpc({ t: 'lane:restart', path, address: addr })
      } else {
        console.warn('[directive r] active lane address の project 不明、 skip:', addr)
      }
      return
    }
    if (sidebar.active_stand) {
      sendIpc({ t: 'process:restart', path: sidebar.active_stand.project_path })
      return
    }
    console.debug('[directive r] active lane / stand なし、 skip')
    return
  }
  // PR 445 `n` (new wing form open): active project の AddWing form を keyboard で open。
  if (key === 'n') {
    const path = activeProjectPath()
    if (!path) {
      console.warn('[directive n] active project 不明、 form open skip')
      return
    }
    const opened = openAddWingFor(path)
    if (!opened) {
      console.warn('[directive n] AddWing setter not registered for', path)
    }
    return
  }
  // PR 445 `d` (delete + 2-click confirm)。
  if (key === 'd') {
    // context 判定 → target を作る。
    let target: PendingDeleteTarget | null = null
    const addr = sidebar.active_lane_address
    if (addr) {
      const lane = findLaneByAddress(addr)
      const path = resolveProjectPathFromAddress(addr)
      if (lane && path && isWingLane(lane)) {
        target = { kind: 'lane', path, address: addr }
      } else {
        console.debug('[directive d] target が Wing でない or path 不明、 skip')
        return
      }
    } else if (sidebar.active_stand) {
      target = { kind: 'project', path: sidebar.active_stand.project_path }
    } else {
      console.debug('[directive d] active lane / stand なし、 skip')
      return
    }

    // pending state check (= 1 秒以内の 2 回目押下、 同 target なら execute)
    if (
      pendingDelete &&
      Date.now() < pendingDelete.expireAt &&
      targetMatches(pendingDelete.target, target)
    ) {
      const t = pendingDelete.target
      clearPendingDelete()
      if (t.kind === 'lane') {
        sendIpc({ t: 'lane:delete', path: t.path, address: t.address })
      } else {
        sendIpc({ t: 'process:delete', path: t.path })
      }
      return
    }

    // 1 回目 → pending state + visual feedback
    if (pendingDeleteTimer !== null) clearTimeout(pendingDeleteTimer)
    pendingDelete = { target, expireAt: Date.now() + DELETE_CONFIRM_WINDOW_MS }
    const label =
      target.kind === 'lane'
        ? `⌘d again to delete wing: ${target.address}`
        : `⌘d again to delete project: ${target.path}`
    setDeleteHintLabel(label)
    setDeleteHintVisible(true)
    pendingDeleteTimer = setTimeout(clearPendingDelete, DELETE_CONFIRM_WINDOW_MS)
    return
  }
  // PR 445 `s` (Lane / project switcher picker)。
  if (key === 's') {
    window.vpLanePicker?.open?.()
    return
  }
  // PR 447 `l` (lane number switcher mode): mode 突入 → 5 秒以内に 1-9 数字キーで visible lane 切替。
  if (key === 'l') {
    enterLaneSelectMode()
    return
  }
  console.debug('[directive] sidebar 側で未実装:', key)
}

// =============================================================================
// `l` directive — Lane number switcher mode (v0.6 / PR 447)
// =============================================================================

const LANE_SELECT_MODE_MS = 5000

interface VisibleLane {
  path: string
  address: string
  label: string
}

let laneSelectModeTimer: ReturnType<typeof setTimeout> | null = null
let laneSelectModeTargets: VisibleLane[] = []

/** expanded project の lane を上から flat list で収集 (= 1-9 で indexing する候補)。 */
function collectVisibleLanes(): VisibleLane[] {
  const out: VisibleLane[] = []
  const map = sidebar.lanes_by_project ?? {}
  for (const proc of sidebar.processes) {
    if (!proc.expanded) continue // collapse 中はスキップ (= user の「colaps で開いているもの」)
    const lanes = map[proc.path] ?? []
    const projectName = proc.path.split('/').pop() ?? proc.path
    for (const lane of lanes) {
      const addr = laneAddressKey(lane)
      const kind = lane.kind || lane.address.kind
      const name = lane.name ?? lane.address.name ?? ''
      const label =
        kind === 'lead' ? `${projectName} / Lead` : `${projectName} / Wing: ${name}`
      out.push({ path: proc.path, address: addr, label })
    }
  }
  return out
}

function exitLaneSelectMode(): void {
  setLaneSelectHintVisible(false)
  setLaneSelectHintLabel('')
  laneSelectModeTargets = []
  if (laneSelectModeTimer !== null) {
    clearTimeout(laneSelectModeTimer)
    laneSelectModeTimer = null
  }
  window.removeEventListener('keydown', laneNumberHandler, true)
}

function laneNumberHandler(e: KeyboardEvent): void {
  // input / textarea / contenteditable にフォーカス時は数字入力を妨げない (= mode abort)
  const target = e.target as HTMLElement | null
  if (target) {
    const tag = target.tagName
    if (tag === 'INPUT' || tag === 'TEXTAREA' || target.isContentEditable) {
      exitLaneSelectMode()
      return
    }
  }
  // Esc で abort
  if (e.key === 'Escape') {
    e.preventDefault()
    exitLaneSelectMode()
    return
  }
  // 1-9 で switch
  if (e.key >= '1' && e.key <= '9') {
    e.preventDefault()
    const n = parseInt(e.key, 10) - 1
    const target = laneSelectModeTargets[n]
    if (target) {
      sendIpc({ t: 'lane:select', path: target.path, address: target.address })
    }
    exitLaneSelectMode()
    return
  }
  // 修飾キー単体 (Meta / Control / Shift / Alt) keydown は mode を keep する (= user が押し続けるだけ)
  if (e.key === 'Meta' || e.key === 'Control' || e.key === 'Shift' || e.key === 'Alt') {
    return
  }
  // それ以外のキーは mode abort (= 通常 input に通す)
  exitLaneSelectMode()
}

function enterLaneSelectMode(): void {
  const targets = collectVisibleLanes()
  if (targets.length === 0) {
    console.debug('[directive l] visible lane なし、 mode skip')
    return
  }
  laneSelectModeTargets = targets
  // hint label: "1. project/lead  2. project/wing/foo  ..."、 9 件で truncate
  const label = targets
    .slice(0, 9)
    .map((t, i) => `${i + 1}. ${t.label}`)
    .join('   ')
  setLaneSelectHintLabel(label)
  setLaneSelectHintVisible(true)

  // 既存 timer / listener があれば clear (= 連打対応)
  if (laneSelectModeTimer !== null) clearTimeout(laneSelectModeTimer)
  window.removeEventListener('keydown', laneNumberHandler, true)

  // 数字キー listener install (capture phase で chord listener より先に取る)
  window.addEventListener('keydown', laneNumberHandler, true)
  laneSelectModeTimer = setTimeout(exitLaneSelectMode, LANE_SELECT_MODE_MS)
}

export function installSidebarKeybindings(): void {
  installDirectiveHandler({ exec: dispatchDirective })
  // main view bridge から呼ばれる経路: app.rs `AppEvent::DirectiveFire` arm が
  // `sidebar.evaluate_script("window.vpSidebar.fireDirective('<key>')")` を発火。
  // sidebar 内発火と同じ `dispatchDirective` を通すことで挙動を一元化する。
  window.vpSidebar = {
    fireDirective: dispatchDirective,
  }
}
