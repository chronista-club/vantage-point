/** now-line 鮮度（quiet N分）の境界 — 閾値・単位繰り上げ・不明値の沈黙。 */
import { describe, expect, it } from "vitest";
import { QUIET_AFTER_MS, quietLabel } from "./activity-freshness";

describe("quietLabel (activity-freshness)", () => {
	const NOW = 1_756_300_000_000; // 適当な epoch ms 基準点（値自体に意味はない）

	it("閾値未満 = 活動が新しい → null（黙る）", () => {
		expect(quietLabel(NOW, NOW)).toBeNull();
		expect(quietLabel(NOW - (QUIET_AFTER_MS - 1), NOW)).toBeNull();
	});

	it("閾値ちょうどから語り始める（5分）", () => {
		expect(quietLabel(NOW - QUIET_AFTER_MS, NOW)).toBe("5分");
	});

	it("60 分未満は分、以降は時間に繰り上げ（切り捨て）", () => {
		expect(quietLabel(NOW - 59 * 60_000, NOW)).toBe("59分");
		expect(quietLabel(NOW - 60 * 60_000, NOW)).toBe("1時間");
		expect(quietLabel(NOW - 130 * 60_000, NOW)).toBe("2時間");
	});

	it("時刻不明（undefined / null / 0）→ null（憶測で quiet を描かない）", () => {
		expect(quietLabel(undefined, NOW)).toBeNull();
		expect(quietLabel(null, NOW)).toBeNull();
		expect(quietLabel(0, NOW)).toBeNull();
	});

	it("server の分粒度量子化（切り下げ）を通っても閾値判定が暴れない", () => {
		// 実活動 4.5 分前 → server が分に切り下げて 5 分前として届くケース:
		// 量子化誤差（最大 1 分）だけ早く quiet になるのは仕様（安全側 = 早めに気づく）。
		const quantized = NOW - QUIET_AFTER_MS - (NOW % 60_000);
		expect(quietLabel(quantized, NOW)).not.toBeNull();
	});
});
