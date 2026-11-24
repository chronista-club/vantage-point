/**
 * engine usage meter の表示ロジック（純関数）— MachineStrip と sub lane 作成の engine 選択が共用する。
 *
 * 値は daemon "daemon-usage" の snapshot（Rust が `SidebarState.engine_usage` に載せて push）。
 * 型は ts-rs 生成物（`crates/vp-app/src/usage.rs` の写し）。tray の表示文字列は Rust 側
 * （`usage::tray_title` / `tray_line`）にあり、**代表の枠の選び方はここと同じ規則**:
 *
 * - engine 全体の枠（`scope === null`）の中で最も使っているもの
 * - 全体の枠が無い時だけ、部分枠（モデル別・グループ別）の最悪値
 *
 * 部分枠を混ぜない理由: 「Fable の枠だけ使い切った」「agy の中の Claude/GPT 枠が 0」で
 * エンジン全体が 100% に見えると、どのエンジンで作業するかの判断を誤らせる。
 *
 * spec: creo `mem_1ChMLKCMUvC73JHTLjG3xg`
 */
import type { EngineUsage } from "../generated/EngineUsage";
import type { UsageSnapshot } from "../generated/UsageSnapshot";
import type { UsageWindow } from "../generated/UsageWindow";

export type UsageSeverity = "normal" | "warning" | "critical";

/** warning / critical の閾値（daemon の `Severity::from_used_pct` と同じ）。 */
export const WARNING_PCT = 80;
export const CRITICAL_PCT = 95;

/** MachineStrip / tray の短い名前（`CC 88% · Cdx 95% · Gem 16% · Grk 12%`）。 */
export function usageShortName(engine: string): string {
	switch (engine) {
		case "claude":
			return "CC";
		case "codex":
			return "Cdx";
		case "agy":
			return "Gem";
		case "grok":
			return "Grk";
		default:
			return engine;
	}
}

/**
 * 窓の severity。daemon が付けた値を使い、知らない値（daemon が増やした時）は % から決め直す。
 */
export function severityOf(window: Pick<UsageWindow, "severity" | "used_pct">): UsageSeverity {
	if (
		window.severity === "normal" ||
		window.severity === "warning" ||
		window.severity === "critical"
	) {
		return window.severity;
	}
	if (window.used_pct >= CRITICAL_PCT) return "critical";
	if (window.used_pct >= WARNING_PCT) return "warning";
	return "normal";
}

/** engine の代表の枠（module doc の規則）。窓が無ければ null。 */
export function summaryWindow(usage: EngineUsage): UsageWindow | null {
	const worst = (scoped: boolean) =>
		usage.windows
			.filter((w) => (w.scope !== null && w.scope !== undefined) === scoped)
			.reduce<UsageWindow | null>(
				(acc, w) => (acc === null || w.used_pct > acc.used_pct ? w : acc),
				null,
			);
	return worst(false) ?? worst(true);
}

/** snapshot から engine 1 つを引く。snapshot 未着・engine 不在は null。 */
export function engineUsageOf(
	snapshot: UsageSnapshot | null | undefined,
	engine: string,
): EngineUsage | null {
	return snapshot?.engines.find((e) => e.engine === engine) ?? null;
}

/** `88` / `17`（一覧・選択肢は整数で足りる）。 */
export function pctText(usedPct: number): string {
	return `${Math.round(usedPct)}`;
}

/**
 * sub lane 作成の engine 選択肢のラベル。使用率が取れていれば `claude (88%)`、
 * 取れていなければ従来どおり `claude` だけ（fail-open — 未取得で選択を邪魔しない）。
 */
export function agentOptionLabel(
	name: string,
	snapshot: UsageSnapshot | null | undefined,
): string {
	const usage = engineUsageOf(snapshot, name);
	const window = usage ? summaryWindow(usage) : null;
	return window ? `${name} (${pctText(window.used_pct)}%)` : name;
}

/** 選択肢の tooltip に添える 1 行（`weekly 88% · 11/26 09:59 にリセット`）。無ければ null。 */
export function agentOptionDetail(
	name: string,
	snapshot: UsageSnapshot | null | undefined,
	nowMs: number,
): string | null {
	const usage = engineUsageOf(snapshot, name);
	const window = usage ? summaryWindow(usage) : null;
	if (!window) return null;
	const reset =
		window.resets_at_ms !== null && window.resets_at_ms !== undefined
			? ` · あと ${formatResetIn(window.resets_at_ms, nowMs)} でリセット`
			: "";
	return `${window.label} ${pctText(window.used_pct)}% 使用${reset}`;
}

/** リセットまでの残り時間（`2d 3h` / `3h 12m` / `12m` / `<1m`）。過ぎていれば `0m`。 */
export function formatResetIn(resetsAtMs: number, nowMs: number): string {
	const min = Math.max(0, Math.floor((resetsAtMs - nowMs) / 60_000));
	if (resetsAtMs > nowMs && min === 0) return "<1m";
	const d = Math.floor(min / 1440);
	const h = Math.floor((min % 1440) / 60);
	const m = min % 60;
	if (d > 0) return `${d}d ${h}h`;
	if (h > 0) return `${h}h ${m}m`;
	return `${m}m`;
}

/** MachineStrip のゲージ 1 本分。 */
export type UsageGauge = {
	engine: string;
	short: string;
	/** 代表の枠の使用率。未取得は null（`—` を出す）。 */
	pct: number | null;
	severity: UsageSeverity;
};

/** 4 エンジンのゲージ（snapshot の並び = claude / codex / agy / grok）。 */
export function usageGauges(snapshot: UsageSnapshot | null | undefined): UsageGauge[] {
	return (snapshot?.engines ?? []).map((e) => {
		const w = summaryWindow(e);
		return {
			engine: e.engine,
			short: usageShortName(e.engine),
			pct: w ? w.used_pct : null,
			severity: w ? severityOf(w) : "normal",
		};
	});
}

/** 最も逼迫しているゲージ（MachineStrip の畳んだ 1 行に出す）。値が 1 つも無ければ null。 */
export function worstGauge(snapshot: UsageSnapshot | null | undefined): UsageGauge | null {
	return usageGauges(snapshot).reduce<UsageGauge | null>(
		(acc, g) => (g.pct !== null && (acc === null || g.pct > (acc.pct ?? -1)) ? g : acc),
		null,
	);
}
