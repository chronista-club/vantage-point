/**
 * edge resident registry（doc 87 §4 / §8.2）— lane の右 edge（edge rail）の住人を 1 枚の表から導く。
 *
 * ## ⚠️ ここで守るのは「住人の顔ぶれと並びを知っているのは表だけ」であること
 *
 * 旧形は住人（+ New / しまった pane / board 取っ手 / PR）を main_area.rs の slot と、`entry.tsx` の
 * host の取り出し（→ `EdgeRail.tsx` の mount 引数）と、`entry.tsx` が R sidebar に渡す付け替え対象の列に
 * それぞれ書いていた。付け替え対象の列を直し忘れると、R を開いた時に住人が rail に取り残される
 * （rail は R の間 display:none なので消える）。
 *
 * 静的 DOM（main_area.rs の `#edge-rail`）との 1:1 と、mount / 付け替えの結線は
 * `shell-registries.dom.test.ts`。
 */
import { describe, expect, it } from "vitest";
import {
	EDGE_ANCHORS,
	EDGE_RESIDENT_REGISTRY,
	type EdgeResidentDef,
	edgeResidentsAt,
	edgeResidentsInRailOrder,
	relocatedResidentHostIds,
} from "./edge-residents";

const ids = (rs: readonly EdgeResidentDef[]) => rs.map((r) => r.id);

describe("表そのもの", () => {
	it("起点は上 / 中央 / 下の 3 つ（doc 82 の規約）", () => {
		expect(EDGE_ANCHORS).toEqual(["top", "center", "bottom"]);
	});

	it("上 = + New / 中央 = しまった pane / 下 = board 取っ手 → PR", () => {
		expect(ids(edgeResidentsAt(EDGE_RESIDENT_REGISTRY, "top"))).toEqual(["new"]);
		expect(ids(edgeResidentsAt(EDGE_RESIDENT_REGISTRY, "center"))).toEqual(["stowed"]);
		expect(ids(edgeResidentsAt(EDGE_RESIDENT_REGISTRY, "bottom"))).toEqual(["board", "prs"]);
	});

	it("id と host の DOM id は表の中で重複しない", () => {
		for (const values of [
			EDGE_RESIDENT_REGISTRY.map((r) => r.id),
			EDGE_RESIDENT_REGISTRY.map((r) => r.hostId),
		]) {
			expect(new Set(values).size, values.join(",")).toBe(values.length);
		}
	});

	it("中身を静的 DOM が持つのは board 取っ手だけ（配線は board-view.ts）。他は EdgeRail が描く", () => {
		expect(EDGE_RESIDENT_REGISTRY.filter((r) => r.content === "static").map((r) => r.id)).toEqual([
			"board",
		]);
	});
});

describe("rail の並び（上の起点から下へ）", () => {
	it("起点の順 → 同じ起点の中は order の順", () => {
		expect(ids(edgeResidentsInRailOrder(EDGE_RESIDENT_REGISTRY))).toEqual([
			"new",
			"stowed",
			"board",
			"prs",
		]);
	});

	it("⚠️ order より起点が先に効く（下の住人の order が小さくても上の住人を追い越さない）", () => {
		const reg: EdgeResidentDef[] = [
			{ id: "b", anchor: "bottom", order: 0, hostId: "b", relocateToRightSidebar: true, content: "solid" },
			{ id: "t", anchor: "top", order: 9, hostId: "t", relocateToRightSidebar: true, content: "solid" },
		];
		expect(ids(edgeResidentsInRailOrder(reg))).toEqual(["t", "b"]);
	});
});

describe("R sidebar の Lane 節へ付け替える住人", () => {
	it("+ New / しまった pane / board 取っ手を rail の並びで（PR は R に PR 節そのものがあるので残す）", () => {
		expect(relocatedResidentHostIds(EDGE_RESIDENT_REGISTRY)).toEqual([
			"edge-rail-new-host",
			"edge-rail-stow-host",
			"board-handle",
		]);
	});
});

describe("open-closed — 表に 1 行足すと、起点・並び・付け替えに出る（PL §5.2）", () => {
	const LOG: EdgeResidentDef = {
		id: "log",
		anchor: "center",
		order: 1,
		hostId: "rail-log-host",
		relocateToRightSidebar: true,
		content: "solid",
	};
	const extended = (): readonly EdgeResidentDef[] => [...EDGE_RESIDENT_REGISTRY, LOG];

	it("足した起点の slot に order の位置で並ぶ", () => {
		expect(ids(edgeResidentsAt(extended(), "center"))).toEqual(["stowed", "log"]);
		expect(ids(edgeResidentsInRailOrder(extended()))).toEqual(["new", "stowed", "log", "board", "prs"]);
	});

	it("付け替える住人なら Lane 節の列にも rail の並びで入る", () => {
		expect(relocatedResidentHostIds(extended())).toEqual([
			"edge-rail-new-host",
			"edge-rail-stow-host",
			"rail-log-host",
			"board-handle",
		]);
	});

	it("付け替えない住人なら Lane 節の列には出ない", () => {
		const stay = [...EDGE_RESIDENT_REGISTRY, { ...LOG, relocateToRightSidebar: false }];
		expect(relocatedResidentHostIds(stay)).not.toContain("rail-log-host");
	});
});
