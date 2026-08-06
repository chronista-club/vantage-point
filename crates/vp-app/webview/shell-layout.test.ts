/**
 * `shell-layout.ts` の純関数（幅の clamp と drag の計算）。
 *
 * ## ⚠️ ここで守るのは「両端が揃っていること」
 *
 * shell の形は **3 つの module に分かれて住んでいる**:
 *
 * - 幅と永続化 … `shell-layout.ts`（main bundle、SSOT）
 * - 形 full/slim … `src/sidebar/form.ts`（sidebar bundle）
 * - R の開閉 … `right-sidebar.ts`（main bundle）
 *
 * bundle が別なので import では繋げず、`document` の CustomEvent bus で伝える。
 * **片側だけ書くと無音で壊れる** — 実際 2026-08-06 に「受け口だけ書いて送り手を書かない」を
 * やり、⌘] で R を開いても取っ手が出ない（= 掴めない）になった。下の bus test はその再発net。
 */
import { describe, expect, it } from "vitest";
import {
	RIGHT_MAX,
	RIGHT_MIN,
	SIDEBAR_MAX,
	SIDEBAR_MIN,
	clampWidth,
	nextWidth,
} from "./shell-layout";

describe("clampWidth", () => {
	it("範囲に丸める（⚠️ Rust 側の clamp と同じ端）", () => {
		expect(clampWidth(0, SIDEBAR_MIN, SIDEBAR_MAX)).toBe(SIDEBAR_MIN);
		expect(clampWidth(9999, SIDEBAR_MIN, SIDEBAR_MAX)).toBe(SIDEBAR_MAX);
		expect(clampWidth(320, SIDEBAR_MIN, SIDEBAR_MAX)).toBe(320);
	});

	it("⚠️ NaN / 無限大でも掴める値に倒す（幅 0 の sidebar を作らない）", () => {
		expect(clampWidth(Number.NaN, SIDEBAR_MIN, SIDEBAR_MAX)).toBe(SIDEBAR_MIN);
		expect(clampWidth(Number.POSITIVE_INFINITY, SIDEBAR_MIN, SIDEBAR_MAX)).toBe(SIDEBAR_MIN);
	});

	it("整数に丸める（px の端数を持ち回らない）", () => {
		expect(clampWidth(320.4, SIDEBAR_MIN, SIDEBAR_MAX)).toBe(320);
	});
});

describe("nextWidth（drag 中の幅）", () => {
	const shell = { left: 0, right: 1600 };

	it("L は境界の x がそのまま幅", () => {
		expect(nextWidth("left", 300, shell)).toBe(300);
	});

	it("⚠️ R は符号が反転する（窓の右端から境界までの距離）", () => {
		// 右端 1600 で pointer が 1200 → R の幅は 400
		expect(nextWidth("right", 1200, shell)).toBe(400);
	});

	it("shell が画面左端に無くても L が正しい（left を引く）", () => {
		expect(nextWidth("left", 500, { left: 100, right: 1600 })).toBe(400);
	});

	it("それぞれの範囲で頭打ちになる", () => {
		expect(nextWidth("left", 10_000, shell)).toBe(SIDEBAR_MAX);
		expect(nextWidth("left", -10, shell)).toBe(SIDEBAR_MIN);
		expect(nextWidth("right", -10_000, shell)).toBe(RIGHT_MAX);
		expect(nextWidth("right", 10_000, shell)).toBe(RIGHT_MIN);
	});
});

/**
 * ⚠️ **shell の CustomEvent bus は、送り手と受け口が必ず対で存在する**。
 *
 * bundle をまたぐので TypeScript の型では繋がらず、片側だけ書いても**コンパイルは通る**。
 * 2026-08-06 に実際に「受け口だけ書いて送り手を書かない」をやり、⌘] で R を開いても
 * 取っ手が出ない（掴めない）になった。source を読んで対の存在を固定する。
 */
describe("shell の CustomEvent bus（送り手と受け口の対）", async () => {
	const read = async (p: string) =>
		await (await import("node:fs/promises")).readFile(new URL(p, import.meta.url), "utf8");

	const sources = Object.fromEntries(
		await Promise.all(
			["./shell-layout.ts", "./right-sidebar.ts", "./src/sidebar/form.ts"].map(
				async (f) => [f, await read(f)] as const,
			),
		),
	);
	const all = Object.values(sources).join("\n");

	for (const ev of [
		// 形が変わった（form.ts → shell-layout.ts）
		"vp:sidebar-form",
		// R の開閉が変わった（right-sidebar.ts → shell-layout.ts）
		"vp:right-sidebar-state",
		// 復元（shell-layout.ts → form.ts / right-sidebar.ts）
		"vp:shell-restore",
	]) {
		it(`${ev} は dispatch と addEventListener が両方ある`, () => {
			expect(all, `${ev} の送り手が無い`).toContain(`CustomEvent("${ev}"`);
			expect(all, `${ev} の受け口が無い`).toContain(`addEventListener("${ev}"`);
		});
	}
});
