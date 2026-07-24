/**
 * ink.ts の unit tests（対話面、doc 52 §3）
 *
 * DOM を持つ描画本体（installInk）は dogfood で見る領分。ここは calculations 層の純関数
 * （送信一行 / mode routing / 微小ストロークの棄却）を固定する。
 */

import { describe, expect, it } from "vitest";
import { inkRoute, inkSendLine, resolveTooSmall } from "./ink";

describe("inkSendLine", () => {
	it("item id があれば自然文に item <id> を挟む（読み手に規約を要求しない）", () => {
		const line = inkSendLine("d48a11e1", "/tmp/ink/x.png");
		expect(line).toContain("item d48a11e1");
		expect(line).toContain("/tmp/ink/x.png");
		expect(line).toContain("対話面");
		// path と id が両方入る（受け手は画像を見て、必要なら read_board で本文を引く）
		expect(line.startsWith("[対話面]")).toBe(true);
	});

	it("item id が null（空 board）でも path だけで成立する", () => {
		const line = inkSendLine(null, "/tmp/ink/y.png");
		expect(line).not.toContain("item ");
		expect(line).toContain("/tmp/ink/y.png");
	});
});

describe("inkRoute", () => {
	it("chat mode は focused chat session への echoes:submit 相当を返す", () => {
		const r = inkRoute("chat", "vantage-point/root", 3, "L");
		expect(r).toEqual({ kind: "chat", lane: "vantage-point/root", session: 3, prompt: "L" });
	});

	it("tui mode は PTY 直書き（行 + CR）を返す", () => {
		const r = inkRoute("tui", "vantage-point/root", 1, "L");
		expect(r).toEqual({ kind: "tui", lane: "vantage-point/root", data: "L\r" });
	});

	it("tui は session を無視する（console は lane の Act I 一本）", () => {
		const a = inkRoute("tui", "p/root", 1, "hi");
		const b = inkRoute("tui", "p/root", 9, "hi");
		expect(a).toEqual(b);
	});
});

describe("resolveTooSmall", () => {
	it("6px 未満のストロークは誤タップとして棄却", () => {
		expect(resolveTooSmall([10, 10], [12, 11])).toBe(true); // 距離 ~2.2
	});
	it("6px 以上は有効", () => {
		expect(resolveTooSmall([10, 10], [20, 20])).toBe(false); // 距離 ~14
	});
	it("境界（ちょうど 6px）は有効側", () => {
		expect(resolveTooSmall([0, 0], [6, 0])).toBe(false);
	});
});
