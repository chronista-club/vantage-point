/**
 * directive dispatcher の判定（`directiveKeyOf`）の固定。
 *
 * 特に守るのは **非 Mac での Scene cyclic との棲み分け**: 「Cmd hold」は非 Mac で
 * Ctrl に折り畳まれるため、shift を通すと既存の `Ctrl+Shift+[ / ]`（Scene cyclic、
 * keybindings.ts）が `[` `]` directive と二重発火する（moody-blues 指摘 #2）。
 * Mac のローカル検証では不可視の経路なので、ここで机上固定する。
 */
import { describe, expect, it } from "vitest";
import { directiveKeyOf, type DirectiveKeyInput } from "./chord";

function ev(partial: Partial<DirectiveKeyInput> & { key: string }): DirectiveKeyInput {
	return {
		metaKey: false,
		ctrlKey: false,
		altKey: false,
		shiftKey: false,
		...partial,
	};
}

describe("directiveKeyOf", () => {
	it("Mac: Cmd+letter / Cmd+symbol が directive に解決する", () => {
		expect(directiveKeyOf(ev({ key: "f", metaKey: true }), true)).toBe("f");
		expect(directiveKeyOf(ev({ key: "[", metaKey: true }), true)).toBe("[");
		expect(directiveKeyOf(ev({ key: "]", metaKey: true }), true)).toBe("]");
	});

	it("非 Mac: Ctrl+letter / Ctrl+symbol が directive に解決する", () => {
		expect(directiveKeyOf(ev({ key: "f", ctrlKey: true }), false)).toBe("f");
		expect(directiveKeyOf(ev({ key: "[", ctrlKey: true }), false)).toBe("[");
	});

	it("非 Mac: Ctrl+Shift+[ / ] は Scene cyclic の領域 — directive は発火しない", () => {
		// keybindings.ts の cycleAppScene と二重発火しないことの固定（指摘 #2 の核心）。
		expect(
			directiveKeyOf(ev({ key: "[", ctrlKey: true, shiftKey: true }), false),
		).toBeNull();
		expect(
			directiveKeyOf(ev({ key: "]", ctrlKey: true, shiftKey: true }), false),
		).toBeNull();
	});

	it("shift 併用は全キー reject（table は shift 無し表記しか持たない）", () => {
		expect(
			directiveKeyOf(ev({ key: "F", metaKey: true, shiftKey: true }), true),
		).toBeNull();
		expect(
			directiveKeyOf(ev({ key: "[", metaKey: true, shiftKey: true }), true),
		).toBeNull();
	});

	it("Alt 併用 / modifier 単独 / 未登録キー / 修飾なしは null", () => {
		expect(directiveKeyOf(ev({ key: "f", metaKey: true, altKey: true }), true)).toBeNull();
		expect(directiveKeyOf(ev({ key: "Meta", metaKey: true }), true)).toBeNull();
		expect(directiveKeyOf(ev({ key: "z", metaKey: true }), true)).toBeNull();
		expect(directiveKeyOf(ev({ key: "f" }), true)).toBeNull();
		// Mac では ctrlKey は directive の修飾ではない
		expect(directiveKeyOf(ev({ key: "f", ctrlKey: true }), true)).toBeNull();
	});
});
