// VP TheWorld / Process glyph mapping (creo-ui-icons-web の VP-domain alias)。
//
// TheWorld = 常駐 daemon (Process Manager)、 SP (Star Platinum) = 各 project 用 server。
// process state を icon で表現: running / spawning / stopped / error / restarting。
//
// 参考 memory: feedback_creo_ui_icon_dual_axis.md (2026-04-29)
// 参考: D10 Reconciliation アーキテクチャ (Push QUIC + Pull port scan)、
//        D12 daemon lifecycle 独立性 (setsid で process group 分離)

import type { IconName } from 'creo-ui-icons-web'

export type ProcessState =
  | 'running'      // up + healthy
  | 'spawning'     // 起動中 (動的 — svg-spinners 推奨)
  | 'stopped'      // 停止中
  | 'error'        // crash / unhealthy
  | 'restarting'   // 再起動中 (動的)

export type WorldEntity =
  | 'theworld'     // TheWorld daemon (port 32000)
  | 'sp'           // Star Platinum (port 33000+、 project SP)
  | 'project'      // generic project entry

export interface ProcessIconSet {
  default: IconName
  active: IconName
}

export const THEWORLD_ICON: Record<WorldEntity, ProcessIconSet> = {
  theworld: {
    default: 'ph:planet',
    active: 'ph:planet-fill',
  },
  sp: {
    default: 'ph:star',
    active: 'ph:star-fill',
  },
  project: {
    default: 'mingcute:folder-line',
    active: 'mingcute:folder-fill',
  },
}

// process state → icon (動的 / 静的 を切替)
export const PROCESS_STATE_ICON: Record<ProcessState, IconName> = {
  running: 'mingcute:check-circle-fill',
  spawning: 'svg-spinners:bars-rotate-fade',
  stopped: 'mingcute:pause-circle-line',
  error: 'mingcute:close-circle-fill',
  restarting: 'svg-spinners:ring-resize',
}

export function iconForWorld(
  entity: WorldEntity,
  state: 'default' | 'active' = 'default',
): IconName {
  return THEWORLD_ICON[entity][state]
}

export function iconForProcessState(state: ProcessState): IconName {
  return PROCESS_STATE_ICON[state]
}
