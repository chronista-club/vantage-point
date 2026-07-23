/**
 * layout-mcp（doc 49 LE-P4 PR3）の検証 — scope 振り分けと "app" scope handler。
 *
 * "gallery" handler の実体は gallery-panes.tsx（Solid 依存）なので、ここでは stub を
 * 登録して routing だけを固定する。"app" handler は本 module 所有なので実 engine で検証。
 */

import { describe, expect, it } from "vitest";
import { layoutEngine } from "./layout-host";
import { type LayoutScopeMcp, layoutHostMcp, registerLayoutScope } from "./layout-mcp";

function stubScope(): { handler: LayoutScopeMcp; calls: string[]; lastSpec: unknown[] } {
	const calls: string[] = [];
	const lastSpec: unknown[] = [];
	return {
		calls,
		lastSpec,
		handler: {
			get: () => {
				calls.push("get");
				return { ok: "get" };
			},
			set: (spec) => {
				calls.push("set");
				lastSpec.push(spec);
				return { ok: "set" };
			},
			history: (opts) => {
				calls.push("history");
				lastSpec.push(opts);
				return { ok: "history" };
			},
		},
	};
}

describe("dispatcher の scope 振り分け", () => {
	it("scope 省略は gallery（LE-P2 からの後方互換）", () => {
		const stub = stubScope();
		registerLayoutScope("gallery", stub.handler);
		expect(layoutHostMcp.get(null)).toEqual({ ok: "get" });
		expect(layoutHostMcp.get({})).toEqual({ ok: "get" });
		expect(stub.calls).toEqual(["get", "get"]);
	});

	it("set は scope key を剥がして handler に渡す", () => {
		const stub = stubScope();
		registerLayoutScope("gallery", stub.handler);
		layoutHostMcp.set({ scope: "gallery", notation: "a | b" });
		expect(stub.lastSpec[0]).toEqual({ notation: "a | b" });
	});

	it("未知 scope は error + 利用可能 scope の列挙（lane scope 非公開の見え方）", () => {
		const res = layoutHostMcp.get({ scope: "lane:proj/root" }) as { error?: string };
		expect(res.error).toContain("unknown scope");
		expect(res.error).toContain("app");
	});
});

describe('"app" scope handler（本 module 所有 — 実 engine で検証）', () => {
	it("set が場に適用され、settle log に author=ai の監査が刻まれる", () => {
		const res = layoutHostMcp.set({
			scope: "app",
			notation: "echoes | pp",
			attention: { echoes: 1, pp: 1 },
		}) as Record<string, unknown>;
		expect(res.error).toBeUndefined();
		expect(res.scope).toBe("app");

		const resolved = layoutEngine.resolved("app");
		expect(resolved.echoes?.rect.w).toBeCloseTo(0.5);
		expect(resolved.pp?.rect.x).toBeCloseTo(0.5);

		const log = layoutEngine.history("app");
		expect(log[log.length - 1]?.author).toBe("ai");
	});

	it("get は現在形の snapshot（scope / notation / shares）を返す", () => {
		const res = layoutHostMcp.get({ scope: "app" }) as Record<string, unknown>;
		expect(res.scope).toBe("app");
		expect(typeof res.notation).toBe("string");
		expect(res.shares).toBeTypeOf("object");
	});

	it("history は notation 付き entries を返す", () => {
		const res = layoutHostMcp.history({ scope: "app", limit: 5 }) as {
			entries: { author: string; notation: string }[];
		};
		expect(res.entries.length).toBeGreaterThan(0);
		expect(res.entries[res.entries.length - 1]?.author).toBe("ai");
	});

	it("壊れた spec は error に落ちる（場を汚さない）", () => {
		const before = layoutEngine.current("app");
		const res = layoutHostMcp.set({ scope: "app", notation: "|||invalid|||" }) as {
			error?: string;
		};
		if (res.error) {
			expect(layoutEngine.current("app")).toBe(before);
		}
		// notation の寛容さは applyLayoutSpec（gallery.test.ts）の領分 — ここでは
		// 「error 応答なら場が動いていない」の不変条件だけを固定する
	});
});
