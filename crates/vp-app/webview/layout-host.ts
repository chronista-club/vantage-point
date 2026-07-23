/**
 * layout-host — webview 全体で共有する LayoutEngine instance（doc 49 LE-P4）。
 *
 * scope 一覧（doc §12: scope 分離 × 0..1 自己相似で入れ子は protocol 変更ゼロ）:
 *   - "app"          … app 全体の pane 配置（app-panes.ts、旧 FrameEngine の領分）
 *   - "lane:<addr>"  … lane 内 tiling（lane-panes.ts、旧 pane-shell.ts の領分）
 *   - "gallery"      … component gallery mode（gallery-panes.tsx。LE-P4 PR3 で独立
 *                      instance から統一 — scope に閉じるので「gallery を閉じても場と
 *                      settle log が残る」寿命の性質は #871 のまま不変）
 */

import { createLayoutEngine } from "@chronista-club/creo-ui-layout";

export const layoutEngine = createLayoutEngine();
