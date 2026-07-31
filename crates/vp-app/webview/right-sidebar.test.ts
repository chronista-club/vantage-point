/**
 * right-sidebar の純関数（trimToTail = 行 cap）の検証。
 *
 * DOM / IPC を伴う installRightSidebar 本体は実機 dogfood の領分（board-view と同じ扱い）。
 */
import { describe, expect, it } from "vitest";
import { trimToTail } from "./right-sidebar";

describe("trimToTail", () => {
	it("上限以下はそのまま返す", () => {
		const text = "a\nb\nc";
		expect(trimToTail(text, 5)).toBe(text);
	});

	it("超過分を古い側（先頭）から捨てる", () => {
		const text = ["1", "2", "3", "4", "5"].join("\n");
		expect(trimToTail(text, 2)).toBe("4\n5");
	});

	it("末尾改行を 1 行として数える（tail の見た目を変えない）", () => {
		// "a\nb\n" は split で ["a","b",""] — 末尾の空要素も行として残り、
		// join 後も末尾改行が保存される。
		expect(trimToTail("a\nb\n", 2)).toBe("b\n");
	});

	it("ちょうど上限は無変換", () => {
		expect(trimToTail("x\ny", 2)).toBe("x\ny");
	});
});
