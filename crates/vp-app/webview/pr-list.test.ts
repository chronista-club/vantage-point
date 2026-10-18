/**
 * pr-list の純関数（doc 82）— 応答の検証 / 「この lane の PR」の先頭固定 / 2 段目の語彙 /
 * 取り直しの時刻表（消費者主導の poll）。
 */
import { describe, expect, it } from "vitest";
import {
	branchSlug,
	createPrPoller,
	orderPrs,
	parseLanePrs,
	prMeta,
	type Pr,
} from "./pr-list";

const pr = (number: number, head: string, extra: Partial<Pr> = {}): Pr => ({
	number,
	title: `PR ${number}`,
	url: `https://github.com/o/r/pull/${number}`,
	draft: false,
	head,
	ci: "none",
	review: "none",
	...extra,
});

describe("branchSlug", () => {
	it("段の prefix（spike / exp / wip / review）を外す", () => {
		expect(branchSlug("wip/r-sidebar-prs")).toBe("r-sidebar-prs");
		expect(branchSlug("review/r-sidebar-prs")).toBe("r-sidebar-prs");
		expect(branchSlug("spike/x")).toBe("x");
		expect(branchSlug("exp/x")).toBe("x");
	});
	it("段でない prefix はそのまま（mako/foo は slug ではなく branch 名そのもの）", () => {
		expect(branchSlug("mako/foo")).toBe("mako/foo");
		expect(branchSlug("nightly")).toBe("nightly");
	});
});

describe("orderPrs", () => {
	it("lane の branch と slug が一致する PR を先頭に固定し、残りは元の並び", () => {
		const prs = [pr(3, "review/other"), pr(2, "review/mine"), pr(1, "wip/third")];
		const o = orderPrs(prs, "wip/mine");
		expect(o.pinned?.number).toBe(2);
		expect(o.rest.map((p) => p.number)).toEqual([3, 1]);
	});
	it("branch 不明 / 一致なしなら pinned は null で並びは不変", () => {
		const prs = [pr(3, "a"), pr(2, "b")];
		expect(orderPrs(prs, null)).toEqual({ pinned: null, rest: prs });
		expect(orderPrs(prs, "wip/zzz").pinned).toBeNull();
	});
});

describe("prMeta", () => {
	it("2 段目は draft → CI → review → head の順（無いものは出さない）", () => {
		const m = prMeta(pr(1, "review/x", { draft: true, ci: "fail", review: "approved" }));
		expect(m.map((c) => c.kind)).toEqual(["draft", "ci", "review", "head"]);
		expect(m.find((c) => c.kind === "ci")?.tone).toBe("bad");
		expect(m.find((c) => c.kind === "review")?.tone).toBe("good");
	});
	it("CI なし・review なしは head だけ", () => {
		expect(prMeta(pr(1, "b")).map((c) => c.kind)).toEqual(["head"]);
	});
});

describe("parseLanePrs", () => {
	it("daemon の LanePrs をそのまま受ける", () => {
		const r = parseLanePrs({
			available: true,
			branch: "wip/x",
			prs: [pr(1, "wip/x")],
		});
		expect(r).toEqual({ available: true, branch: "wip/x", prs: [pr(1, "wip/x")], error: null });
	});
	it("未知の語彙は none に倒し、number / url の無い行は捨てる", () => {
		const r = parseLanePrs({
			available: true,
			prs: [
				{ ...pr(1, "a"), ci: "weird", review: 3 },
				{ title: "no number", url: "https://x" },
			],
		});
		expect(r.prs).toHaveLength(1);
		expect(r.prs[0]).toMatchObject({ ci: "none", review: "none" });
	});
	it("形が壊れていれば error 付きの空として扱う（読み込み中のまま止めない）", () => {
		expect(parseLanePrs(null)).toMatchObject({ available: true, prs: [], error: expect.any(String) });
		expect(parseLanePrs({ available: false })).toMatchObject({ available: false, prs: [] });
	});
});

describe("createPrPoller", () => {
	/** 手動で進める時計（setInterval / clearInterval の差し替え）。 */
	function fakeTimers() {
		let next = 1;
		const live = new Map<number, () => void>();
		return {
			set: (fn: () => void, _ms: number) => {
				const id = next++;
				live.set(id, fn);
				return id;
			},
			clear: (id: number) => void live.delete(id),
			tick: () => {
				for (const fn of [...live.values()]) fn();
			},
			count: () => live.size,
		};
	}

	it("開いた時に即取り直し、開いている間は間隔ごと、閉じたら止める", () => {
		const t = fakeTimers();
		const fetched: string[] = [];
		const p = createPrPoller({
			fetch: (lane) => fetched.push(lane),
			intervalMs: 60_000,
			setInterval: t.set,
			clearInterval: t.clear,
		});
		p.update({ active: true, lane: "vp/a" });
		expect(fetched).toEqual(["vp/a"]);
		t.tick();
		expect(fetched).toEqual(["vp/a", "vp/a"]);
		p.update({ active: false, lane: "vp/a" });
		expect(t.count()).toBe(0);
		t.tick();
		expect(fetched).toHaveLength(2);
	});

	it("同じ状態の再通知では撃たない / lane が変われば新 lane で取り直す", () => {
		const t = fakeTimers();
		const fetched: string[] = [];
		const p = createPrPoller({
			fetch: (lane) => fetched.push(lane),
			intervalMs: 60_000,
			setInterval: t.set,
			clearInterval: t.clear,
		});
		p.update({ active: true, lane: "vp/a" });
		p.update({ active: true, lane: "vp/a" });
		expect(fetched).toEqual(["vp/a"]);
		p.update({ active: true, lane: "vp/b" });
		expect(fetched).toEqual(["vp/a", "vp/b"]);
		expect(t.count()).toBe(1);
		t.tick();
		expect(fetched.at(-1)).toBe("vp/b");
	});

	it("lane 不在は active でも撃たない", () => {
		const t = fakeTimers();
		const fetched: string[] = [];
		const p = createPrPoller({
			fetch: (lane) => fetched.push(lane),
			intervalMs: 60_000,
			setInterval: t.set,
			clearInterval: t.clear,
		});
		p.update({ active: true, lane: null });
		expect(fetched).toEqual([]);
		expect(t.count()).toBe(0);
	});
});
