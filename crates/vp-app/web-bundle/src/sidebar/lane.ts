/**
 * Lane (Lead / Wing) の表示ヘルパー。
 *
 * v1.0 柱 2 PR-2。 旧 SIDEBAR_HTML の `STAND_GLYPH` / `standDisplayName` /
 * `laneLabel` / `laneAddressKey` を SolidJS sidebar 用に port したもの。
 *
 * Lane の役割は Lead / Wing の 2 種 (Wing は旧称 Worker)。 SP から来る wire の
 * `kind` 文字列は legacy `"worker"` も `wing` として扱う (Worker → Wing rename
 * 前の永続データ / 旧 SP との互換)。
 */
import type { IconName } from 'creoui-icons-web'
import type { LaneInfo } from '../generated/LaneInfo'

/**
 * Lane Stand kind → Phosphor icon (default / active=fill weight) のペア。
 * 旧 SIDEBAR_HTML `STAND_GLYPH` の port。 `-fill` を別 literal で持ち、
 * `IconName` 型に収まるようにする (文字列連結だと型が string に広がるため)。
 */
const STAND_ICON: Record<string, { default: IconName; active: IconName }> = {
  echoes: { default: 'ph:chat-circle', active: 'ph:chat-circle-fill' },
  hd: { default: 'ph:chat-circle', active: 'ph:chat-circle-fill' }, // legacy alias
  shell: { default: 'ph:terminal-window', active: 'ph:terminal-window-fill' },
  tmux: { default: 'ph:presentation', active: 'ph:presentation-fill' },
  paisley_park: { default: 'ph:compass', active: 'ph:compass-fill' },
  gold_experience: { default: 'ph:plant', active: 'ph:plant-fill' },
  hermit_purple: { default: 'ph:plug', active: 'ph:plug-fill' },
}

/** Stand kind から icon 名を解決。 active 時は fill weight。 未知 stand は `null`。 */
export function standIcon(stand: string, active: boolean): IconName | null {
  const set = STAND_ICON[stand]
  if (!set) return null
  return active ? set.active : set.default
}

/** Stand の表示名 (Architecture v4 metaphor)。 */
export function standDisplayName(stand: string): string {
  switch (stand) {
    case 'echoes':
    case 'hd': // legacy alias (旧 Heaven's Door)
      return 'Echoes'
    case 'shell':
      return 'Shell'
    case 'tmux':
      return 'Tmux'
    case 'paisley_park':
      return 'Paisley Park'
    case 'gold_experience':
      return 'Gold Experience'
    case 'hermit_purple':
      return 'Hermit Purple'
    default:
      return stand
  }
}

/** Lane kind が Wing か (`"worker"` は legacy alias)。 */
function isWingKind(kind: string): boolean {
  return kind === 'wing' || kind === 'worker'
}

/** Lane が Wing か (Lead との対)。 */
export function isWingLane(lane: LaneInfo): boolean {
  return isWingKind(lane.kind) || isWingKind(lane.address.kind)
}

/** Lane の表示ラベル。 Lead はそのまま、 Wing は `Wing: <name>`。 */
export function laneLabel(lane: LaneInfo): string {
  const kind = lane.kind || lane.address.kind
  if (kind === 'lead') return 'Lead'
  if (isWingKind(kind)) return `Wing: ${lane.name ?? lane.address.name ?? '?'}`
  return kind
}

/**
 * Lane address を Display 形 (`<project>/lead` / `<project>/wing/<name>`) に変換。
 * Rust `LaneAddressWire::key()` と完全一致させる (active selection 比較に使うため)。
 * legacy `"worker"` kind は `wing` に正規化する。
 */
export function laneAddressKey(lane: LaneInfo): string {
  const a = lane.address
  if (isWingKind(a.kind)) {
    return `${a.project}/wing/${a.name ?? '<unnamed>'}`
  }
  return `${a.project}/${a.kind || 'lead'}`
}
