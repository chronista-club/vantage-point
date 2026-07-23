/**
 * layout-host — webview 全体で共有する LayoutEngine instance（doc 49 LE-P4）。
 *
 * scope 一覧（doc §12: scope 分離 × 0..1 自己相似で入れ子は protocol 変更ゼロ）:
 *   - "app"          … app 全体の pane 配置（app-panes.ts、旧 FrameEngine の領分）
 *   - "lane:<addr>"  … lane 内 tiling（PR2 で pane-shell.ts を置換予定）
 *
 * gallery は mode 単位の独立 engine を維持する（gallery-panes.tsx の module 所有 —
 * 場と settle log の寿命が gallery mode に閉じる設計、#871）。
 */

import { createLayoutEngine } from "@chronista-club/creo-ui-layout";

export const layoutEngine = createLayoutEngine();
