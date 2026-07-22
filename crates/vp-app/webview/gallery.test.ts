// gallery.ts の純 data / 純 calculation テスト（vitest は node 環境なので DOM action は対象外 —
// syncGalleryDom / installGallery は実機 dogfood と Cmd+R ループで確認する）。
import { describe, expect, it } from "vitest";
import {
	GALLERY_CSS,
	GALLERY_HASH,
	STORIES,
	isGalleryHash,
	renderGalleryHtml,
	toggleGalleryHash,
} from "./gallery";

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
	it("id は非空かつ unique（DOM の data-story-id / 将来の hash 下位 path の前提）", () => {
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

describe("renderGalleryHtml", () => {
	it("全 story が section として出力され、案内 header を持つ", () => {
		const html = renderGalleryHtml(STORIES);
		for (const s of STORIES) {
			expect(html).toContain(`data-story-id="${s.id}"`);
			expect(html).toContain(s.title);
		}
		expect(html).toContain("Component Gallery");
		expect(html).toContain("Ctrl+Shift+G");
	});
});
