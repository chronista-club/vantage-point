import { testPath } from './test-utils/paths'
/**
 * Editor の region の幅（doc 87 §8.3）— 本物の VpEditor と shell-layout を 1 つの document で動かす。
 *
 * ## ⚠️ 守るもの
 *
 * - Editor / MCP（Link hub は `host.setValue` を呼ぶ）で幅を変えると、`#sidebar-root` / `#app-shell` に効く。
 *   `:root` には書かない（子の inline に負けて届かない）
 * - **幅はテーマに入らない**。持ち主は shell-layout（`session.json`）で、テーマを切り替えても幅は変わらない
 * - Editor の登録・テーマの復元が、復元済みの幅を初期値（280px / 25%）で潰さない。Editor は今の幅を指す
 */
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window } from "happy-dom";
import { afterEach, beforeAll, expect, it } from "vitest";

type Win = Window & {
	vpEditorHost: any;
	boot(): void;
	dispose(): void;
	applyShellLayout(l: Record<string, unknown>): void;
	installShellLayout(): void;
	shellLayoutState(): { sidebarWidth: number; rightSidebarPct: number };
	ipc?: { postMessage(m: string): void };
};

let bundle: string;
const windows: Win[] = [];
beforeAll(async () => {
	const result = await build({
		stdin: {
			contents: `
        import { render } from '@solidjs/web';
        import { createComponent } from 'solid-js';
        import { VpEditor } from './editor/VpEditor';
        import { applyShellLayout, installShellLayout, shellLayoutState } from './shell-layout';
        window.applyShellLayout = applyShellLayout;
        window.installShellLayout = installShellLayout;
        window.shellLayoutState = shellLayoutState;
        window.boot = () => { window.dispose = render(() => createComponent(VpEditor, {}), document.getElementById('editor-root')); };
      `,
			loader: "tsx",
			resolveDir: testPath(import.meta.url),
		},
		bundle: true,
		write: false,
		format: "iife",
		platform: "browser",
		preserveSymlinks: true,
		plugins: [solidPlugin()],
		define: { "process.env.NODE_ENV": '"production"' },
	});
	bundle = result.outputFiles[0].text;
});

const SHELL =
	'<div id="app-shell"><div id="sidebar-root"></div><div id="shell-resizer-left"></div>' +
	'<div id="host"></div><div id="shell-resizer-right"></div><div id="right-sidebar"></div></div>';

/** `restoreFirst` を渡すと、Editor を mount する前に shell-layout の復元を当てる（起動順の片方）。 */
function fixture(opts: { storage?: Record<string, string>; restoreFirst?: Record<string, unknown> } = {}) {
	const win = new Window({
		url: "http://localhost:12889",
		settings: { enableJavaScriptEvaluation: true, suppressInsecureJavaScriptEnvironmentWarning: true },
	}) as unknown as Win;
	windows.push(win);
	for (const [k, v] of Object.entries(opts.storage ?? {})) win.localStorage.setItem(k, v);
	win.document.documentElement.dataset.theme = "contrast-dark";
	win.document.body.innerHTML = `${SHELL}<div id="editor-root"></div>`;
	evalDom(win, bundle);
	const restoreFirst = opts.restoreFirst;
	if (restoreFirst) callDom(win, () => win.applyShellLayout(restoreFirst));
	callDom(win, () => win.boot());
	callDom(win, () => win.vpEditorHost.enable());
	return win;
}
const layout = (over: Record<string, unknown> = {}) => ({
	sidebar_width: 420,
	right_sidebar_width: 420,
	sidebar_form: "full",
	right_sidebar_open: true,
	right_sidebar_pct: 35,
	...over,
});
const tick = () => new Promise((r) => setTimeout(r, 0));
const peekW = (win: Win) => (win.document.getElementById("sidebar-root") as unknown as HTMLElement).style.getPropertyValue("--vp-peek-w");
const rsbPct = (win: Win) => (win.document.getElementById("app-shell") as unknown as HTMLElement).style.getPropertyValue("--vp-rsb-pct");
const click = (win: Win, text: string) => {
	const el = Array.from(win.document.querySelectorAll("button")).find((b) => b.textContent?.trim() === text)!;
	dispatchDom(el, new win.MouseEvent("click", { bubbles: true }));
};
const changeTheme = async (win: Win, id: string) => {
	const select = win.document.querySelector('[aria-label="テーマを選ぶ"]') as any;
	select.value = id;
	dispatchDom(select, new win.Event("change", { bubbles: true }));
	await tick();
};
afterEach(async () => {
	for (const win of windows.splice(0)) {
		callDom(win, () => win.dispose?.());
		await win.happyDOM.abort();
	}
});

it("SIDEBAR タブの先頭に左右の幅、THEME に pane の名札と Lane の端末が並ぶ", async () => {
	const win = fixture();
	await tick();
	click(win, "SIDEBAR");
	const sidebar = Array.from(win.document.querySelectorAll("[data-field]")).map((el) => el.getAttribute("data-field"));
	expect(sidebar.slice(0, 2)).toEqual(["shell.leftEdge.peek.width", "shell.rightEdge.width"]);
	expect(sidebar).not.toContain("pane.lane.terminal.font.size");
	click(win, "THEME");
	for (const id of ["shell.center.nameplate.height", "pane.lane.terminal.font.size", "pane.lane.terminal.line.height", "shell.resizer.breathe.ms"]) {
		expect(win.document.querySelector(`[data-field="${id}"]`), id).not.toBeNull();
	}
	expect(win.document.querySelector('[data-field="shell.leftEdge.peek.width"]')).toBeNull();
});

it("Editor / MCP で幅を変えると #sidebar-root / #app-shell に効き、:root には書かない", async () => {
	const win = fixture();
	await tick();
	callDom(win, () => win.vpEditorHost.setValue("shell.leftEdge.peek.width", 400));
	callDom(win, () => win.vpEditorHost.setValue("shell.rightEdge.width", 40));
	expect(peekW(win)).toBe("400px");
	expect(rsbPct(win)).toBe("40%");
	expect(win.document.documentElement.style.getPropertyValue("--vp-peek-w")).toBe("");
	expect(win.document.documentElement.style.getPropertyValue("--vp-rsb-pct")).toBe("");
	expect(callDom(win, () => win.shellLayoutState())).toMatchObject({ sidebarWidth: 400, rightSidebarPct: 40 });
});

it("範囲の外は shell-layout が丸め、Editor の値も丸めた値に戻る", async () => {
	const win = fixture();
	await tick();
	callDom(win, () => win.vpEditorHost.setValue("shell.leftEdge.peek.width", 5000));
	expect(peekW(win)).toBe("640px");
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(640);
});

it("Editor の変更は shell-layout の IPC で 1 回保存し、テーマの保存には入らない", async () => {
	const win = fixture();
	const sent: Array<Record<string, unknown>> = [];
	win.ipc = { postMessage: (m: string) => sent.push(JSON.parse(m)) };
	await tick();
	for (const w of [300, 320, 340]) callDom(win, () => win.vpEditorHost.setValue("shell.leftEdge.peek.width", w));
	callDom(win, () => win.vpEditorHost.setValue("sb.selection.bg", "#334455"));
	await new Promise((r) => setTimeout(r, 400));
	expect(sent.filter((m) => m.t === "shell:layout")).toEqual([expect.objectContaining({ sidebar_width: 340 })]);
	const themes = JSON.parse(win.localStorage.getItem("vp:editor:themes:v1")!);
	expect(themes.drafts["contrast-dark"]).toEqual({ "sb.selection.bg": "#334455" });
});

it("先に復元された幅を、Editor の登録とテーマの復元が初期値で潰さない", async () => {
	const win = fixture({
		restoreFirst: layout(),
		// テーマの保存値に幅の key があっても（書き出し JSON を手で戻した等）、テーマからは読まない
		storage: {
			"vp:editor:themes:v1": JSON.stringify({ version: 1, selected: "contrast-dark", custom: [], drafts: { "contrast-dark": { "shell.leftEdge.peek.width": 222 } } }),
		},
	});
	await tick();
	expect(peekW(win)).toBe("420px");
	expect(rsbPct(win)).toBe("35%");
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(420);
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.rightEdge.width"))).toBe(35);
});

it("Editor の後に来た復元（起動順のもう片方）にも、Editor の値が追従する", async () => {
	const win = fixture();
	await tick();
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(280);
	callDom(win, () => win.applyShellLayout(layout({ sidebar_width: 360 })));
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(360);
	expect(peekW(win)).toBe("360px");
});

it("テーマを切り替えても幅は変わらない（幅はテーマではない）", async () => {
	const win = fixture({ restoreFirst: layout() });
	await tick();
	callDom(win, () => win.vpEditorHost.setValue("shell.leftEdge.peek.width", 500));
	click(win, "THEME");
	await changeTheme(win, "mint-light");
	expect(peekW(win)).toBe("500px");
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(500);
	await changeTheme(win, "contrast-dark");
	expect(peekW(win)).toBe("500px");
});

it("registry 同梱の端末の field は :root に書き、初期値に戻すと inline を消す", async () => {
	const win = fixture();
	await tick();
	callDom(win, () => win.vpEditorHost.setValue("pane.lane.terminal.font.size", 18));
	expect(win.document.documentElement.style.getPropertyValue("--terminal-font-size")).toBe("18");
	callDom(win, () => win.vpEditorHost.setValue("pane.lane.terminal.font.size", 16));
	expect(win.document.documentElement.style.getPropertyValue("--terminal-font-size")).toBe("");
});

it("テーマの切替で、registry 同梱の field が :root に書いた値も消える（テーマごとに隔離）", async () => {
	const win = fixture();
	await tick();
	callDom(win, () => win.vpEditorHost.setValue("pane.lane.terminal.font.size", 20));
	click(win, "THEME");
	await changeTheme(win, "mint-light");
	expect(win.document.documentElement.style.getPropertyValue("--terminal-font-size")).toBe("");
	await changeTheme(win, "contrast-dark");
	expect(win.document.documentElement.style.getPropertyValue("--terminal-font-size")).toBe("20");
});

it("境界の drag を確定すると、Editor の値も確定した幅に追従する", async () => {
	const win = fixture();
	callDom(win, () => win.installShellLayout());
	await tick();
	const handle = win.document.getElementById("shell-resizer-left") as unknown as HTMLElement & { setPointerCapture(id: number): void };
	handle.setPointerCapture = () => {};
	const pointer = (type: string, clientX: number) =>
		dispatchDom(handle, new win.PointerEvent(type, { clientX, pointerId: 1, bubbles: true }) as unknown as Event);
	pointer("pointerdown", 300);
	pointer("pointermove", 400);
	pointer("pointerup", 400);
	// rect は happy-dom で 0、rail 列は測れないので既定の 48px を引く: 400 - 48
	expect(callDom(win, () => win.shellLayoutState()).sidebarWidth).toBe(352);
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(352);
});

it("書き口が丸めた値が今の幅と同じでも、Editor の値は今の幅に戻る（MCP が端数を送った時）", async () => {
	const win = fixture({ restoreFirst: layout({ sidebar_width: 300, right_sidebar_pct: 30 }) });
	await tick();
	callDom(win, () => win.vpEditorHost.setValue("shell.leftEdge.peek.width", 300.4));
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(300);
	callDom(win, () => win.vpEditorHost.setValue("shell.rightEdge.width", 33.33));
	callDom(win, () => win.vpEditorHost.setValue("shell.rightEdge.width", 33.33));
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.rightEdge.width"))).toBe(33.3);
});

it("drag・復元から Editor への写し返しは silent — 変更通知（テーマ保存・Link への送信の口）を起こさない", async () => {
	const win = fixture();
	await tick();
	const changes: string[] = [];
	callDom(win, () => win.vpEditorHost.onAnyChange((id: string) => changes.push(id)));
	callDom(win, () => win.applyShellLayout(layout({ sidebar_width: 360 })));
	expect(callDom(win, () => win.vpEditorHost.getValue("shell.leftEdge.peek.width"))).toBe(360);
	expect(changes).toEqual([]);
});

it("数の欄に打っている途中の値（範囲の外）は当てず、確定（change）で範囲に丸める", async () => {
	const win = fixture({ restoreFirst: layout({ sidebar_width: 300 }) });
	await tick();
	click(win, "SIDEBAR");
	const input = win.document.getElementById("vp-field-shell.leftEdge.peek.width") as any;
	for (const typed of ["4", "40", "400"]) {
		input.value = typed;
		dispatchDom(input, new win.Event("input", { bubbles: true }));
		// 打っている途中（4 / 40）は範囲の外 — 幅は 300 のまま、180 にも 640 にも跳ねない
		if (typed !== "400") expect(peekW(win), typed).toBe("300px");
	}
	expect(peekW(win)).toBe("400px");
	input.value = "9999";
	dispatchDom(input, new win.Event("input", { bubbles: true }));
	expect(peekW(win)).toBe("400px");
	dispatchDom(input, new win.Event("change", { bubbles: true }));
	expect(peekW(win)).toBe("640px");
	expect(input.value).toBe("640");
});
