/**
 * lane-panes（doc 49 LE-P4 PR2 → doc 51 §1 A1）の検証 — 純 calculation と lane scope の意味論。
 *
 * 旧 pane-shell.test.ts の後継。doc 51 §1 A1（tiling 既定 + 帯撤去）で固定し直した要件:
 *   既定で並ぶ = initialLaneLayout + syncPaneColumns の可視入場（enterShare）/
 *   roster は mode × root で term / chat を排他にする（同じ session を 2 枚にしない）/
 *   構成は lane ごと = engine scope。
 * DOM 反映（installLanePanes の render）は node 環境のため対象外（薄い action 層）。
 */

import { resolve, visibleIds } from "@chronista-club/creo-ui-layout";
import { describe, expect, it } from "vitest";
import {
	chatHostId,
	enterShare,
	initialLaneLayout,
	lanePaneRefs,
	laneScope,
	newPaneChoices,
	sessionOfHostId,
	syncPaneColumns,
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

describe("lanePaneRefs（roster = mode × root で term / chat を排他、doc 51 §2）", () => {
	it("tui: Console + 非 root session の chat pane（root は Act I 面に居る）", () => {
		expect(
			lanePaneRefs(
				[
					{ key: 1, stand: "echoes", root: true },
					{ key: 3, stand: "codex" },
				],
				"tui",
			),
		).toEqual([
			{ id: "lane-host", label: "Console" },
			{ id: "chat-session-3", label: "cdx#3", session: 3 },
		]);
	});

	it("chat: 全 session の chat pane（root も chat。抜け殻の xterm は台に並べない）", () => {
		expect(
			lanePaneRefs(
				[
					{ key: 1, stand: "echoes", root: true },
					{ key: 3, stand: "codex" },
				],
				"chat",
			),
		).toEqual([
			{ id: "chat-session-1", label: "cc#1", session: 1 },
			{ id: "chat-session-3", label: "cdx#3", session: 3 },
		]);
	});

	it("tui + session なし = Console のみ", () => {
		expect(lanePaneRefs([], "tui").map((p) => p.id)).toEqual([TERM]);
	});

	it("board 非空: mode を問わず末尾に board pane が並ぶ（doc 52 §10 wave 0）", () => {
		// tui: Console + board
		expect(lanePaneRefs([], "tui", true).map((p) => p.id)).toEqual([
			TERM,
			"lane-board",
		]);
		// chat: 全 chat + board（session あり）
		expect(
			lanePaneRefs([{ key: 1, stand: "echoes", root: true }], "chat", true).map(
				(p) => p.id,
			),
		).toEqual(["chat-session-1", "lane-board"]);
	});

	it("board 空（既定）は board pane を出さない", () => {
		expect(lanePaneRefs([], "tui", false).map((p) => p.id)).toEqual([TERM]);
		expect(lanePaneRefs([], "tui").map((p) => p.id)).toEqual([TERM]);
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

describe("syncPaneColumns（layout 列 ↔ roster の同期、tiling 既定）", () => {
	it("新しい pane は右端に列 append、**可視で入場**（enterShare = 可視 raw 平均）", () => {
		const l = syncPaneColumns(initialLaneLayout(), [TERM, CHAT]);
		expect(l.structure.columns.map((c) => c.panes)).toEqual([[TERM], [CHAT]]);
		// tiling 既定: 畳まれて生まれない。Console(1) と等しい share で並ぶ = 等分
		expect(l.attention[CHAT]).toBe(1);
		expect(l.attention[TERM]).toBe(1);
		const r = resolve(l);
		expect(r[CHAT]?.rect.w).toBeCloseTo(0.5);
	});

	it("消えた pane の列は除去される（既存の attention は保たれる）", () => {
		const grown = syncPaneColumns(initialLaneLayout(), [TERM, CHAT, chatHostId(2)]);
		const shrunk = syncPaneColumns(grown, [TERM, CHAT]);
		expect(shrunk.structure.columns.map((c) => c.panes)).toEqual([[TERM], [CHAT]]);
		expect(chatHostId(2) in shrunk.attention).toBe(false);
	});

	it("冪等（sync → sync は不動点）", () => {
		const once = syncPaneColumns(initialLaneLayout(), [TERM, CHAT]);
		expect(syncPaneColumns(once, [TERM, CHAT])).toEqual(once);
	});

	it("再入場も可視（roster から外れた pane が戻る = attention 記録は消えている → enterShare）", () => {
		// mode 切替で Console が roster を出て（chat）、戻る（tui）往復の縮図
		const noTerm = syncPaneColumns(initialLaneLayout(), [CHAT]);
		expect(TERM in noTerm.attention).toBe(false);
		const back = syncPaneColumns(noTerm, [TERM, CHAT]);
		expect(back.attention[TERM]).toBeGreaterThan(0);
	});

	it("lane-host は ids に含める限り常在（refs 側が必ず先頭に置く前提の裏）", () => {
		const l = syncPaneColumns(initialLaneLayout(), [TERM]);
		expect(l.structure.columns).toEqual([{ panes: [TERM] }]);
	});
});

describe("enterShare（入場 share = 可視 raw 平均、creo-ui-layout admit と同じ規則）", () => {
	it("可視 pane が居なければ 1", () => {
		expect(enterShare({ structure: { columns: [] }, attention: {} })).toBe(1);
	});
	it("可視 pane の raw 平均（非可視 0 は数えない）", () => {
		expect(
			enterShare({
				structure: { columns: [{ panes: ["a", "b", "c"] }] },
				attention: { a: 1, b: 0.5, c: 0 },
			}),
		).toBeCloseTo(0.75);
	});
});

describe("lane scope（doc 47 §3: 構成は lane ごと）", () => {
	it("scope key は lane ごとに分かれ、配置は独立に動く", () => {
		const a = laneScope("proj/root");
		const b = laneScope("proj/performer/w1");
		expect(a).not.toBe(b);

		layoutEngine.update(a, () => syncPaneColumns(initialLaneLayout(), [TERM, CHAT]));
		layoutEngine.update(b, () => syncPaneColumns(initialLaneLayout(), [TERM, CHAT]));
		// a だけ session 1 が閉じた（roster から消えた）
		layoutEngine.update(a, (l) => syncPaneColumns(l, [TERM]));

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
