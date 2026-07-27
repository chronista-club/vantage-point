/**
 * app-panes — app 全体の pane 配置を creo-ui-layout の場に写す（doc 49 LE-P4 PR1）。
 *
 * 旧 3 点セット（frame-engine.ts の Scene engine / scenes.ts の preset / renderer.ts の
 * DOM 反映、VP-140）の後継。離散 Scene は attention 連続場の named snapshot になり、
 * 「hidden = attention 0」「maximized = 独占」「overlay = 構造非所属の floating」に写る。
 *
 * ## 層の分離（CLAUDE.md: data / calculations / actions）
 *
 * - **data**: `APP_SCENES` — preset の Layout 群。DOM を知らない
 * - **calculations**: `primaryAppPane` ほか純関数（vitest で固定）
 * - **actions**: `installAppPanes` — engine 購読 → 既存 `.pane[data-frame-id]` への
 *   inline style 書き込みだけを行う（R1: DOM を reparent しない — xterm は再生成不可）
 *
 * ## 投影規則（DOM の縁の作法、engine の意味論ではない）
 *
 * - **非表示 pane は「全面サイズ + 透明」**に写す。resolve は非表示を面積 0 に畳むが、
 *   それを DOM に書くと xterm の container が 0×0 になり PTY resize が走る。旧
 *   HIDDEN_TRANSFORM（w1 h1 opacity 0）が守っていた xterm-friendly な hidden を継ぐ
 * - motion は CSS transition（`--frame-transition-ms` token、main_area.rs）が担う。
 *   scrub / driver を app scope に入れる時はこの CSS transition を外すこと（両立しない）
 * - `.active` class は legacy 互換（sendSlotRect）で primary 1 枚に付け替える
 */

import {
	type Layout,
	type ResolvedMap,
	type Scene,
	cloneLayout,
	resolve,
} from "@chronista-club/creo-ui-layout";
import { layoutEngine } from "./layout-host";

export const APP_SCOPE = "app";

/** data-frame-id と 1:1 の pane id 群（main_area.rs の静的 DOM が SSOT）。
 *  doc 52 §10 wave 0: pp（Board）は app pane を退役し、lane tiling の board pane
 *  （#lane-board、lane-panes.ts）へ移った。lane pane（= lane workbench 全体）の中に board が並ぶ。 */
export const APP_PANE_IDS = ["lane", "runner", "devices", "preview", "empty"] as const;
export type AppPaneId = (typeof APP_PANE_IDS)[number];

// ---------- data: preset Scene 群（旧 scenes.ts の後継） ----------

/** 基本構造 = 全 pane が 1 列ずつ（横並び）。可視性・大小は場（attention）が決める */
function baseStructure(exclude?: AppPaneId) {
	return {
		columns: APP_PANE_IDS.filter((id) => id !== exclude).map((id) => ({ panes: [id] })),
	};
}

/** 場の指定（未指定 pane は 0 = 非表示） */
function fieldOf(values: Partial<Record<AppPaneId, number>>): Record<string, number> {
	const attention: Record<string, number> = {};
	for (const id of APP_PANE_IDS) attention[id] = values[id] ?? 0;
	return attention;
}

/** 単独 focus（旧 generateFocusScene）: 対象が独占、他は非表示 */
function focusLayout(id: AppPaneId): Layout {
	return { structure: baseStructure(), attention: fieldOf({ [id]: 1 }) };
}

export const APP_SCENES: readonly Scene[] = [
	{
		id: "lane-focus",
		name: "Lead Focus",
		description: "集中 coding — lane workbench（Conversation）独占",
		layout: focusLayout("lane"),
	},
	// kind → `${pane}-focus` bridge（entry.tsx）が使う単独 focus 群。pp は退役（doc 52 §10
	// wave 0 — board は lane workbench 内の pane に移り、app scene の関心事から外れた）。
	{ id: "runner-focus", name: "runner Focus", description: "Runner 単独", layout: focusLayout("runner") },
	{ id: "devices-focus", name: "Devices Focus", description: "Devices 単独", layout: focusLayout("devices") },
	{
		id: "preview-focus",
		name: "Preview Focus",
		description: "Preview 単独",
		layout: focusLayout("preview"),
	},
	{
		id: "empty",
		name: "Empty",
		description: "何も選択していない — placeholder のみ",
		layout: focusLayout("empty"),
	},
];

const APP_SCENE_BY_ID = new Map(APP_SCENES.map((s) => [s.id, s]));

/** Ctrl+Shift+]/[ で巡る preset（doc 52 §10 wave 0: pp scene 退役後は 4 つの agent focus を巡る。
 *  empty は巡回に入れない）。lane-focus = lane workbench（board も console も chat もこの中の tiling）。 */
export const PRESET_CYCLE = ["lane-focus", "runner-focus", "devices-focus", "preview-focus"] as const;

// ---------- calculations ----------

/**
 * primary pane = 「手前にいる主役」（旧 renderer の z 最大の後継）。
 * float は tiled より常に手前（z の意味論）、同格は後勝ち = resolve の出力順で
 * 後の pane（旧実装の DOM 後勝ちと同じ結果）。
 */
export function primaryAppPane(resolved: ResolvedMap): string | null {
	let best: string | null = null;
	let bestKey = Number.NEGATIVE_INFINITY;
	for (const [id, p] of Object.entries(resolved)) {
		if (p.rect.w <= 0 || p.rect.h <= 0) continue;
		const key = (p.floating ? 10 : 0) + p.attention;
		if (key >= bestKey) {
			best = id;
			bestKey = key;
		}
	}
	return best;
}

// ---------- actions: scene 適用 / lane 別記憶 / DOM 反映 ----------

/** 今の preset id（cycle の現在位置）。lane recall 後は復元した lane の記憶に従う */
let currentSceneId: string | null = null;

export function currentAppSceneId(): string | null {
	return currentSceneId;
}

/** scene 適用の共通経路（preset / lane recall 両方が通る）。 */
function applySceneToEngine(scene: Scene): void {
	layoutEngine.applyScene(APP_SCOPE, scene);
}

/**
 * AI（MCP layout_set、doc 49 LE-P4 PR3）からの直接適用。jump — CSS transition が
 * 視覚を均す（scrub / driver は app scope 未導入、冒頭 doc）。author="ai" が settle
 * 監査に残る。preset 外の形になるので cycle の現在位置はリセットする。
 * AI の明示配置は agent 訪問を終える（訪問中の場を無関係な出発点で上書きしない —
 * 未終了だと後続の ✕ / lane 切替が古い beforeVisit で AI の配置を握り潰す）。
 */
export function applyAppLayoutFromAi(next: Layout): void {
	layoutEngine.update(APP_SCOPE, () => cloneLayout(next));
	layoutEngine.settle(APP_SCOPE, "ai");
	currentSceneId = null;
	transientVisit = false;
	beforeVisit = null;
}

/** preset を適用する（author = "scene" で settle log に刻まれる）。未知 id は false */
export function applyAppScene(id: string): boolean {
	// 明示の scene 選択（hotkey / cycle / empty 等）は agent 訪問を終える
	transientVisit = false;
	const scene = APP_SCENE_BY_ID.get(id);
	if (!scene) {
		console.warn(`[app-panes] unknown scene: ${id}`);
		return false;
	}
	applySceneToEngine(scene);
	currentSceneId = id;
	return true;
}

// ---------- stand pane の「訪問」（sidebar click の一時 view、2026-07-23 dogfood） ----------
// sidebar から board/runner/Devices を開くのは「ちょっと見る」訪問であって workspace の形の
// 選択ではない — 訪問を lane の配置記憶に焼き込むと、lane を行き来しても agent 画面が
// 出っ放しになり console に戻る口が hotkey しかなくなる（Devices 可視化で表面化した
// 新旧共通の UX ギャップ）。訪問は出発点を覚え、✕（close-pane）で戻る。

let transientVisit = false;
let beforeVisit: { layout: Layout; sceneId: string | null } | null = null;

/**
 * stand pane を訪問する（bridge の kind≠terminal 経路用）。
 * 訪問の入れ子（Devices → runner）は最初の出発点を保つ。
 */
export function visitAppPane(paneId: string): boolean {
	if (!transientVisit) {
		beforeVisit = {
			layout: cloneLayout(layoutEngine.current(APP_SCOPE)),
			sceneId: currentSceneId,
		};
	}
	const ok = applyAppScene(`${paneId}-focus`);
	transientVisit = ok;
	return ok;
}

/** 訪問を閉じて出発点の配置へ戻る（✕ ボタン）。訪問中でなければ lane-focus に倒す */
export function closeAppPaneVisit(): void {
	if (transientVisit && beforeVisit) {
		applySceneToEngine({
			id: "visit-return",
			name: "Visit return",
			layout: beforeVisit.layout,
		});
		currentSceneId = beforeVisit.sceneId;
		transientVisit = false;
		beforeVisit = null;
		return;
	}
	transientVisit = false;
	beforeVisit = null;
	applyAppScene("lane-focus");
}

/** preset の cyclic 切替（direction = 1 で next、-1 で prev） */
export function cycleAppScene(direction: 1 | -1): boolean {
	const idx = currentSceneId ? (PRESET_CYCLE as readonly string[]).indexOf(currentSceneId) : -1;
	const next = PRESET_CYCLE[(idx + direction + PRESET_CYCLE.length) % PRESET_CYCLE.length];
	return applyAppScene(next);
}

/** app scope に一度でも配置が適用されたか（boot 前の auto-open 暴発 guard） */
export function appLayoutReady(): boolean {
	return layoutEngine.history(APP_SCOPE).length > 0;
}

/** pane が今見えているか（面積 > 0）。board-handler の auto-open 判定用 */
export function isAppPaneVisible(id: string): boolean {
	const p = layoutEngine.resolved(APP_SCOPE)[id];
	return !!p && p.rect.w > 0 && p.rect.h > 0;
}

/**
 * lane 別の app 配置の記憶（旧 laneScenes の後継）。
 * Scene id でなく**場の snapshot** を覚える — user が share を調整した形もそのまま蘇る。
 */
const laneStates = new Map<string, { layout: Layout; sceneId: string | null }>();

/** lane を離れる時に呼ぶ。「empty が主役」（何も選択していない）の形は覚えない */
export function saveAppStateFor(lane: string): void {
	// agent 訪問中は**出発点**の形を覚える — 一時 view を lane の記憶に焼き込まない
	if (transientVisit && beforeVisit) {
		const primary = primaryAppPane(resolve(beforeVisit.layout));
		if (primary === null || primary === "empty") return;
		laneStates.set(lane, {
			layout: cloneLayout(beforeVisit.layout),
			sceneId: beforeVisit.sceneId,
		});
		return;
	}
	const primary = primaryAppPane(layoutEngine.resolved(APP_SCOPE));
	if (primary === null || primary === "empty") return;
	laneStates.set(lane, {
		layout: cloneLayout(layoutEngine.current(APP_SCOPE)),
		sceneId: currentSceneId,
	});
}

/** lane に入る時に呼ぶ。初訪問は lane-focus（旧 default と同じ）。agent 訪問は終わる */
export function restoreAppStateFor(lane: string): void {
	transientVisit = false;
	const saved = laneStates.get(lane);
	if (!saved) {
		applyAppScene("lane-focus");
		return;
	}
	// 復元は Scene の total recall と同じ意味論（author = "scene" が監査に刻まれる）
	applySceneToEngine({ id: `lane:${lane}`, name: `Lane ${lane}`, layout: saved.layout });
	currentSceneId = saved.sceneId;
}

/** テスト用: module 状態を初期化する（engine の scope は上書き apply で足りる） */
export function _resetForTest(): void {
	currentSceneId = null;
	laneStates.clear();
	transientVisit = false;
	beforeVisit = null;
}

/** resolved に居ない pane の投影（非表示扱い） */
const ABSENT = { rect: { x: 0, y: 0, w: 0, h: 0 }, attention: 0, floating: false } as const;

/**
 * engine 購読 → 既存 `.pane[data-frame-id]` 要素への inline style 書き込み（純 action）。
 * 返り値は unsubscribe。
 */
export function installAppPanes(root: ParentNode = document): () => void {
	return layoutEngine.subscribe((scope, resolved) => {
		if (scope === APP_SCOPE) renderAppPanes(root, resolved);
	});
}

function renderAppPanes(root: ParentNode, resolved: ResolvedMap): void {
	const primary = primaryAppPane(resolved);
	const elements = root.querySelectorAll<HTMLElement>(".pane[data-frame-id]");
	elements.forEach((el) => {
		const id = el.dataset.frameId;
		if (!id) return;
		const p = resolved[id] ?? ABSENT;
		const visible = p.rect.w > 0 && p.rect.h > 0;
		// 投影規則: 非表示は「全面サイズ + 透明」（xterm の fit を 0×0 で壊さない — 冒頭 doc）
		el.style.left = visible ? `${p.rect.x * 100}%` : "0%";
		el.style.top = visible ? `${p.rect.y * 100}%` : "0%";
		el.style.width = visible ? `${p.rect.w * 100}%` : "100%";
		el.style.height = visible ? `${p.rect.h * 100}%` : "100%";
		el.style.opacity = visible ? "1" : "0";
		el.style.pointerEvents = visible ? "auto" : "none";
		// ⚠️ visibility は opacity と別に必須（2026-07-24 実測の根治）: opacity:0 +
		// pointer-events:none でも、pane 内の iframe（#preview-frame / board の sandbox）は
		// WebKit の **compositor 側 scroll hit-test に残り**、main area 上の wheel を
		// 空の iframe に吸い込む（macOS 26.5 で顕在化 — click は main-thread 判定で
		// 素通りするため「wheel だけ死ぬ」）。visibility:hidden は両スレッドの
		// hit-test から外れ、layout は保たれるので xterm の fit も壊さない。
		el.style.visibility = visible ? "visible" : "hidden";
		el.style.zIndex = p.floating ? String(100 + Math.round(p.attention * 100)) : "0";
		el.classList.toggle("active", id === primary);
	});
}
