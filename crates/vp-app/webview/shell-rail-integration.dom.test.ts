/**
 * 左 edge = Rail + peek の結線（doc 87 §8 slice 1、task mem_1ChNmWC2c12iRHNmGTtgHZ）。
 *
 * Shell を丸ごと mount し、route（選択の SSOT）・peek の開閉（form）・設定 Scene・
 * slim の repo badge 列が 1 本の筋で動くことを確かめる。個々の純関数は
 * `src/sidebar/route.test.ts`、Rail の DOM 契約は `src/sidebar/ShellRail.dom.test.ts`。
 */
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { build } from "esbuild";
import { solidPlugin } from "esbuild-plugin-solid";
import { Window } from "happy-dom";
import { fileURLToPath } from "node:url";

type Probe = Window & {
	vpSettings: { handleResult(s: unknown): void };
	runCaptureMode(): void;
	runNewSub(): void;
	sent: { t: string }[];
	selectLane(addr: string): void;
	setAwaiting(addr: string): void;
};

let bundle: string;
const windows: Window[] = [];

beforeAll(async () => {
	const result = await build({
		stdin: {
			contents: `
        import { render } from 'solid-js/web';
        import { createComponent } from 'solid-js';
        import { Shell } from './src/sidebar/Shell';
        import { applySidebarState, emptyState } from './src/sidebar/store';
        import { runCaptureMode, runNewSub } from './src/sidebar/actions/handlers';
        window.runCaptureMode = runCaptureMode;
        window.runNewSub = runNewSub;
        window.sent = [];
        window.ipc = { postMessage: m => window.sent.push(JSON.parse(m)) };
        const base = (addr, awaiting) => ({ ...emptyState(),
          processes: [
            { path: '/vp', name: 'vantage-point', state: 'running', expanded: true },
            { path: '/creo', name: 'creo', state: 'running', expanded: false },
          ],
          lanes_by_repo: { '/vp': [
            { address: { repo: 'vp', name: 'lead', key: 'vp/lane/lead' }, sessions: { sessions: [] } },
          ] },
          awaiting_input: awaiting ? { [awaiting]: true } : {},
          active_lane_address: addr });
        window.selectLane = addr => applySidebarState(base(addr, null));
        window.setAwaiting = addr => applySidebarState(base('vp/lane/lead', addr));
        window.selectLane('vp/lane/lead');
        render(() => createComponent(Shell, {}), document.getElementById('sidebar-root'));
      `,
			resolveDir: fileURLToPath(new URL(".", import.meta.url)),
			loader: "tsx",
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

function fixture(): Probe {
	const win = new Window({
		settings: {
			enableJavaScriptEvaluation: true,
			suppressInsecureJavaScriptEnvironmentWarning: true,
		},
	}) as unknown as Probe;
	windows.push(win);
	win.document.body.innerHTML =
		'<div id="app-shell"><div id="sidebar-root" data-region="leftEdge"></div>' +
		'<div id="host" data-region="center"><div id="work"></div></div></div>';
	win.eval(bundle);
	return win;
}

afterEach(async () => {
	for (const win of windows.splice(0)) await win.happyDOM.abort();
});

const q = (win: Probe, sel: string) => win.document.querySelector(sel) as unknown as HTMLElement | null;
const click = (win: Probe, rail: string) => q(win, `button[data-rail="${rail}"]`)!.click();
const current = (win: Probe) =>
	q(win, 'button[data-rail][aria-current="page"]')?.getAttribute("data-rail") ?? null;
const shown = (win: Probe) =>
	Array.from(win.document.querySelectorAll("[data-peek]"))
		.filter((el) => !el.hasAttribute("hidden"))
		.map((el) => el.getAttribute("data-peek"));

describe("開発 Scene — Lanes / Actions", () => {
	it("起動直後は Lanes が選ばれ、peek に名簿、route に active lane が載る", () => {
		const win = fixture();
		expect(current(win)).toBe("lanes");
		expect(shown(win)).toEqual(["lanes"]);
		expect(q(win, '[data-peek="lanes"] .vp-sidebar-list')).not.toBeNull();
		const shell = q(win, "#app-shell")!;
		expect(shell.dataset.route).toBe("/lanes/vp/lane/lead");
		expect(shell.dataset.scene).toBe("dev");
	});

	it("Actions を押すと peek が ACTIONS に替わり、名簿は mount されたまま隠れる", () => {
		const win = fixture();
		const roster = q(win, ".vp-sidebar-list");
		click(win, "actions");
		expect(current(win)).toBe("actions");
		expect(shown(win)).toEqual(["actions"]);
		expect(q(win, '[data-peek="actions"] .vp-act-buckets')).not.toBeNull();
		expect(q(win, ".vp-sidebar-list")).toBe(roster);
		expect(q(win, "#app-shell")!.dataset.route).toBe("/actions");
	});

	it("machine 帯はどの rail でも peek の下端に居る", () => {
		const win = fixture();
		const strip = q(win, "[data-expanded] > .vp-machine-strip");
		expect(strip).not.toBeNull();
		click(win, "actions");
		expect(q(win, "[data-expanded] > .vp-machine-strip")).toBe(strip);
	});

	it("旧 sidebar 下部の ⚙ 行は無い（rail の歯車が入口）", () => {
		const win = fixture();
		expect(q(win, ".vp-settings-entry")).toBeNull();
		expect(q(win, '.anchor-bottom button[data-rail="settings"]')).not.toBeNull();
	});
});

describe("設定 Scene（doc 87 §3）", () => {
	it("歯車 → center に設定面、rail は Settings、戻ると直前の開発 route", () => {
		const win = fixture();
		click(win, "actions");
		click(win, "settings");
		expect(current(win)).toBe("settings");
		expect(q(win, "#app-shell")!.dataset.scene).toBe("settings");
		expect(q(win, "#host .vp-settings-panel")).not.toBeNull();
		expect(win.sent.filter((m) => m.t === "settings:fetch")).toHaveLength(1);
		expect(shown(win)).toEqual(["settings"]);
		win.document.dispatchEvent(new win.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
		expect(q(win, ".vp-settings-panel")).toBeNull();
		expect(current(win)).toBe("actions");
		expect(q(win, "#app-shell")!.dataset.scene).toBe("dev");
	});

	it("設定の peek は項目の索引。押すと route に section が載る", () => {
		const win = fixture();
		click(win, "settings");
		const items = Array.from(
			win.document.querySelectorAll('[data-peek="settings"] [data-setting-link]'),
		) as unknown as HTMLElement[];
		expect(items.length).toBeGreaterThan(0);
		items[1].click();
		expect(q(win, "#app-shell")!.dataset.route).toBe(
			`/settings/${items[1].getAttribute("data-setting-link")}`,
		);
		expect(items[1].getAttribute("aria-current")).toBe("page");
	});

	it("索引の同じ項目をもう一度押しても、その行へ寄せ直す（手で scroll して離れた後の戻り道）", () => {
		const win = fixture();
		const calls: string[] = [];
		(win as unknown as { HTMLElement: { prototype: { scrollIntoView(): void } } }).HTMLElement.prototype.scrollIntoView =
			function (this: { dataset: { setting?: string } }) {
				if (this.dataset?.setting) calls.push(this.dataset.setting);
			};
		click(win, "settings");
		win.vpSettings.handleResult({ developerMode: false, developerModeLocked: false, defaultRepoRoot: "", resolvedRepoRoot: "/repos", daemonReachable: true, logLevel: "", idleTimeoutMinutes: 5, defaultAgent: "", defaultModel: "", defaultAgentTakesModel: true });
		const item = q(win, '[data-setting-link="update"]')!;
		const count = () => calls.filter((id) => id === "update").length;
		item.click();
		const first = count();
		expect(first).toBeGreaterThan(0);
		item.click(); // route は同じ（signal は変わらない）
		expect(count()).toBeGreaterThan(first);
	});

	it("設定中に lane を選ぶと設定を抜けてその lane へ", () => {
		const win = fixture();
		click(win, "settings");
		win.selectLane("vp/lane/other");
		expect(q(win, ".vp-settings-panel")).toBeNull();
		expect(current(win)).toBe("lanes");
		expect(q(win, "#app-shell")!.dataset.route).toBe("/lanes/vp/lane/other");
	});
});

describe("slim（peek を畳む）", () => {
	it("選択中の rail を押すと slim、もう一度押すと full", () => {
		const win = fixture();
		click(win, "lanes");
		expect(q(win, "#sidebar-root")!.classList.contains("slim")).toBe(true);
		expect(q(win, "[data-expanded]")!.getAttribute("data-expanded")).toBe("false");
		expect(current(win)).toBe("lanes");
		click(win, "lanes");
		expect(q(win, "#sidebar-root")!.classList.contains("slim")).toBe(false);
		expect(shown(win)).toEqual(["lanes"]);
	});

	it("slim では列の中央に repo badge、下端に daemon dot（旧 slim 帯と同じ監視の形）", () => {
		const win = fixture();
		click(win, "lanes");
		const badges = win.document.querySelectorAll(
			'nav[aria-label="Rail"] .anchor-center .vp-slim-badge',
		);
		expect(Array.from(badges).map((b) => b.textContent)).toEqual(["V", "C"]);
		expect(q(win, 'nav[aria-label="Rail"] .anchor-bottom .vp-slim-foot')).not.toBeNull();
	});

	it("full では badge 列を出さない（名簿と二重にしない）", () => {
		const win = fixture();
		expect(q(win, ".vp-slim-badge")).toBeNull();
	});

	it("slim の badge を押すと Lanes の peek を開いて戻る", () => {
		const win = fixture();
		click(win, "actions");
		click(win, "actions");
		expect(q(win, "#sidebar-root")!.classList.contains("slim")).toBe(true);
		(win.document.querySelectorAll(".vp-slim-badge")[0] as unknown as HTMLElement).click();
		expect(q(win, "#sidebar-root")!.classList.contains("slim")).toBe(false);
		expect(current(win)).toBe("lanes");
	});

	it("別の rail を slim で押すと、その rail で peek が開く", () => {
		const win = fixture();
		click(win, "lanes");
		click(win, "actions");
		expect(q(win, "#sidebar-root")!.classList.contains("slim")).toBe(false);
		expect(shown(win)).toEqual(["actions"]);
	});
});

describe("選択行への追随（mako 2026-11-06「サイドバーも、表示されるように追随する」）", () => {
	it("⚠️ Actions を見ている間に選んだ lane へ、Lanes に戻った時に寄せる", () => {
		const win = fixture();
		const calls: string[] = [];
		(win as unknown as { HTMLElement: { prototype: { scrollIntoView(): void } } }).HTMLElement.prototype.scrollIntoView =
			function (this: { getAttribute(n: string): string | null }) {
				const addr = this.getAttribute("data-address");
				if (addr) calls.push(addr);
			};
		click(win, "actions");
		win.selectLane("vp/lane/lead");
		calls.length = 0;
		click(win, "lanes");
		expect(calls).toContain("vp/lane/lead");
	});
});

describe("Lanes rail の用事 dot", () => {
	it("input 待ちの lane が在る間だけ Lanes の icon に dot が付く", () => {
		const win = fixture();
		expect(q(win, 'button[data-rail="lanes"] .vp-rail-flag')).toBeNull();
		win.setAwaiting("vp/lane/lead");
		expect(q(win, 'button[data-rail="lanes"] .vp-rail-flag')).not.toBeNull();
	});
});

describe("directive が rail に追随する", () => {
	// ⚠️ 実際の打鍵と同じく body から撃つ — window の capture 段（mode の handler）と document の
	// bubble 段（SettingsPanel の Esc）の**両方**を通る。window に直接撃つと後者を踏めない。
	const key = (win: Probe, k: string) =>
		win.document.body.dispatchEvent(new win.KeyboardEvent("keydown", { key: k, bubbles: true }));

	it("`b`（ACTIONS capture）は Actions の peek を開き、取り消すと元の rail と形に戻る", () => {
		const win = fixture();
		click(win, "lanes"); // slim にしておく
		win.runCaptureMode();
		expect(current(win)).toBe("actions");
		expect(shown(win)).toEqual(["actions"]);
		expect(q(win, "#sidebar-root")!.classList.contains("slim")).toBe(false);
		key(win, "Escape");
		expect(current(win)).toBe("lanes");
		expect(q(win, "#app-shell")!.dataset.route).toBe("/lanes/vp/lane/lead");
		expect(q(win, "#sidebar-root")!.classList.contains("slim")).toBe(true);
	});

	it("`b` で区画を選んだら Actions に留まる（これから打ち込むので畳まない）", () => {
		const win = fixture();
		win.runCaptureMode();
		key(win, "1");
		expect(current(win)).toBe("actions");
		expect(shown(win)).toEqual(["actions"]);
	});

	it("`b` を設定から打つと設定を抜け、取り消すと設定に戻る", () => {
		const win = fixture();
		click(win, "settings");
		win.runCaptureMode();
		expect(current(win)).toBe("actions");
		expect(q(win, ".vp-settings-panel")).toBeNull();
		key(win, "Escape");
		expect(current(win)).toBe("settings");
		// ⚠️ 取り消しの Esc は capture mode が消費する — 戻した設定を同じ Esc で閉じない
		expect(q(win, "#host .vp-settings-panel")).not.toBeNull();
	});

	it("⚠️ capture 中に user が別の rail へ移ったら、取り消し（timeout / Esc）で巻き戻さない", () => {
		const win = fixture();
		win.runCaptureMode();
		click(win, "settings");
		key(win, "Escape");
		expect(current(win)).toBe("settings");
		expect(q(win, "#host .vp-settings-panel")).not.toBeNull();
	});

	it("⚠️ 設定から capture に入り、その間に lane を選んで取り消すと、設定ではなくその lane へ", () => {
		const win = fixture();
		click(win, "settings");
		win.runCaptureMode();
		win.selectLane("vp/lane/other");
		key(win, "Escape");
		expect(current(win)).toBe("lanes");
		expect(q(win, ".vp-settings-panel")).toBeNull();
		expect(q(win, "#app-shell")!.dataset.route).toBe("/lanes/vp/lane/other");
	});

	it("Lanes から capture に入り、その間に lane を選んで取り消すと、route は今の lane", () => {
		const win = fixture();
		win.runCaptureMode();
		win.selectLane("vp/lane/other");
		key(win, "Escape");
		expect(q(win, "#app-shell")!.dataset.route).toBe("/lanes/vp/lane/other");
	});

	it("capture 中に ACTIONS の入力欄へ打ち始めたら、Actions に留まる（打っている面を奪わない）", () => {
		const win = fixture();
		win.runCaptureMode();
		// happy-dom の要素と event で揃える（q() は DOM 型に寄せて返すので、ここは素の querySelector）
		const memo = win.document.querySelector('[data-peek="actions"] textarea')!;
		memo.dispatchEvent(new win.KeyboardEvent("keydown", { key: "x", bubbles: true }));
		expect(current(win)).toBe("actions");
		expect(shown(win)).toEqual(["actions"]);
	});

	it("`n`（New Sub）は Actions にいても Lanes の peek を開いてから form を出す", () => {
		const win = fixture();
		click(win, "actions");
		win.runNewSub();
		expect(current(win)).toBe("lanes");
		expect(shown(win)).toEqual(["lanes"]);
		expect(q(win, '[data-peek="lanes"] .vp-add-sub-form')).not.toBeNull();
	});
});
