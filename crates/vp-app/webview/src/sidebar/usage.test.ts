/** engine usage meter — MachineStrip / engine 選択の表示ロジック（spec mem_1ChMLKCMUvC73JHTLjG3xg）。 */
import { describe, expect, it } from "vitest";
import type { EngineUsage } from "../generated/EngineUsage";
import type { UsageSnapshot } from "../generated/UsageSnapshot";
import type { UsageWindow } from "../generated/UsageWindow";
import {
	agentOptionDetail,
	agentOptionLabel,
	formatResetIn,
	severityOf,
	summaryWindow,
	usageGauges,
	worstGauge,
} from "./usage";

function win(id: string, pct: number, scope: string | null = null, extra: Partial<UsageWindow> = {}): UsageWindow {
	return {
		id,
		label: id,
		used_pct: pct,
		resets_at_ms: null,
		severity: pct >= 95 ? "critical" : pct >= 80 ? "warning" : "normal",
		scope,
		...extra,
	};
}

function eng(engine: string, windows: UsageWindow[], extra: Partial<EngineUsage> = {}): EngineUsage {
	return {
		engine,
		plan: null,
		tier: null,
		windows,
		fetched_at_ms: null,
		source: windows.length ? "poll" : "none",
		error: null,
		...extra,
	};
}

/** 2026-11-24 の実測に近い形（Fable 枠だけ 100%、agy の Claude/GPT 枠は 0 残）。 */
const SNAPSHOT: UsageSnapshot = {
	engines: [
		eng("claude", [win("weekly", 88), win("model:fable", 100, "Fable")]),
		eng("codex", [win("weekly", 95)]),
		eng("agy", [win("gemini-weekly", 10), win("gemini-5h", 17.3), win("3p-5h", 100, "Claude/GPT")]),
		eng("grok", []),
	],
	updated_at_ms: 1,
};

describe("summaryWindow — 代表の枠は engine 全体の枠の最悪値", () => {
	it("model 別の枠（Fable 100%）は Claude 全体の % にしない", () => {
		expect(summaryWindow(SNAPSHOT.engines[0]!)?.id).toBe("weekly");
	});
	it("agy は Gemini 群の最悪値（Claude/GPT 群は部分枠）", () => {
		expect(summaryWindow(SNAPSHOT.engines[2]!)?.id).toBe("gemini-5h");
	});
	it("同率は先に並んでいる方（Rust の summary_window / tray_title と同じ）", () => {
		expect(summaryWindow(eng("claude", [win("5h", 100), win("weekly", 100)]))?.id).toBe("5h");
		const tie: UsageSnapshot = {
			engines: [eng("claude", [win("weekly", 100)]), eng("codex", [win("weekly", 100)])],
			updated_at_ms: 1,
		};
		expect(worstGauge(tie)?.short).toBe("CC");
	});
	it("全体の枠が無ければ部分枠の最悪値に倒す / 窓なしは null", () => {
		expect(summaryWindow(eng("x", [win("a", 3, "A"), win("b", 9, "B")]))?.id).toBe("b");
		expect(summaryWindow(eng("grok", []))).toBeNull();
	});
});

describe("usageGauges / worstGauge — MachineStrip", () => {
	it("4 エンジンを固定順・短い名前で並べ、未取得は pct=null", () => {
		expect(usageGauges(SNAPSHOT)).toEqual([
			{ engine: "claude", short: "CC", pct: 88, severity: "warning" },
			{ engine: "codex", short: "Cdx", pct: 95, severity: "critical" },
			{ engine: "agy", short: "Gem", pct: 17.3, severity: "normal" },
			{ engine: "grok", short: "Grk", pct: null, severity: "normal" },
		]);
	});
	it("畳んだ 1 行には最も逼迫しているエンジン", () => {
		expect(worstGauge(SNAPSHOT)?.short).toBe("Cdx");
		expect(worstGauge(undefined)).toBeNull();
		expect(worstGauge({ engines: [eng("grok", [])], updated_at_ms: 0 })).toBeNull();
	});
});

describe("agentOptionLabel — sub lane 作成の engine 選択", () => {
	it("取れていれば % を添える", () => {
		expect(agentOptionLabel("claude", SNAPSHOT)).toBe("claude (88%)");
		expect(agentOptionLabel("agy", SNAPSHOT)).toBe("agy (17%)");
	});
	it("未取得・snapshot 未着・usage の無い engine は名前だけ（fail-open）", () => {
		expect(agentOptionLabel("grok", SNAPSHOT)).toBe("grok");
		expect(agentOptionLabel("claude", undefined)).toBe("claude");
		expect(agentOptionLabel("opencode", SNAPSHOT)).toBe("opencode");
		expect(agentOptionLabel("shell", null)).toBe("shell");
	});
	it("tooltip にはリセットまでの時間", () => {
		const now = Date.UTC(2026, 10, 24, 0, 0);
		const snap: UsageSnapshot = {
			engines: [eng("codex", [win("weekly", 95, null, { resets_at_ms: now + (2 * 1440 + 3 * 60) * 60_000 })])],
			updated_at_ms: 1,
		};
		expect(agentOptionDetail("codex", snap, now)).toBe("weekly 95% 使用 · あと 2d 3h でリセット");
		expect(agentOptionDetail("grok", SNAPSHOT, now)).toBeNull();
	});
});

describe("formatResetIn / severityOf", () => {
	const now = 1_000_000_000_000;
	it("残り時間を粗く出す", () => {
		expect(formatResetIn(now + 30_000, now)).toBe("<1m");
		expect(formatResetIn(now + 12 * 60_000, now)).toBe("12m");
		expect(formatResetIn(now + (3 * 60 + 12) * 60_000, now)).toBe("3h 12m");
		expect(formatResetIn(now + (26 * 60) * 60_000, now)).toBe("1d 2h");
		expect(formatResetIn(now - 5_000, now)).toBe("0m");
	});
	it("知らない severity は % から決め直す", () => {
		expect(severityOf({ severity: "someday-new", used_pct: 96 })).toBe("critical");
		expect(severityOf({ severity: "", used_pct: 80 })).toBe("warning");
		expect(severityOf({ severity: "normal", used_pct: 99 })).toBe("normal");
	});
});
