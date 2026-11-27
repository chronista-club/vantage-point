// @vitest-environment happy-dom
// ⚠️ happy-dom なのは route.ts が `@chronista-club/creo-ui/shells` を借りるため（module 評価時に window を引く）。
/**
 * 3 つの registry（doc 87 §4 / §8.2）— 表と、表が指す実物が 1:1 であることを並べて固定する。
 *
 * | registry | 表 | 実物 |
 * |---|---|---|
 * | pane kind（center の種類） | `APP_PANE_REGISTRY`（app-pane-registry.ts） | `#host` の `.pane[data-app-pane]`（main_area.rs の静的 HTML） |
 * | edge resident（lane の右 edge の住人） | `EDGE_RESIDENT_REGISTRY`（edge-residents.ts） | `#edge-rail` の 3 起点の slot（同上）+ EdgeRail.tsx の描画 + R sidebar への付け替え |
 * | rail（左 edge の相） | `SHELL_RAILS`（src/sidebar/route.ts） | CURail の列と peek（ShellRail.tsx） |
 *
 * ## ⚠️ ここで守るのは PL §5.2 の open-closed
 *
 * どの検査も**表を反復して**書く — 表に 1 行足すと、その行の検査が自動で増える（足した行が実物に
 * 出ていなければここで落ちる）。rail は表に 1 行足した版でも同じ検査を回し、描画・選択・peek が
 * 表だけで決まることを確かめる。導出（場・巡回・kind 解決・並び・付け替え）の open-closed は
 * 各 module の test（`app-pane-registry.test.ts` / `edge-residents.test.ts` / `src/sidebar/route.test.ts`）。
 *
 * 静的 HTML は**ファイルからそのまま**読む（写しを持たない — 写しは本物が変わっても古いまま残る）。
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { solidTestPlugin as solidPlugin, callDom, flushDom, evalDom, clickDom, dispatchDom } from './test-utils/solid'
import { Window } from "happy-dom";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { APP_PANE_REGISTRY, appPaneIdsOf } from "./app-pane-registry";
import {
	EDGE_ANCHORS,
	EDGE_RESIDENT_REGISTRY,
	edgeResidentsAt,
	relocatedResidentHostIds,
} from "./edge-residents";
import { SHELL_RAILS } from "./src/sidebar/route";

const here = dirname(fileURLToPath(import.meta.url));
const rs = readFileSync(join(here, "../src/webview/main_area.rs"), "utf8");
/** 額縁の静的 DOM（bundle の <script> は読み込まない）。 */
const staticBody = rs.match(/<body>([\s\S]*?)<\/body>/)![1].replace(/<script[\s\S]*?<\/script>/g, "");

type Probe = Window & {
	mountRails(extra: boolean): void;
	setPath(p: string): void;
	clicked: string[];
	mountEdge(lane: string): void;
};

let bundle: string;
const windows: Window[] = [];

beforeAll(async () => {
	const result = await build({
		stdin: {
			contents: `
        import { render } from '@solidjs/web';
        import { createComponent, createSignal } from 'solid-js';
        import { ShellRail } from './src/sidebar/ShellRail';
        import { SHELL_RAILS } from './src/sidebar/route';
        import { mountEdgeRail } from './EdgeRail';
        import { installRightSidebar } from './right-sidebar';
        import { EDGE_RESIDENT_REGISTRY, relocatedResidentHostIds } from './edge-residents';

        // ---- rail: 表（+ 1 行）を CURail に渡す。icon / peek は rail id を名乗る仮の中身 ----
        window.mountRails = (extra) => {
          const icon = (id) => () => { const s = document.createElement('i'); s.className = 'icon-' + id; return s; };
          const peek = (id) => () => { const d = document.createElement('div'); d.className = 'peek-' + id; return d; };
          const specs = extra ? [...SHELL_RAILS, ${JSON.stringify(EXTRA_RAIL_SPEC())}] : SHELL_RAILS;
          const registry = specs.map((r) => ({ id: r.id, route: r.route, order: r.order, labelKey: r.label,
            anchor: r.anchor, icon: icon(r.id), peek: peek(r.id) }));
          const [path, setPath] = createSignal('/lanes');
          window.setPath = setPath;
          window.clicked = [];
          render(() => createComponent(ShellRail, {
            registry,
            get pathname() { return path(); },
            peekOpen: true,
            onSelect: (id) => window.clicked.push(id),
          }), document.getElementById('sidebar-root'));
        };

        // ---- edge resident: entry.tsx と同じ結線（EdgeRail の mount + R sidebar の付け替え対象）----
        window.mountEdge = (lane) => {
          const byId = (id) => document.getElementById(id);
          const rail = mountEdgeRail(byId('edge-rail'), byId);
          const rsb = installRightSidebar({
            log: byId('rsb-log'),
            tabs: Array.from(document.querySelectorAll('#right-sidebar .rsb-tab')),
            sections: Array.from(document.querySelectorAll('#right-sidebar .rsb-section')),
            laneBody: byId('rsb-lane-body'),
            residents: relocatedResidentHostIds(EDGE_RESIDENT_REGISTRY).map(byId).filter((el) => el !== null),
            prsBody: byId('rsb-prs-body'),
            prsCount: byId('rsb-prs-count'),
            prsRefresh: byId('rsb-prs-refresh'),
          });
          rail.setLane(lane);
          rsb.setLane(lane);
        };
      `,
			resolveDir: here,
			loader: "tsx",
		},
		bundle: true,
		write: false,
		format: "iife",
		platform: "browser",
		preserveSymlinks: true,
		plugins: [solidPlugin()],
		loader: { ".css": "text" },
		define: { "process.env.NODE_ENV": '"production"' },
	});
	bundle = result.outputFiles[0].text;
});

/** 表に 1 行足す rail（拡張の契約を確かめる仮の相）。上起点の Actions の後に並ぶ。 */
function EXTRA_RAIL_SPEC() {
	return { id: "gallery", route: "/gallery", order: 1.5, label: "Gallery", anchor: "top" } as const;
}

function fixture(html: string): Probe {
	const win = new Window({
		settings: {
			enableJavaScriptEvaluation: true,
			suppressInsecureJavaScriptEnvironmentWarning: true,
		},
	}) as unknown as Probe;
	windows.push(win);
	win.document.body.innerHTML = html;
	evalDom(win, bundle);
	return win;
}

afterEach(async () => {
	for (const win of windows.splice(0)) await win.happyDOM.abort();
});

/** happy-dom の NodeList は DOM lib の Iterable<Element> と型が合わないので、読む口だけを要求する。 */
type ElList = { length: number; [i: number]: { id: string; getAttribute(n: string): string | null } };
const idsOf = (els: ElList) => Array.from({ length: els.length }, (_, i) => els[i].id);
const attrOf = (els: ElList, name: string) =>
	Array.from({ length: els.length }, (_, i) => els[i].getAttribute(name));

// ============================================================================
// 1. pane kind — 表 ⇄ #host の静的 pane
// ============================================================================

describe("pane kind registry ⇄ #host の静的 pane（doc 49 R1: pane の DOM は作り直さない）", () => {
	it("`.pane[data-app-pane]` の集合と表の住所は 1:1", () => {
		const win = fixture(staticBody);
		const ids = attrOf(win.document.querySelectorAll(".pane[data-app-pane]"), "data-app-pane");
		expect([...ids].sort()).toEqual(appPaneIdsOf(APP_PANE_REGISTRY).sort());
	});

	for (const pane of APP_PANE_REGISTRY) {
		it(`${pane.id}: #${pane.domId} は #host 直下の .pane で、住所と正規の kind を名乗る`, () => {
			const win = fixture(staticBody);
			const el = win.document.getElementById(pane.domId);
			expect(el, `#${pane.domId} が静的 HTML に無い`).not.toBeNull();
			expect(el!.classList.contains("pane")).toBe(true);
			expect(el!.parentElement!.id).toBe("host");
			expect(el!.getAttribute("data-app-pane")).toBe(pane.id);
			// slot rect の kind（active-pane.ts の sendSlotRect）は data-kind を読む
			expect(el!.getAttribute("data-kind")).toBe(pane.kinds[0]);
		});
	}
});

// ============================================================================
// 2. edge resident — 表 ⇄ #edge-rail の 3 起点 ⇄ EdgeRail の描画 ⇄ R sidebar の付け替え
// ============================================================================

describe("edge resident registry ⇄ #edge-rail の 3 起点（doc 82）", () => {
	const slotIds = (win: Probe, anchor: string) =>
		idsOf(win.document.querySelectorAll(`#edge-rail > .anchor-${anchor} > [id]`));

	it("帯の slot は上 / 中央 / 下の 3 つ、この順", () => {
		const win = fixture(staticBody);
		const slots = Array.from(win.document.querySelectorAll("#edge-rail > [class*='anchor-']")).map(
			(el) => el.className.split(/\s+/).find((c) => c.startsWith("anchor-")),
		);
		expect(slots).toEqual(EDGE_ANCHORS.map((a) => `anchor-${a}`));
	});

	for (const anchor of EDGE_ANCHORS) {
		it(`.anchor-${anchor} の住人は表の ${anchor} 起点と同じ顔ぶれ・同じ並び`, () => {
			const win = fixture(staticBody);
			expect(slotIds(win, anchor)).toEqual(
				edgeResidentsAt(EDGE_RESIDENT_REGISTRY, anchor).map((r) => r.hostId),
			);
		});
	}

	for (const r of EDGE_RESIDENT_REGISTRY) {
		it(`${r.id}: 静的 HTML の host は中身の持ち主（${r.content}）に合っている`, () => {
			const win = fixture(staticBody);
			const host = win.document.getElementById(r.hostId)!;
			// static = 静的 HTML が中身ごと持つ / solid = 空の host（EdgeRail.tsx が描く）
			if (r.content === "static") expect(host.children.length).toBeGreaterThan(0);
			else expect(host.children.length).toBe(0);
		});
	}
});

describe("edge resident registry ⇄ EdgeRail の描画と R sidebar の付け替え（entry.tsx と同じ結線）", () => {
	const LANE = "vp/lane/lead";
	const mounted = (): Probe => {
		const win = fixture(staticBody);
		callDom(win, () => win.mountEdge(LANE));
		// しまった pane は lane-panes が流す一覧の鏡 — 1 枚しまってあることにする
		dispatchDom(win.document, new win.CustomEvent("vp:stowed-panes", {
				detail: { lane: LANE, panes: [{ id: "board", kind: "board", label: "Board" }] },
			}));
		return win;
	};
	const toggle = (win: Probe) =>
		dispatchDom(win.document, new win.CustomEvent("vp:right-sidebar-toggle"));

	for (const r of EDGE_RESIDENT_REGISTRY.filter((x) => x.content === "solid")) {
		it(`${r.id}: EdgeRail が #${r.hostId} に描く`, () => {
			const win = mounted();
			expect(win.document.getElementById(r.hostId)!.children.length).toBeGreaterThan(0);
		});
	}

	it("R を開くと、付け替える住人だけが Lane 節へ rail の並びで移る", () => {
		const win = mounted();
		toggle(win);
		const laneBody = win.document.getElementById("rsb-lane-body")!;
		expect(idsOf(laneBody.querySelectorAll(":scope > [id]"))).toEqual(
			relocatedResidentHostIds(EDGE_RESIDENT_REGISTRY),
		);
		for (const r of EDGE_RESIDENT_REGISTRY.filter((x) => !x.relocateToRightSidebar)) {
			expect(win.document.getElementById(r.hostId)!.closest("#edge-rail"), r.id).not.toBeNull();
		}
	});

	it("R を閉じると、全員が自分の起点の元の位置へ戻る", () => {
		const win = mounted();
		toggle(win);
		toggle(win);
		for (const anchor of EDGE_ANCHORS) {
			expect(idsOf(win.document.querySelectorAll(`#edge-rail > .anchor-${anchor} > [id]`)), anchor).toEqual(
				edgeResidentsAt(EDGE_RESIDENT_REGISTRY, anchor).map((r) => r.hostId),
			);
		}
	});
});

// ============================================================================
// 3. rail — 表（と 1 行足した表）⇄ CURail の描画・選択・peek（PL §5.2 の枠そのもの）
// ============================================================================

describe.each([
	{ name: "表のまま", extra: false, rails: [...SHELL_RAILS] },
	{ name: "1 行足した表", extra: true, rails: [...SHELL_RAILS, EXTRA_RAIL_SPEC()] },
])("rail registry ⇄ CURail（$name）", ({ extra, rails }) => {
	const mounted = (): Probe => {
		const win = fixture('<div id="sidebar-root"></div>');
		callDom(win, () => win.mountRails(extra));
		return win;
	};
	const byOrder = [...rails].sort((a, b) => a.order - b.order);

	it("列の icon は表の order の順", () => {
		const win = mounted();
		expect(attrOf(win.document.querySelectorAll("button[data-rail]"), "data-rail")).toEqual(
			byOrder.map((r) => r.id),
		);
	});

	for (const rail of rails) {
		it(`${rail.id}: ${rail.anchor} 起点の列に label 付きで描かれる`, () => {
			const win = mounted();
			const btn = win.document.querySelector(
				`nav[aria-label="Rail"] .anchor-${rail.anchor} button[data-rail="${rail.id}"]`,
			);
			expect(btn, `${rail.id} が ${rail.anchor} 起点に無い`).not.toBeNull();
			expect(btn!.getAttribute("aria-label")).toBe(rail.label);
			expect(btn!.querySelector(`.icon-${rail.id}`)).not.toBeNull();
		});

		it(`${rail.id}: route（とその下位）で選ばれ、peek に自分の中身が出る`, () => {
			const win = mounted();
			for (const path of [rail.route, `${rail.route}/sub`]) {
				callDom(win, () => win.setPath(path));
				expect(
					attrOf(win.document.querySelectorAll('button[data-rail][aria-current="page"]'), "data-rail"),
					path,
				).toEqual([rail.id]);
				const shown = Array.from(win.document.querySelectorAll("[data-peek]"))
					.filter((el) => !el.hasAttribute("hidden"))
					.map((el) => el.getAttribute("data-peek"));
				expect(shown, path).toEqual([rail.id]);
				expect(win.document.querySelector(`[data-peek="${rail.id}"] .peek-${rail.id}`)).not.toBeNull();
			}
		});

		it(`${rail.id}: click は rail id を呼び手に渡す`, () => {
			const win = mounted();
			clickDom((win.document.querySelector(`button[data-rail="${rail.id}"]`) as unknown as HTMLElement));
			expect(win.clicked).toEqual([rail.id]);
		});
	}
});
