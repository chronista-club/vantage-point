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
	LANE_PANE_REFS,
	initialLaneLayout,
	laneScope,
	newPaneChoices,
	toggleLanePane,
} from "./lane-panes";
import { layoutEngine } from "./layout-host";

const TERM = "lane-host";
const CHAT = "console-chat-host";

describe("initialLaneLayout", () => {
	it("既定では全 Pane が横並び・等分（要件 1: Console → Chat の順）", () => {
		const r = resolve(initialLaneLayout());
		expect(LANE_PANE_REFS.map((p) => p.id)).toEqual([TERM, CHAT]);
		expect(r[TERM]?.rect).toEqual({ x: 0, y: 0, w: 0.5, h: 1 });
		expect(r[CHAT]?.rect).toEqual({ x: 0.5, y: 0, w: 0.5, h: 1 });
	});
});

describe("toggleLanePane（chip の 1 クリック往復、要件 2）", () => {
	it("可視 Pane は mute で畳まれる", () => {
		const l = toggleLanePane(initialLaneLayout(), CHAT);
		expect(visibleIds(l)).toEqual([TERM]);
		const r = resolve(l);
		expect(r[TERM]?.rect.w).toBe(1);
	});

	it("畳んだ Pane は等分で復帰する（構造不変 = 元の位置に戻る）", () => {
		const folded = toggleLanePane(initialLaneLayout(), CHAT);
		const restored = toggleLanePane(folded, CHAT);
		const r = resolve(restored);
		expect(r[TERM]?.rect.w).toBeCloseTo(0.5);
		expect(r[CHAT]?.rect.x).toBeCloseTo(0.5);
	});

	it("最後の 1 枚は畳ませない（mute の全零 guard = 同一参照が返る）", () => {
		const folded = toggleLanePane(initialLaneLayout(), CHAT);
		expect(toggleLanePane(folded, TERM)).toBe(folded);
	});
});

describe("lane scope（doc 47 §3: 構成は lane ごと）", () => {
	it("scope key は lane ごとに分かれ、配置は独立に動く", () => {
		const a = laneScope("proj/root");
		const b = laneScope("proj/performer/w1");
		expect(a).not.toBe(b);

		layoutEngine.update(a, () => initialLaneLayout());
		layoutEngine.update(b, () => initialLaneLayout());
		layoutEngine.update(a, (l) => toggleLanePane(l, CHAT));

		expect(visibleIds(layoutEngine.current(a))).toEqual([TERM]);
		expect(visibleIds(layoutEngine.current(b))).toEqual([TERM, CHAT]);
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
