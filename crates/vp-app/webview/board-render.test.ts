/**
 * `board-render.ts` の html 土台（`boardHtmlPrelude`）。
 *
 * html item は sandbox iframe に隔離されるので親の CSS を継承しない。土台を srcdoc に
 * 注ぐことで **AI は素の semantic HTML だけ**を書けばよくなる（生成コストと保存コストが
 * 同時に下がり、見た目の制御は VP 側に残る）。
 *
 * ここで守るのは 3 点:
 *
 * 1. **class を配らない** — class を配ると AI がそれを使い始め、「検索しやすい素の HTML」
 *    という目的と逆に働く。セレクタは要素だけであること
 * 2. **token をハードコピーしない** — 値は実行時に親から読む。書き写すと creo-tokens.css と
 *    二重管理になり、片方だけ変わった日に無音でずれる
 * 3. **作者の style を壊さない** — 土台は前に置き、後から来る作者の `<style>` が勝つ
 */
import { describe, expect, it } from "vitest";
import { boardHtmlPrelude } from "./board-render";

describe("boardHtmlPrelude（html item の土台）", () => {
	it("⚠️ class セレクタを配らない（要素セレクタだけ）", () => {
		// class を配ると AI が `<div class="...">` を書き始め、素の semantic HTML という
		// 目的から離れていく。`.foo{` の形が 1 つも無いことを固定する。
		const css = boardHtmlPrelude();
		expect(css).not.toMatch(/\.[a-zA-Z][\w-]*\s*\{/);
	});

	it("semantic 要素の既定を持つ（AI が style を書かなくて済む集合）", () => {
		const css = boardHtmlPrelude();
		for (const sel of ["h1", "h2", "p", "code", "pre", "a", "ul", "blockquote", "table", "th,td", "hr"]) {
			expect(css).toContain(sel);
		}
	});

	it("背景と文字色を body に敷く（暗い VP の中に白い紙を出さない）", () => {
		const css = boardHtmlPrelude();
		expect(css).toContain("--color-surface-bg-base");
		expect(css).toContain("--color-text-primary");
	});

	it("⚠️ token の値を書き写さない（参照だけ持つ）", () => {
		// 具体的な色が焼き込まれていたら、creo-tokens.css との二重管理が始まっている。
		// `:root` への注入は実行時（DOM 不在の本 test では空）なので、ここに残るのは
		// `var(--…)` 参照だけであるべき。
		const css = boardHtmlPrelude();
		expect(css).not.toMatch(/#[0-9a-fA-F]{6}\b/);
		expect(css).not.toMatch(/rgb\(/);
	});

	it("style 要素として閉じている（srcdoc に連結して壊れない）", () => {
		const css = boardHtmlPrelude();
		expect(css.startsWith("<style>")).toBe(true);
		expect(css.endsWith("</style>")).toBe(true);
	});

	it("DOM 不在（単体テスト環境）でも落ちない — 土台なしでも HTML 自体は出る", () => {
		// `tokenBlock` は getComputedStyle が無い環境で空を返す。土台の要素既定は残る。
		expect(() => boardHtmlPrelude()).not.toThrow();
		expect(boardHtmlPrelude()).toContain("blockquote");
	});
});
