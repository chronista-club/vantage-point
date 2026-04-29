// VP Stand glyph mapping (creo-ui-icons-web の VP-domain alias)。
//
// Stand 概念翻訳辞書も兼ねる: 各 JoJo Stand → Phosphor icon の対応。
// state-driven: default = regular, active = fill, disabled = thin weight。
//
// 参考 memory: ~/.claude/projects/.../memory/feedback_creo_ui_icon_dual_axis.md

import type { IconName } from 'creo-ui-icons-web'

export type StandKind =
  | 'heavens_door'
  | 'paisley_park'
  | 'gold_experience'
  | 'hermit_purple'
  | 'whitesnake'
  | 'theworld'

export interface StandIconSet {
  default: IconName  // idle/regular state (Phosphor Regular weight)
  active: IconName   // active/lit state (Phosphor Fill weight)
  disabled?: IconName // disabled state (Phosphor Thin weight、 optional)
}

export const STAND_ICON: Record<StandKind, StandIconSet> = {
  heavens_door: {
    default: 'ph:book-open',
    active: 'ph:book-open-fill',
    disabled: 'ph:book-open-thin',
  },
  paisley_park: {
    default: 'ph:compass',
    active: 'ph:compass-fill',
    disabled: 'ph:compass-thin',
  },
  gold_experience: {
    default: 'ph:plant',
    active: 'ph:plant-fill',
    disabled: 'ph:plant-thin',
  },
  hermit_purple: {
    default: 'ph:plug',
    active: 'ph:plug-fill',
    disabled: 'ph:plug-thin',
  },
  whitesnake: {
    default: 'ph:database',
    active: 'ph:database-fill',
    disabled: 'ph:database-thin',
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
