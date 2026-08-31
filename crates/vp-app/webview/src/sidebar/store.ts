/**
 * sidebar の Solid store — Rust 側 `SidebarState` の mirror。
 *
 * v1.0 柱 2 PR-1 足場。 Rust が push envelope `sidebar:state` で送ってくる
 * `SidebarState` を、 ここで定義する単一の Solid store に流し込む。 sidebar component は
 * この `sidebar` proxy を読むだけで、 Rust からの state 更新に細粒度で追従する。
 *
 * 型は `src/generated/` の ts-rs 生成物をそのまま使う (手書き 1:1 の drift を排除)。
 */
import { createStore, reconcile } from "solid-js/store";
import type { SidebarState } from "../generated/SidebarState";

/**
 * `SidebarState` の空 state。 Rust 側 `#[derive(Default)]` と等価。
 *
 * 必須 field (processes / widget / activity / lanes_by_repo /
 * unread_notifications / awaiting_input) のみ埋め、 optional field は省略する。
 */
export function emptyState(): SidebarState {
	return {
		processes: [],
		widget: "activity",
		activity: {
			node_online: false,
			repo_count: 0,
			running_repo_count: 0,
			hub: "",
			hub_nodes: [],
			hub_auth: "",
			presence: {},
			// 0 = 未取得。`quietAfterMs` が既定 5 分に倒す（daemon から届くまでの間）。
			idle_timeout_minutes: 0n,
			update_available: false,
			update_applying: false,
			// ACTIONS は `actions-panel/store.ts` の signal が持つ。ここは Rust mirror の
			// 空値で、`actions_rev: 0` = 未取得 = **取り込まない**印（doc 57 Phase 3）。
			actions: [],
			actions_rev: 0,
		},
		lanes_by_repo: {},
		unread_notifications: {},
		awaiting_input: {},
		canvas_unread: {},
	};
}

const [sidebar, setSidebarStore] = createStore<SidebarState>(emptyState());

/** sidebar component が読む reactive な state proxy。 */
export { sidebar };

/**
 * Rust から push された `SidebarState` 全体を store に反映する。
 *
 * `reconcile` で差分のみ適用し、 変化していない subtree の再描画を避ける
 * (Solid の細粒度反応性を活かす)。
 */
export function applySidebarState(next: SidebarState): void {
	setSidebarStore(reconcile(next));
}

/**
 * `currents_order` だけを即時更新する (#124 D&D の楽観更新)。
 *
 * Rust の `process:reorder` ハンドラは re-push しない設計なので、 drop 直後に
 * ここでローカル store を更新しないと、 次の `SidebarState` push まで並びが
 * 変わらない (ポーリング待ちのタイムラグ)。 Rust は同じ order を永続化するため、
 * 次の push 時には値が一致し reconcile が no-op になる (flash しない)。
 */
export function setCurrentsOrder(order: string[]): void {
	setSidebarStore("currents_order", order);
}
