/**
 * fleet — mapping registry（純 calculation）の検証。
 * wire 形（midistage-profiles の serde tag 形式）→ FleetOp の写像を固定する。
 */

import { describe, expect, it } from "vitest";
import { computeFeedback, deviceOf, mapControl } from "./fleet";

describe("deviceOf", () => {
	it("port 名の部分一致で 3 台を識別（Bastet の pattern 規約と同一）", () => {
		expect(deviceOf("ROTO-CONTROL Roto")).toBe("roto");
		expect(deviceOf("X-Touch X-TOUCH")).toBe("xtouch");
		expect(deviceOf("LPD8 mk2")).toBe("lpd8");
		expect(deviceOf("KeyStage 61")).toBeNull();
	});
});

describe("mapControl — ROTO（knob = share、touch = 奪取/settle）", () => {
	it("knob → share（structure 順の pane index 対応、0.95 上限）", () => {
		expect(mapControl("Roto", { type: "knob", index: 2, value: 0.5 })).toEqual({
			op: "share",
			paneIndex: 2,
			share: 0.5,
		});
		expect(mapControl("Roto", { type: "knob", index: 0, value: 1.0 })).toEqual({
			op: "share",
			paneIndex: 0,
			share: 0.95,
		});
	});

	it("knob_touch → touch（source = センサー個体 — 複数同時 touch を区別する鍵）", () => {
		expect(mapControl("Roto", { type: "knob_touch", index: 3, pressed: true })).toEqual({
			op: "touch",
			source: "roto:3",
			pressed: true,
		});
		expect(mapControl("Roto", { type: "knob_touch", index: 3, pressed: false })).toEqual({
			op: "touch",
			source: "roto:3",
			pressed: false,
		});
		// knob 0 と knob 7 の同時 touch は別 source（action 層の Set が「最後の手放し」を判定できる）
		expect(mapControl("Roto", { type: "knob_touch", index: 0, pressed: true })).not.toEqual(
			mapControl("Roto", { type: "knob_touch", index: 7, pressed: true }),
		);
	});

	it("button は gallery の対象外（lane-nav の領分）", () => {
		expect(mapControl("Roto", { type: "button", index: 0, pressed: true })).toBeNull();
	});
});

describe("mapControl — X-Touch（fader 1 = t の hand driver）", () => {
	it("fader index 0 → scrub", () => {
		expect(mapControl("X-Touch", { type: "fader", index: 0, value: 0.42 })).toEqual({
			op: "scrub",
			t: 0.42,
		});
	});

	it("fader_touch index 0 → touch（release = 着地の合図。roto の touch とは別 source）", () => {
		expect(mapControl("X-Touch", { type: "fader_touch", index: 0, pressed: false })).toEqual({
			op: "touch",
			source: "xtouch:fader0",
			pressed: false,
		});
	});

	it("t は 1 本のスカラー — fader 2 番以降は無視", () => {
		expect(mapControl("X-Touch", { type: "fader", index: 1, value: 0.5 })).toBeNull();
		expect(mapControl("X-Touch", { type: "fader_touch", index: 8, pressed: true })).toBeNull();
	});
});

describe("mapControl — LPD8（pad = Scene slot、knob = share）", () => {
	it("pad → slot（velocity > 0 = press / 0 = release）", () => {
		expect(mapControl("LPD8", { type: "pad", index: 5, velocity: 100 })).toEqual({
			op: "pad",
			slot: 5,
			pressed: true,
		});
		expect(mapControl("LPD8", { type: "pad", index: 5, velocity: 0 })).toEqual({
			op: "pad",
			slot: 5,
			pressed: false,
		});
	});

	it("knob CC → share（ROTO と同型）", () => {
		expect(mapControl("LPD8", { type: "knob", index: 7, value: 0.25 })).toEqual({
			op: "share",
			paneIndex: 7,
			share: 0.25,
		});
	});
});

describe("mapControl — 防御", () => {
	it("未知 device / 未知 type / 値欠落は null", () => {
		expect(mapControl("KeyStage", { type: "knob", index: 0, value: 0.5 })).toBeNull();
		expect(mapControl("Roto", { type: "mystery", index: 0 })).toBeNull();
		expect(mapControl("Roto", { type: "knob", index: 0 })).toBeNull(); // value 欠落
	});

	it("範囲外の値は clamp（wire の壊れた値で場を汚さない）", () => {
		expect(mapControl("Roto", { type: "knob", index: 0, value: -1 })).toEqual({
			op: "share",
			paneIndex: 0,
			share: 0,
		});
		expect(mapControl("X-Touch", { type: "fader", index: 0, value: 2 })).toEqual({
			op: "scrub",
			t: 1,
		});
	});
});

describe("computeFeedback — 場 → 機材の投影（LE-19 フィードバック方向）", () => {
	const base = {
		memberOrder: ["a", "b"] as const,
		shares: { a: 0.55, b: 0.45 },
		transitionT: null,
		filledSlots: new Set<number>(),
		touched: new Set<string>(),
	};

	it("knobs = structure 順の member share、pads は 8 slot 全部の占有状態", () => {
		const fb = computeFeedback({ ...base, filledSlots: new Set([0, 3]) });
		expect(fb.knobs).toEqual([
			{ index: 0, value: 0.55 },
			{ index: 1, value: 0.45 },
		]);
		expect(fb.pads).toHaveLength(8);
		expect(fb.pads[0]).toEqual({ index: 0, filled: true });
		expect(fb.pads[1]).toEqual({ index: 1, filled: false });
		expect(fb.pads[3]).toEqual({ index: 3, filled: true });
		expect(fb.fader).toBeNull();
	});

	it("touch 保持中の knob は省く（手とモーターを戦わせない — §9 Touch 中 = 指定）", () => {
		const fb = computeFeedback({ ...base, touched: new Set(["roto:0"]) });
		expect(fb.knobs).toEqual([{ index: 1, value: 0.45 }]);
	});

	it("fader = transition の t。fader touch 中（human が握っている）は null", () => {
		expect(computeFeedback({ ...base, transitionT: 0.42 }).fader).toBe(0.42);
		expect(
			computeFeedback({
				...base,
				transitionT: 0.42,
				touched: new Set(["xtouch:fader0"]),
			}).fader,
		).toBeNull();
	});

	it("member 9 枚以上は knob 8 本に切り詰め、欠落 share は 0", () => {
		const many = Array.from({ length: 10 }, (_, i) => `p${i}`);
		const fb = computeFeedback({ ...base, memberOrder: many, shares: { p0: 0.5 } });
		expect(fb.knobs).toHaveLength(8);
		expect(fb.knobs[0]).toEqual({ index: 0, value: 0.5 });
		expect(fb.knobs[1]).toEqual({ index: 1, value: 0 });
	});
});
