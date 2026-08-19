/** doc 58 ②-a — sidebar 側 now-line store の往復（set → 上書き → null 消去）。 */
import { describe, expect, it } from "vitest";
import { sessionNowKey } from "../../session-now-bridge";
import { applySessionNow, sessionNow } from "./session-now";

describe("applySessionNow (doc 58 ②-a)", () => {
	const LANE = "vp/lane/main";

	it("set → 読める / 上書き → 最新が勝つ", () => {
		applySessionNow(LANE, 13, "panic 箇所を特定中");
		expect(sessionNow[sessionNowKey(LANE, 13)]).toBe("panic 箇所を特定中");
		applySessionNow(LANE, 13, "lock 順を確認");
		expect(sessionNow[sessionNowKey(LANE, 13)]).toBe("lock 順を確認");
	});

	it("null = 「今」は turn より長生きしない — 鍵ごと消える", () => {
		applySessionNow(LANE, 13, "作業中");
		applySessionNow(LANE, 13, null);
		expect(sessionNow[sessionNowKey(LANE, 13)]).toBeUndefined();
	});

	it("session が違えば別の「今」（相部屋の 2 人は混ざらない）", () => {
		applySessionNow(LANE, 1, "root の今");
		applySessionNow(LANE, 2, "slot 2 の今");
		expect(sessionNow[sessionNowKey(LANE, 1)]).toBe("root の今");
		expect(sessionNow[sessionNowKey(LANE, 2)]).toBe("slot 2 の今");
	});
});
