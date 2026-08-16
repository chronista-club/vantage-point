/**
 * `code-view.ts` — code pane の view 状態（純関数部）。
 *
 * ⚠️ board-view と違い float が無いので、状態遷移は open 1 bit だけ。
 * ここが薄いのは設計どおり（「DOM に触らない / 形は 1 つ」を守る限り薄いまま）。
 */
import { describe, expect, it } from "vitest";
import { DEFAULT_CODE_VIEW, toggleOpen } from "./code-view";

describe("toggleOpen", () => {
	it("閉 → 開 → 閉 の往復で恒等", () => {
		const opened = toggleOpen(DEFAULT_CODE_VIEW);
		expect(opened.open).toBe(true);
		expect(toggleOpen(opened)).toEqual(DEFAULT_CODE_VIEW);
	});

	it("既定は閉（表示は user が起こす）", () => {
		expect(DEFAULT_CODE_VIEW.open).toBe(false);
	});
});
