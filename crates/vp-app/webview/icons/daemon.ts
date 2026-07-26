// VP daemon / Process glyph mapping (@chronista-club/creo-ui-icons-web の VP-domain alias)。
//
// daemon = 常駐 daemon (Process Manager)、 repo (repo) = 各 repo 用 server。
// process state を icon で表現: running / spawning / stopped / error / restarting。
//
// 参考 memory: feedback_creo_ui_icon_dual_axis.md (2026-04-29)
// 参考: D10 Reconciliation アーキテクチャ (Push QUIC + Pull port scan)、
//        D12 daemon lifecycle 独立性 (setsid で process group 分離)

import type { IconName } from '@chronista-club/creo-ui-icons-web'

export type ProcessState =
  | 'running'      // up + healthy
  | 'spawning'     // 起動中 (動的 — svg-spinners 推奨)
  | 'stopped'      // 停止中
  | 'error'        // crash / unhealthy
  | 'restarting'   // 再起動中 (動的)

export type DaemonEntity =
  | 'daemon'     // daemon (port 32000)
  | 'sp'           // repo (port 33000+、 repo)
  | 'repo'      // generic repo entry

export interface ProcessIconSet {
  default: IconName
  active: IconName
}

export const DAEMON_ICON: Record<DaemonEntity, ProcessIconSet> = {
  daemon: {
    default: 'ph:planet',
    active: 'ph:planet-fill',
  },
  sp: {
    default: 'ph:star',
    active: 'ph:star-fill',
  },
  repo: {
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

export function iconForDaemon(
  entity: DaemonEntity,
  state: 'default' | 'active' = 'default',
): IconName {
  return DAEMON_ICON[entity][state]
}

export function iconForProcessState(state: ProcessState): IconName {
  return PROCESS_STATE_ICON[state]
}
