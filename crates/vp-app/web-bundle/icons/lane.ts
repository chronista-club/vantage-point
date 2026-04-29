// VP Lane glyph mapping (creo-ui-icons-web の VP-domain alias)。
//
// Lane = Stand 配下で稼働する個別実行単位。 Lead Lane (project lead 1 体) +
// Worker Lane (Issue 別 ccws worker 群) の構成。
// state-driven: default = regular, active = fill。
//
// 参考 memory: feedback_creo_ui_icon_dual_axis.md (2026-04-29)
// 参考 spec: project_lane_as_process.md (Lead Autonomy L0〜L3)

import type { IconName } from 'creo-ui-icons-web'

export type LaneKind =
  | 'lead'         // Lead Lane (project lead、 master agent)
  | 'worker'       // Worker Lane (ccws worker、 Issue 専属)
  | 'init_script'  // init_script で起動した scripted Stand
  | 'idle'         // sleeping / awaiting Lane
  | 'meta'         // Meta Lane (catalog / inspect 用)

export interface LaneIconSet {
  default: IconName
  active: IconName
}

export const LANE_ICON: Record<LaneKind, LaneIconSet> = {
  lead: {
    default: 'ph:crown-simple',
    active: 'ph:crown-simple-fill',
  },
  worker: {
    default: 'codicon:git-branch',
    active: 'codicon:git-branch', // codicon は weight 切替なし、 active は color 強調で表現
  },
  init_script: {
    default: 'codicon:notebook',
    active: 'codicon:notebook-template',
  },
  idle: {
    default: 'mingcute:moon-line',
    active: 'mingcute:moon-fill',
  },
  meta: {
    default: 'ph:list-magnifying-glass',
    active: 'ph:list-magnifying-glass-fill',
  },
}

export function iconForLane(
  kind: LaneKind,
  state: 'default' | 'active' = 'default',
): IconName {
  return LANE_ICON[kind][state]
}
