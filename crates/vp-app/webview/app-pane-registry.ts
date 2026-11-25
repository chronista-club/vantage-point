/**
 * pane kind registry — center（app scope）の pane の種類を 1 枚の表で持つ（doc 87 §4 / §8.2）。
 *
 * VP の「増やせる部品」3 つのうちの 1 つ（他は左 edge の rail = `src/sidebar/route.ts` の
 * `SHELL_RAILS`、右 edge の住人 = `edge-residents.ts`）。primitive は意味づけせず、app が表を渡す
 * （creo-ui PL-3）という向きを 3 つで揃えている。
 *
 * ## 1 行が持つもの
 *
 * 同じ pane を指す 4 つの名前を 1 行に並べる。**どれも別の軸で、互いに代用しない**:
 *
 * | 列 | 何の名前か | 誰が読むか |
 * |---|---|---|
 * | `id` | 配置の住所（`data-app-pane`、layout の pane id） | app-panes.ts（場の構造と投影） |
 * | `domId` | DOM 要素の id（可視性の gate） | active-pane.ts（`.active` の付け替え） |
 * | `kinds` | Rust `push_active_view` が送る kind | entry.tsx / active-pane.ts（受け口の解決） |
 * | `scene` | その pane の独占の場（Scene） | app-panes.ts（preset / 訪問 / 巡回） |
 *
 * 旧形は `entry.tsx` と `active-pane.ts` が kind → 住所 / kind → DOM id の表を別々に持ち、
 * 「別の写像なので統合してはいけない」と注記していた。写像が別なのは正しい — だから表を 1 つにしても
 * 列は分けたままにする。分けていた理由は軸の違いで、行き先の pane の集合は同じだった（違っていたのは
 * kind の受け口 = 表の key で、旧名 "terminal" を片方だけが持っていた）。
 *
 * ## 静的 DOM との約束（doc 49 R1）
 *
 * pane の DOM（`#host` の中の `.pane[data-app-pane]`）は Rust の静的 HTML（`main_area.rs`）にあり、
 * Solid で作り直さない・動かさない（xterm / iframe は作り直せない）。表に 1 行足したら、同じ id の
 * pane を静的 HTML にも置く。両者の 1:1 は `shell-registries.dom.test.ts` が固定していて、片方だけ
 * 足すとそこで落ちる。
 *
 * ## 層の分離
 *
 * この module は **data + calculations だけ**（DOM も engine も持たない、vitest 対象）。
 * 導出は表を引数に取る純関数にしてある — 表に 1 行足した時の振る舞いを test で確かめられるように
 * （PL §5.2 の open-closed）。
 */
import type { Layout, Scene } from "@chronista-club/creo-ui-layout";

export interface AppPaneSceneSpec {
	/** Scene id（hotkey / 巡回 / `window.vpAppLayout.applyScene` が名指す） */
	readonly id: string;
	readonly name: string;
	readonly description: string;
}

export interface AppPaneDef {
	/** 配置の住所（`data-app-pane` の値、layout の pane id）。 */
	readonly id: string;
	/** DOM 要素の id（静的 HTML の `.pane` の `id`）。 */
	readonly domId: string;
	/**
	 * `setActivePane` が受ける kind。先頭が正規名（= 静的 DOM の `data-kind`、slot rect の kind）、
	 * 後ろは旧名の受け口。kind は表の中で 1 つの pane にしか属さない。
	 */
	readonly kinds: readonly string[];
	/** この pane が独占する場。 */
	readonly scene: AppPaneSceneSpec;
	/** `Ctrl+Shift+]/[` の巡回と `Ctrl+Shift+数字` に入れるか。 */
	readonly inCycle: boolean;
	/** 並び（場の構造の列順 = 同格の主役の後勝ち順、巡回の順、数字キーの順）。 */
	readonly order: number;
}

/**
 * app pane の表。doc 52 §10 wave 0 で pp（Board）は app pane を退役し、lane tiling の board pane
 * （`#lane-board`、lane-panes.ts）へ移った — board は lane pane（lane workbench）の中に並ぶ。
 */
export const APP_PANE_REGISTRY = [
	{
		id: "lane",
		domId: "pane-lane",
		// "terminal" は旧名。Rust の push_active_view（lane_view.rs）は lane には "lane" しか送らないが、
		// 旧い呼び手を empty に落とさないために受け口だけ残す。
		kinds: ["lane", "terminal"],
		scene: {
			id: "lane-focus",
			name: "Lead Focus",
			description: "集中 coding — lane workbench（Conversation）独占",
		},
		inCycle: true,
		order: 0,
	},
	{
		id: "runner",
		domId: "pane-runner",
		kinds: ["runner"],
		scene: { id: "runner-focus", name: "runner Focus", description: "Runner 単独" },
		inCycle: true,
		order: 1,
	},
	{
		id: "devices",
		domId: "pane-devices",
		kinds: ["devices"],
		scene: { id: "devices-focus", name: "Devices Focus", description: "Devices 単独" },
		inCycle: true,
		order: 2,
	},
	{
		id: "preview",
		domId: "pane-preview",
		kinds: ["preview"],
		scene: { id: "preview-focus", name: "Preview Focus", description: "Preview 単独" },
		inCycle: true,
		order: 3,
	},
	{
		id: "empty",
		domId: "pane-empty",
		kinds: ["empty"],
		scene: { id: "empty", name: "Empty", description: "何も選択していない — placeholder のみ" },
		// 「何も選んでいない」は巡って行く場所ではない
		inCycle: false,
		order: 4,
	},
] as const satisfies readonly AppPaneDef[];

/** 表の 1 行（literal 型のまま — `pane.id === "lane"` の比較が表に無い名前なら型で落ちる）。 */
export type AppPane = (typeof APP_PANE_REGISTRY)[number];
export type AppPaneId = AppPane["id"];

/** kind が不明 / 空の時の落ち先（DOM の可視性は「何か 1 枚」が active である必要がある）。 */
export const EMPTY_APP_PANE: AppPane = APP_PANE_REGISTRY.find((p) => p.id === "empty")!;

// ============================================================================
// 導出（表を引数に取る純関数）
// ============================================================================

/** order 昇順のコピー（元の表は壊さない）。 */
export function appPanesByOrder<D extends AppPaneDef>(registry: readonly D[]): D[] {
	return [...registry].sort((a, b) => a.order - b.order);
}

/** 配置の住所の列（order 順）。 */
export function appPaneIdsOf(registry: readonly AppPaneDef[]): string[] {
	return appPanesByOrder(registry).map((p) => p.id);
}

/** kind → pane。表に無い kind と空は `undefined`（落ち先は呼び手が決める）。 */
export function appPaneOfKind<D extends AppPaneDef>(
	registry: readonly D[],
	kind: string | null | undefined,
): D | undefined {
	if (!kind) return undefined;
	return registry.find((p) => p.kinds.includes(kind));
}

/** 住所 → pane。 */
export function appPaneById<D extends AppPaneDef>(registry: readonly D[], id: string): D | undefined {
	return registry.find((p) => p.id === id);
}

/**
 * pane ごとの独占の場（旧 `APP_SCENES` の直書き）。
 *
 * 構造は全 pane が 1 列ずつ（横並び）で、可視性・大小は場（attention）が決める — 対象が 1、他は 0
 * （= 非表示）。場ごとに構造 object を作り直す（engine が 1 つの場を書き換えても他に波及しない）。
 */
export function appScenesOf(registry: readonly AppPaneDef[]): Scene[] {
	const ordered = appPanesByOrder(registry);
	const focusLayout = (focus: string): Layout => ({
		structure: { columns: ordered.map((p) => ({ panes: [p.id] })) },
		attention: Object.fromEntries(ordered.map((p) => [p.id, p.id === focus ? 1 : 0])),
	});
	return ordered.map((p) => ({
		id: p.scene.id,
		name: p.scene.name,
		description: p.scene.description,
		layout: focusLayout(p.id),
	}));
}

/** `Ctrl+Shift+]/[` で巡る場の id（order 順、inCycle だけ）。 */
export function presetCycleOf(registry: readonly AppPaneDef[]): string[] {
	return appPanesByOrder(registry)
		.filter((p) => p.inCycle)
		.map((p) => p.scene.id);
}

/** 数字キーは 1〜9 まで（`event.code` の Digit1〜Digit9）。 */
const MAX_DIGIT_HOTKEYS = 9;

/**
 * `Ctrl+Shift+数字` → 場の id（巡回の順に Digit1 から）。
 *
 * `event.code`（物理キー位置）で照合する — `event.key` は Shift で "1" → "!" 等に変わるため。
 */
export function sceneHotkeysOf(cycle: readonly string[]): Record<string, string> {
	return Object.fromEntries(
		cycle.slice(0, MAX_DIGIT_HOTKEYS).map((sceneId, i) => [`Digit${i + 1}`, sceneId]),
	);
}
