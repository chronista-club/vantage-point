/**
 * pane kind registry（doc 87 §4 / §8.2）— center の種類を 1 枚の表から導く。
 *
 * ## ⚠️ ここで守るのは「1 行足せば全部に出る」こと（PL §5.2 の open-closed）
 *
 * 旧形は同じ pane の顔ぶれを別々の表に書いていた: `APP_PANE_IDS`（配置の住所）/ `APP_SCENES`（場）/
 * `PRESET_CYCLE`（巡回）/ `keybindings.ts` の数字キー / `entry.tsx` の kind → 住所 / `active-pane.ts` の
 * kind → DOM id。どれかを直し忘れると片肺の状態が作れた（旧体系では devices が kind の表にも場にも無く、
 * Devices を選んでも empty に落ちていた — LE-P4 PR1 で両方に補充した）。表を 1 枚にし、導出は純関数に
 * して、表に無い pane を誰も知らない形にする。
 *
 * 静的 DOM（main_area.rs）との 1:1 は `shell-registries.dom.test.ts`。
 */
import { resolve } from "@chronista-club/creo-ui-layout";
import { describe, expect, it } from "vitest";
import {
	APP_PANE_REGISTRY,
	type AppPaneDef,
	EMPTY_APP_PANE,
	appPaneById,
	appPaneIdsOf,
	appPaneOfKind,
	appScenesOf,
	presetCycleOf,
	sceneHotkeysOf,
} from "./app-pane-registry";

/** 表に 1 行足した registry（拡張の契約を確かめる仮の pane）。 */
const GALLERY: AppPaneDef = {
	id: "gallery",
	domId: "pane-gallery",
	kinds: ["gallery"],
	scene: { id: "gallery-focus", name: "Gallery Focus", description: "Gallery 単独" },
	inCycle: true,
	order: 3.5,
};
const extended = (): readonly AppPaneDef[] => [...APP_PANE_REGISTRY, GALLERY];

describe("表そのもの", () => {
	it("並びは lane / runner / devices / preview / empty（同格の主役は後勝ちなので並びが意味を持つ）", () => {
		expect(appPaneIdsOf(APP_PANE_REGISTRY)).toEqual(["lane", "runner", "devices", "preview", "empty"]);
	});

	it("id / domId / scene id / kind はどれも表の中で重複しない（解決が 1 つに決まる）", () => {
		const all = (pick: (p: AppPaneDef) => readonly string[]) => APP_PANE_REGISTRY.flatMap(pick);
		for (const values of [
			all((p) => [p.id]),
			all((p) => [p.domId]),
			all((p) => [p.scene.id]),
			all((p) => p.kinds),
			all((p) => [String(p.order)]),
		]) {
			expect(new Set(values).size, values.join(",")).toBe(values.length);
		}
	});

	it("各 pane の先頭の kind は自分の id（= 静的 DOM の data-kind）", () => {
		for (const p of APP_PANE_REGISTRY) expect(p.kinds[0], p.id).toBe(p.id);
	});

	it("巡回に入らないのは empty だけ", () => {
		expect(APP_PANE_REGISTRY.filter((p) => !p.inCycle).map((p) => p.id)).toEqual(["empty"]);
	});

	it("EMPTY_APP_PANE は表の empty 行そのもの（kind 不明の落ち先）", () => {
		expect(EMPTY_APP_PANE).toBe(appPaneById(APP_PANE_REGISTRY, "empty"));
		expect(EMPTY_APP_PANE.domId).toBe("pane-empty");
	});
});

describe("kind → pane の解決", () => {
	it("表の kind は全部、自分の pane に解決する", () => {
		for (const p of APP_PANE_REGISTRY) {
			for (const kind of p.kinds) expect(appPaneOfKind(APP_PANE_REGISTRY, kind)?.id, kind).toBe(p.id);
		}
	});

	it("旧名 terminal は lane の別名（Rust は lane には kind = lane しか送らないが、旧い呼び手を empty に落とさない）", () => {
		const lane = appPaneOfKind(APP_PANE_REGISTRY, "terminal");
		expect(lane?.id).toBe("lane");
		expect(lane?.domId).toBe("pane-lane");
	});

	it("表に無い kind（退役した board 等）と空は undefined — 落ち先は呼び手が決める", () => {
		expect(appPaneOfKind(APP_PANE_REGISTRY, "board")).toBeUndefined();
		expect(appPaneOfKind(APP_PANE_REGISTRY, "")).toBeUndefined();
		expect(appPaneOfKind(APP_PANE_REGISTRY, null)).toBeUndefined();
		expect(appPaneOfKind(APP_PANE_REGISTRY, undefined)).toBeUndefined();
	});
});

describe("場（Scene）の導出", () => {
	it("pane ごとに 1 つ、その pane が独占して他は面積 0 の場ができる", () => {
		const scenes = appScenesOf(APP_PANE_REGISTRY);
		expect(scenes.map((s) => s.id)).toEqual(APP_PANE_REGISTRY.map((p) => p.scene.id));
		for (const p of APP_PANE_REGISTRY) {
			const scene = scenes.find((s) => s.id === p.scene.id)!;
			expect(scene.name).toBe(p.scene.name);
			expect(scene.description).toBe(p.scene.description);
			const r = resolve(scene.layout);
			expect(r[p.id]?.rect, p.id).toEqual({ x: 0, y: 0, w: 1, h: 1 });
			for (const other of APP_PANE_REGISTRY) {
				if (other.id === p.id) continue;
				const rect = r[other.id]?.rect;
				expect((rect?.w ?? 0) * (rect?.h ?? 0), `${p.id} の場で ${other.id} が見える`).toBe(0);
			}
		}
	});

	it("構造は全 pane が 1 列ずつ（可視性と大小は attention が決める）", () => {
		const [scene] = appScenesOf(APP_PANE_REGISTRY);
		expect(scene.layout.structure).toEqual({
			columns: APP_PANE_REGISTRY.map((p) => ({ panes: [p.id] })),
		});
	});

	it("⚠️ 場ごとに別の構造 object（engine が 1 つの場を書き換えても他の場が巻き込まれない）", () => {
		const [a, b] = appScenesOf(APP_PANE_REGISTRY);
		expect(a.layout.structure).not.toBe(b.layout.structure);
	});

	it("巡回は inCycle の場を表の並びで（旧 PRESET_CYCLE と同じ 4 つ）", () => {
		expect(presetCycleOf(APP_PANE_REGISTRY)).toEqual([
			"lane-focus",
			"runner-focus",
			"devices-focus",
			"preview-focus",
		]);
	});

	it("Ctrl+Shift+数字 は巡回の順に 1 から（旧 keybindings の表と同じ写像）", () => {
		expect(sceneHotkeysOf(presetCycleOf(APP_PANE_REGISTRY))).toEqual({
			Digit1: "lane-focus",
			Digit2: "runner-focus",
			Digit3: "devices-focus",
			Digit4: "preview-focus",
		});
	});

	it("数字キーは 9 まで（10 個目以降の場は巡回だけで届く）", () => {
		const cycle = Array.from({ length: 11 }, (_, i) => `s${i}`);
		const keys = Object.keys(sceneHotkeysOf(cycle));
		expect(keys).toHaveLength(9);
		expect(keys.at(-1)).toBe("Digit9");
	});
});

describe("open-closed — 表に 1 行足すと、場・巡回・kind の解決に出る（PL §5.2）", () => {
	it("足した pane の独占の場ができ、既存の場の構造にも列が増える", () => {
		const scenes = appScenesOf(extended());
		const gallery = scenes.find((s) => s.id === "gallery-focus");
		expect(gallery).toBeDefined();
		expect(resolve(gallery!.layout).gallery?.rect).toEqual({ x: 0, y: 0, w: 1, h: 1 });
		const lane = scenes.find((s) => s.id === "lane-focus")!;
		expect(lane.layout.structure.columns.flatMap((c) => c.panes)).toContain("gallery");
		expect(lane.layout.attention.gallery).toBe(0);
	});

	it("order の位置に並ぶ（preview の後、empty の前）", () => {
		expect(appPaneIdsOf(extended())).toEqual(["lane", "runner", "devices", "preview", "gallery", "empty"]);
		expect(presetCycleOf(extended()).at(-1)).toBe("gallery-focus");
	});

	it("kind が解決し、DOM id も引ける", () => {
		expect(appPaneOfKind(extended(), "gallery")?.domId).toBe("pane-gallery");
	});

	it("inCycle: false なら場はできるが巡回と数字キーには出ない", () => {
		const quiet = [...APP_PANE_REGISTRY, { ...GALLERY, inCycle: false }];
		expect(appScenesOf(quiet).map((s) => s.id)).toContain("gallery-focus");
		expect(presetCycleOf(quiet)).not.toContain("gallery-focus");
		expect(Object.values(sceneHotkeysOf(presetCycleOf(quiet)))).not.toContain("gallery-focus");
	});
});
