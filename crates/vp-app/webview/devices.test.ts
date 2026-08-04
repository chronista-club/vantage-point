/**
 * `devices.ts` の計器表示（艦隊スイッチ）— 純関数だけを固定する。
 *
 * ここで守るのは **「掴んでいない」の 3 つを潰さないこと**。1 つの bool に潰すと、
 * pane を見た人が「ROTO が反応しない」の原因を取り違える:
 *
 * - `released`   = user が `vp midi off` で譲っている → `vp midi on` すべき
 * - `unsupported`= VP に parser が無い機材        → **何もしなくていい**（元々取り合っていない）
 * - `idle`       = 掴めるはずだが今は掴んでいない
 */
import { describe, expect, it } from "vitest";
import { holdBadge, lastTouchLabel, type DeviceSnapshot } from "./devices";

function dev(over: Partial<DeviceSnapshot>): DeviceSnapshot {
	return {
		port_name: "LPD8 mk2",
		has_input: true,
		has_output: true,
		...over,
	};
}

describe("holdBadge", () => {
	it("掴んでいる（listener）", () => {
		const b = holdBadge(dev({ held: true, hold_reason: "listener" }));
		expect(b.state).toBe("held");
		expect(b.label).toBe("掴んでいる");
	});

	it("ROTO は常駐が持っていることを出す（listener とは出所が違う）", () => {
		expect(holdBadge(dev({ held: true, hold_reason: "roto" })).label).toBe(
			"掴んでいる（常駐）",
		);
	});

	it("⚠️ 譲渡中と対応外を混ぜない（user の次の一手が正反対）", () => {
		const released = holdBadge(dev({ held: false, hold_reason: "released" }));
		const unsupported = holdBadge(
			dev({ held: false, hold_reason: "unsupported" }),
		);
		expect(released.label).toBe("譲渡中");
		expect(unsupported.label).toBe("対応外");
		expect(released.state).not.toBe(unsupported.state);
	});

	it("旧 daemon（field 不在）は掴んでいない扱いに倒す", () => {
		expect(holdBadge(dev({})).state).toBe("idle");
	});
});

describe("lastTouchLabel", () => {
	const now = new Date("2026-08-04T12:00:30");

	it("⚠️ 掴んでいない間は伏せる（観測していないので「触っていない」と言えない）", () => {
		// 譲渡中に古い時刻が残っていても、それは「今触られていない」の証拠にならない。
		const d = dev({
			held: false,
			hold_reason: "released",
			last_input_at: "2026-08-04T11:59:00",
		});
		expect(lastTouchLabel(d, now)).toBe("—");
	});

	it("掴んでいて記録が無ければ「まだ」と言う（不明とは違う）", () => {
		expect(lastTouchLabel(dev({ held: true }), now)).toBe("まだ触られていない");
	});

	it("経過で粒度が上がる", () => {
		const at = (s: string) =>
			lastTouchLabel(dev({ held: true, last_input_at: s }), now);
		expect(at("2026-08-04T12:00:29")).toBe("いま");
		expect(at("2026-08-04T12:00:10")).toBe("20 秒前");
		expect(at("2026-08-04T11:58:30")).toBe("2 分前");
		expect(at("2026-08-04T09:00:30")).toBe("3 時間前");
	});
});
