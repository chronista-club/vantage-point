/**
 * board-view の calculations（doc 55: 投影と表示所有権）。
 * 状態機械（toggleOpen / toggleForm）と float 幾何（初回既定値 / clamp / 移動 / リサイズ）を固定する。
 */
import { describe, expect, it } from "vitest";
import {
	DEFAULT_BOARD_VIEW,
	FLOAT_MIN_H,
	FLOAT_MIN_W,
	clampRect,
	initialFloatRect,
	moveRect,
	resizeRect,
	toggleForm,
	toggleOpen,
} from "./board-view";

const WB = { w: 1500, h: 900 };

describe("toggleOpen / toggleForm（doc 55 §7 — 2 動詞 × 各 1 操作）", () => {
	it("開閉 toggle は form を保つ（閉→開は前回の form のまま）", () => {
		const opened = toggleOpen(DEFAULT_BOARD_VIEW);
		expect(opened.open).toBe(true);
		expect(opened.form).toBe("float");
		expect(toggleOpen(opened).open).toBe(false);
	});

	it("開いていれば form toggle は float ⇄ docked（open は保つ）", () => {
		const s = { ...DEFAULT_BOARD_VIEW, open: true };
		const docked = toggleForm(s);
		expect(docked).toMatchObject({ open: true, form: "docked" });
		expect(toggleForm(docked)).toMatchObject({ open: true, form: "float" });
	});

	it("閉時の form toggle は「切替先の form で開く」に化ける（doc 55 §7）", () => {
		// 閉 × float で N → docked で開く（B = 前回のまま開く、と役割が分かれる）
		expect(toggleForm(DEFAULT_BOARD_VIEW)).toMatchObject({
			open: true,
			form: "docked",
		});
		expect(
			toggleForm({ open: false, form: "docked", rect: null }),
		).toMatchObject({ open: true, form: "float" });
	});

	it("rect（記憶）は toggle で壊れない", () => {
		const rect = { x: 10, y: 20, w: 400, h: 600 };
		const s = { open: true, form: "float" as const, rect };
		expect(toggleOpen(s).rect).toBe(rect);
		expect(toggleForm(s).rect).toBe(rect);
	});
});

describe("initialFloatRect（doc 55 §9 — 縦 92% / 横 = 縦 × 1/√2 A4 比率 / 右寄せ）", () => {
	it("縦 = workbench の 92%、横は A4 縦置き比率", () => {
		const r = initialFloatRect(WB);
		expect(r.h).toBe(Math.round(900 * 0.92)); // 828
		expect(r.w).toBe(Math.round(r.h * Math.SQRT1_2)); // ≈ 586
	});

	it("右端に寄り、縦は中央", () => {
		const r = initialFloatRect(WB);
		expect(r.x + r.w).toBeLessThanOrEqual(WB.w);
		expect(r.x + r.w).toBeGreaterThan(WB.w - 20); // margin 12 で右寄せ
		expect(r.y).toBe(Math.round((WB.h - r.h) / 2));
	});

	it("狭い workbench では比率より clamp が勝つ（横に収まる）", () => {
		const r = initialFloatRect({ w: 400, h: 900 });
		expect(r.w).toBeLessThanOrEqual(400);
		expect(r.x).toBeGreaterThanOrEqual(0);
	});
});

describe("clampRect（workbench 内へ収める。記憶は呼び手が保持 = ここは見た目だけ）", () => {
	it("はみ出しは中へ、過大サイズは workbench まで縮む", () => {
		const r = clampRect({ x: 2000, y: -50, w: 3000, h: 2000 }, WB);
		expect(r).toEqual({ x: 0, y: 0, w: WB.w, h: WB.h });
	});

	it("最小サイズを下回らない", () => {
		const r = clampRect({ x: 0, y: 0, w: 10, h: 10 }, WB);
		expect(r.w).toBe(FLOAT_MIN_W);
		expect(r.h).toBe(FLOAT_MIN_H);
	});
});

describe("moveRect / resizeRect（doc 55 §7.1 — 移動・リサイズ）", () => {
	const base = { x: 900, y: 40, w: 500, h: 800 };

	it("移動はサイズ不変で clamp（画面外に迷子にしない）", () => {
		const r = moveRect(base, 5000, -5000, WB);
		expect(r.w).toBe(500);
		expect(r.h).toBe(800);
		expect(r.x).toBe(WB.w - 500);
		expect(r.y).toBe(0);
	});

	it("左辺リサイズは右端 anchor（右端の位置が変わらない）", () => {
		const right = base.x + base.w;
		const wider = resizeRect(base, "w", -100, 0, WB);
		expect(wider.x + wider.w).toBe(right);
		expect(wider.w).toBe(600);
		const narrower = resizeRect(base, "w", 450, 0, WB);
		expect(narrower.x + narrower.w).toBe(right);
		expect(narrower.w).toBe(FLOAT_MIN_W); // 最小で止まる
	});

	it("下辺リサイズは上端 anchor + workbench 下端で止まる", () => {
		const taller = resizeRect(base, "s", 0, 5000, WB);
		expect(taller.y).toBe(base.y);
		expect(taller.y + taller.h).toBeLessThanOrEqual(WB.h);
		const shorter = resizeRect(base, "s", 0, -5000, WB);
		expect(shorter.h).toBe(FLOAT_MIN_H);
	});

	it("角（sw）は両軸が同時に効く", () => {
		const r = resizeRect(base, "sw", -60, 40, WB);
		expect(r.w).toBe(560);
		expect(r.h).toBe(840);
	});
});
