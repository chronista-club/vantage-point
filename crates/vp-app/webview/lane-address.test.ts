/**
 * `lane-address.ts` — address の分解（webview で唯一の場所）。
 *
 * ⚠️ ここが崩れると **無音で劣化する**。旧実装は `/sub/` `/wing/` を**探す**形で、
 * address が `<repo>/lane/<name>` になった瞬間に 3 箇所が同時に外れ:
 *
 * - ヘッダに `vantage-point/lane/sampler` が丸ごと出る
 * - board が全 lane で Main のキーに集約される
 *
 * という壊れ方をした。例外は出ないので、実機で見るまで気づけない。
 */
import { describe, expect, it } from "vitest";
import {
	MAIN_LANE_NAME,
	isMainAddress,
	laneNameOfAddress,
	repoOfAddress,
	subNameOfAddress,
} from "./lane-address";

describe("laneNameOfAddress（最後の分節）", () => {
	it("⚠️ 世代を問わず lane 名が取れる（分節を探さない）", () => {
		// canonical / 旧 3 分節 / 旧 2 分節 のどれでも同じ答え。
		for (const addr of [
			"vp/lane/sampler",
			"vp/sub/sampler",
			"vp/wing/sampler",
			"vp/sampler",
		]) {
			expect(laneNameOfAddress(addr), addr).toBe("sampler");
		}
	});

	it("⚠️ 分節 1 つは address ではない — lane 名は無い", () => {
		// repo 名を lane 名と誤ると、board が repo ごとに別キーへ散る。
		expect(laneNameOfAddress("weird")).toBe("");
		expect(laneNameOfAddress("")).toBe("");
	});
});

describe("isMainAddress（旧世代の予約名も Main）", () => {
	it("現行の予約名", () => {
		expect(isMainAddress(`vp/lane/${MAIN_LANE_NAME}`)).toBe(true);
	});

	it("⚠️ 旧予約名（lead / conductor / root）も Main とみなす — 永続 state に残る", () => {
		for (const addr of [
			"vp/lead",
			"vp/lane/lead",
			"vp/conductor",
			"vp/root",
			"vp/lane/root",
		]) {
			expect(isMainAddress(addr), addr).toBe(true);
		}
	});

	it("Sub は Main ではない", () => {
		expect(isMainAddress("vp/lane/sampler")).toBe(false);
		expect(isMainAddress("vp/sub/sampler")).toBe(false);
	});
});

describe("subNameOfAddress（Main は null の流儀）", () => {
	it("Sub は名前、Main は null", () => {
		expect(subNameOfAddress("vp/lane/sampler")).toBe("sampler");
		expect(subNameOfAddress(`vp/lane/${MAIN_LANE_NAME}`)).toBeNull();
		expect(subNameOfAddress("vp/lead")).toBeNull(); // 旧予約名
	});

	it("未知形は null（board は最悪 Main に出す側）", () => {
		expect(subNameOfAddress("weird")).toBeNull();
	});
});

describe("repoOfAddress", () => {
	it("先頭分節", () => {
		expect(repoOfAddress("vp/lane/sampler")).toBe("vp");
		expect(repoOfAddress("weird")).toBe("weird");
	});
});
