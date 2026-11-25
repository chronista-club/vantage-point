/**
 * 左 edge の rail の表（pure data）— doc 87 §4 / §8.2 の 3 つの registry の 1 つ。
 *
 * route の計算（`route.ts`）と描画（`Shell.tsx` の `SHELL_RAIL_REGISTRY`）がこの表を引く。表だけを
 * 別の module に置くのは、**Editor（main bundle）からも読む**ため（doc 87 §8.3 — 行が同梱した Editor field を
 * `editor/app-fields.ts` が集める）。`route.ts` は creo-ui の `selectRailId` を借りていて、import すると
 * creo-ui/shells の Solid の部品（module 評価時に `document` を引く）まで main bundle に入るので、表は
 * runtime の依存を持たない形にした（`import type` だけ）。`route.ts` は従来どおりこの表を re-export する。
 */
import type { RailAnchor } from "@chronista-club/creo-ui/shells";
import type { BundledFieldSpec } from "../../editor/field-spec";

export type ShellRailId = "lanes" | "actions" | "settings";

/** 左 edge の rail（registry の component を除いた部分 — route の計算に要る分だけ）。 */
export interface ShellRailSpec {
	id: ShellRailId;
	route: string;
	order: number;
	/** 表示名（aria-label / title）。i18n 層は無いので日本語 UI の語をそのまま持つ。 */
	label: string;
	/** rail 列の中の起点（creo-ui の `RailAnchor`。上 = 起点、下 = app 級の予約席）。 */
	anchor: RailAnchor;
	/**
	 * この rail が同梱する Editor field（doc 87 §8.3）。id は `rail.<id>.<key>`、region は leftEdge で
	 * Editor の SIDEBAR タブに出る。⚠️ cssVar は `:root` に既定値の宣言がある token に限る（field-spec.ts）。
	 */
	fields?: readonly BundledFieldSpec[];
}

/**
 * rail の registry（PL-3: primitive は rail を hardcode しない — app が渡す）。
 * 描画用の component（icon / peek）は `Shell.tsx` の `SHELL_RAIL_REGISTRY` が同じ id で足す。
 *
 * VP の「増やせる部品」3 つの registry の 1 つ（他は center の種類 = `app-pane-registry.ts`、
 * lane の右 edge の住人 = `edge-residents.ts`）。行の足し方は doc 87 §8.2。
 */
export const SHELL_RAILS: readonly ShellRailSpec[] = [
	{ id: "lanes", route: "/lanes", order: 0, label: "Lanes", anchor: "top" },
	{ id: "actions", route: "/actions", order: 1, label: "Actions", anchor: "top" },
	// 下起点の歯車（app 級の設定。doc 56 §7 の予約席を rail に移した形）
	{ id: "settings", route: "/settings", order: 2, label: "設定", anchor: "bottom" },
];
