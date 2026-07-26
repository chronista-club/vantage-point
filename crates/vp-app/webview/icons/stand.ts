// VP Stand glyph mapping (@chronista-club/creo-ui-icons-web の VP-domain alias)。
//
// Stand 概念翻訳辞書も兼ねる: 各 stand kind → Phosphor icon の対応。
// state-driven: default = regular, active = fill, disabled = thin weight。
//
// 参考 memory: ~/.claude/projects/.../memory/feedback_creo_ui_icon_dual_axis.md

import type { IconName } from '@chronista-club/creo-ui-icons-web'

// PR-pre2 (VP-118): heavens_door → echoes rename。
// emoji 📖 → 💬、 icon 'ph:book-open' → 'ph:chat-circle' (prompt/response 対話型)。
export type StandKind =
  | 'echoes'
  | 'board'
  | 'runner'
  | 'theworld'

export interface StandIconSet {
  default: IconName  // idle/regular state (Phosphor Regular weight)
  active: IconName   // active/lit state (Phosphor Fill weight)
  disabled?: IconName // disabled state (Phosphor Thin weight、 optional)
}

export const STAND_ICON: Record<StandKind, StandIconSet> = {
  echoes: {
    default: 'ph:chat-circle',
    active: 'ph:chat-circle-fill',
    disabled: 'ph:chat-circle-thin',
  },
  board: {
    default: 'ph:compass',
    active: 'ph:compass-fill',
    disabled: 'ph:compass-thin',
  },
  runner: {
    default: 'ph:plant',
    active: 'ph:plant-fill',
    disabled: 'ph:plant-thin',
  },
  theworld: {
    default: 'ph:planet',
    active: 'ph:planet-fill',
    disabled: 'ph:planet-thin',
  },
}

// Helper: Stand kind + state から icon name を解決
export function iconForStand(
  stand: StandKind,
  state: 'default' | 'active' | 'disabled' = 'default',
): IconName {
  const set = STAND_ICON[stand]
  return set[state] ?? set.default
}
