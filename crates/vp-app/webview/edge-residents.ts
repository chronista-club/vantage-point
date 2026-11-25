/**
 * edge resident registry — lane の右 edge（edge rail）の住人を 1 枚の表で持つ（doc 87 §4 / §8.2）。
 *
 * VP の「増やせる部品」3 つのうちの 1 つ（他は左 edge の rail = `src/sidebar/route.ts` の
 * `SHELL_RAILS`、center の種類 = `app-pane-registry.ts`）。住人は lane 級の動詞（doc 56「動詞の級 =
 * 住所」）で、帯の上 / 中央 / 下の 3 起点のどれかに属する（doc 82 の規約。上詰めで積まない）。
 *
 * ## 1 行が持つもの
 *
 * - `anchor` / `order`: どの起点の slot に、その中で何番目に居るか
 * - `hostId`: 静的 HTML（`main_area.rs` の `#edge-rail`）にある住人の DOM 要素の id
 * - `relocateToRightSidebar`: R sidebar を開いている間、R の Lane 節へ付け替えるか（right-sidebar.ts）。
 *   付け替えるのは「同じ住人の見え方の切替」で、住人を 2 つ描かない（状態が 1 つのまま）。PR は
 *   R 側に PR 節そのもの（伸びた形）があるので付け替えない
 * - `content`: 中身を誰が持つか。`"solid"` = `EdgeRail.tsx` が host に描く / `"static"` = 静的 HTML が
 *   中身ごと持ち、配線は別 module（board 取っ手 = board-view.ts、doc 55 の user 専有の表示状態）
 *
 * - `fields`（任意）: この住人が同梱する Editor field（doc 87 §8.3）。id は `resident.<id>.<key>`、region は
 *   rightEdge で Editor の SIDEBAR タブに出る。⚠️ cssVar は `:root` に既定値の宣言がある token に限る
 *
 * ## 静的 DOM との約束
 *
 * 表に 1 行足したら、`#edge-rail` の該当する `.anchor-*` に host を置く（`content: "solid"` なら空の
 * `<div id=hostId>`）。`EdgeRail.tsx` の描画表は `SolidEdgeResidentId` で型が決まるので、描き手を
 * 書き忘れると tsc が止める。静的 HTML との 1:1 は `shell-registries.dom.test.ts` が固定する。
 *
 * ## 層の分離
 *
 * この module は **data + calculations だけ**（DOM を持たない、vitest 対象）。導出は表を引数に取る
 * 純関数 — 表に 1 行足した時の振る舞いを test で確かめられるように（PL §5.2 の open-closed）。
 */

import type { BundledFieldSpec } from "./editor/field-spec";

/** 縦の帯の 3 起点（doc 82）。並びはこの配列の順（上 → 中央 → 下）。 */
export const EDGE_ANCHORS = ["top", "center", "bottom"] as const;
export type EdgeAnchor = (typeof EDGE_ANCHORS)[number];

export interface EdgeResidentDef {
	readonly id: string;
	readonly anchor: EdgeAnchor;
	/** 同じ起点の中の並び（起点の順が先に効く）。 */
	readonly order: number;
	/** 静的 HTML にある住人の DOM 要素の id。 */
	readonly hostId: string;
	/** R sidebar を開いている間、Lane 節へ付け替えるか。 */
	readonly relocateToRightSidebar: boolean;
	/** 中身の持ち主: `"solid"` = EdgeRail.tsx が描く / `"static"` = 静的 HTML（配線は別 module）。 */
	readonly content: "solid" | "static";
	/** この住人が同梱する Editor field（任意）。 */
	readonly fields?: readonly BundledFieldSpec[];
}

export const EDGE_RESIDENT_REGISTRY = [
	// 上: + New（session を足す。menu は帯の左横に開く）
	{
		id: "new",
		anchor: "top",
		order: 0,
		hostId: "edge-rail-new-host",
		relocateToRightSidebar: true,
		content: "solid",
	},
	// 中央: しまった pane（Pane のしまうモード、creo mem_1CfbF4m1sGusje8oMouTu8）
	{
		id: "stowed",
		anchor: "center",
		order: 0,
		hostId: "edge-rail-stow-host",
		relocateToRightSidebar: true,
		content: "solid",
	},
	// 下: board 取っ手（doc 55）— 中身は静的 HTML、開閉と badge は board-view.ts
	{
		id: "board",
		anchor: "bottom",
		order: 0,
		hostId: "board-handle",
		relocateToRightSidebar: true,
		content: "static",
	},
	// 下: PR ボタン（doc 82）— 押すと R の PR 節が開く。R 側では PR 節が伸びた形なので付け替えない
	{
		id: "prs",
		anchor: "bottom",
		order: 1,
		hostId: "rail-pr-host",
		relocateToRightSidebar: false,
		content: "solid",
	},
] as const satisfies readonly EdgeResidentDef[];

export type EdgeResident = (typeof EDGE_RESIDENT_REGISTRY)[number];
export type EdgeResidentId = EdgeResident["id"];
/** EdgeRail.tsx が描く住人（描画表の key。行を足すと描き手の書き忘れが型で落ちる）。 */
export type SolidEdgeResidentId = Extract<EdgeResident, { content: "solid" }>["id"];

// ============================================================================
// 導出（表を引数に取る純関数）
// ============================================================================

const anchorRank = (a: EdgeAnchor): number => EDGE_ANCHORS.indexOf(a);

/** rail の上から下への並び（起点の順 → 同じ起点の中は order の順）。元の表は壊さない。 */
export function edgeResidentsInRailOrder<D extends EdgeResidentDef>(registry: readonly D[]): D[] {
	return [...registry].sort((a, b) => anchorRank(a.anchor) - anchorRank(b.anchor) || a.order - b.order);
}

/** 1 つの起点の slot に居る住人（order の順）。 */
export function edgeResidentsAt<D extends EdgeResidentDef>(registry: readonly D[], anchor: EdgeAnchor): D[] {
	return edgeResidentsInRailOrder(registry).filter((r) => r.anchor === anchor);
}

/** R sidebar の Lane 節へ付け替える住人の host id（rail の並び = Lane 節での並び）。 */
export function relocatedResidentHostIds(registry: readonly EdgeResidentDef[]): string[] {
	return edgeResidentsInRailOrder(registry)
		.filter((r) => r.relocateToRightSidebar)
		.map((r) => r.hostId);
}
