/**
 * layout-mcp — MCP layout bridge の scope 振り分け（doc 49 LE-P4 PR3）。
 *
 * 読み手: app.rs `editor_bridge_js` の layout_* arm（`window.vpLayoutHost.mcp`）。
 * 旧実装（LE-P2 PR2）は gallery-panes.tsx が vpLayoutHost を単独所有し scope 固定だった。
 * 本 module が dispatcher を所有し、各 surface が自分の scope handler を登録する:
 *
 *   - "gallery"（既定 — 後方互換）… gallery-panes.tsx が登録（policy read = spring 適用）
 *   - "app"                       … 本 module が登録（app-panes の jump 適用 +
 *                                   CSS transition が実質の driver。author="ai" 監査）
 *
 * ⚠️ lane scope（`lane:<addr>` = work lane）は**意図的に非公開**。公開する時は
 * apply policy 既定を "write"（hitl gate = 提案は人間の承認まで適用しない）にすること
 * （LE-16、team-b 申し送り）。承認 UX が固まるまで dispatcher は unknown scope として返す。
 */

import { type ResolvedMap, type SettleEntry, resolve } from "@chronista-club/creo-ui-layout";
import { APP_SCOPE, applyAppLayoutFromAi } from "./app-panes";
import {
	type LayoutSpec,
	applyLayoutSpec,
	layoutNotation,
	layoutSnapshot,
	takeRecent,
} from "./gallery";
import { layoutEngine } from "./layout-host";

/** scope 1 つ分の MCP 面（get / set / history）。set の spec は scope 剥がし済みで届く */
export interface LayoutScopeMcp {
	get(): unknown;
	set(spec: LayoutSpec): unknown;
	history(opts?: { limit?: number } | null): unknown;
}

const handlers = new Map<string, LayoutScopeMcp>();

/** scope 省略時の既定（LE-P2 からの後方互換 — 既存の CC 会話が壊れない） */
const DEFAULT_SCOPE = "gallery";

export function registerLayoutScope(scope: string, handler: LayoutScopeMcp): void {
	handlers.set(scope, handler);
}

function handlerFor(scope: unknown): LayoutScopeMcp | null {
	const key = typeof scope === "string" && scope.length > 0 ? scope : DEFAULT_SCOPE;
	return handlers.get(key) ?? null;
}

function unknownScope(scope: unknown): { error: string } {
	return {
		error: `unknown scope: ${String(scope)} — available: ${[...handlers.keys()].join(", ")}`,
	};
}

/** dispatcher 本体（純 calculation に近い routing — テストから直接叩ける） */
export const layoutHostMcp = {
	get(body?: { scope?: string } | null): unknown {
		const h = handlerFor(body?.scope);
		return h ? h.get() : unknownScope(body?.scope);
	},
	set(spec?: (LayoutSpec & { scope?: string }) | null): unknown {
		const { scope, ...rest } = spec ?? {};
		const h = handlerFor(scope);
		return h ? h.set(rest) : unknownScope(scope);
	},
	history(opts?: { scope?: string; limit?: number } | null): unknown {
		const h = handlerFor(opts?.scope);
		return h ? h.history(opts) : unknownScope(opts?.scope);
	},
};

// gallery-panes / 本 module の registration は module top-level なので、bundle が
// 読み込まれた時点で全 scope 登録済み（entry.tsx の import 連鎖で保証）。
// window guard は node（vitest）で dispatcher を単体 import するため
if (typeof window !== "undefined") {
	(window as unknown as { vpLayoutHost?: unknown }).vpLayoutHost = { mcp: layoutHostMcp };
}

// ---------- "app" scope handler（app-panes の場を AI が読み書きする口） ----------

function sharesFrom(resolvedMap: ResolvedMap): Record<string, number> {
	const out: Record<string, number> = {};
	for (const [id, pane] of Object.entries(resolvedMap)) {
		out[id] = pane.attention;
	}
	return out;
}

/** settle log → MCP history 応答（gallery 側 layoutMcp.history と同形） */
export function historyEntries(
	log: readonly SettleEntry[],
	limit: number,
): { entries: { author: string; at: number; notation: string }[] } {
	return {
		entries: takeRecent(log, limit).map((e) => ({
			author: e.author,
			at: e.at,
			notation: layoutNotation(e.layout),
		})),
	};
}

registerLayoutScope(APP_SCOPE, {
	get() {
		return layoutSnapshot(
			APP_SCOPE,
			layoutEngine.current(APP_SCOPE),
			sharesFrom(layoutEngine.resolved(APP_SCOPE)),
		);
	},
	set(spec) {
		try {
			const next = applyLayoutSpec(layoutEngine.current(APP_SCOPE), spec);
			// jump 適用（scrub / driver は app scope 未導入 — CSS transition が視覚を均す）。
			// gallery の proposeLayout(read) と違い seize 対象の遷移は無いが、適用は即時 +
			// author="ai" settle で監査が残る点は同じ HITL ループ
			applyAppLayoutFromAi(next);
			return layoutSnapshot(APP_SCOPE, next, sharesFrom(resolve(next)));
		} catch (e) {
			return { error: e instanceof Error ? e.message : String(e) };
		}
	},
	history(opts) {
		return historyEntries(layoutEngine.history(APP_SCOPE), opts?.limit ?? 10);
	},
});
