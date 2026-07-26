// gallery.ts の純 data / 純 calculation テスト（vitest は node 環境なので DOM action は対象外 —
// gallery-panes.tsx の syncGalleryDom / installGallery / PaneStage は実機 dogfood と
// Cmd+R ループで確認する）。
import { type Layout, parseNotation } from "@chronista-club/creo-ui-layout";
import { describe, expect, it } from "vitest";
import {
	GALLERY_CSS,
	GALLERY_HASH,
	STORIES,
	applyLayoutSpec,
	isGalleryHash,
	layoutNotation,
	layoutSnapshot,
	storyPaneHtml,
	takeRecent,
	toggleGalleryHash,
} from "./gallery";

function layoutOf(
	notation: string,
	attention: Record<string, number>,
	locks?: Record<string, number>,
): Layout {
	return { structure: parseNotation(notation).structure, attention, locks };
}

describe("gallery css", () => {
	it("Editor Mode パネル (#editor-root) を gallery overlay より上に明示する（同時使用が本義）", () => {
		// gallery(500) < editor(600) の順序が崩れると Ctrl+Shift+E / editor_set の UI が
		// 不透明 overlay に塗り潰される — その回帰を固定する
		expect(GALLERY_CSS).toContain("#editor-root{position:relative;z-index:600;}");
		expect(GALLERY_CSS).toContain("z-index:500");
	});
});

describe("gallery hash", () => {
	it("`#gallery` と下位 path を gallery と判定、他は非該当", () => {
		expect(isGalleryHash("#gallery")).toBe(true);
		expect(isGalleryHash("#gallery/sidebar-text-scale")).toBe(true);
		expect(isGalleryHash("")).toBe(false);
		expect(isGalleryHash("#galleryX")).toBe(false);
		expect(isGalleryHash("#editor")).toBe(false);
	});

	it("toggle は gallery ⇄ 素 の往復（往復テストで写像を固定）", () => {
		expect(toggleGalleryHash("")).toBe(GALLERY_HASH);
		expect(toggleGalleryHash(GALLERY_HASH)).toBe("");
		// 往復で元に戻る
		expect(toggleGalleryHash(toggleGalleryHash(""))).toBe("");
	});
});

describe("story registry", () => {
	it("id は非空かつ unique（pane id / 将来の hash 下位 path の前提）", () => {
		const ids = STORIES.map((s) => s.id);
		expect(ids.every((id) => id.length > 0)).toBe(true);
		expect(new Set(ids).size).toBe(ids.length);
	});

	it("bind 済み knob の CSS var を参照している（Editor Mode ⇄ gallery の同居が本義）", () => {
		const all = STORIES.map((s) => s.html).join("");
		for (const cssVar of ["--sb-text-base", "--sb-conn-hitl", "--sb-glow"]) {
			expect(all).toContain(`var(${cssVar})`);
		}
	});
});

describe("storyPaneHtml", () => {
	it("title / note / body を持つ pane 断片を返す（pane 化後の story 中身）", () => {
		for (const s of STORIES) {
			const html = storyPaneHtml(s);
			expect(html).toContain(s.title);
			expect(html).toContain('class="g-body"');
			expect(html).toContain(s.html.trim().slice(0, 20));
		}
	});

	it("note 無し story は g-note を出さない", () => {
		const html = storyPaneHtml({ id: "x", title: "X", html: "<p>x</p>" });
		expect(html).not.toContain("g-note");
	});
});

describe("gallery css — pane 化（LE-P2）", () => {
	it("stage と pane host の骨格 class を持つ", () => {
		expect(GALLERY_CSS).toContain(".gp-stage");
		expect(GALLERY_CSS).toContain(".gp-pane");
	});
});

describe("layout bridge — applyLayoutSpec（LE-P2 PR2、純 calculation）", () => {
	// f は構造非所属 × 正値 = float
	const base = layoutOf("a | b/c", { a: 0.5, b: 0.3, c: 0.2, f: 0.4 });

	it("notation 省略 = 構造・float 維持で attention を部分 overlay（0 = 非表示も可）", () => {
		const next = applyLayoutSpec(base, { attention: { a: 0.8, c: 0 } });
		expect(next.structure).toBe(base.structure);
		expect(next.attention.a).toBe(0.8);
		expect(next.attention.c).toBe(0);
		expect(next.attention.f).toBe(0.4);
		expect(layoutNotation(next)).toBe("a | b/c ~ f");
	});

	it("notation 指定は total — 旧 float は落ち、新規 id は可視平均で入る", () => {
		const next = applyLayoutSpec(base, { notation: "a | nu" });
		expect(layoutNotation(next)).toBe("a | nu");
		expect(next.attention.f).toBeUndefined();
		expect(next.attention.nu).toBeCloseTo((0.5 + 0.3 + 0.2 + 0.4) / 4);
	});

	it("overlay の新 id に正値 = float として現れる（2×2 の非所属 × >0）", () => {
		const next = applyLayoutSpec(base, { attention: { board: 0.3 } });
		expect(layoutNotation(next)).toBe("a | b/c ~ f board");
	});

	it("attention 0 の既存 member は overlay 省略で 0 のまま（蘇生させない — app scope 実弾の回帰）", () => {
		// app scope の常態: 大半の pane が 0。{echoes:1, pp:1} の overlay で他が mean に
		// 蘇生して 6 分割になった実バグ（2026-07-23、LE-P4 実機答え合わせ初弾）を固定する
		const app = layoutOf("echoes | pp | ge | devices", { echoes: 1, pp: 0, ge: 0, devices: 0 });
		const next = applyLayoutSpec(app, { attention: { pp: 1 } });
		expect(next.attention).toEqual({ echoes: 1, pp: 1, ge: 0, devices: 0 });
	});

	it("全零 guard: 全 pane 非表示になる spec / 空 notation は throw", () => {
		expect(() => applyLayoutSpec(layoutOf("a", { a: 1 }), { attention: { a: 0 } })).toThrow(
			/全零/,
		);
		expect(() => applyLayoutSpec(base, { notation: "" })).toThrow(/空の layout/);
	});

	it("locks: null = 維持 / {} = 全消し / 指定 = 全置換", () => {
		const locked = layoutOf("a | b", { a: 1, b: 1 }, { a: 0.3 });
		expect(applyLayoutSpec(locked, {}).locks).toEqual({ a: 0.3 });
		expect(applyLayoutSpec(locked, { locks: null }).locks).toEqual({ a: 0.3 });
		expect(applyLayoutSpec(locked, { locks: {} }).locks).toBeUndefined();
		expect(applyLayoutSpec(locked, { locks: { b: 0.4 } }).locks).toEqual({ b: 0.4 });
	});

	it("不正な overlay 値・記法に使えない新 id は throw（get の直列化を壊さない guard）", () => {
		expect(() => applyLayoutSpec(base, { attention: { a: Number.NaN } })).toThrow(/不正/);
		expect(() => applyLayoutSpec(base, { attention: { "ba d": 1 } })).toThrow(/使えない/);
	});
});

describe("layout bridge — takeRecent", () => {
	it("limit 0 は空（slice(-0) = 全件の JS 罠を封じる）", () => {
		expect(takeRecent([1, 2, 3], 0)).toEqual([]);
		expect(takeRecent([1, 2, 3], -5)).toEqual([]);
		expect(takeRecent([1, 2, 3], Number.NaN)).toEqual([]);
	});

	it("末尾 limit 件（非整数は切り捨て、超過は全件）", () => {
		expect(takeRecent([1, 2, 3, 4], 2)).toEqual([3, 4]);
		expect(takeRecent([1, 2, 3, 4], 2.9)).toEqual([3, 4]);
		expect(takeRecent([1, 2], 10)).toEqual([1, 2]);
	});
});

describe("layout bridge — layoutSnapshot", () => {
	it("scope / notation / attention / locks / shares を持つ", () => {
		const snap = layoutSnapshot("gallery", layoutOf("a | b", { a: 1, b: 1 }, { a: 0.25 }), {
			a: 0.5,
			b: 0.5,
		});
		expect(snap.scope).toBe("gallery");
		expect(snap.notation).toBe("a | b");
		expect(snap.attention).toEqual({ a: 1, b: 1 });
		expect(snap.locks).toEqual({ a: 0.25 });
		expect(snap.shares).toEqual({ a: 0.5, b: 0.5 });
	});
});
