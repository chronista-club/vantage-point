/**
 * pseudo-route の状態の箱と配線（doc 87 §3）。計算は `route.ts`（純関数）が持ち、ここは
 * signal 1 本と、それを動かす action だけを置く。
 *
 * - **data**: `routeState` signal（この module が SSOT。sidebar bundle の module scope に 1 つ）
 * - **actions**: `navigateTo` / `leaveSettings` / `clickRail` / `showRail` / `useRouteSync`
 *
 * peek の開閉（full / slim）は `form.ts` が持ち主のまま — route は「どこを選んでいるか」、
 * form は「peek を開いているか」で、別の問い。rail の click は両方を動かすので、ここで
 * `route.ts` の `railClick` が返した形を `form.ts` に渡す。
 */
import { createEffect, createSignal, on } from "solid-js";
import { collapseSidebar, expandSidebar, sidebarForm } from "./form";
import {
	type RouteState,
	type ShellRailId,
	closeSettings,
	initialRouteState,
	navigate,
	onWorkspaceSelected,
	railClick,
	railOf,
	sceneOf,
} from "./route";
import { sidebar } from "./store";

const [routeState, setRouteState] = createSignal<RouteState>(initialRouteState());

/** 今の route（reactive）。 */
export function currentRoute(): string {
	return routeState().route;
}

/** 今の Scene（reactive）。`settings` の間、center は設定面を表示している。 */
export function currentScene(): "dev" | "settings" {
	return sceneOf(routeState().route);
}

/** 今選ばれている rail（reactive）。 */
export function currentRail(): ShellRailId | undefined {
	return railOf(routeState().route);
}

/** route へ移る（peek の開閉は触らない）。 */
export function navigateTo(route: string): void {
	setRouteState((s) => navigate(s, route));
}

/** 設定を閉じて直前の開発 route に戻る（「作業に戻る」/ Esc）。 */
export function leaveSettings(): void {
	setRouteState((s) => closeSettings(s));
}

/** 今の active lane（lane 以外の component が選ばれている間は null）。 */
function activeLane(): string | null {
	if (sidebar.active_component) return null;
	return sidebar.active_lane_address ?? null;
}

function applyForm(next: "full" | "slim"): void {
	if (next === sidebarForm()) return;
	if (next === "full") expandSidebar();
	else collapseSidebar();
}

/** rail の icon click — 別の rail なら移って peek を開く、選択中の rail なら peek を開閉する。 */
export function clickRail(rail: ShellRailId): void {
	const r = railClick(routeState(), sidebarForm(), rail, activeLane());
	setRouteState(r.state);
	applyForm(r.form);
}

/**
 * rail を確実に見せる（directive / slim badge から「その rail の中身を操作したい」時）。
 * click と違い toggle しない — 既に選ばれていても畳まず、必ず peek を開く。
 */
export function showRail(rail: ShellRailId): void {
	if (railOf(routeState().route) !== rail) {
		setRouteState(railClick(routeState(), "full", rail, activeLane()).state);
	}
	expandSidebar();
}

/** 一時遷移（`b` の ACTIONS capture）の退避用。 */
export function routeSnapshot(): RouteState {
	return routeState();
}

/** [`routeSnapshot`] で退避した状態に戻す。 */
export function restoreRoute(saved: RouteState): void {
	setRouteState(saved);
}

/** 作業の選択（Rust から配られた active lane / component）。同じ選択の配り直しでは値が変わらない。 */
function workspaceSelection(): string {
	return JSON.stringify([
		sidebar.active_lane_address ?? null,
		sidebar.active_component?.repo_path ?? null,
		sidebar.active_component?.kind ?? null,
	]);
}

/**
 * route を作業の選択に追随させ、route を DOM に映す。Shell の mount 中に 1 回呼ぶ（owner が要る）。
 *
 * - 選択が**変わった時だけ** `onWorkspaceSelected` を通す（定期 push や会話の更新では動かない）。
 *   mount 時にも 1 回通す — 起動時点で既に選ばれている lane を route に載せるため（defer しない）
 * - `#app-shell` に `data-route` / `data-scene` を書く — `vp shot` / DOM 検査で「今どの場面か」が
 *   読める（AI の address、doc 51）
 */
export function useRouteSync(): void {
	createEffect(
		on(workspaceSelection, () =>
			setRouteState((s) => onWorkspaceSelected(s, activeLane())),
		),
	);
	createEffect(() => {
		const shell = document.getElementById("app-shell");
		if (!shell) return;
		shell.dataset.route = currentRoute();
		shell.dataset.scene = currentScene();
	});
}

/** テスト用: module 状態を初期化する。 */
export function _resetRouteForTest(): void {
	setRouteState(initialRouteState());
}
