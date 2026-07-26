/**
 * app-panes（doc 49 LE-P4 PR1 → doc 52 §10 wave 0）の検証 — preset data / 純 calculation /
 * scene 適用と lane 記憶。
 *
 * engine は layout-host の module singleton なので、テストは vitest の file 分離に乗り、
 * file 内は `_resetForTest` + applyScene の上書きで状態を作り直す。
 * DOM 反映（installAppPanes / renderAppPanes）は node 環境のため対象外（薄い action 層）。
 *
 * doc 52 §10 wave 0: pp（Board）は app pane を退役（board = lane tiling へ）。旧テストの
 * pp scene（side-review / pp-overlay / pp-focus）依存は撤去し、2 pane 形が要る箇所は AI layout
 * （echoes + runner の 50/50）で組む。float は app scene が使わなくなったのでテスト対象外。
 */

import { type Layout, resolve } from "@chronista-club/creo-ui-layout";
import { beforeEach, describe, expect, it } from "vitest";
import {
	APP_SCENES,
	APP_SCOPE,
	PRESET_CYCLE,
	_resetForTest,
	appLayoutReady,
	applyAppLayoutFromAi,
	applyAppScene,
	closeAppPaneVisit,
	currentAppSceneId,
	cycleAppScene,
	isAppPaneVisible,
	primaryAppPane,
	restoreAppStateFor,
	saveAppStateFor,
	visitAppPane,
} from "./app-panes";
import { layoutEngine } from "./layout-host";

const sceneById = (id: string) => {
	const s = APP_SCENES.find((s) => s.id === id);
	if (!s) throw new Error(`scene not found: ${id}`);
	return s;
};

/** echoes / runner の 50/50 2 pane（旧 side-review の代役 — board scene 退役後に 2 pane 形を作る手段）。 */
const twoPaneLayout = (): Layout => ({
	structure: { columns: [{ panes: ["echoes"] }, { panes: ["runner"] }] },
	attention: { echoes: 1, runner: 1 },
});

// ============================================================================
// 初期状態（このブロックだけは file 先頭で走る必要がある — engine は module singleton）
// ============================================================================

describe("初期状態", () => {
	it("何も apply される前は not-ready + 全 pane 非表示（auto-open 暴発 guard の前提）", () => {
		expect(appLayoutReady()).toBe(false);
		expect(isAppPaneVisible("echoes")).toBe(false);
		expect(currentAppSceneId()).toBeNull();
	});
});

// ============================================================================
// preset data（純 data を resolve に通して形を固定）
// ============================================================================

describe("preset Scene 群", () => {
	it("lead-focus: echoes（lane workbench）独占、他は面積 0", () => {
		const r = resolve(sceneById("lead-focus").layout);
		expect(r.echoes?.rect).toEqual({ x: 0, y: 0, w: 1, h: 1 });
		expect((r.runner?.rect.w ?? 0) * (r.runner?.rect.h ?? 0)).toBe(0);
	});

	it("devices-focus が存在する（旧体系の欠落補充 — devices click が empty に落ちていた回帰 guard）", () => {
		const r = resolve(sceneById("devices-focus").layout);
		expect(r.devices?.rect).toEqual({ x: 0, y: 0, w: 1, h: 1 });
	});

	it("kind bridge が使う focus 群 + empty が揃っている（pp-focus は退役）", () => {
		const ids = APP_SCENES.map((s) => s.id);
		for (const id of ["runner-focus", "devices-focus", "preview-focus", "empty"]) {
			expect(ids).toContain(id);
		}
		expect(ids).not.toContain("pp-focus");
	});

	it("pp scene（side-review / pp-overlay / pp-focus）は退役済み", () => {
		const ids = APP_SCENES.map((s) => s.id);
		expect(ids).not.toContain("side-review");
		expect(ids).not.toContain("pp-overlay");
	});
});

// ============================================================================
// primaryAppPane（純 calculation）
// ============================================================================

describe("primaryAppPane", () => {
	it("同格の tiled は後勝ち（旧 renderer の DOM 後勝ちと同じ結果）", () => {
		// echoes → runner の順で並ぶ 2 pane では、後の runner が主役
		expect(primaryAppPane(resolve(twoPaneLayout()))).toBe("runner");
	});

	it("empty scene では empty が主役", () => {
		expect(primaryAppPane(resolve(sceneById("empty").layout))).toBe("empty");
	});

	it("全 pane 非表示なら null", () => {
		expect(primaryAppPane({})).toBeNull();
	});
});

// ============================================================================
// scene 適用 / cycle / lane 記憶（engine を実駆動）
// ============================================================================

describe("applyAppScene / cycleAppScene", () => {
	beforeEach(() => {
		_resetForTest();
		applyAppScene("lead-focus");
	});

	it("apply で場が入れ替わり、現在 id を追跡する", () => {
		expect(isAppPaneVisible("echoes")).toBe(true);
		expect(isAppPaneVisible("devices")).toBe(false);
		expect(currentAppSceneId()).toBe("lead-focus");
		expect(appLayoutReady()).toBe(true);

		applyAppScene("devices-focus");
		expect(isAppPaneVisible("devices")).toBe(true);
		expect(isAppPaneVisible("echoes")).toBe(false);
	});

	it("未知 id は false + 現状維持", () => {
		expect(applyAppScene("no-such-scene")).toBe(false);
		expect(currentAppSceneId()).toBe("lead-focus");
	});

	it("cycle は preset 4 つを巡回し、末尾から先頭へ巻き戻る", () => {
		cycleAppScene(1);
		expect(currentAppSceneId()).toBe(PRESET_CYCLE[1]);
		applyAppScene(PRESET_CYCLE[PRESET_CYCLE.length - 1]);
		cycleAppScene(1);
		expect(currentAppSceneId()).toBe(PRESET_CYCLE[0]);
		cycleAppScene(-1);
		expect(currentAppSceneId()).toBe(PRESET_CYCLE[PRESET_CYCLE.length - 1]);
	});

	it("非 preset の場（AI layout）からの cycle は先頭側から入り直す", () => {
		applyAppLayoutFromAi(twoPaneLayout());
		cycleAppScene(1);
		expect(currentAppSceneId()).toBe(PRESET_CYCLE[0]);
	});
});

describe("lane 別の配置記憶", () => {
	beforeEach(() => {
		_resetForTest();
		applyAppScene("lead-focus");
	});

	it("save → 別配置 → restore で share 調整込みの形が蘇る", () => {
		// 2 pane の share 調整済みの形（旧 side-review 相当）を AI layout で作る
		applyAppLayoutFromAi(twoPaneLayout());
		saveAppStateFor("proj/root");
		applyAppScene("devices-focus");

		restoreAppStateFor("proj/root");
		expect(layoutEngine.resolved(APP_SCOPE).echoes?.rect.w).toBeCloseTo(0.5);
		expect(layoutEngine.resolved(APP_SCOPE).runner?.rect.w).toBeCloseTo(0.5);
	});

	it("初訪問 lane は lead-focus", () => {
		applyAppScene("devices-focus");
		restoreAppStateFor("proj/performer/new");
		expect(currentAppSceneId()).toBe("lead-focus");
		expect(isAppPaneVisible("echoes")).toBe(true);
	});

	it("empty が主役の形は save されない（復帰先として無意味）", () => {
		applyAppScene("empty");
		saveAppStateFor("proj/root");
		applyAppScene("devices-focus");
		restoreAppStateFor("proj/root");
		expect(currentAppSceneId()).toBe("lead-focus");
	});

	it("restore は settle log に刻まれる（author = scene の監査）", () => {
		applyAppScene("runner-focus");
		saveAppStateFor("proj/root");
		restoreAppStateFor("proj/root");
		const log = layoutEngine.history(APP_SCOPE);
		expect(log[log.length - 1]?.author).toBe("scene");
	});
});

describe("stand pane の訪問（sidebar click の一時 view — 2026-07-23 dogfood）", () => {
	beforeEach(() => {
		_resetForTest();
		// 出発点 = 2 pane の share 調整済みの形（旧 side-review 相当）
		applyAppLayoutFromAi(twoPaneLayout());
	});

	it("visit → ✕ で出発点の配置に戻る（「出っ放しで close できない」の根治）", () => {
		visitAppPane("devices");
		expect(isAppPaneVisible("devices")).toBe(true);
		closeAppPaneVisit();
		expect(layoutEngine.resolved(APP_SCOPE).echoes?.rect.w).toBeCloseTo(0.5);
		expect(layoutEngine.resolved(APP_SCOPE).runner?.rect.w).toBeCloseTo(0.5);
	});

	it("訪問中の lane save は出発点を覚える（agent 画面を記憶に焼き込まない）", () => {
		visitAppPane("devices");
		saveAppStateFor("proj/root");
		applyAppScene("runner-focus");
		restoreAppStateFor("proj/root");
		expect(layoutEngine.resolved(APP_SCOPE).echoes?.rect.w).toBeCloseTo(0.5);
	});

	it("訪問の入れ子（Devices → runner）は最初の出発点を保つ", () => {
		visitAppPane("devices");
		visitAppPane("runner");
		expect(isAppPaneVisible("runner")).toBe(true);
		closeAppPaneVisit();
		expect(layoutEngine.resolved(APP_SCOPE).echoes?.rect.w).toBeCloseTo(0.5);
	});

	it("明示の scene 選択（hotkey）は訪問を終える — 以後の ✕ は lead-focus に倒れる", () => {
		visitAppPane("devices");
		applyAppScene("runner-focus");
		closeAppPaneVisit();
		expect(currentAppSceneId()).toBe("lead-focus");
	});

	it("未訪問時の ✕ は lead-focus（stale な出発点に飛ばない）", () => {
		closeAppPaneVisit();
		expect(currentAppSceneId()).toBe("lead-focus");
	});

	it("AI の layout_set（applyAppLayoutFromAi）は訪問を終える — 古い出発点で AI 配置を握り潰さない", () => {
		visitAppPane("devices");
		applyAppLayoutFromAi(sceneById("runner-focus").layout);
		// 訪問はもう終わっているので、✕ は stale な beforeVisit へ戻さず lead-focus に倒れる
		closeAppPaneVisit();
		expect(currentAppSceneId()).toBe("lead-focus");
	});

	it("訪問中の AI layout_set 後の lane save は AI の配置を覚える（古い出発点を焼き込まない）", () => {
		visitAppPane("devices");
		applyAppLayoutFromAi(sceneById("runner-focus").layout);
		saveAppStateFor("proj/root");
		applyAppScene("devices-focus");
		restoreAppStateFor("proj/root");
		expect(isAppPaneVisible("runner")).toBe(true);
	});
});
