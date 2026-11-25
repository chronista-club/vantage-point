/**
 * app-panes — app 全体の pane 配置を creo-ui-layout の場に写す（doc 49 LE-P4 PR1）。
 *
 * 旧 3 点セット（frame-engine.ts の Scene engine / scenes.ts の preset / renderer.ts の
 * DOM 反映、VP-140）の後継。離散 Scene は attention 連続場の named snapshot になり、
 * 「hidden = attention 0」「maximized = 独占」「overlay = 構造非所属の floating」に写る。
 *
 * ## 層の分離（CLAUDE.md: data / calculations / actions）
 *
 * - **data**: `APP_SCENES` / `PRESET_CYCLE` — pane kind registry（app-pane-registry.ts）から導く
 *   preset の Layout 群と巡回。DOM を知らない
 * - **calculations**: `primaryAppPane` ほか純関数（vitest で固定）
 * - **actions**: `installAppPanes` — engine 購読 → 既存 `.pane[data-app-pane]` への
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
import { APP_PANE_REGISTRY, appPaneById, appScenesOf, presetCycleOf } from "./app-pane-registry";
import { layoutEngine } from "./layout-host";

export const APP_SCOPE = "app";

// ---------- data: 表から導く preset Scene 群（旧 scenes.ts の後継） ----------
// pane の顔ぶれ（配置の住所 / DOM id / kind / 場 / 巡回）は pane kind registry
// （app-pane-registry.ts、doc 87 §8.2）1 枚が持つ。旧 `APP_PANE_IDS` / 直書きの `APP_SCENES` と
// `PRESET_CYCLE` はそこから導く — 表に 1 行足せば場・巡回・数字キー・kind の解決に出る。
// pane の DOM そのものは main_area.rs の静的 HTML にあり、作り直さない（doc 49 R1）。表との 1:1 は
// `shell-registries.dom.test.ts` が固定する。doc 52 §10 wave 0: pp（Board）は app pane を退役し、
// lane tiling の board pane（#lane-board、lane-panes.ts）へ移った。

/** pane ごとの独占の場（構造は全 pane が 1 列ずつ、可視性・大小は attention が決める）。 */
export const APP_SCENES: readonly Scene[] = appScenesOf(APP_PANE_REGISTRY);

const APP_SCENE_BY_ID = new Map(APP_SCENES.map((s) => [s.id, s]));

/** Ctrl+Shift+]/[ で巡る preset（表の inCycle — empty は巡回に入れない）。
 *  lane-focus = lane workbench（board も console も chat もこの中の tiling）。 */
export const PRESET_CYCLE: readonly string[] = presetCycleOf(APP_PANE_REGISTRY);

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

// ---------- agent pane の「訪問」（sidebar click の一時 view、2026-07-23 dogfood） ----------
// sidebar から board/runner/Devices を開くのは「ちょっと見る」訪問であって workspace の形の
// 選択ではない — 訪問を lane の配置記憶に焼き込むと、lane を行き来しても agent 画面が
// 出っ放しになり console に戻る口が hotkey しかなくなる（Devices 可視化で表面化した
// 新旧共通の UX ギャップ）。訪問は出発点を覚え、✕（close-pane）で戻る。

let transientVisit = false;
let beforeVisit: { layout: Layout; sceneId: string | null } | null = null;

/**
 * agent pane を訪問する（entry.tsx の訪問経路 = `activePaneRouteOf` の `visit` 用）。
 * 訪問の入れ子（Devices → runner）は最初の出発点を保つ。
 */
export function visitAppPane(paneId: string): boolean {
	// 場の id は表の scene から引く（旧形は `${paneId}-focus` の命名規約に頼っていた）。
	// 表に無い pane は何も動かさない — 訪問中なら訪問はそのまま（✕ で出発点に戻れる）
	const sceneId = appPaneById(APP_PANE_REGISTRY, paneId)?.scene.id;
	if (!sceneId) {
		console.warn(`[app-panes] unknown pane to visit: ${paneId}`);
		return false;
	}
	if (!transientVisit) {
		beforeVisit = {
			layout: cloneLayout(layoutEngine.current(APP_SCOPE)),
			sceneId: currentSceneId,
		};
	}
	const ok = applyAppScene(sceneId);
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
	const idx = currentSceneId ? PRESET_CYCLE.indexOf(currentSceneId) : -1;
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
 * engine 購読 → 既存 `.pane[data-app-pane]` 要素への inline style 書き込み（純 action）。
 * 返り値は unsubscribe。
 */
export function installAppPanes(root: ParentNode = document): () => void {
	return layoutEngine.subscribe((scope, resolved) => {
		if (scope === APP_SCOPE) renderAppPanes(root, resolved);
	});
}

function renderAppPanes(root: ParentNode, resolved: ResolvedMap): void {
	const primary = primaryAppPane(resolved);
	const elements = root.querySelectorAll<HTMLElement>(".pane[data-app-pane]");
	elements.forEach((el) => {
		const id = el.dataset.appPane;
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
