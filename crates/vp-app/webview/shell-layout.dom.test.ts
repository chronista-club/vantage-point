// @vitest-environment happy-dom
/**
 * `shell-layout.ts` の DOM への当て方（doc 87 §8 slice 1 — 額縁が grid になった後の形）。
 *
 * ## ⚠️ 守るもの
 *
 * - L の保存値は **peek の幅**。DOM には `#sidebar-root` の `--vp-peek-w` として当て、creo-ui の
 *   `CURail` が `peekWidth` で受ける（`#sidebar-root` は中身の幅）。inline の `width` を書くと、
 *   slim（CURail が peek を畳む）でも peek 分の幅が残る
 * - R の幅（%）は `#app-shell` の `--vp-rsb-pct` に当てる（grid の right track が読む）。
 *   grid item の % 幅は track に対して解決され auto の track では効かないので、`#right-sidebar` の
 *   inline `width` には書かない
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	RIGHT_MAX_PCT,
	SIDEBAR_MAX,
	SIDEBAR_MIN,
	applyShellLayout,
	onShellLayoutChange,
	setPeekWidth,
	setRightPct,
	shellLayoutState,
} from "./shell-layout";

beforeEach(() => {
	document.body.innerHTML =
		'<div id="app-shell"><div id="sidebar-root"></div><div id="shell-resizer-left"></div>' +
		'<div id="host"></div><div id="shell-resizer-right"></div><div id="right-sidebar"></div></div>';
});

const restore = (over: Partial<Parameters<typeof applyShellLayout>[0]> = {}) =>
	applyShellLayout({
		sidebar_width: 300,
		right_sidebar_width: 420,
		sidebar_form: "full",
		right_sidebar_open: true,
		right_sidebar_pct: 30,
		...over,
	});

describe("復元の当て方", () => {
	it("L は peek の幅を --vp-peek-w に書き、inline の width は書かない", () => {
		restore();
		const left = document.getElementById("sidebar-root")!;
		expect(left.style.getPropertyValue("--vp-peek-w")).toBe("300px");
		expect(left.style.width).toBe("");
	});

	it("slim でも peek の幅は保持したまま（full に戻した時の幅）、取っ手は隠す", () => {
		restore({ sidebar_form: "slim" });
		expect(document.getElementById("sidebar-root")!.style.getPropertyValue("--vp-peek-w")).toBe(
			"300px",
		);
		expect(document.getElementById("shell-resizer-left")!.style.display).toBe("none");
	});

	it("R の % は #app-shell の --vp-rsb-pct に書き、#right-sidebar の width には書かない", () => {
		restore();
		expect(document.getElementById("app-shell")!.style.getPropertyValue("--vp-rsb-pct")).toBe(
			"30%",
		);
		expect(document.getElementById("right-sidebar")!.style.width).toBe("");
	});
});

/**
 * Editor / MCP（editor_set）/ Link hub からの書き口（doc 87 §8.3）。
 *
 * ⚠️ Editor は値を `:root` の inline style に書くが、`#sidebar-root` / `#app-shell` は自分の inline style で
 * 同じ変数を持つので `:root` に書いても届かない。幅の field は cssVar を持たず、ここの書き口を通る —
 * drag と同じ箱（state）を動かし、同じ場所（#sidebar-root / #app-shell）に当て、同じ IPC で保存する。
 */
describe("Editor / MCP からの書き口", () => {
	const sent: Array<Record<string, unknown>> = [];
	beforeEach(() => {
		sent.length = 0;
		(globalThis as { ipc?: unknown }).ipc = {
			postMessage: (m: string) => sent.push(JSON.parse(m)),
		};
		vi.useFakeTimers();
		restore({ sidebar_width: 300, right_sidebar_pct: 30 });
		vi.runAllTimers();
		sent.length = 0;
	});
	afterEach(() => {
		vi.useRealTimers();
		delete (globalThis as { ipc?: unknown }).ipc;
	});

	it("peek の幅は #sidebar-root の --vp-peek-w に当て、:root には書かない", () => {
		setPeekWidth(420);
		expect(document.getElementById("sidebar-root")!.style.getPropertyValue("--vp-peek-w")).toBe("420px");
		expect(document.documentElement.style.getPropertyValue("--vp-peek-w")).toBe("");
		expect(shellLayoutState().sidebarWidth).toBe(420);
	});

	it("R の幅（%）は #app-shell の --vp-rsb-pct に当てる", () => {
		setRightPct(40);
		expect(document.getElementById("app-shell")!.style.getPropertyValue("--vp-rsb-pct")).toBe("40%");
		expect(shellLayoutState().rightSidebarPct).toBe(40);
	});

	it("範囲の外は drag と同じ端に丸める", () => {
		setPeekWidth(9999);
		expect(shellLayoutState().sidebarWidth).toBe(SIDEBAR_MAX);
		setPeekWidth(10);
		expect(shellLayoutState().sidebarWidth).toBe(SIDEBAR_MIN);
		setRightPct(99);
		expect(shellLayoutState().rightSidebarPct).toBe(RIGHT_MAX_PCT);
		setRightPct(Number.NaN);
		expect(shellLayoutState().rightSidebarPct).toBe(RIGHT_MAX_PCT); // 不正値は無視（今の幅のまま）
	});

	it("保存は落ち着いてから 1 回 — slider を動かし続けても IPC は 1 本（drag の「確定時だけ」と同じ規律）", () => {
		for (const w of [310, 320, 330, 340]) setPeekWidth(w);
		expect(sent).toEqual([]);
		vi.runAllTimers();
		expect(sent).toHaveLength(1);
		expect(sent[0]).toMatchObject({ t: "shell:layout", sidebar_width: 340, right_sidebar_pct: 30 });
	});

	it("同じ値なら何もしない（保存も通知もしない）— Editor へ写し返した値が戻ってきても輪にならない", () => {
		const seen: number[] = [];
		const off = onShellLayoutChange((s) => seen.push(s.sidebarWidth));
		setPeekWidth(300);
		vi.runAllTimers();
		expect(sent).toEqual([]);
		expect(seen).toEqual([]);
		off();
	});

	it("形が変わると購読者に今の形を知らせる（書き口・復元）。解除すると止まる", () => {
		const seen: Array<[number, number]> = [];
		const off = onShellLayoutChange((s) => seen.push([s.sidebarWidth, s.rightSidebarPct]));
		setPeekWidth(500);
		setRightPct(20);
		restore({ sidebar_width: 260, right_sidebar_pct: 35 });
		off();
		setPeekWidth(400);
		expect(seen).toEqual([
			[500, 30],
			[500, 20],
			[260, 35],
		]);
	});
});
