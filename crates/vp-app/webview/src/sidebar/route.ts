/**
 * pseudo-route — 左 rail の選択と center の Scene の SSOT（doc 87 §3、PL-4「route が唯一の SSOT」）。
 *
 * VP に router は無いので、文字列を 1 本だけ持つ:
 *
 * | route | rail | Scene |
 * |---|---|---|
 * | `/lanes` / `/lanes/<addr>` | Lanes | 開発 |
 * | `/actions` | Actions | 開発 |
 * | `/settings` / `/settings/<section>` | Settings | 設定 |
 *
 * rail の選択（`aria-current`）も「設定を開いているか」もここから導く。別の signal で
 * 「今どこか」を持たない — 2 本あると片方だけ進む状態が作れてしまう。
 *
 * ## 層の分離
 *
 * この module は **calculations だけ**（DOM も signal も持たない、vitest 対象）。
 * 状態の箱と配線は `route-store.ts`、描画は `ShellRail.tsx`。
 *
 * ## rail の導出は creo-ui の `selectRailId` を使う
 *
 * 最長 prefix 一致・segment 境界の判定は creo-ui の Principal Layout が持つ規則。VP 側で
 * 書き直さず、そのまま借りる（左 edge の描画も creo-ui の `CURail` — `ShellRail.tsx`）。
 */
import { type RailAnchor, type RailDef, selectRailId } from "@chronista-club/creo-ui/shells";
import type { SidebarForm } from "./form";

export type ShellRailId = "lanes" | "actions" | "settings";
export type ShellScene = "dev" | "settings";

/** 左 edge の rail（registry の component を除いた部分 — route の計算に要る分だけ）。 */
export interface ShellRailSpec {
	id: ShellRailId;
	route: string;
	order: number;
	/** 表示名（aria-label / title）。i18n 層は無いので日本語 UI の語をそのまま持つ。 */
	label: string;
	/** rail 列の中の起点（creo-ui の `RailAnchor`。上 = 起点、下 = app 級の予約席）。 */
	anchor: RailAnchor;
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

export const DEFAULT_ROUTE = "/lanes";

const LANES = "/lanes";
const SETTINGS = "/settings";

/**
 * `selectRailId` に渡すための RailDef 形。route の計算は `id` と `route` しか読まないので
 * component は空で足りる（描画は `Shell.tsx` の `SHELL_RAIL_REGISTRY` が持つ）。
 */
const NOTHING = () => null;
const ROUTE_TABLE: readonly RailDef[] = SHELL_RAILS.map((r) => ({
	id: r.id,
	route: r.route,
	order: r.order,
	labelKey: r.label,
	icon: NOTHING,
	peek: NOTHING,
}));

// ============================================================================
// 導出（route → rail / Scene / lane / section）
// ============================================================================

/** route が選ぶ rail。どれにも当たらなければ `undefined`（= rail 非選択）。 */
export function railOf(route: string): ShellRailId | undefined {
	return selectRailId(route, ROUTE_TABLE) as ShellRailId | undefined;
}

/** center の Scene。Settings rail の下だけが設定で、それ以外（未知含む）は開発。 */
export function sceneOf(route: string): ShellScene {
	return railOf(route) === "settings" ? "settings" : "dev";
}

/** `/lanes/<addr>` の addr。address は `/` を含むので 2 段目以降を丸ごと返す。 */
export function laneOf(route: string): string | undefined {
	if (railOf(route) !== "lanes") return undefined;
	const rest = route.slice(LANES.length + 1);
	return rest.length > 0 ? rest : undefined;
}

/** `/settings/<section>` の section。 */
export function sectionOf(route: string): string | undefined {
	if (railOf(route) !== "settings") return undefined;
	const rest = route.slice(SETTINGS.length + 1);
	return rest.length > 0 ? rest : undefined;
}

// ============================================================================
// 組み立て
// ============================================================================

/** lane の route。未選択（null / 空）は `/lanes` だけ。address はエンコードしない（人と AI が読む住所）。 */
export function laneRoute(addr: string | null | undefined): string {
	return addr ? `${LANES}/${addr}` : LANES;
}

export function settingsRoute(section?: string): string {
	return section ? `${SETTINGS}/${section}` : SETTINGS;
}

/** どの rail にも当たらない route は既定（Lanes）へ倒す。 */
export function normalizeRoute(route: string): string {
	return railOf(route) === undefined ? DEFAULT_ROUTE : route;
}

// ============================================================================
// 遷移（純関数 — 状態の箱は route-store.ts）
// ============================================================================

/**
 * route の状態。`devReturn` = 設定を解いたときに戻る開発 route。
 *
 * 不変条件: 開発 Scene にいる間は `devReturn === route`（戻り先は「今いる開発」そのもの）。
 * 設定に入った瞬間だけ両者が分かれ、`devReturn` は入る直前の開発 route を指し続ける。
 */
export interface RouteState {
	route: string;
	devReturn: string;
}

export function initialRouteState(): RouteState {
	return { route: DEFAULT_ROUTE, devReturn: DEFAULT_ROUTE };
}

/** route へ移る。開発の中の移動は戻り先も一緒に進み、設定へ入るときは戻り先を据え置く。 */
export function navigate(state: RouteState, to: string): RouteState {
	const route = normalizeRoute(to);
	if (route === state.route) return state;
	if (sceneOf(route) === "settings") return { route, devReturn: state.devReturn };
	return { route, devReturn: route };
}

/**
 * 設定を閉じて直前の開発 route に戻る（「作業に戻る」/ Esc）。
 * center は設定の間も layout engine に触れていないので、戻れば前の settle がそのまま見える。
 */
export function closeSettings(state: RouteState): RouteState {
	if (sceneOf(state.route) !== "settings") return state;
	return { route: state.devReturn, devReturn: state.devReturn };
}

/**
 * 作業（lane / component）が選ばれた — Rust から active lane / component が配られた時。
 *
 * - 設定中: 設定を抜ける（旧 `SettingsPanel` の「選択が変わったら閉じる」と同じ意味）。
 *   lane なら その lane へ、lane 以外（devices 等）なら直前の開発 route へ。戻り先が Lanes なら
 *   lane は今の選択に合わせ直す（古い lane を route に残さない — route は AI の address でもある）
 * - Lanes にいる: 選んだ lane を route に載せる
 * - Actions にいる: 動かない（user が見ている peek を奪わない）
 *
 * @param addr 選ばれた lane の address。lane 以外の選択は `null`
 */
export function onWorkspaceSelected(state: RouteState, addr: string | null): RouteState {
	if (sceneOf(state.route) === "settings") {
		if (addr) return navigate(state, laneRoute(addr));
		const back = closeSettings(state);
		return railOf(back.route) === "lanes" ? navigate(back, laneRoute(null)) : back;
	}
	if (railOf(state.route) === "lanes") return navigate(state, laneRoute(addr));
	return state;
}

/**
 * 一時遷移（`b` の ACTIONS capture）からの戻り先。
 *
 * - capture が置いた route（`placed`）のままなら、入る前（`saved`）へ戻す
 * - ⚠️ その間に user が別の場所へ移っていたら**戻さない** — 後から（timeout / Esc で）user の操作を
 *   打ち消さない
 * - ⚠️ その間に作業の選択が変わっていたら、戻り先にも [`onWorkspaceSelected`] を当てる — 設定から
 *   入って lane を選んだなら設定は開き直さず、Lanes から入ったなら route に今の lane を載せる
 *
 * @param activeLane 今の active lane（lane 以外の選択は null）
 * @param selectionChanged capture の間に作業の選択が変わったか
 */
export function restoreAfterTransient(
	current: RouteState,
	saved: RouteState,
	placed: string,
	activeLane: string | null,
	selectionChanged: boolean,
): RouteState {
	if (current.route !== placed) return current;
	return selectionChanged ? onWorkspaceSelected(saved, activeLane) : saved;
}

/**
 * rail の icon click。form は peek の開閉（`form.ts` の語。slim = rail 列だけ、full = peek を開く）。
 *
 * - 別の rail → その rail へ移り、peek を開く
 * - 選択中の rail → peek の開閉だけ toggle（route は動かない）
 *
 * @param activeLane 今の active lane。Lanes へ移るときに route へ載せる
 */
export function railClick(
	state: RouteState,
	form: SidebarForm,
	rail: ShellRailId,
	activeLane: string | null | undefined,
): { state: RouteState; form: SidebarForm } {
	if (railOf(state.route) === rail) {
		return { state, form: form === "full" ? "slim" : "full" };
	}
	const target = rail === "lanes" ? laneRoute(activeLane) : railRouteOf(rail);
	return { state: navigate(state, target), form: "full" };
}

function railRouteOf(rail: ShellRailId): string {
	return SHELL_RAILS.find((r) => r.id === rail)?.route ?? DEFAULT_ROUTE;
}
