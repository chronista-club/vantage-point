/**
 * lane-panes（doc 49 LE-P4 PR2）の検証 — 純 calculation と lane scope の意味論。
 *
 * 旧 pane-shell.test.ts の後継。旧 PaneLayout の要件（doc 46 P1 要件 1-3）は
 * engine + gestures の意味論に写して固定し直す:
 *   既定で並ぶ = initialLaneLayout / 縮小・復元 = toggleLanePane（mute / setShare）/
 *   最後の 1 枚は畳ませない = mute の全零 guard / 構成は lane ごと = engine scope。
 * DOM 反映（installLanePanes の render / chips）は node 環境のため対象外（薄い action 層）。
 */

import { resolve, visibleIds } from "@chronista-club/creo-ui-layout";
import { describe, expect, it } from "vitest";
import {
	chatHostId,
	initialLaneLayout,
	lanePaneRefs,
	laneScope,
	newPaneChoices,
	sessionOfHostId,
	syncPaneColumns,
	toggleLanePane,
} from "./lane-panes";
import { layoutEngine } from "./layout-host";

const TERM = "lane-host";
/** doc 50 P1: 固定の Chat pane は退役 — session ↔ Pane 1:1。テストは session 1 の pane で行う */
const CHAT = chatHostId(1);

describe("initialLaneLayout（doc 50 P1: boot は Console 1 枚）", () => {
	it("Console が全面（chat host は session 一覧の到着後にしか存在しない = boot 窓を覆えない）", () => {
		const r = resolve(initialLaneLayout());
		expect(r[TERM]?.rect).toEqual({ x: 0, y: 0, w: 1, h: 1 });
		expect(r[CHAT]).toBeUndefined();
	});
});

describe("lanePaneRefs（pane の顔ぶれ = Console + session 群）", () => {
	it("session の数だけ chat pane が生え、label は engine prefix#key", () => {
		expect(
			lanePaneRefs([
				{ key: 1, stand: "echoes" },
				{ key: 3, stand: "codex" },
			]),
		).toEqual([
			{ id: "lane-host", label: "Console" },
			{ id: "chat-session-1", label: "cc#1", session: 1 },
			{ id: "chat-session-3", label: "cdx#3", session: 3 },
		]);
	});

	it("session なし = Console のみ", () => {
		expect(lanePaneRefs([]).map((p) => p.id)).toEqual([TERM]);
	});
});

describe("chatHostId / sessionOfHostId（往復）", () => {
	it("往復が恒等", () => {
		expect(sessionOfHostId(chatHostId(7))).toBe(7);
	});
	it("term pane / 未知 id は null", () => {
		expect(sessionOfHostId("lane-host")).toBeNull();
		expect(sessionOfHostId("chat-session-x")).toBeNull();
	});
});

describe("syncPaneColumns（layout 列 ↔ session 一覧の同期）", () => {
	it("新しい session は右端に列 append、attention 0（chip に生えるだけ）", () => {
		const l = syncPaneColumns(initialLaneLayout(), [TERM, CHAT]);
		expect(l.structure.columns.map((c) => c.panes)).toEqual([[TERM], [CHAT]]);
		expect(l.attention[CHAT]).toBe(0);
		// 既存（Console）の attention は保たれる
		expect(l.attention[TERM]).toBe(1);
	});

	it("消えた session の列は除去される（既存の attention は保たれる）", () => {
		const grown = syncPaneColumns(initialLaneLayout(), [TERM, CHAT, chatHostId(2)]);
		const shrunk = syncPaneColumns(grown, [TERM, CHAT]);
		expect(shrunk.structure.columns.map((c) => c.panes)).toEqual([[TERM], [CHAT]]);
		expect(chatHostId(2) in shrunk.attention).toBe(false);
	});

	it("冪等（sync → sync は不動点）", () => {
		const once = syncPaneColumns(initialLaneLayout(), [TERM, CHAT]);
		expect(syncPaneColumns(once, [TERM, CHAT])).toEqual(once);
	});

	it("lane-host は ids に含める限り常在（refs 側が必ず先頭に置く前提の裏）", () => {
		const l = syncPaneColumns(initialLaneLayout(), [TERM]);
		expect(l.structure.columns).toEqual([{ panes: [TERM] }]);
	});
});

/** 2 枚構成（Console + session 1 の pane を開いた状態）を作る helper。 */
function twoPane() {
	const synced = syncPaneColumns(initialLaneLayout(), [TERM, CHAT]);
	return toggleLanePane(synced, CHAT); // attention 0 → 0.5 で開く
}

describe("toggleLanePane（chip の 1 クリック往復、要件 2）", () => {
	it("attention 0 で生えた chat pane は toggle で開く", () => {
		const l = twoPane();
		expect(visibleIds(l).sort()).toEqual([CHAT, TERM].sort());
	});

	it("可視 Pane は mute で畳まれ、畳んだ Pane は復帰する（往復）", () => {
		const folded = toggleLanePane(twoPane(), CHAT);
		expect(visibleIds(folded)).toEqual([TERM]);
		const restored = toggleLanePane(folded, CHAT);
		const r = resolve(restored);
		expect(r[CHAT]?.rect.w).toBeCloseTo(0.5);
	});

	it("最後の 1 枚は畳ませない（mute の全零 guard = 同一参照が返る）", () => {
		const folded = toggleLanePane(twoPane(), CHAT);
		expect(toggleLanePane(folded, TERM)).toBe(folded);
	});
});

describe("lane scope（doc 47 §3: 構成は lane ごと）", () => {
	it("scope key は lane ごとに分かれ、配置は独立に動く", () => {
		const a = laneScope("proj/root");
		const b = laneScope("proj/performer/w1");
		expect(a).not.toBe(b);

		layoutEngine.update(a, () => twoPane());
		layoutEngine.update(b, () => twoPane());
		layoutEngine.update(a, (l) => toggleLanePane(l, CHAT));

		expect(visibleIds(layoutEngine.current(a))).toEqual([TERM]);
		expect(visibleIds(layoutEngine.current(b)).sort()).toEqual([CHAT, TERM].sort());
	});
});

describe("newPaneChoices（doc 46 P2 要件 4: Engine × Act）", () => {
	it("chat 非対応 engine は Act II を出さない（行き止まりを作らない）", () => {
		const choices = newPaneChoices([
			{ name: "echoes", label: "Claude", chat_capable: true },
			{ name: "shell", label: "Shell", chat_capable: false },
		]);
		expect(choices).toEqual([
			{ engine: "echoes", engineLabel: "Claude", act: "tui" },
			{ engine: "echoes", engineLabel: "Claude", act: "chat" },
			// shell は chat host を持たないので tui だけ
			{ engine: "shell", engineLabel: "Shell", act: "tui" },
		]);
	});

	it("label 未設定なら stand 名をそのまま出す", () => {
		const choices = newPaneChoices([{ name: "codex", chat_capable: true }]);
		expect(choices.map((c) => c.engineLabel)).toEqual(["codex", "codex"]);
	});

	it("name が空の entry は捨てる（送っても解決できない）", () => {
		expect(newPaneChoices([{ name: "", chat_capable: true }])).toEqual([]);
	});

	it("chat_capable 未指定は非対応扱い（不明なら行き止まりを作らない側に倒す）", () => {
		const choices = newPaneChoices([{ name: "unknown" }]);
		expect(choices.map((c) => c.act)).toEqual(["tui"]);
	});
});
