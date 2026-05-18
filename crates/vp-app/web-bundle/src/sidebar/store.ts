/**
 * sidebar の Solid store — Rust 側 `SidebarState` の mirror。
 *
 * v1.0 柱 2 PR-1 足場。 Rust が `window.renderSidebarState(json)` で push してくる
 * `SidebarState` を、 ここで定義する単一の Solid store に流し込む。 sidebar component は
 * この `sidebar` proxy を読むだけで、 Rust からの state 更新に細粒度で追従する。
 *
 * 型は `src/generated/` の ts-rs 生成物をそのまま使う (手書き 1:1 の drift を排除)。
 */
import { createStore, reconcile } from 'solid-js/store'
import type { SidebarState } from '../generated/SidebarState'

/**
 * `SidebarState` の空 state。 Rust 側 `#[derive(Default)]` と等価。
 *
 * 必須 field (processes / widget / activity / lanes_by_project /
 * unread_notifications / awaiting_input) のみ埋め、 optional field は省略する。
 */
export function emptyState(): SidebarState {
  return {
    processes: [],
    widget: 'activity',
    activity: {
      world_online: false,
      project_count: 0,
      running_process_count: 0,
    },
    lanes_by_project: {},
    unread_notifications: {},
    awaiting_input: {},
  }
}

const [sidebar, setSidebarStore] = createStore<SidebarState>(emptyState())

/** sidebar component が読む reactive な state proxy。 */
export { sidebar }

/**
 * Rust から push された `SidebarState` 全体を store に反映する。
 *
 * `reconcile` で差分のみ適用し、 変化していない subtree の再描画を避ける
 * (Solid の細粒度反応性を活かす)。
 */
export function applySidebarState(next: SidebarState): void {
  setSidebarStore(reconcile(next))
}
