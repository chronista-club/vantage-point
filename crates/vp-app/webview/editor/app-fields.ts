/**
 * VP の app field — Editor（Ctrl+Shift+E）と MCP の `editor_*`（doc 86 の Link hub）が触る knob（doc 87 §5 / §8.3）。
 *
 * field は 3 つの出どころを持つ:
 *
 * | 出どころ | 例 | 値の行き先 | 保存 |
 * |---|---|---|---|
 * | 基本の宣言（`BASE_FIELD_SPECS`） | `sb.*` / `chat.*` / `shell.resizer.*` / `shell.center.*` | `:root` の inline style（token） | テーマ（`vp:editor:themes:v1`） |
 * | registry の同梱（`APP_FIELD_REGISTRIES`） | `pane.lane.terminal.*` | 同上 | 同上 |
 * | region の幅（`SHELL_LAYOUT_FIELDS`） | `shell.leftEdge.peek.width` / `shell.rightEdge.width` | shell-layout.ts の書き口 | shell-layout（`session.json`） |
 *
 * 各 field は region（leftEdge / center / rightEdge、無し = 全体）を持ち、Editor Mode の semantic（D-2）と
 * Editor の置き場（SIDEBAR = 左右の edge / CHAT / THEME = それ以外）はそこから導く（`field-spec.ts`）。
 *
 * ## ⚠️ 幅はテーマに入れない
 *
 * peek と R の幅は shell の形で、持ち主は shell-layout.ts（drag・復元・保存）。Editor の field はその
 * もう 1 つの操作口で、テーマの保存（`VpEditor.tsx`）からは外す — テーマを切り替えると幅が変わる、
 * drag の保存値とテーマの保存値が食い違う、を作らないため。`:root` にも書かない（`#sidebar-root` /
 * `#app-shell` が同じ変数を inline で持つので、`:root` の値は子に負けて届かない）。
 */
import { type EditorField, type EditorHost, useEditorHost } from "@chronista-club/creo-ui-editor-host";
import type { Region } from "@chronista-club/creo-ui/shells";
import { onCleanup } from "solid-js";
import { APP_PANE_REGISTRY } from "../app-pane-registry";
import { EDGE_RESIDENT_REGISTRY } from "../edge-residents";
import {
	DEFAULT_RIGHT_PCT,
	DEFAULT_SIDEBAR,
	RIGHT_MAX_PCT,
	RIGHT_MIN_PCT,
	SIDEBAR_MAX,
	SIDEBAR_MIN,
	type ShellLayoutState,
	onShellLayoutChange,
	setPeekWidth,
	setRightPct,
	shellLayoutState,
} from "../shell-layout";
import { SHELL_RAILS } from "../src/sidebar/shell-rails";
import {
	type AppFieldSpec,
	type FieldConstraints,
	type FieldRegistrySource,
	collectBundledFields,
	managedVarsOf,
	semanticOfRegion,
} from "./field-spec";

// ============================================================================
// data
// ============================================================================

/** 増やせる部品の 3 つの registry（doc 87 §4）。行の `fields` は collector がここから集める。 */
export const APP_FIELD_REGISTRIES: readonly FieldRegistrySource[] = [
	{ prefix: "rail", region: "leftEdge", entries: SHELL_RAILS },
	{ prefix: "resident", region: "rightEdge", entries: EDGE_RESIDENT_REGISTRY },
	{ prefix: "pane", region: "center", entries: APP_PANE_REGISTRY },
];

const px = (min: number, max: number, step = 0.5): FieldConstraints => ({ min, max, step, unit: "px" });

/**
 * 基本の宣言。⚠️ id は保存済みテーマの key — 変えない・消さない（`app-fields.test.ts` の LEGACY_IDS）。
 * 色の `initial` は stylesheet が無い時の控えで、登録時に `:root` の computed の値で上書きする（テーマの
 * プリセットが変えた色が、そのまま初期値になる）。
 */
const BASE_FIELD_SPECS: readonly AppFieldSpec[] = [
	// 左 edge（Lanes / Actions / 設定の peek）— 文字・配色。名前は sidebar 時代の `sb.*` のまま（保存 key）
	{ id: "sb.text.base", label: "本文", group: "文字サイズ", region: "leftEdge", cssVar: "--sb-text-base", initial: 13, constraints: px(10, 18) },
	{ id: "sb.text.hint", label: "補足", group: "文字サイズ", region: "leftEdge", cssVar: "--sb-text-hint", initial: 12, constraints: px(9, 16) },
	{ id: "sb.text.meta", label: "メタ情報", group: "文字サイズ", region: "leftEdge", cssVar: "--sb-text-meta", initial: 11, constraints: px(8, 15) },
	{ id: "sb.text.micro", label: "小さなラベル", group: "文字サイズ", region: "leftEdge", cssVar: "--sb-text-micro", initial: 10, constraints: px(7, 14) },
	{ id: "sb.background", label: "背景", group: "背景と文字", region: "leftEdge", cssVar: "--lg-void", initial: "#05070A" },
	{ id: "sb.panel", label: "メニュー・パネル", group: "背景と文字", region: "leftEdge", cssVar: "--lg-panel", initial: "#0A0E15" },
	{ id: "sb.text.primary", label: "本文の色", group: "背景と文字", region: "leftEdge", cssVar: "--lg-hot", initial: "#EAFBFF" },
	{ id: "sb.text.secondary", label: "補足の色", group: "背景と文字", region: "leftEdge", cssVar: "--lg-mute", initial: "#5C7A85" },
	{ id: "sb.text.tertiary", label: "控えめな文字", group: "背景と文字", region: "leftEdge", cssVar: "--lg-mute-2", initial: "#38525b" },
	{ id: "sb.border", label: "境界線", group: "背景と文字", region: "leftEdge", cssVar: "--lg-hairline", initial: "#12222b" },
	{ id: "sb.conn.auto", label: "操作のアクセント", group: "背景と文字", region: "leftEdge", cssVar: "--sb-conn-auto", initial: "#FFF76B" },
	// hover・repo の地に重ねる薄い光の素（doc 87 §8.3、--lg-veil-1〜4 がここから混ぜる）。Light Grid では白、
	// 他のプリセットでは文字の色に従う。Light Grid のまま地を明るくした時は、これを暗くすると重ねが見える
	{ id: "sb.veil", label: "重ねる光（hover・repo の地）", group: "背景と文字", region: "leftEdge", cssVar: "--lg-veil", initial: "#fff" },
	{ id: "sb.selection.bg", label: "Lane＋メイン背景", group: "選択行", region: "leftEdge", cssVar: "--sb-selection-bg", initial: "#151c27" },
	{ id: "sb.selection.border", label: "選択 Lane の輪郭色", group: "選択行", region: "leftEdge", cssVar: "--sb-selection-border", initial: "#68758b" },
	{ id: "sb.activity.thinking", label: "思考中", group: "状態ポイント", region: "leftEdge", cssVar: "--sb-activity-thinking", initial: "#c3b4e8" },
	{ id: "sb.activity.working", label: "作業中", group: "状態ポイント", region: "leftEdge", cssVar: "--sb-activity-working", initial: "#a7c9ec" },
	{ id: "sb.activity.waiting", label: "確認待ち", group: "状態ポイント", region: "leftEdge", cssVar: "--sb-activity-waiting", initial: "#ef777d" },
	{ id: "sb.activity.completed", label: "応答完了", group: "状態ポイント", region: "leftEdge", cssVar: "--sb-activity-completed", initial: "#527ec6" },
	{ id: "sb.activity.idle", label: "停止中", group: "状態ポイント", region: "leftEdge", cssVar: "--sb-activity-idle", initial: "#8c98a5" },
	{ id: "sb.activity.error", label: "エラー", group: "状態ポイント", region: "leftEdge", cssVar: "--sb-activity-error", initial: "#ef777d" },
	{ id: "sb.selection.width", label: "選択 Lane の線幅", group: "選択行", region: "leftEdge", cssVar: "--sb-selection-width", initial: 1, constraints: px(0, 3) },
	// center — Chat（lane pane の会話）の文字
	{ id: "chat.text.body", label: "本文", group: "Chat の文字", region: "center", cssVar: "--chat-text-body", initial: 15, constraints: px(12, 22) },
	{ id: "chat.text.user", label: "自分のメッセージ", group: "Chat の文字", region: "center", cssVar: "--chat-text-user", initial: 13.5, constraints: px(10, 20) },
	{ id: "chat.text.tool", label: "ツール・思考", group: "Chat の文字", region: "center", cssVar: "--chat-text-tool", initial: 12, constraints: px(9, 17) },
	{ id: "chat.text.meta", label: "メタ情報", group: "Chat の文字", region: "center", cssVar: "--chat-text-meta", initial: 11, constraints: px(8, 15) },
	{ id: "chat.text.micro", label: "詳細ラベル", group: "Chat の文字", region: "center", cssVar: "--chat-text-micro", initial: 10, constraints: px(7, 14) },
	// center — pane の名札（静的 .pane-header と #lane-header が共有する高さ。main_area.rs の :root）
	{ id: "shell.center.nameplate.height", label: "pane の名札の高さ", group: "pane の名札", region: "center", cssVar: "--vp-nameplate-h", initial: 28, constraints: px(22, 40, 1) },
	// 境界（region と region の間の取っ手）— どの region にも属さない
	{ id: "shell.resizer.breathe.ms", label: "境界の明滅周期", group: "境界の動き", cssVar: "--vp-resizer-breathe-ms", initial: 1694, constraints: { min: 300, max: 2000, step: 1, unit: "ms" } },
	{ id: "shell.resizer.breathe.dip", label: "境界の沈み込み", group: "境界の動き", cssVar: "--vp-resizer-breathe-dip", initial: 0.775, constraints: { min: 0.2, max: 1, step: 0.025 } },
];

/** region の幅 1 本 — 持ち主は shell-layout.ts。Editor は読み（`read`）と書き口（`write`）だけを持つ。 */
export interface ShellLayoutFieldSpec {
	readonly id: string;
	readonly label: string;
	readonly group: string;
	readonly region: "leftEdge" | "rightEdge";
	readonly initial: number;
	readonly constraints: FieldConstraints;
	read(state: Readonly<ShellLayoutState>): number;
	write(value: number): void;
}

/** 左右の edge の幅。範囲は drag と同じ端（shell-layout.ts と Rust の clamp）。 */
export const SHELL_LAYOUT_FIELDS: readonly ShellLayoutFieldSpec[] = [
	{
		id: "shell.leftEdge.peek.width",
		label: "左 peek の幅",
		group: "幅",
		region: "leftEdge",
		initial: DEFAULT_SIDEBAR,
		constraints: { min: SIDEBAR_MIN, max: SIDEBAR_MAX, step: 1, unit: "px" },
		read: (s) => s.sidebarWidth,
		write: setPeekWidth,
	},
	{
		id: "shell.rightEdge.width",
		label: "右の幅（画面に対する割合）",
		group: "幅",
		region: "rightEdge",
		initial: DEFAULT_RIGHT_PCT,
		constraints: { min: RIGHT_MIN_PCT, max: RIGHT_MAX_PCT, step: 0.5, unit: "%" },
		read: (s) => s.rightSidebarPct,
		write: setRightPct,
	},
];

const LAYOUT_FIELD_IDS: ReadonlySet<string> = new Set(SHELL_LAYOUT_FIELDS.map((f) => f.id));

// ============================================================================
// calculations
// ============================================================================

/** `:root` に書く field の宣言（基本 + registry の同梱）。表を渡すと、その表から集める。 */
export function appFieldSpecs(registries: readonly FieldRegistrySource[] = APP_FIELD_REGISTRIES): AppFieldSpec[] {
	return [...BASE_FIELD_SPECS, ...collectBundledFields(registries)];
}

const APP_FIELD_REGION: ReadonlyMap<string, Region | undefined> = new Map([
	...appFieldSpecs().map((s) => [s.id, s.region] as const),
	...SHELL_LAYOUT_FIELDS.map((f) => [f.id, f.region] as const),
]);

/** VP の field の region（Editor のタブを決める）。VP の field でなければ `undefined`。 */
export function appFieldRegion(id: string): Region | undefined {
	return APP_FIELD_REGION.get(id);
}

/** 幅の field か（テーマの保存・復元から外す — 持ち主は shell-layout.ts）。 */
export function isShellLayoutField(id: string): boolean {
	return LAYOUT_FIELD_IDS.has(id);
}

/** creo-ui の token と component の knob（`--_<component>__<knob>`、D-13）。 */
const CREO_MANAGED_VAR = /^--(?:color-(?:brand|surface)-|typography-|layout-gap-sibling$|_)/;
const APP_MANAGED_VARS = managedVarsOf(appFieldSpecs());

/**
 * Editor が `:root` の inline style に書く変数か（テーマを切り替える時に消す対象）。app の分は宣言から導くので、
 * registry の行が同梱した field の変数も、行を足すだけでここに入る。
 */
export function isEditorManagedVar(name: string): boolean {
	return CREO_MANAGED_VAR.test(name) || APP_MANAGED_VARS.has(name);
}

// ============================================================================
// EditorField への組み立て（DOM を読む — 色の初期値は computed から）
// ============================================================================

/** 既定値は stylesheet に残す（初期値に戻したら inline を消す）— テーマのプリセットの値が流れ込めるように。 */
function cssField(spec: AppFieldSpec, computed: CSSStyleDeclaration): EditorField {
	const initial = typeof spec.initial === "number" ? spec.initial : computed.getPropertyValue(spec.cssVar).trim() || spec.initial;
	return {
		id: spec.id,
		label: spec.label,
		cssVar: spec.cssVar,
		initial,
		group: spec.group,
		constraints: spec.constraints,
		type: typeof initial === "number" ? "number" : "color",
		semantic: semanticOfRegion(spec.region),
		scope: "token",
		apply: (value) => {
			if (value === initial) document.documentElement.style.removeProperty(spec.cssVar);
		},
	};
}

/** 幅の field。cssVar も apply も持たない — 値は `bindShellLayoutFields` が shell-layout と繋ぐ。 */
function layoutField(spec: ShellLayoutFieldSpec): EditorField {
	return {
		id: spec.id,
		label: spec.label,
		group: spec.group,
		initial: spec.initial,
		constraints: spec.constraints,
		type: "number",
		semantic: semanticOfRegion(spec.region),
		scope: "instance",
	};
}

/** Editor に登録する field（幅 → 基本 → registry の同梱の順。Editor の見出しはこの順に並ぶ）。 */
export function appFields(registries: readonly FieldRegistrySource[] = APP_FIELD_REGISTRIES): EditorField[] {
	const computed = getComputedStyle(document.documentElement);
	return [...SHELL_LAYOUT_FIELDS.map(layoutField), ...appFieldSpecs(registries).map((spec) => cssField(spec, computed))];
}

// ============================================================================
// actions
// ============================================================================

/**
 * 幅の field と shell-layout を両向きに繋ぐ。Editor / MCP / Link hub の set → shell-layout の書き口、
 * drag・復元 → Editor の値。返り値で外す。
 *
 * - 写し返し（shell-layout → Editor）は **silent**。host の値だけを今の幅に合わせ、subscriber（= 書き口への
 *   送り返し）も onAnyChange（テーマ保存・Link への送信）も起こさない。MCP の `editor_values` は hub が
 *   surface に `get` して host の値を取り直すので、silent でも今の幅が届く
 * - 書き口へ送った後も写し返す。書き口は値を丸め（範囲・R の小数 1 桁）、丸めた値が今と同じなら何もしない
 *   ので、送った値（例: 33.33）のまま Editor に残らないように
 */
export function bindShellLayoutFields(host: EditorHost): () => void {
	const sync = (state: Readonly<ShellLayoutState>) => {
		for (const f of SHELL_LAYOUT_FIELDS) {
			const value = f.read(state);
			if (host.getValue(f.id) !== value) host.setValue(f.id, value, { silent: true });
		}
	};
	const offs = SHELL_LAYOUT_FIELDS.map((f) =>
		host.subscribe<unknown>(f.id, (value) => {
			if (typeof value === "number") f.write(value);
			sync(shellLayoutState());
		}),
	);
	sync(shellLayoutState());
	offs.push(onShellLayoutChange(sync));
	return () => {
		for (const off of offs) off();
	};
}

export function AppTokenBinds() {
	const host = useEditorHost();
	onCleanup(host.register(appFields()));
	onCleanup(bindShellLayoutFields(host));
	return null;
}
