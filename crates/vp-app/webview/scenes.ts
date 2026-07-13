/**
 * VP の default Scene 定義 — UX narrative としての Pane 配置 preset。
 *
 * VP-140 / PR-ε-1。 doc 13 §7 5 surface のうち Echoes / PP の 2 軸を 4 Scene で展開。
 * (Inline / Modal / Dev Panel は overlay 系で常駐 host = body 直下に mount するため
 *  frame Scene には登場しない、 PR-ε-2 以降で再考)
 *
 * Hotkey:
 * - Cmd+Shift+1 → Lead Focus
 * - Cmd+Shift+2 → Side Review
 * - Cmd+Shift+3 → PP Overlay
 * - Cmd+Shift+4 → PP Focus
 * - Cmd+Shift+]/[ → cycle next/prev
 */

import type { PaneId, PaneTransform, Scene } from './frame-engine';

const NORMAL = 'normal' as const;
const HIDDEN = 'hidden' as const;
const MAXIMIZED = 'maximized' as const;

/** 集中 coding — Echoes 全画面、 PP は背後で待機. */
const LEAD_FOCUS: Scene = {
  id: 'lead-focus',
  name: 'Lead Focus',
  description: '集中 coding — Echoes 全画面、 PP は背後で待機',
  panes: {
    echoes: { x: 0, y: 0, w: 1, h: 1, z: 10, opacity: 1, state: MAXIMIZED },
    pp:     { x: 0, y: 0, w: 1, h: 1, z: 0,  opacity: 0, state: HIDDEN },
  },
};

/** 並列 review — 左で打鍵、 右で参照. */
const SIDE_REVIEW: Scene = {
  id: 'side-review',
  name: 'Side Review',
  description: '並列 review — 左で打鍵、 右で参照',
  panes: {
    echoes: { x: 0,   y: 0, w: 0.5, h: 1, z: 10, opacity: 1, state: NORMAL },
    pp:     { x: 0.5, y: 0, w: 0.5, h: 1, z: 10, opacity: 1, state: NORMAL },
  },
};

/** 軽い参照 — Echoes は薄く、 PP が右上に浮かぶ overlay. */
const PP_OVERLAY: Scene = {
  id: 'pp-overlay',
  name: 'PP Overlay',
  description: '軽い参照 — Echoes は薄く、 PP が右上に浮かぶ',
  panes: {
    echoes: { x: 0,    y: 0,    w: 1,    h: 1,   z: 0,  opacity: 0.25, state: NORMAL },
    pp:     { x: 0.58, y: 0.04, w: 0.38, h: 0.6, z: 20, opacity: 1,    state: NORMAL },
  },
};

/** Canvas 集中 — PP 全画面、 Echoes は背後で待機. */
const PP_FOCUS: Scene = {
  id: 'pp-focus',
  name: 'PP Focus',
  description: 'Canvas 集中 — PP 全画面、 Echoes は背後',
  panes: {
    echoes: { x: 0, y: 0, w: 1, h: 1, z: 0,  opacity: 0, state: HIDDEN },
    pp:     { x: 0, y: 0, w: 1, h: 1, z: 10, opacity: 1, state: MAXIMIZED },
  },
};

/** Cmd+Shift+1..4 で apply される 4 Scene (登録順 = cycleScene 順). */
export const DEFAULT_SCENES: readonly Scene[] = [
  LEAD_FOCUS,
  SIDE_REVIEW,
  PP_OVERLAY,
  PP_FOCUS,
];

/** sidebar から「何も選択していない」 状態で apply される Scene. */
export const EMPTY_SCENE: Scene = {
  id: 'empty',
  name: 'Empty',
  description: '何も表示しない — placeholder のみ表示',
  panes: {
    empty: { x: 0, y: 0, w: 1, h: 1, z: 0, opacity: 1, state: NORMAL },
  },
};

/** Pane 1 つを全画面 maximize、 他を全 hide する single-Pane focus Scene を生成. */
export function generateFocusScene(focusId: PaneId, allIds: readonly PaneId[]): Scene {
  const panes: Record<PaneId, PaneTransform> = {};
  for (const id of allIds) {
    panes[id] = id === focusId
      ? { x: 0, y: 0, w: 1, h: 1, z: 10, opacity: 1, state: MAXIMIZED }
      : { x: 0, y: 0, w: 1, h: 1, z: 0,  opacity: 0, state: HIDDEN };
  }
  return {
    id: `${focusId}-focus`,
    name: `${focusId} focus`,
    description: `${focusId} を全画面で focus、 他は背後で待機`,
    panes,
  };
}

/** 全 Pane 分の focus Scene を一括生成 (旧 setActivePane({kind}) 互換用). */
export function generateAllFocusScenes(allIds: readonly PaneId[]): Scene[] {
  return allIds.map((id) => generateFocusScene(id, allIds));
}
