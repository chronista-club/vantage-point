/**
 * app-panes（doc 49 LE-P4 PR1）の検証 — preset data / 純 calculation / scene 適用と lane 記憶。
 *
 * engine は layout-host の module singleton なので、テストは vitest の file 分離に乗り、
 * file 内は `_resetForTest` + applyScene の上書きで状態を作り直す。
 * DOM 反映（installAppPanes / renderAppPanes）は node 環境のため対象外（薄い action 層）。
 */

import { resolve } from "@chronista-club/creo-ui-layout";
import { beforeEach, describe, expect, it } from "vitest";
import {
	APP_SCENES,
	APP_SCOPE,
	PRESET_CYCLE,
	_resetForTest,
	appLayoutReady,
	applyAppScene,
	currentAppSceneId,
	cycleAppScene,
	isAppPaneVisible,
	primaryAppPane,
	restoreAppStateFor,
	saveAppStateFor,
} from "./app-panes";
import { layoutEngine } from "./layout-host";

const sceneById = (id: string) => {
	const s = APP_SCENES.find((s) => s.id === id);
	if (!s) throw new Error(`scene not found: ${id}`);
	return s;
};

// ============================================================================
// 初期状態（このブロックだけは file 先頭で走る必要がある — engine は module singleton）
// ============================================================================

describe("初期状態", () => {
	it("何も apply される前は not-ready + 全 pane 非表示（auto-open 暴発 guard の前提）", () => {
		expect(appLayoutReady()).toBe(false);
		expect(isAppPaneVisible("pp")).toBe(false);
		expect(currentAppSceneId()).toBeNull();
	});
});

// ============================================================================
// preset data（純 data を resolve に通して形を固定）
// ============================================================================

describe("preset Scene 群", () => {
	it("lead-focus: echoes 独占、他は面積 0", () => {
		const r = resolve(sceneById("lead-focus").layout);
		expect(r.echoes?.rect).toEqual({ x: 0, y: 0, w: 1, h: 1 });
		expect((r.pp?.rect.w ?? 0) * (r.pp?.rect.h ?? 0)).toBe(0);
	});

	it("side-review: echoes / pp が左右 50/50", () => {
		const r = resolve(sceneById("side-review").layout);
		expect(r.echoes?.rect.x).toBe(0);
		expect(r.echoes?.rect.w).toBeCloseTo(0.5);
		expect(r.pp?.rect.x).toBeCloseTo(0.5);
		expect(r.pp?.rect.w).toBeCloseTo(0.5);
	});

	it("pp-overlay: pp は構造非所属の floating、echoes は tiled 全面", () => {
		const r = resolve(sceneById("pp-overlay").layout);
		expect(r.pp?.floating).toBe(true);
		expect(r.echoes?.floating).toBe(false);
		expect(r.echoes?.rect.w).toBe(1);
	});

	it("bs-focus が存在する（旧体系の欠落補充 — bastet click が empty に落ちていた回帰 guard）", () => {
		const r = resolve(sceneById("bs-focus").layout);
		expect(r.bs?.rect).toEqual({ x: 0, y: 0, w: 1, h: 1 });
	});

	it("kind bridge が使う focus 群 + empty が揃っている", () => {
		const ids = APP_SCENES.map((s) => s.id);
		for (const id of ["pp-focus", "ge-focus", "bs-focus", "preview-focus", "empty"]) {
			expect(ids).toContain(id);
		}
	});
});

// ============================================================================
// primaryAppPane（純 calculation）
// ============================================================================

describe("primaryAppPane", () => {
	it("同格の tiled は後勝ち（旧 renderer の DOM 後勝ちと同じ結果）", () => {
		expect(primaryAppPane(resolve(sceneById("side-review").layout))).toBe("pp");
	});

	it("float は tiled より常に手前（z の意味論）", () => {
		expect(primaryAppPane(resolve(sceneById("pp-overlay").layout))).toBe("pp");
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
		expect(isAppPaneVisible("pp")).toBe(false);
		expect(currentAppSceneId()).toBe("lead-focus");
		expect(appLayoutReady()).toBe(true);

		applyAppScene("pp-focus");
		expect(isAppPaneVisible("pp")).toBe(true);
		expect(isAppPaneVisible("echoes")).toBe(false);
	});

	it("未知 id は false + 現状維持", () => {
		expect(applyAppScene("no-such-scene")).toBe(false);
		expect(currentAppSceneId()).toBe("lead-focus");
	});

	it("pp-overlay の float は右上へ置き直される（位置は ephemeral）", () => {
		applyAppScene("pp-overlay");
		const pp = layoutEngine.resolved(APP_SCOPE).pp;
		expect(pp?.floating).toBe(true);
		// x は右端 clamp（1 - w）、y は上端 0.04
		expect(pp?.rect.x).toBeCloseTo(1 - (pp?.rect.w ?? 0));
		expect(pp?.rect.y).toBeCloseTo(0.04);
	});

	it("cycle は preset 4 つを巡回し、末尾から先頭へ巻き戻る", () => {
		cycleAppScene(1);
		expect(currentAppSceneId()).toBe("side-review");
		applyAppScene(PRESET_CYCLE[PRESET_CYCLE.length - 1]);
		cycleAppScene(1);
		expect(currentAppSceneId()).toBe(PRESET_CYCLE[0]);
		cycleAppScene(-1);
		expect(currentAppSceneId()).toBe(PRESET_CYCLE[PRESET_CYCLE.length - 1]);
	});

	it("非 preset（focus scene）からの cycle は先頭側から入り直す", () => {
		applyAppScene("ge-focus");
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
		applyAppScene("side-review");
		saveAppStateFor("proj/root");
		applyAppScene("pp-focus");

		restoreAppStateFor("proj/root");
		expect(layoutEngine.resolved(APP_SCOPE).echoes?.rect.w).toBeCloseTo(0.5);
		expect(currentAppSceneId()).toBe("side-review");
	});

	it("初訪問 lane は lead-focus", () => {
		applyAppScene("pp-focus");
		restoreAppStateFor("proj/performer/new");
		expect(currentAppSceneId()).toBe("lead-focus");
		expect(isAppPaneVisible("echoes")).toBe(true);
	});

	it("empty が主役の形は save されない（復帰先として無意味）", () => {
		applyAppScene("empty");
		saveAppStateFor("proj/root");
		applyAppScene("pp-focus");
		restoreAppStateFor("proj/root");
		expect(currentAppSceneId()).toBe("lead-focus");
	});

	it("pp-overlay の float 位置は他 lane 経由の recall でも右上に戻る（ephemeral prune 対策）", () => {
		applyAppScene("pp-overlay");
		saveAppStateFor("proj/root");
		// pp が非 floating の形を経由すると engine が float 位置を prune する
		restoreAppStateFor("proj/other");
		restoreAppStateFor("proj/root");
		const pp = layoutEngine.resolved(APP_SCOPE).pp;
		expect(pp?.floating).toBe(true);
		expect(pp?.rect.x).toBeCloseTo(1 - (pp?.rect.w ?? 0));
		expect(pp?.rect.y).toBeCloseTo(0.04);
	});

	it("restore は settle log に刻まれる（author = scene の監査）", () => {
		applyAppScene("side-review");
		saveAppStateFor("proj/root");
		restoreAppStateFor("proj/root");
		const log = layoutEngine.history(APP_SCOPE);
		expect(log[log.length - 1]?.author).toBe("scene");
	});
});
